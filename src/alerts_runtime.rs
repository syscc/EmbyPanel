use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tokio::sync::mpsc;

use crate::{
    alerts::{AlertNotifyEvent, AlertRuleView, Alerts, MAX_EVALUATION_WINDOW_SECS, now_ms},
    alerts_api::attempt_webhook_for_notification,
    db::RequestStatKind,
    error::{AppResult, safe_error_message},
};

const EVALUATION_INTERVAL: Duration = Duration::from_secs(5);
const METRIC_QUEUE_CAPACITY: usize = 8_192;
const MAX_TRACKED_SERVERS: usize = 128;
const RECENT_BUCKET_SECS: u64 = 5;
const RECENT_RETENTION_SECS: u64 = 10 * 60;
const LONG_BUCKET_SECS: u64 = 60;
const MAX_RECENT_BUCKETS: usize = (RECENT_RETENTION_SECS / RECENT_BUCKET_SECS) as usize + 2;
const MAX_LONG_BUCKETS: usize = (MAX_EVALUATION_WINDOW_SECS / LONG_BUCKET_SECS) as usize + 2;

#[derive(Clone)]
pub struct AlertRuntime {
    sender: Option<mpsc::Sender<MetricEvent>>,
    dropped: Arc<AtomicU64>,
    control: Option<Arc<RuntimeControl>>,
}

struct RuntimeControl {
    shutdown: tokio::sync::watch::Sender<bool>,
    handle: tokio::sync::Mutex<Option<tokio::task::JoinHandle<AlertRuntimeShutdown>>>,
    alerts: Alerts,
    ownership: Arc<DeliveryOwnership>,
    total_timeout: Duration,
}

#[derive(Debug)]
struct MetricEvent {
    server_id: String,
    kind: RequestStatKind,
    at_secs: u64,
}

#[derive(Debug)]
struct DeliveryJob {
    event: AlertNotifyEvent,
    payload: String,
}

#[derive(Debug)]
struct DeliveryOutcome {
    event: AlertNotifyEvent,
    result: Result<
        crate::alerts_api::WebhookDeliverySuccess,
        crate::alerts_api::WebhookDeliveryFailure,
    >,
}

const MAX_DELIVERY_CONCURRENCY: usize = 4;
const MAX_PENDING_DELIVERIES: usize = 256;
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TOTAL_TIMEOUT: Duration = Duration::from_secs(6);
const SHUTDOWN_ABORT_JOIN_TIMEOUT: Duration = Duration::from_millis(100);
const SHUTDOWN_FAILURE_MESSAGE: &str = "alert runtime shutdown deadline exceeded";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlertRuntimeShutdown {
    Drained,
    TimedOut { aborted_jobs: usize },
    TaskFailed,
}

#[derive(Default)]
struct DeliveryOwnership {
    by_rule: Mutex<HashMap<String, AlertNotifyEvent>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnershipClaim {
    Claimed,
    AlreadyOwned,
    Conflicting,
}

impl DeliveryOwnership {
    fn claim(&self, event: &AlertNotifyEvent) -> OwnershipClaim {
        let mut owned = self
            .by_rule
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match owned.get(&event.rule_id) {
            Some(current)
                if current.kind == event.kind
                    && current.notification_id == event.notification_id =>
            {
                OwnershipClaim::AlreadyOwned
            }
            Some(_) => OwnershipClaim::Conflicting,
            None => {
                owned.insert(event.rule_id.clone(), event.clone());
                OwnershipClaim::Claimed
            }
        }
    }

    fn release(&self, event: &AlertNotifyEvent) {
        let mut owned = self
            .by_rule
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let matches = owned.get(&event.rule_id).is_some_and(|current| {
            current.kind == event.kind && current.notification_id == event.notification_id
        });
        if matches {
            owned.remove(&event.rule_id);
        }
    }

    fn snapshot(&self) -> Vec<AlertNotifyEvent> {
        self.by_rule
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn take_all(&self) -> Vec<AlertNotifyEvent> {
        std::mem::take(
            &mut *self
                .by_rule
                .lock()
                .unwrap_or_else(|error| error.into_inner()),
        )
        .into_values()
        .collect()
    }
}

#[derive(Debug)]
enum EnqueueError {
    Full(DeliveryJob),
    AlreadyOwned,
    Conflicting(DeliveryJob),
}

struct DeliveryQueue {
    pending: VecDeque<DeliveryJob>,
    active: tokio::task::JoinSet<DeliveryOutcome>,
    active_events: HashMap<tokio::task::Id, AlertNotifyEvent>,
    active_count: usize,
    ownership: Arc<DeliveryOwnership>,
}

impl Default for DeliveryQueue {
    fn default() -> Self {
        Self::with_ownership(Arc::new(DeliveryOwnership::default()))
    }
}

impl DeliveryQueue {
    fn with_ownership(ownership: Arc<DeliveryOwnership>) -> Self {
        Self {
            pending: VecDeque::new(),
            active: tokio::task::JoinSet::new(),
            active_events: HashMap::new(),
            active_count: 0,
            ownership,
        }
    }

    fn has_active(&self) -> bool {
        self.active_count > 0
    }

    fn has_work(&self) -> bool {
        self.has_active() || !self.pending.is_empty()
    }

    fn enqueue(&mut self, job: DeliveryJob) -> Result<(), EnqueueError> {
        if self.pending.len() >= MAX_PENDING_DELIVERIES {
            return Err(EnqueueError::Full(job));
        }
        let claim = self.ownership.claim(&job.event);
        match claim {
            OwnershipClaim::Claimed => {
                self.pending.push_back(job);
                Ok(())
            }
            OwnershipClaim::AlreadyOwned => Err(EnqueueError::AlreadyOwned),
            OwnershipClaim::Conflicting => Err(EnqueueError::Conflicting(job)),
        }
    }

    fn start_available(&mut self, alerts: &Alerts, client: &reqwest::Client) {
        while self.active_count < MAX_DELIVERY_CONCURRENCY {
            let Some(job) = self.pending.pop_front() else {
                break;
            };
            if !alerts.notification_is_current(&job.event).unwrap_or(false) {
                self.ownership.release(&job.event);
                continue;
            }
            let job_event = job.event;
            let rule_id = job_event.rule_id.clone();
            let payload = job.payload;
            let active_event = job_event.clone();
            let alerts = alerts.clone();
            let client = client.clone();
            let task = self.active.spawn(async move {
                let result = attempt_webhook_for_notification(
                    &alerts,
                    &client,
                    &rule_id,
                    &payload,
                    Some(&job_event),
                )
                .await;
                DeliveryOutcome {
                    event: job_event,
                    result,
                }
            });
            self.active_events.insert(task.id(), active_event);
            self.active_count += 1;
        }
    }

    fn take_completed(
        &mut self,
    ) -> Option<Result<(tokio::task::Id, DeliveryOutcome), tokio::task::JoinError>> {
        let result = self.active.try_join_next_with_id()?;
        self.active_count = self.active_count.saturating_sub(1);
        Some(result)
    }

    fn handle_completed(
        &mut self,
        alerts: &Alerts,
        result: Result<(tokio::task::Id, DeliveryOutcome), tokio::task::JoinError>,
    ) {
        if let Some(event) = handle_delivery_result(alerts, result, &mut self.active_events) {
            self.ownership.release(&event);
        }
    }

    fn renew_owned_leases(&self, alerts: &Alerts, renewed_at: u128) {
        let events = self.ownership.snapshot();
        if events.is_empty() {
            return;
        }
        if let Err(error) = alerts.renew_notification_leases(&events, renewed_at) {
            tracing::warn!(
                error = %error.safe_log_message(),
                "failed to renew alert delivery reservations"
            );
        }
    }

    async fn shutdown(
        &mut self,
        alerts: &Alerts,
        client: &reqwest::Client,
        drain_timeout: Duration,
    ) -> AlertRuntimeShutdown {
        let deadline = Instant::now() + drain_timeout;
        loop {
            self.start_available(alerts, client);
            if !self.has_work() {
                return AlertRuntimeShutdown::Drained;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, self.active.join_next_with_id()).await {
                Ok(Some(result)) => {
                    self.active_count = self.active_count.saturating_sub(1);
                    self.handle_completed(alerts, result);
                }
                Ok(None) => {
                    self.active_count = 0;
                    break;
                }
                Err(_) => break,
            }
        }

        let pending_count = self.pending.len();
        let active_count = self.active_count;
        self.active.abort_all();
        while self.active.join_next_with_id().await.is_some() {}
        self.active_count = 0;
        self.active_events.clear();
        self.pending.clear();
        let events = self.ownership.take_all();
        if let Err(error) =
            alerts.fail_notification_deliveries(&events, SHUTDOWN_FAILURE_MESSAGE, 0, now_ms())
        {
            tracing::warn!(
                error = %error.safe_log_message(),
                "failed to finalize alert reservations during shutdown"
            );
        }
        let aborted_jobs = pending_count.saturating_add(active_count).max(events.len());
        tracing::warn!(
            aborted_jobs,
            "alert runtime shutdown deadline reached; jobs aborted"
        );
        AlertRuntimeShutdown::TimedOut { aborted_jobs }
    }
}

impl AlertRuntime {
    pub fn start(alerts: Alerts, client: reqwest::Client) -> Self {
        Self::start_with_timeouts(
            alerts,
            client,
            EVALUATION_INTERVAL,
            SHUTDOWN_DRAIN_TIMEOUT,
            SHUTDOWN_TOTAL_TIMEOUT,
        )
    }

    fn start_with_timeouts(
        alerts: Alerts,
        client: reqwest::Client,
        evaluation_interval: Duration,
        drain_timeout: Duration,
        total_timeout: Duration,
    ) -> Self {
        let (sender, receiver) = mpsc::channel(METRIC_QUEUE_CAPACITY);
        let (shutdown, shutdown_receiver) = tokio::sync::watch::channel(false);
        let dropped = Arc::new(AtomicU64::new(0));
        let ownership = Arc::new(DeliveryOwnership::default());
        let runtime_alerts = alerts.clone();
        let runtime_ownership = ownership.clone();
        let handle = tokio::spawn(run_alert_runtime_loop_with_ownership(
            alerts,
            client,
            receiver,
            shutdown_receiver,
            evaluation_interval,
            drain_timeout,
            runtime_ownership,
        ));
        Self {
            sender: Some(sender),
            dropped,
            control: Some(Arc::new(RuntimeControl {
                shutdown,
                handle: tokio::sync::Mutex::new(Some(handle)),
                alerts: runtime_alerts,
                ownership,
                total_timeout,
            })),
        }
    }

    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            sender: None,
            dropped: Arc::new(AtomicU64::new(0)),
            control: None,
        }
    }

    pub async fn shutdown(&self) -> AlertRuntimeShutdown {
        let Some(control) = self.control.as_ref() else {
            return AlertRuntimeShutdown::Drained;
        };
        let _ = control.shutdown.send(true);
        let Some(mut handle) = control.handle.lock().await.take() else {
            return AlertRuntimeShutdown::Drained;
        };
        match tokio::time::timeout(control.total_timeout, &mut handle).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                tracing::warn!(error = %error, "alert runtime task failed during shutdown");
                let events = control.ownership.take_all();
                if !events.is_empty()
                    && let Err(cleanup_error) = control.alerts.fail_notification_deliveries(
                        &events,
                        SHUTDOWN_FAILURE_MESSAGE,
                        0,
                        now_ms(),
                    )
                {
                    tracing::warn!(
                        error = %cleanup_error.safe_log_message(),
                        "failed to finalize alert reservations after runtime task failure"
                    );
                }
                AlertRuntimeShutdown::TaskFailed
            }
            Err(_) => {
                handle.abort();
                if tokio::time::timeout(SHUTDOWN_ABORT_JOIN_TIMEOUT, &mut handle)
                    .await
                    .is_err()
                {
                    tracing::warn!("alert runtime task did not stop within abort grace period");
                }
                let events = control.ownership.take_all();
                if let Err(error) = control.alerts.fail_notification_deliveries(
                    &events,
                    SHUTDOWN_FAILURE_MESSAGE,
                    0,
                    now_ms(),
                ) {
                    tracing::warn!(
                        error = %error.safe_log_message(),
                        "failed to finalize alert reservations after shutdown timeout"
                    );
                }
                tracing::warn!(
                    aborted_jobs = events.len(),
                    "alert runtime shutdown exceeded total deadline; jobs aborted"
                );
                AlertRuntimeShutdown::TimedOut {
                    aborted_jobs: events.len(),
                }
            }
        }
    }

    pub fn record_stat(&self, server_id: String, kind: RequestStatKind) {
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        let event = MetricEvent {
            server_id,
            kind,
            at_secs: unix_seconds(),
        };
        if sender.try_send(event).is_err() {
            let dropped = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if dropped.is_power_of_two() {
                tracing::warn!(
                    dropped,
                    "alert runtime metric queue is full; samples dropped"
                );
            }
        }
    }
}

async fn run_alert_runtime_loop(
    alerts: Alerts,
    client: reqwest::Client,
    receiver: mpsc::Receiver<MetricEvent>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    evaluation_interval: Duration,
) -> AlertRuntimeShutdown {
    run_alert_runtime_loop_with_ownership(
        alerts,
        client,
        receiver,
        shutdown,
        evaluation_interval,
        SHUTDOWN_DRAIN_TIMEOUT,
        Arc::new(DeliveryOwnership::default()),
    )
    .await
}

async fn run_alert_runtime_loop_with_ownership(
    alerts: Alerts,
    client: reqwest::Client,
    mut receiver: mpsc::Receiver<MetricEvent>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    evaluation_interval: Duration,
    drain_timeout: Duration,
    ownership: Arc<DeliveryOwnership>,
) -> AlertRuntimeShutdown {
    let mut metrics = RuntimeMetrics::new(unix_seconds());
    let mut interval = tokio::time::interval(evaluation_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    interval.tick().await;
    let mut deliveries = DeliveryQueue::with_ownership(ownership);

    let mut last_history_prune = Instant::now();
    loop {
        if *shutdown.borrow() {
            break;
        }
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
            }
            event = receiver.recv() => {
                let Some(event) = event else {
                    break;
                };
                metrics.record(event.server_id, event.kind, event.at_secs);
            }
            result = deliveries.active.join_next_with_id(), if deliveries.has_active() => {
                if let Some(result) = result {
                    deliveries.active_count = deliveries.active_count.saturating_sub(1);
                    deliveries.handle_completed(&alerts, result);
                    deliveries.start_available(&alerts, &client);
                }
            }
            _ = interval.tick() => {
                if last_history_prune.elapsed() >= Duration::from_secs(60 * 60) {
                    if let Err(error) = alerts.prune_delivery_history() {
                        tracing::warn!(error = %error.safe_log_message(), "failed to prune alert delivery history");
                    }
                    last_history_prune = Instant::now();
                }
                while let Ok(event) = receiver.try_recv() {
                    metrics.record(event.server_id, event.kind, event.at_secs);
                }
                while let Some(result) = deliveries.take_completed() {
                    deliveries.handle_completed(&alerts, result);
                }
                deliveries.start_available(&alerts, &client);
                deliveries.renew_owned_leases(&alerts, now_ms());
                evaluate_and_queue(&alerts, &mut metrics, &mut deliveries);
                deliveries.start_available(&alerts, &client);
            }
        }
    }

    deliveries.shutdown(&alerts, &client, drain_timeout).await
}

fn evaluate_and_queue(
    alerts: &Alerts,
    metrics: &mut RuntimeMetrics,
    deliveries: &mut DeliveryQueue,
) {
    let now_secs = unix_seconds();
    let events = match collect_notify_events(alerts, metrics, now_secs, now_ms()) {
        Ok(events) => events,
        Err(error) => {
            tracing::warn!(
                error = %error.safe_log_message(),
                "failed to evaluate alert rules"
            );
            return;
        }
    };

    for event in events {
        let payload = match serde_json::to_string(&event) {
            Ok(payload) => payload,
            Err(_) => {
                finish_failed_delivery(alerts, &event, "alert payload serialization failed", 0);
                continue;
            }
        };
        match deliveries.enqueue(DeliveryJob { event, payload }) {
            Ok(()) | Err(EnqueueError::AlreadyOwned) => {}
            Err(EnqueueError::Full(job)) => {
                // The reservation is intentionally finalized as a failed delivery so the
                // alert state remains retryable instead of being held by an unbounded queue.
                finish_failed_delivery(alerts, &job.event, "alert delivery queue is full", 0);
            }
            Err(EnqueueError::Conflicting(job)) => {
                // A healthy runtime renews its reservation before evaluation. If an
                // external evaluator nevertheless replaced it, do not enqueue a second
                // logical notification; finalize the replacement and let the owner finish.
                finish_failed_delivery(
                    alerts,
                    &job.event,
                    "alert delivery already owned by runtime",
                    0,
                );
            }
        }
    }
}

fn handle_delivery_result(
    alerts: &Alerts,
    result: Result<(tokio::task::Id, DeliveryOutcome), tokio::task::JoinError>,
    active_events: &mut HashMap<tokio::task::Id, AlertNotifyEvent>,
) -> Option<AlertNotifyEvent> {
    let outcome = match result {
        Ok((id, outcome)) => {
            active_events.remove(&id);
            outcome
        }
        Err(error) => {
            let event = active_events.remove(&error.id());
            if let Some(event) = event.as_ref() {
                finish_failed_delivery(alerts, event, "alert Webhook task exited unexpectedly", 0);
            }
            tracing::warn!(error = %error, "alert Webhook task exited unexpectedly");
            return event;
        }
    };
    match outcome.result {
        Ok(success) => {
            let message = format!("{} delivery success", outcome.event.kind);
            if let Err(error) = alerts.finish_notification_delivery(
                &outcome.event,
                true,
                &message,
                success.retry_count,
                now_ms(),
            ) {
                tracing::warn!(
                    rule_id = %outcome.event.rule_id,
                    kind = %outcome.event.kind,
                    error = %error.safe_log_message(),
                    "failed to persist successful alert delivery"
                );
            }
        }
        Err(failure) => {
            let error_message = safe_error_message(&failure.error);
            if let Err(error) = alerts.finish_notification_delivery(
                &outcome.event,
                false,
                &failure.message,
                failure.retry_count,
                now_ms(),
            ) {
                tracing::warn!(
                    rule_id = %outcome.event.rule_id,
                    kind = %outcome.event.kind,
                    error = %error.safe_log_message(),
                    "failed to persist failed alert delivery"
                );
            }
            tracing::warn!(
                rule_id = %outcome.event.rule_id,
                kind = %outcome.event.kind,
                error = %error_message,
                "alert Webhook delivery failed"
            );
        }
    }
    Some(outcome.event)
}

fn finish_failed_delivery(
    alerts: &Alerts,
    event: &AlertNotifyEvent,
    message: &str,
    retry_count: u32,
) {
    if let Err(error) =
        alerts.finish_notification_delivery(event, false, message, retry_count, now_ms())
    {
        tracing::warn!(
            rule_id = %event.rule_id,
            kind = %event.kind,
            error = %error.safe_log_message(),
            "failed to persist failed alert delivery"
        );
    }
}

fn collect_notify_events(
    alerts: &Alerts,
    metrics: &mut RuntimeMetrics,
    now_secs: u64,
    now_millis: u128,
) -> AppResult<Vec<AlertNotifyEvent>> {
    let rules = alerts.get_alert_rules()?;
    let interval_values = metrics.take_interval_deltas();
    let mut events = Vec::new();

    for rule in rules
        .into_iter()
        .filter(|rule| rule.enabled && !rule.metric.is_empty())
    {
        let Some(observed) = observed_value(&rule, metrics, &interval_values, now_secs) else {
            continue;
        };
        let threshold = Alerts::parse_numeric_threshold(&rule.threshold)?;
        let is_breaching = Alerts::compare_metric(&rule.operator, observed, threshold)?;
        if let Some(event) =
            alerts.evaluate_condition_at(&rule.id, is_breaching, Some(observed), now_millis)?
        {
            events.push(event);
        }
    }

    Ok(events)
}

fn observed_value(
    rule: &AlertRuleView,
    metrics: &RuntimeMetrics,
    interval_values: &HashMap<String, MetricTotals>,
    now_secs: u64,
) -> Option<f64> {
    let value = if rule.evaluation_window_secs == 0 {
        metric_from_map(interval_values, &rule.metric, &rule.server_id)
    } else {
        metrics.window_value(
            &rule.metric,
            &rule.server_id,
            rule.evaluation_window_secs,
            now_secs,
        )?
    };
    Some(value as f64)
}

fn metric_from_map(values: &HashMap<String, MetricTotals>, metric: &str, server_id: &str) -> u64 {
    if server_id.is_empty() {
        values
            .values()
            .fold(0_u64, |sum, value| sum.saturating_add(value.metric(metric)))
    } else {
        values
            .get(server_id)
            .map(|value| value.metric(metric))
            .unwrap_or(0)
    }
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MetricTotals([u64; 5]);

impl MetricTotals {
    fn increment(&mut self, kind: RequestStatKind) {
        let index = match kind {
            RequestStatKind::Request => 0,
            RequestStatKind::Redirect => 1,
            RequestStatKind::CacheHit => 2,
            RequestStatKind::Block => 3,
            RequestStatKind::Error => 4,
        };
        self.0[index] = self.0[index].saturating_add(1);
    }

    fn delta_since(self, previous: Self) -> Self {
        let mut delta = Self::default();
        for (index, value) in self.0.into_iter().enumerate() {
            delta.0[index] = value.saturating_sub(previous.0[index]);
        }
        delta
    }

    fn add_assign(&mut self, other: Self) {
        for (index, value) in other.0.into_iter().enumerate() {
            self.0[index] = self.0[index].saturating_add(value);
        }
    }

    fn metric(self, metric: &str) -> u64 {
        match metric {
            "proxy.requests" => self.0[0],
            "proxy.redirects" => self.0[1],
            "proxy.cache_hits" => self.0[2],
            "proxy.blocks" => self.0[3],
            "proxy.errors" => self.0[4],
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct MetricBucket {
    start_secs: u64,
    totals: MetricTotals,
}

#[derive(Default)]
struct ServerMetrics {
    totals: MetricTotals,
    last_interval_totals: MetricTotals,
    recent: VecDeque<MetricBucket>,
    long: VecDeque<MetricBucket>,
}

impl ServerMetrics {
    fn record(&mut self, kind: RequestStatKind, at_secs: u64) {
        self.totals.increment(kind);
        Self::record_bucket(
            &mut self.recent,
            at_secs - at_secs % RECENT_BUCKET_SECS,
            kind,
            MAX_RECENT_BUCKETS,
        );
        Self::record_bucket(
            &mut self.long,
            at_secs - at_secs % LONG_BUCKET_SECS,
            kind,
            MAX_LONG_BUCKETS,
        );
    }

    fn record_bucket(
        buckets: &mut VecDeque<MetricBucket>,
        start_secs: u64,
        kind: RequestStatKind,
        max_buckets: usize,
    ) {
        if let Some(bucket) = buckets.back_mut()
            && bucket.start_secs == start_secs
        {
            bucket.totals.increment(kind);
            return;
        }
        if buckets
            .back()
            .is_some_and(|bucket| bucket.start_secs > start_secs)
        {
            return;
        }
        let mut totals = MetricTotals::default();
        totals.increment(kind);
        buckets.push_back(MetricBucket { start_secs, totals });
        while buckets.len() > max_buckets {
            buckets.pop_front();
        }
    }

    fn take_interval_delta(&mut self) -> MetricTotals {
        let delta = self.totals.delta_since(self.last_interval_totals);
        self.last_interval_totals = self.totals;
        delta
    }

    fn window_totals(&self, window_secs: u64, now_secs: u64) -> MetricTotals {
        let cutoff = now_secs.saturating_sub(window_secs);
        let (buckets, bucket_secs) = if window_secs <= RECENT_RETENTION_SECS {
            (&self.recent, RECENT_BUCKET_SECS)
        } else {
            (&self.long, LONG_BUCKET_SECS)
        };
        let mut totals = MetricTotals::default();
        for bucket in buckets {
            if bucket.start_secs <= now_secs
                && bucket.start_secs.saturating_add(bucket_secs) > cutoff
            {
                totals.add_assign(bucket.totals);
            }
        }
        totals
    }
}

struct RuntimeMetrics {
    started_at_secs: u64,
    servers: HashMap<String, ServerMetrics>,
    dropped_servers: u64,
}

impl RuntimeMetrics {
    fn new(started_at_secs: u64) -> Self {
        Self {
            started_at_secs,
            servers: HashMap::new(),
            dropped_servers: 0,
        }
    }

    fn record(&mut self, server_id: String, kind: RequestStatKind, at_secs: u64) {
        if !self.servers.contains_key(&server_id) && self.servers.len() >= MAX_TRACKED_SERVERS {
            self.dropped_servers = self.dropped_servers.saturating_add(1);
            if self.dropped_servers.is_power_of_two() {
                tracing::warn!(
                    dropped = self.dropped_servers,
                    max_servers = MAX_TRACKED_SERVERS,
                    "alert runtime server limit reached; samples dropped"
                );
            }
            return;
        }
        self.servers
            .entry(server_id)
            .or_default()
            .record(kind, at_secs);
    }

    fn take_interval_deltas(&mut self) -> HashMap<String, MetricTotals> {
        self.servers
            .iter_mut()
            .map(|(server_id, metrics)| (server_id.clone(), metrics.take_interval_delta()))
            .collect()
    }

    fn window_value(
        &self,
        metric: &str,
        server_id: &str,
        window_secs: u64,
        now_secs: u64,
    ) -> Option<u64> {
        if now_secs < self.started_at_secs.saturating_add(window_secs) {
            return None;
        }
        if server_id.is_empty() {
            Some(self.servers.values().fold(0_u64, |sum, server| {
                sum.saturating_add(server.window_totals(window_secs, now_secs).metric(metric))
            }))
        } else {
            Some(
                self.servers
                    .get(server_id)
                    .map(|server| server.window_totals(window_secs, now_secs).metric(metric))
                    .unwrap_or(0),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alerts::{AlertRule, NOTIFICATION_LEASE_MS};
    use std::sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    };
    use std::time::Instant;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinSet,
    };

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_alerts() -> (String, Alerts) {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/alerts_runtime_test_{}_{}.db", std::process::id(), seq);
        let _ = std::fs::remove_file(&path);
        (path.clone(), Alerts::new(&path).unwrap())
    }

    fn rule(id: &str, server_id: &str, window: u64, threshold: &str) -> AlertRule {
        rule_with_url(id, server_id, window, threshold, "http://127.0.0.1:9/hook")
    }

    fn rule_with_url(
        id: &str,
        server_id: &str,
        window: u64,
        threshold: &str,
        url: &str,
    ) -> AlertRule {
        AlertRule {
            id: id.to_string(),
            name: id.to_string(),
            enabled: true,
            rule_config: url.to_string(),
            threshold: threshold.to_string(),
            silence_period: 30,
            created_at: 1,
            metric: "proxy.requests".to_string(),
            operator: "gte".to_string(),
            server_id: server_id.to_string(),
            evaluation_window_secs: window,
        }
    }

    async fn wait_for_requests(received: &AtomicUsize, expected: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while received.load(Ordering::Acquire) < expected {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for Webhook request"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    async fn failing_webhook_server(
        listener: TcpListener,
        expected_requests: usize,
        received: Arc<AtomicUsize>,
    ) {
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).await;
            received.fetch_add(1, Ordering::Release);
        }
    }

    async fn hanging_webhook_server(
        listener: TcpListener,
        expected_requests: usize,
        received: Arc<AtomicUsize>,
    ) {
        let mut handlers = JoinSet::new();
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().await.unwrap();
            let received = received.clone();
            handlers.spawn(async move {
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).await;
                received.fetch_add(1, Ordering::Release);
                std::future::pending::<()>().await;
            });
        }
        while handlers.join_next().await.is_some() {}
    }

    async fn delayed_webhook_server(
        listener: TcpListener,
        expected_requests: usize,
        delay: Duration,
        received: Arc<AtomicUsize>,
    ) {
        let mut handlers = JoinSet::new();
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().await.unwrap();
            let received = received.clone();
            handlers.spawn(async move {
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request).await;
                received.fetch_add(1, Ordering::Release);
                tokio::time::sleep(delay).await;
                let response =
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";
                let _ = stream.write_all(response).await;
            });
        }
        while handlers.join_next().await.is_some() {}
    }

    #[tokio::test]
    async fn slow_webhooks_do_not_block_later_metric_evaluation() {
        let (path, alerts) = temp_alerts();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let received = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(delayed_webhook_server(
            listener,
            3,
            Duration::from_millis(250),
            received.clone(),
        ));
        for id in ["slow-a", "slow-b", "later"] {
            alerts
                .create_alert_rule(rule_with_url(id, id, 0, "1", &url))
                .unwrap();
        }

        let (metrics_tx, metrics_rx) = mpsc::channel(16);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let runtime = tokio::spawn(run_alert_runtime_loop(
            alerts.clone(),
            reqwest::Client::new(),
            metrics_rx,
            shutdown_rx,
            Duration::from_millis(15),
        ));
        for server_id in ["slow-a", "slow-b"] {
            metrics_tx
                .send(MetricEvent {
                    server_id: server_id.to_string(),
                    kind: RequestStatKind::Request,
                    at_secs: unix_seconds(),
                })
                .await
                .unwrap();
        }
        wait_for_requests(&received, 2).await;
        let later_metric_at = Instant::now();
        metrics_tx
            .send(MetricEvent {
                server_id: "later".to_string(),
                kind: RequestStatKind::Request,
                at_secs: unix_seconds(),
            })
            .await
            .unwrap();
        wait_for_requests(&received, 3).await;
        assert!(later_metric_at.elapsed() < Duration::from_millis(220));

        let _ = shutdown_tx.send(true);
        tokio::time::timeout(Duration::from_secs(2), runtime)
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
        let deliveries = alerts.list_alert_deliveries(None, 100).unwrap();
        assert_eq!(deliveries.len(), 3);
        assert!(
            deliveries
                .iter()
                .all(|delivery| delivery.status == "success")
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn owned_queue_renews_lease_and_stale_completion_is_ignored() {
        let (path, alerts) = temp_alerts();
        alerts
            .create_alert_rule(rule("lease-owned", "", 0, "1"))
            .unwrap();
        alerts
            .create_alert_rule(rule("lease-stale", "", 0, "1"))
            .unwrap();
        let t0 = 10_000_u128;

        let owned = alerts
            .evaluate_condition_at("lease-owned", true, Some(1.0), t0)
            .unwrap()
            .expect("owned firing reservation");
        let mut queue = DeliveryQueue::default();
        queue
            .enqueue(DeliveryJob {
                event: owned.clone(),
                payload: serde_json::to_string(&owned).unwrap(),
            })
            .unwrap();
        let after_old_lease = t0 + NOTIFICATION_LEASE_MS + 1;
        queue.renew_owned_leases(&alerts, after_old_lease);
        assert!(
            alerts
                .evaluate_condition_at("lease-owned", true, Some(2.0), after_old_lease + 1)
                .unwrap()
                .is_none()
        );
        let owned_state = alerts.get_alert_state("lease-owned").unwrap();
        assert_eq!(
            owned_state.pending_notification_id,
            Some(owned.notification_id)
        );
        assert_eq!(queue.pending.len(), 1);
        assert!(
            alerts
                .finish_notification_delivery(
                    &owned,
                    true,
                    "firing delivery success",
                    0,
                    after_old_lease + 2,
                )
                .unwrap()
        );
        queue.ownership.release(&owned);
        assert_eq!(
            alerts
                .list_alert_deliveries(Some("lease-owned"), 10)
                .unwrap()
                .len(),
            1
        );

        let stale = alerts
            .evaluate_condition_at("lease-stale", true, Some(1.0), t0)
            .unwrap()
            .expect("stale firing reservation");
        let replacement = alerts
            .evaluate_condition_at(
                "lease-stale",
                true,
                Some(2.0),
                t0 + NOTIFICATION_LEASE_MS + 1,
            )
            .unwrap()
            .expect("replacement after process-style stale lease");
        assert_ne!(replacement.notification_id, stale.notification_id);
        assert!(
            !alerts
                .finish_notification_delivery(
                    &stale,
                    true,
                    "stale delivery success",
                    0,
                    t0 + NOTIFICATION_LEASE_MS + 2,
                )
                .unwrap()
        );
        assert!(
            alerts
                .finish_notification_delivery(
                    &replacement,
                    false,
                    "network delivery failed",
                    0,
                    t0 + NOTIFICATION_LEASE_MS + 3,
                )
                .unwrap()
        );
        let deliveries = alerts
            .list_alert_deliveries(Some("lease-stale"), 10)
            .unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].status, "failed");
        assert_ne!(deliveries[0].message, "stale delivery success");

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn shutdown_deadline_aborts_jobs_and_releases_reservations() {
        let (path, alerts) = temp_alerts();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let received = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(hanging_webhook_server(
            listener,
            MAX_DELIVERY_CONCURRENCY,
            received.clone(),
        ));
        let rule_ids = (0..MAX_DELIVERY_CONCURRENCY + 1)
            .map(|index| format!("shutdown-{index}"))
            .collect::<Vec<_>>();
        for rule_id in &rule_ids {
            alerts
                .create_alert_rule(rule_with_url(rule_id, rule_id, 0, "1", &url))
                .unwrap();
        }
        let runtime = AlertRuntime::start_with_timeouts(
            alerts.clone(),
            reqwest::Client::new(),
            Duration::from_millis(10),
            Duration::from_millis(80),
            Duration::from_millis(300),
        );
        for rule_id in &rule_ids {
            runtime.record_stat(rule_id.clone(), RequestStatKind::Request);
        }
        wait_for_requests(&received, MAX_DELIVERY_CONCURRENCY).await;

        let started = Instant::now();
        let result = runtime.shutdown().await;
        let elapsed = started.elapsed();
        assert!(matches!(
            result,
            AlertRuntimeShutdown::TimedOut { aborted_jobs }
                if aborted_jobs == MAX_DELIVERY_CONCURRENCY + 1
        ));
        assert!(elapsed >= Duration::from_millis(60));
        assert!(elapsed < Duration::from_millis(500));
        server.abort();
        let _ = server.await;

        for rule_id in &rule_ids {
            let state = alerts.get_alert_state(rule_id).unwrap();
            assert!(state.pending_notification_kind.is_none());
            assert_eq!(state.retry_notification_kind.as_deref(), Some("firing"));
            assert!(!state.notification_active);
            let deliveries = alerts.list_alert_deliveries(Some(rule_id), 10).unwrap();
            assert_eq!(deliveries.len(), 1);
            assert_eq!(deliveries[0].status, "failed");
            assert_eq!(deliveries[0].message, SHUTDOWN_FAILURE_MESSAGE);
        }
        let retry = alerts
            .evaluate_condition_at(&rule_ids[0], true, Some(2.0), now_ms().saturating_add(1))
            .unwrap()
            .expect("shutdown failure remains retryable");
        assert_eq!(retry.kind, "firing");

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn shutdown_signal_joins_alert_runtime() {
        let (path, alerts) = temp_alerts();
        let runtime = AlertRuntime::start(alerts, reqwest::Client::new());
        tokio::time::timeout(Duration::from_secs(1), runtime.shutdown())
            .await
            .expect("alert runtime did not shut down");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn failed_delivery_clears_lease_and_remains_retryable() {
        let (path, alerts) = temp_alerts();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/hook", listener.local_addr().unwrap());
        let received = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(failing_webhook_server(listener, 4, received.clone()));
        alerts
            .create_alert_rule(rule_with_url("failed", "", 0, "1", &url))
            .unwrap();
        let (metrics_tx, metrics_rx) = mpsc::channel(4);
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let runtime = tokio::spawn(run_alert_runtime_loop(
            alerts.clone(),
            reqwest::Client::new(),
            metrics_rx,
            shutdown_rx,
            Duration::from_millis(10),
        ));
        metrics_tx
            .send(MetricEvent {
                server_id: "failed".to_string(),
                kind: RequestStatKind::Request,
                at_secs: unix_seconds(),
            })
            .await
            .unwrap();

        wait_for_requests(&received, 1).await;
        let _ = shutdown_tx.send(true);
        tokio::time::timeout(Duration::from_secs(2), runtime)
            .await
            .unwrap()
            .unwrap();
        server.await.unwrap();
        let state = alerts.get_alert_state("failed").unwrap();
        assert!(state.pending_notification_kind.is_none());
        assert_eq!(state.retry_notification_kind.as_deref(), Some("firing"));
        assert!(!state.notification_active);
        let deliveries = alerts.list_alert_deliveries(Some("failed"), 10).unwrap();
        assert!(
            deliveries
                .iter()
                .any(|delivery| delivery.status == "failed")
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn interval_metrics_are_server_filtered_and_consumed_once() {
        let mut metrics = RuntimeMetrics::new(100);
        metrics.record("a".to_string(), RequestStatKind::Request, 101);
        metrics.record("a".to_string(), RequestStatKind::Redirect, 101);
        metrics.record("b".to_string(), RequestStatKind::Request, 102);

        let first = metrics.take_interval_deltas();
        assert_eq!(metric_from_map(&first, "proxy.requests", ""), 2);
        assert_eq!(metric_from_map(&first, "proxy.requests", "a"), 1);
        assert_eq!(metric_from_map(&first, "proxy.redirects", "a"), 1);

        let second = metrics.take_interval_deltas();
        assert_eq!(metric_from_map(&second, "proxy.requests", ""), 0);
    }

    #[test]
    fn rolling_window_filters_servers_and_waits_for_warmup() {
        let mut metrics = RuntimeMetrics::new(100);
        metrics.record("a".to_string(), RequestStatKind::Request, 105);
        metrics.record("b".to_string(), RequestStatKind::Request, 130);
        metrics.record("b".to_string(), RequestStatKind::Block, 130);

        assert_eq!(metrics.window_value("proxy.requests", "", 60, 159), None);
        assert_eq!(metrics.window_value("proxy.requests", "", 60, 160), Some(2));
        assert_eq!(
            metrics.window_value("proxy.requests", "a", 30, 160),
            Some(0)
        );
        assert_eq!(
            metrics.window_value("proxy.requests", "b", 30, 160),
            Some(1)
        );
        assert_eq!(metrics.window_value("proxy.blocks", "b", 30, 160), Some(1));
    }

    #[test]
    fn automatic_evaluation_uses_window_and_server_filter() {
        let (path, alerts) = temp_alerts();
        alerts
            .create_alert_rule(rule("only-a", "a", 60, "2"))
            .unwrap();
        alerts.create_alert_rule(rule("all", "", 60, "3")).unwrap();

        let mut metrics = RuntimeMetrics::new(100);
        metrics.record("a".to_string(), RequestStatKind::Request, 110);
        metrics.record("a".to_string(), RequestStatKind::Request, 120);
        metrics.record("b".to_string(), RequestStatKind::Request, 130);

        let events = collect_notify_events(&alerts, &mut metrics, 160, 160_000).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|event| event.rule_id == "only-a"));
        assert!(events.iter().any(|event| event.rule_id == "all"));

        let _ = std::fs::remove_file(path);
    }
    #[tokio::test]
    async fn disabled_pending_job_drops_without_http_or_history() {
        let (path, alerts) = temp_alerts();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/review-repro", listener.local_addr().unwrap());
        alerts
            .create_alert_rule(rule_with_url("review-disabled-pending", "a", 0, "1", &url))
            .unwrap();
        let event = alerts
            .evaluate_condition_at("review-disabled-pending", true, Some(1.0), now_ms())
            .unwrap()
            .expect("firing reservation");
        let mut queue = DeliveryQueue::default();
        queue
            .enqueue(DeliveryJob {
                payload: serde_json::to_string(&event).unwrap(),
                event: event.clone(),
            })
            .unwrap();
        assert_eq!(queue.pending.len(), 1);
        assert_eq!(queue.active_count, 0);
        alerts.toggle_rule(&event.rule_id, false).unwrap();
        assert!(
            !alerts
                .get_alert_rule(&event.rule_id)
                .unwrap()
                .unwrap()
                .enabled
        );
        assert!(
            alerts
                .get_alert_state(&event.rule_id)
                .unwrap()
                .pending_notification_id
                .is_none()
        );

        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        queue.start_available(&alerts, &client);
        let shutdown = queue
            .shutdown(&alerts, &client, Duration::from_secs(2))
            .await;
        assert_eq!(shutdown, AlertRuntimeShutdown::Drained);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_err()
        );
        assert!(queue.ownership.snapshot().is_empty());
        assert!(
            alerts
                .list_alert_deliveries(Some(&event.rule_id), 10)
                .unwrap()
                .is_empty(),
            "invalidated queued jobs must not create delivery history"
        );
        assert!(
            !alerts
                .get_alert_state(&event.rule_id)
                .unwrap()
                .notification_active
        );
        drop(queue);
        drop(alerts);
        for suffix in ["", ".key", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{suffix}"));
        }
    }

    #[tokio::test]
    async fn invalidation_at_each_retry_boundary_stops_http_and_releases_ownership() {
        for change in ["disable", "update", "webhook", "reservation"] {
            for stop_after in 1..=crate::alerts::MAX_RETRY_COUNT {
                let (path, alerts) = temp_alerts();
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let url = format!("http://{}/retry", listener.local_addr().unwrap());
                let rule = rule_with_url("retry-boundary", "a", 0, "1", &url);
                alerts.create_alert_rule(rule.clone()).unwrap();
                let event = alerts
                    .evaluate_condition_at(&rule.id, true, Some(1.0), now_ms())
                    .unwrap()
                    .unwrap();
                let received = Arc::new(AtomicUsize::new(0));
                let server_alerts = alerts.clone();
                let server_received = received.clone();
                let server_event = event.clone();
                let server = tokio::spawn(async move {
                    for attempt in 1..=stop_after {
                        let (mut stream, _) = listener.accept().await.unwrap();
                        let mut request = [0; 4096];
                        stream.read(&mut request).await.unwrap();
                        server_received.fetch_add(1, Ordering::Release);
                        if attempt == stop_after {
                            match change {
                                "disable" => server_alerts.toggle_rule(&rule.id, false).unwrap(),
                                "update" => {
                                    let mut updated = rule.clone();
                                    updated.threshold = "2".into();
                                    server_alerts
                                        .update_alert_rule(
                                            updated,
                                            crate::alerts::RuleConfigUpdate::Preserve,
                                        )
                                        .unwrap();
                                }
                                "webhook" => {
                                    server_alerts
                                        .update_alert_rule(
                                            rule.clone(),
                                            crate::alerts::RuleConfigUpdate::Replace(url.clone()),
                                        )
                                        .unwrap();
                                }
                                _ => {
                                    let replacement = server_alerts
                                        .evaluate_condition_at(
                                            &rule.id,
                                            true,
                                            Some(1.0),
                                            now_ms() + crate::alerts::NOTIFICATION_LEASE_MS + 1,
                                        )
                                        .unwrap()
                                        .unwrap();
                                    assert_ne!(
                                        replacement.notification_id,
                                        server_event.notification_id
                                    );
                                }
                            }
                        }
                        stream.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    }
                    assert!(
                        tokio::time::timeout(Duration::from_millis(800), listener.accept())
                            .await
                            .is_err()
                    );
                });
                let client = reqwest::Client::builder().no_proxy().build().unwrap();
                let mut queue = DeliveryQueue::default();
                queue
                    .enqueue(DeliveryJob {
                        payload: "{}".into(),
                        event,
                    })
                    .unwrap();
                queue.start_available(&alerts, &client);
                assert_eq!(
                    queue
                        .shutdown(&alerts, &client, Duration::from_secs(10))
                        .await,
                    AlertRuntimeShutdown::Drained
                );
                server.await.unwrap();
                assert_eq!(received.load(Ordering::Acquire), stop_after as usize);
                assert!(queue.ownership.snapshot().is_empty());
                assert!(alerts.list_alert_deliveries(None, 100).unwrap().is_empty());
                drop(queue);
                drop(alerts);
                for suffix in ["", ".key", "-wal", "-shm"] {
                    let _ = std::fs::remove_file(format!("{path}{suffix}"));
                }
            }
        }
    }

    #[tokio::test]
    async fn manual_webhook_test_remains_available_for_disabled_rule() {
        let (path, alerts) = temp_alerts();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/manual", listener.local_addr().unwrap());
        alerts
            .create_alert_rule(rule_with_url("manual-disabled", "a", 0, "1", &url))
            .unwrap();
        alerts.toggle_rule("manual-disabled", false).unwrap();
        let received = Arc::new(AtomicUsize::new(0));
        let server = tokio::spawn(delayed_webhook_server(
            listener,
            1,
            Duration::ZERO,
            received.clone(),
        ));
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        crate::alerts_api::attempt_webhook(&alerts, &client, "manual-disabled", "{}")
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(received.load(Ordering::Acquire), 1);
        assert!(
            alerts
                .get_alert_state("manual-disabled")
                .unwrap()
                .pending_notification_id
                .is_none()
        );
        drop(alerts);
        for suffix in ["", ".key", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{path}{suffix}"));
        }
    }
}
