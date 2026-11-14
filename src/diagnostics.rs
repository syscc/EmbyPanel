//! Playback diagnosis collector, record construction, and security sanitization.
//!
//! Security boundary: every record that enters the buffer or leaves via the HTTP API
//! must pass through [`sanitize_diagnosis_record`]. Callers must still avoid feeding
//! raw credentials when possible; sanitization is defense-in-depth, not a license to
//! log secrets.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};

/// Stable placeholder substituted for secret values.
pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";

/// Maximum number of diagnosis records retained in memory.
pub const DIAGNOSTICS_CAPACITY: usize = 1000;

const MAX_DETAIL_CHARS: usize = 1024;
const MAX_ID_CHARS: usize = 128;
const MAX_UA_CHARS: usize = 512;
const MAX_STEP_NAME_CHARS: usize = 64;
const MAX_QUERY_VALUE_CHARS: usize = 256;

const SENSITIVE_KEYS: &[&str] = &[
    "api_key",
    "api-key",
    "apikey",
    "access_token",
    "access-token",
    "refresh_token",
    "token",
    "password",
    "passwd",
    "authorization",
    "cookie",
    "set-cookie",
    "x-emby-token",
    "x-mediabrowser-token",
    "x-emby-authorization",
    "x-mediabrowser-authorization",
    "secret",
    "client_secret",
    "credential",
    "sign",
    "signature",
    "sig",
    "x-amz-signature",
    "x-amz-credential",
    "x-amz-security-token",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiagnosisRecord {
    pub request_id: String,
    /// Unix epoch milliseconds; absent on records collected before timing was supported.
    #[serde(default)]
    pub started_at_ms: Option<i64>,
    #[serde(default)]
    pub finished_at_ms: Option<i64>,
    /// Monotonic elapsed time for the whole diagnosis, not the sum of its steps.
    #[serde(default)]
    pub duration_ms: Option<u64>,
    pub server_id: String,
    pub media_id: String,
    pub ua: String,
    pub steps: Vec<DiagnosisStep>,
    pub cache_hit: bool,
    pub failure_node: Option<String>,
    pub sanitized_errors: Vec<String>,
    #[serde(default)]
    pub context: DiagnosisContext,
    #[serde(default)]
    pub result: DiagnosisResult,
    #[serde(default)]
    pub cache: DiagnosisCache,
    #[serde(default)]
    pub paths: DiagnosisPaths,
    #[serde(default)]
    pub timing: DiagnosisTiming,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DiagnosisStep {
    pub name: String,
    #[serde(default)]
    pub started_at_ms: Option<i64>,
    #[serde(default)]
    pub finished_at_ms: Option<i64>,
    pub duration_ms: u64,
    pub details: String,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub upstream_status: Option<u16>,
    #[serde(default)]
    pub error_kind: Option<String>,
    #[serde(default)]
    pub response_headers_ms: Option<u64>,
    #[serde(default)]
    pub response_body_ms: Option<u64>,
    /// Completed resolver-call wait sum for this operation, including executor
    /// scheduling; not DNS packet RTT. Parallel event sums may exceed wall time.
    #[serde(default)]
    pub name_resolution_ms: Option<u64>,
    /// Completed resolver calls (success or error); excludes pending/cancelled calls.
    #[serde(default)]
    pub name_resolution_calls: Option<u32>,
    /// Completed connector-attempt sum, including DNS/proxy/CONNECT/TLS. Attempts
    /// need not supply this response's connection; never add to step/wall duration.
    #[serde(default)]
    pub connection_setup_ms: Option<u64>,
    /// Completed connector attempts (success or error), not pending/cancelled ones.
    #[serde(default)]
    pub connection_attempts: Option<u32>,
    /// Same not_observed/observed/partial semantics as DiagnosisTiming; legacy None.
    #[serde(default)]
    pub connection_observation: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosisContext {
    pub server_name: Option<String>,
    pub media_name: Option<String>,
    pub series_name: Option<String>,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
    pub media_source_id: Option<String>,
    pub user_name: Option<String>,
    pub client_name: Option<String>,
    pub device_name: Option<String>,
    pub device_id: Option<String>,
    pub source_ip: Option<String>,
    pub method: Option<String>,
    pub request_path: Option<String>,
    pub range: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosisResult {
    /// redirect/proxy/failed/cancelled/blocked, or absent when not observed.
    pub outcome: Option<String>,
    pub status_code: Option<u16>,
    pub target_host: Option<String>,
    pub reason: Option<String>,
    pub error_kind: Option<String>,
    pub upstream_status: Option<u16>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosisCache {
    /// hit/miss/disabled/not_checked; a miss does not prove expiry.
    pub status: Option<String>,
    pub stored: Option<bool>,
    pub reason: Option<String>,
    pub ttl_seconds: Option<u64>,
    pub remaining_ttl_ms: Option<u64>,
    pub target_host: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosisPaths {
    pub original: Option<String>,
    pub mapped: Option<String>,
    pub matched_rules: Vec<usize>,
    pub resolved: Option<String>,
    pub final_url: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagnosisTiming {
    pub slowest_step: Option<String>,
    pub slowest_step_ms: Option<u64>,
    /// Whole-trace time not covered by real operation intervals (union, not sum).
    pub unaccounted_ms: Option<u64>,
    pub redirect_count: Option<u32>,
    /// Completed resolver-call wait totals, including blocking-executor scheduling;
    /// not DNS packet RTT. Event sums can overlap and exceed wall-clock duration.
    #[serde(default)]
    pub name_resolution_ms: Option<u64>,
    /// Completed calls (success or error), absent when no call completed.
    #[serde(default)]
    pub name_resolution_calls: Option<u32>,
    /// Completed connector-attempt totals (DNS/proxy/CONNECT/TLS included). An
    /// attempt need not supply the response's connection. Never add these totals
    /// or the contained resolver time to duration/unaccounted; TCP/TLS unsupported.
    #[serde(default)]
    pub connection_setup_ms: Option<u64>,
    /// Completed attempts (success or error), excluding cancelled/pending ones.
    #[serde(default)]
    pub connection_attempts: Option<u32>,
    /// not_observed: no captured events (not proof of pool reuse); observed: all started
    /// events completed before send ended; partial: cancelled/in-flight events
    /// omitted. Legacy records remain None/unknown. No claim of response usage.
    #[serde(default)]
    pub connection_observation: Option<String>,
}

#[derive(Clone)]
pub struct DiagnosisCollector {
    buf: Arc<Mutex<Vec<DiagnosisRecord>>>,
    capacity: usize,
    persistence: Arc<Mutex<Option<crate::db::TelemetryWriteQueue>>>,
}

impl DiagnosisCollector {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            buf: Arc::new(Mutex::new(Vec::with_capacity(capacity))),
            capacity,
            persistence: Arc::new(Mutex::new(None)),
        }
    }

    pub fn get_records(&self) -> Vec<DiagnosisRecord> {
        // Safe snapshot under short lock; does not drain retention buffer.
        let buf = self.buf.lock().unwrap();
        buf.clone()
    }

    pub fn clear(&self) {
        let mut buf = self.buf.lock().unwrap();
        buf.clear();
    }

    pub async fn record(&self, record: DiagnosisRecord) {
        self.ingest(record);
    }

    fn ingest(&self, record: DiagnosisRecord) {
        // Sanitize at the collector edge so raw records cannot bypass
        // create_diagnosis_record. The write is synchronous so a successful
        // record().await is immediately visible and `new()` does not need a runtime.
        let record = sanitize_diagnosis_record(record);
        let evicted = {
            #[cfg(test)]
            TEST_BEFORE_BUFFER_LOCK.with(|hook| {
                if let Some(hook) = hook.borrow_mut().take() {
                    hook();
                }
            });
            let mut guard = self.buf.lock().unwrap();
            // Retain the newest records instead of draining the whole buffer when full.
            let evicted = if guard.len() >= self.capacity {
                Some(guard.remove(0))
            } else {
                None
            };
            guard.push(record.clone());
            // Commit memory before enqueue while holding the same short buffer lock.
            // Once the FIFO clear barrier sees this write, clear() can no longer race
            // with a delayed memory push and resurrect it. Enqueue never waits for SQLite.
            if let Some(queue) = self.persistence.lock().unwrap().as_ref() {
                queue.diagnosis_record(record);
            }
            evicted
        };
        if let Some(record) = evicted {
            tracing::info!(
                ?record,
                "playback diagnosis record evicted from retention buffer"
            );
        }
    }
}

static COLLECTOR: OnceLock<DiagnosisCollector> = OnceLock::new();
tokio::task_local! {
    /// Locally generated correlation id; never taken from request headers.
    pub(crate) static REQUEST_ID: String;
}

pub(crate) fn new_request_id() -> String {
    format!("diag-{:032x}", rand::random::<u128>())
}

pub(crate) fn current_request_id() -> Option<String> {
    REQUEST_ID.try_with(Clone::clone).ok()
}

/// Observations from an existing upstream request, without headers, bodies or URLs.
/// Header time covers send through response headers; body time covers JSON read/decode.
#[derive(Debug, Default)]
pub(crate) struct UpstreamObservation {
    pub status: Option<u16>,
    pub response_headers_ms: Option<u64>,
    pub response_body_ms: Option<u64>,
    pub error_kind: Option<String>,
    pub name_resolution_ms: Option<u64>,
    pub name_resolution_calls: Option<u32>,
    pub connection_setup_ms: Option<u64>,
    pub connection_attempts: Option<u32>,
    pub connection_observation: Option<String>,
}

// Both production clients and operation-level send hooks use this boundary. It
// carries numeric totals only, never request destinations or resolver addresses.
pub(crate) mod network {
    use super::{DiagnosisTiming, UpstreamObservation};
    use hyper_util::client::legacy::connect::dns::GaiResolver;
    use reqwest::dns::{Addrs, Name, Resolve, Resolving};
    use std::{
        future::Future,
        pin::Pin,
        sync::{Arc, Mutex},
        task::{Context, Poll},
        time::Instant,
    };
    use tower::{Layer, Service};

    tokio::task_local! {
        // Option explicitly masks any unrelated outer scope during every poll.
        static SEND_SCOPE: Option<Arc<Mutex<SendTotals>>>;
        static OPERATION_SCOPE: Arc<Mutex<UpstreamObservation>>;
    }

    #[derive(Debug, Default)]
    struct SendTotals {
        resolution_ms: Option<u64>,
        resolution_calls: Option<u32>,
        connection_ms: Option<u64>,
        connection_attempts: Option<u32>,
        in_flight: u32,
        partial: bool,
        closed: bool,
    }

    #[derive(Clone, Copy)]
    enum EventKind {
        Resolution,
        Connection,
    }

    struct Event {
        scope: Option<Arc<Mutex<SendTotals>>>,
        kind: EventKind,
        started: Instant,
    }

    fn current_scope() -> Option<Arc<Mutex<SendTotals>>> {
        SEND_SCOPE.try_with(Clone::clone).ok().flatten()
    }

    fn add<T: Copy>(total: &mut Option<T>, value: T, sum: impl FnOnce(T, T) -> T) {
        *total = Some(total.map_or(value, |previous| sum(previous, value)));
    }

    impl Event {
        fn start(kind: EventKind) -> Self {
            let scope = current_scope();
            if let Some(scope) = &scope {
                let mut totals = scope.lock().unwrap_or_else(|err| err.into_inner());
                if !totals.closed {
                    totals.in_flight = totals.in_flight.saturating_add(1);
                }
            }
            Self {
                scope,
                kind,
                started: Instant::now(),
            }
        }

        fn complete(&mut self) {
            let Some(scope) = self.scope.take() else {
                return;
            };
            let ms = self.started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            let mut totals = scope.lock().unwrap_or_else(|err| err.into_inner());
            if totals.closed {
                return;
            }
            totals.in_flight = totals.in_flight.saturating_sub(1);
            match self.kind {
                EventKind::Resolution => {
                    add(&mut totals.resolution_ms, ms, u64::saturating_add);
                    add(&mut totals.resolution_calls, 1, u32::saturating_add);
                }
                EventKind::Connection => {
                    add(&mut totals.connection_ms, ms, u64::saturating_add);
                    add(&mut totals.connection_attempts, 1, u32::saturating_add);
                }
            }
        }
    }

    impl Drop for Event {
        fn drop(&mut self) {
            if let Some(scope) = self.scope.take() {
                let mut totals = scope.lock().unwrap_or_else(|err| err.into_inner());
                if !totals.closed {
                    totals.in_flight = totals.in_flight.saturating_sub(1);
                    totals.partial = true;
                }
            }
        }
    }

    // Pin<Box<F>> keeps F pinned without unsafe projections or requiring F: Unpin.
    // The captured scope is reinstalled explicitly if hyper moves this future to
    // a pool background task. A spawn never inherits task-local state implicitly.
    pub(crate) struct ObservedFuture<F> {
        inner: Pin<Box<F>>,
        event: Event,
    }

    impl<F: Future> Future for ObservedFuture<F> {
        type Output = F::Output;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();
            let result =
                SEND_SCOPE.sync_scope(this.event.scope.clone(), || this.inner.as_mut().poll(cx));
            if result.is_ready() {
                this.event.complete();
            }
            result
        }
    }

    /// Same spawn_blocking + ToSocketAddrs resolver as reqwest 0.12.28's default.
    /// No sorting/filtering/port rewriting; reqwest's connector still owns ports.
    struct DefaultGaiResolver;

    impl Resolve for DefaultGaiResolver {
        fn resolve(&self, name: Name) -> Resolving {
            let mut resolver = GaiResolver::new();
            Box::pin(async move {
                let name = name.as_str().parse()?;
                resolver
                    .call(name)
                    .await
                    .map(|addrs| Box::new(addrs) as Addrs)
                    .map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync>)
            })
        }
    }

    struct ObservedResolver<R>(R);

    impl<R: Resolve> Resolve for ObservedResolver<R> {
        fn resolve(&self, name: Name) -> Resolving {
            let event = Event::start(EventKind::Resolution);
            let inner = self.0.resolve(name);
            Box::pin(ObservedFuture {
                inner: Box::pin(inner),
                event,
            })
        }
    }

    #[derive(Clone)]
    pub(crate) struct ObservationLayer;

    #[derive(Clone)]
    pub(crate) struct ObservedConnector<S>(S);

    impl<S> Layer<S> for ObservationLayer {
        type Service = ObservedConnector<S>;
        fn layer(&self, inner: S) -> Self::Service {
            ObservedConnector(inner)
        }
    }

    impl<S, Request> Service<Request> for ObservedConnector<S>
    where
        S: Service<Request>,
    {
        type Response = S::Response;
        type Error = S::Error;
        type Future = ObservedFuture<S::Future>;

        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.0.poll_ready(cx)
        }

        fn call(&mut self, request: Request) -> Self::Future {
            // In locked hyper-util 0.1.20, connect_to's lazy future polls the
            // reqwest connector (always ready here) before a pool loser can be
            // spawned. Capture here; every subsequent poll reinstalls this Arc.
            let event = Event::start(EventKind::Connection);
            // Exactly one existing connector invocation; no clone/retry/probe.
            let inner = self.0.call(request);
            ObservedFuture {
                inner: Box::pin(inner),
                event,
            }
        }
    }

    pub(crate) fn instrument_client(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        // reqwest wraps dns_resolver with its static overrides OUTSIDE this
        // observer. resolve()/resolve_to_addrs() therefore bypass resolver timing.
        builder
            .dns_resolver(Arc::new(ObservedResolver(DefaultGaiResolver)))
            .connector_layer(ObservationLayer)
    }

    struct SendGuard<'a> {
        scope: Arc<Mutex<SendTotals>>,
        observation: &'a mut UpstreamObservation,
        completed: bool,
        operation: Option<Arc<Mutex<UpstreamObservation>>>,
    }

    impl Drop for SendGuard<'_> {
        fn drop(&mut self) {
            let mut totals = self.scope.lock().unwrap_or_else(|err| err.into_inner());
            totals.closed = true;
            let state = if !self.completed || totals.partial || totals.in_flight > 0 {
                "partial"
            } else if totals.resolution_calls.is_some() || totals.connection_attempts.is_some() {
                "observed"
            } else {
                "not_observed"
            };
            let summary = UpstreamObservation {
                name_resolution_ms: totals.resolution_ms,
                name_resolution_calls: totals.resolution_calls,
                connection_setup_ms: totals.connection_ms,
                connection_attempts: totals.connection_attempts,
                connection_observation: Some(state.to_string()),
                ..Default::default()
            };
            drop(totals);
            merge_observation(self.observation, &summary);
            if let Some(operation) = &self.operation {
                let mut aggregate = operation.lock().unwrap_or_else(|err| err.into_inner());
                merge_observation(&mut aggregate, &summary);
            }
        }
    }

    /// One scope per existing send, not per playback handler. On cancellation the
    /// guard snapshots completed events; late background results cannot mutate it.
    /// The future is constructed lazily by production callers, so send() itself
    /// and all its polling run inside the scope. Not a network/request interceptor.
    pub(crate) async fn observe_send<F: Future>(
        observation: &mut UpstreamObservation,
        send: F,
    ) -> F::Output {
        let scope = Arc::new(Mutex::new(SendTotals::default()));
        let mut guard = SendGuard {
            scope: scope.clone(),
            observation,
            completed: false,
            operation: OPERATION_SCOPE.try_with(Clone::clone).ok(),
        };
        let result = SEND_SCOPE.scope(Some(scope), send).await;
        guard.completed = true;
        result
    }

    fn add_optional<T: Copy>(total: &mut Option<T>, value: Option<T>, sum: impl FnOnce(T, T) -> T) {
        if let Some(value) = value {
            add(total, value, sum);
        }
    }

    fn merge_state(total: &mut Option<String>, next: Option<String>) {
        let Some(next) = next else { return };
        let state = match (total.as_deref(), next.as_str()) {
            (Some("partial"), _) | (_, "partial") => "partial",
            (Some("observed"), _) | (_, "observed") => "observed",
            _ => "not_observed",
        };
        *total = Some(state.to_string());
    }

    fn merge_observation(target: &mut UpstreamObservation, source: &UpstreamObservation) {
        add_optional(
            &mut target.name_resolution_ms,
            source.name_resolution_ms,
            u64::saturating_add,
        );
        add_optional(
            &mut target.name_resolution_calls,
            source.name_resolution_calls,
            u32::saturating_add,
        );
        add_optional(
            &mut target.connection_setup_ms,
            source.connection_setup_ms,
            u64::saturating_add,
        );
        add_optional(
            &mut target.connection_attempts,
            source.connection_attempts,
            u32::saturating_add,
        );
        merge_state(
            &mut target.connection_observation,
            source.connection_observation.clone(),
        );
    }

    struct OperationGuard<'a> {
        scope: Arc<Mutex<UpstreamObservation>>,
        observation: &'a mut UpstreamObservation,
    }

    impl Drop for OperationGuard<'_> {
        fn drop(&mut self) {
            let summary = self.scope.lock().unwrap_or_else(|err| err.into_inner());
            merge_observation(self.observation, &summary);
        }
    }

    /// Aggregate each existing send inside a context/policy operation without
    /// propagating request headers, response status or errors from optional calls.
    /// Individual send guards still close their own snapshots on cancellation.
    pub(crate) async fn observe_operation<F: Future>(
        observation: &mut UpstreamObservation,
        operation: F,
    ) -> F::Output {
        let scope = Arc::new(Mutex::new(UpstreamObservation::default()));
        let _guard = OperationGuard {
            scope: scope.clone(),
            observation,
        };
        OPERATION_SCOPE.scope(scope, operation).await
    }

    /// Isolate each real send; optional operation scopes retain its network totals.
    pub(crate) async fn send(
        request: reqwest::RequestBuilder,
    ) -> reqwest::Result<reqwest::Response> {
        observe_send(&mut UpstreamObservation::default(), async {
            request.send().await
        })
        .await
    }

    pub(crate) fn merge_into_timing(
        observation: &mut UpstreamObservation,
        timing: &mut DiagnosisTiming,
    ) {
        add_optional(
            &mut timing.name_resolution_ms,
            observation.name_resolution_ms.take(),
            u64::saturating_add,
        );
        add_optional(
            &mut timing.name_resolution_calls,
            observation.name_resolution_calls.take(),
            u32::saturating_add,
        );
        add_optional(
            &mut timing.connection_setup_ms,
            observation.connection_setup_ms.take(),
            u64::saturating_add,
        );
        add_optional(
            &mut timing.connection_attempts,
            observation.connection_attempts.take(),
            u32::saturating_add,
        );
        merge_state(
            &mut timing.connection_observation,
            observation.connection_observation.take(),
        );
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{
            net::SocketAddr,
            sync::atomic::{AtomicUsize, Ordering},
            time::Duration,
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Same observer used by production, with only the GAI delegate replaced.
        // Never call system DNS for synthetic names in controlled tests.
        struct DelayedResolver {
            calls: Arc<AtomicUsize>,
            address: SocketAddr,
            fail: bool,
        }

        impl Resolve for DelayedResolver {
            fn resolve(&self, _: Name) -> Resolving {
                self.calls.fetch_add(1, Ordering::Relaxed);
                let address = self.address;
                let fail = self.fail;
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    if fail {
                        return Err(
                            Box::new(std::io::Error::other("synthetic resolution failed")) as _,
                        );
                    }
                    Ok(Box::new(std::iter::once(address)) as Addrs)
                })
            }
        }

        fn resolver(
            address: SocketAddr,
            fail: bool,
        ) -> (ObservedResolver<DelayedResolver>, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            (
                ObservedResolver(DelayedResolver {
                    calls: calls.clone(),
                    address,
                    fail,
                }),
                calls,
            )
        }

        async fn loopback() -> (SocketAddr, tokio::task::JoinHandle<()>) {
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let address = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(5), async move {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        request.push(stream.read_u8().await.unwrap());
                        assert!(request.len() < 8192);
                    }
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await
                        .unwrap();
                })
                .await
                .unwrap();
            });
            (address, task)
        }

        #[tokio::test]
        async fn diagnostics_network_delayed_resolver_uses_production_wrapper_once_without_identifiers()
         {
            let (address, server) = loopback().await;
            // Port 0 is returned unchanged, then reqwest supplies the URL port.
            let (resolver, calls) = resolver(SocketAddr::new(address.ip(), 0), false);
            let client = instrument_client(reqwest::Client::builder())
                .dns_resolver(Arc::new(resolver))
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap();
            let mut observation = UpstreamObservation::default();
            let response = observe_send(
                &mut observation,
                client
                    .get(format!(
                        "http://synthetic-private.invalid:{}/",
                        address.port()
                    ))
                    .send(),
            )
            .await
            .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            server.await.unwrap();
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            assert_eq!(observation.name_resolution_calls, Some(1));
            assert!(observation.name_resolution_ms.unwrap() >= 15);
            assert_eq!(observation.connection_attempts, Some(1));
            assert!(
                observation.connection_setup_ms.unwrap() >= observation.name_resolution_ms.unwrap()
            );
            assert_eq!(
                observation.connection_observation.as_deref(),
                Some("observed")
            );
            let mut timing = DiagnosisTiming::default();
            merge_into_timing(&mut observation, &mut timing);
            let value = serde_json::to_string(&timing).unwrap();
            for private in ["synthetic-private", "127.0.0.1", "http://"] {
                assert!(!value.contains(private));
            }
        }

        #[tokio::test]
        async fn diagnostics_network_operation_retains_system_resolver_wrapper_metrics() {
            let (address, server) = loopback().await;
            let (resolver, calls) = resolver(SocketAddr::new(address.ip(), 0), false);
            let client = instrument_client(reqwest::Client::builder())
                .dns_resolver(Arc::new(resolver))
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap();
            let mut operation = UpstreamObservation::default();
            let response = observe_operation(
                &mut operation,
                send(client.get(format!("http://operation.invalid:{}/", address.port()))),
            )
            .await
            .unwrap();
            assert_eq!(response.status(), reqwest::StatusCode::OK);
            server.await.unwrap();
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            assert_eq!(operation.name_resolution_calls, Some(1));
            assert!(operation.name_resolution_ms.unwrap() >= 15);
            assert_eq!(operation.connection_attempts, Some(1));
            assert_eq!(
                operation.connection_observation.as_deref(),
                Some("observed")
            );
            assert!(operation.status.is_none() && operation.response_headers_ms.is_none());
        }

        #[tokio::test]
        async fn diagnostics_network_static_overrides_bypass_observer_and_keep_port_mapping() {
            for explicit_port in [false, true] {
                let (address, server) = loopback().await;
                let (resolver, calls) = resolver(address, true);
                let overridden = if explicit_port {
                    SocketAddr::new(address.ip(), 1)
                } else {
                    address
                };
                let client = instrument_client(reqwest::Client::builder())
                    .dns_resolver(Arc::new(resolver))
                    .resolve_to_addrs("override.invalid", &[overridden])
                    .no_proxy()
                    .timeout(Duration::from_secs(3))
                    .build()
                    .unwrap();
                let url = if explicit_port {
                    format!("http://override.invalid:{}/", address.port())
                } else {
                    "http://override.invalid/".to_string()
                };
                let mut observation = UpstreamObservation::default();
                observe_send(&mut observation, client.get(url).send())
                    .await
                    .unwrap();
                server.await.unwrap();
                assert_eq!(calls.load(Ordering::Relaxed), 0);
                assert!(observation.name_resolution_ms.is_none());
                assert!(observation.name_resolution_calls.is_none());
                assert_eq!(observation.connection_attempts, Some(1));
                assert_eq!(
                    observation.connection_observation.as_deref(),
                    Some("observed")
                );
            }
        }

        #[tokio::test]
        async fn diagnostics_network_http_proxy_and_connect_keep_original_targets_and_single_attempt()
         {
            for tunnel in [false, true] {
                let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                    .await
                    .unwrap();
                let address = listener.local_addr().unwrap();
                let server = tokio::spawn(async move {
                    tokio::time::timeout(Duration::from_secs(3), async move {
                        let (mut stream, _) = listener.accept().await.unwrap();
                        let mut request = Vec::new();
                        while !request.ends_with(b"\r\n\r\n") {
                            request.push(stream.read_u8().await.unwrap());
                            assert!(request.len() < 8192);
                        }
                        let first = std::str::from_utf8(&request).unwrap().lines().next().unwrap();
                        if tunnel {
                            assert_eq!(first, "CONNECT never-resolve.invalid:443 HTTP/1.1");
                            stream.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                        } else {
                            assert_eq!(first, "GET http://never-resolve.invalid/synthetic HTTP/1.1");
                            stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").await.unwrap();
                        }
                    }).await.unwrap();
                });
                let (resolver, calls) = resolver(address, true);
                let client = instrument_client(reqwest::Client::builder())
                    .dns_resolver(Arc::new(resolver))
                    .no_proxy()
                    .proxy(reqwest::Proxy::all(format!("http://{address}")).unwrap())
                    .timeout(Duration::from_secs(3))
                    .build()
                    .unwrap();
                let url = if tunnel {
                    "https://never-resolve.invalid/synthetic"
                } else {
                    "http://never-resolve.invalid/synthetic"
                };
                let mut observation = UpstreamObservation::default();
                let result =
                    observe_send(&mut observation, async { client.get(url).send().await }).await;
                if tunnel {
                    assert!(result.unwrap_err().is_connect());
                } else {
                    assert_eq!(result.unwrap().status(), reqwest::StatusCode::NO_CONTENT);
                }
                server.await.unwrap();
                assert_eq!(calls.load(Ordering::Relaxed), 0);
                assert!(observation.name_resolution_ms.is_none());
                assert_eq!(observation.connection_attempts, Some(1));
                assert_eq!(
                    observation.connection_observation.as_deref(),
                    Some("observed")
                );
            }
        }

        #[tokio::test]
        async fn diagnostics_network_tls_trust_is_unchanged_and_attempts_do_not_claim_tls_timing() {
            use tokio_rustls::{
                TlsAcceptor,
                rustls::{
                    self,
                    pki_types::{CertificateDer, PrivatePkcs8KeyDer, pem::PemObject},
                },
            };
            let cert = include_bytes!("test-fixtures/alerts_tls_cert.pem");
            let key = include_bytes!("test-fixtures/alerts_tls_key.pem");
            let config = rustls::ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from_pem_slice(cert).unwrap()],
                    PrivatePkcs8KeyDer::from_pem_slice(key).unwrap().into(),
                )
                .unwrap();
            let acceptor = TlsAcceptor::from(Arc::new(config));
            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(5), async move {
                    for trusted in [true, false] {
                        let (stream, _) = listener.accept().await.unwrap();
                        match acceptor.accept(stream).await {
                            Ok(mut stream) => {
                                assert!(trusted);
                                let mut request = Vec::new();
                                while !request.ends_with(b"\r\n\r\n") {
                                    request.push(stream.read_u8().await.unwrap());
                                    assert!(request.len() < 8192);
                                }
                                stream
                                    .write_all(
                                        b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n",
                                    )
                                    .await
                                    .unwrap();
                            }
                            Err(_) => assert!(!trusted),
                        }
                    }
                })
                .await
                .unwrap();
            });
            for trusted in [true, false] {
                let builder = instrument_client(reqwest::Client::builder())
                    .no_proxy()
                    .timeout(Duration::from_secs(3));
                let builder = if trusted {
                    builder.add_root_certificate(reqwest::Certificate::from_pem(cert).unwrap())
                } else {
                    builder
                };
                let client = builder.build().unwrap();
                let mut observation = UpstreamObservation::default();
                let result = observe_send(&mut observation, async {
                    client.get(format!("https://{address}/")).send().await
                })
                .await;
                if trusted {
                    assert_eq!(result.unwrap().status(), reqwest::StatusCode::NO_CONTENT);
                } else {
                    assert!(result.unwrap_err().is_connect());
                }
                assert_eq!(observation.connection_attempts, Some(1));
                assert_eq!(
                    observation.connection_observation.as_deref(),
                    Some("observed")
                );
                let mut timing = DiagnosisTiming::default();
                merge_into_timing(&mut observation, &mut timing);
            }
            server.await.unwrap();
        }

        #[tokio::test]
        async fn diagnostics_network_connect_timeout_stays_outermost_and_incomplete_attempt_is_partial()
         {
            let (resolver, calls) = resolver(([127, 0, 0, 1], 1).into(), false);
            let client = instrument_client(reqwest::Client::builder())
                .dns_resolver(Arc::new(resolver))
                .no_proxy()
                .connect_timeout(Duration::from_millis(2))
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap();
            let mut observation = UpstreamObservation::default();
            let error = observe_send(&mut observation, async {
                client.get("http://delayed.invalid/").send().await
            })
            .await
            .unwrap_err();
            assert!(error.is_timeout());
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            assert!(observation.name_resolution_calls.is_none());
            assert!(observation.connection_attempts.is_none());
            assert_eq!(
                observation.connection_observation.as_deref(),
                Some("partial")
            );
        }

        #[tokio::test]
        async fn diagnostics_network_no_events_and_pool_reuse_never_invent_zero_or_dns() {
            let mut empty = UpstreamObservation::default();
            observe_send(&mut empty, std::future::ready(())).await;
            assert!(empty.name_resolution_ms.is_none() && empty.connection_setup_ms.is_none());
            assert!(empty.name_resolution_calls.is_none() && empty.connection_attempts.is_none());
            assert_eq!(
                empty.connection_observation.as_deref(),
                Some("not_observed")
            );

            let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                tokio::time::timeout(Duration::from_secs(5), async move {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    for _ in 0..2 {
                        let mut request = Vec::new();
                        while !request.ends_with(b"\r\n\r\n") {
                            request.push(stream.read_u8().await.unwrap());
                            assert!(request.len() < 8192);
                        }
                        stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                            .await
                            .unwrap();
                    }
                })
                .await
                .unwrap();
            });
            let client = instrument_client(reqwest::Client::builder())
                .no_proxy()
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap();
            let url = format!("http://{address}/");
            let mut first = UpstreamObservation::default();
            observe_send(&mut first, client.get(&url).send())
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            let mut second = UpstreamObservation::default();
            observe_send(&mut second, client.get(&url).send())
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap();
            server.await.unwrap();
            assert_eq!(first.connection_attempts, Some(1));
            assert!(
                first.name_resolution_calls.is_none(),
                "IP literals bypass DNS"
            );
            assert!(second.connection_setup_ms.is_none() && second.connection_attempts.is_none());
            assert!(second.name_resolution_ms.is_none());
            assert_eq!(
                second.connection_observation.as_deref(),
                Some("not_observed")
            );
        }

        #[tokio::test]
        async fn diagnostics_network_resolver_error_and_cancellation_only_count_completed_events() {
            let (resolver, calls) = resolver(([127, 0, 0, 1], 0).into(), true);
            let mut failed = UpstreamObservation::default();
            assert!(
                observe_send(&mut failed, async {
                    resolver.resolve("failure.invalid".parse().unwrap()).await
                })
                .await
                .is_err()
            );
            assert_eq!(failed.name_resolution_calls, Some(1));
            assert!(failed.name_resolution_ms.unwrap() >= 15);
            assert_eq!(failed.connection_observation.as_deref(), Some("observed"));
            let mut cancelled = UpstreamObservation::default();
            let mut future = Box::pin(observe_send(&mut cancelled, async {
                resolver.resolve("cancel.invalid".parse().unwrap()).await
            }));
            std::future::poll_fn(|cx| {
                assert!(future.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(future);
            assert_eq!(calls.load(Ordering::Relaxed), 2);
            assert!(
                cancelled.name_resolution_ms.is_none() && cancelled.name_resolution_calls.is_none()
            );
            assert_eq!(cancelled.connection_observation.as_deref(), Some("partial"));
        }

        #[tokio::test]
        async fn diagnostics_network_connector_calls_once_and_restores_scope_after_task_move() {
            let owner = Arc::new(Mutex::new(SendTotals::default()));
            let unrelated = Arc::new(Mutex::new(SendTotals::default()));
            let calls = Arc::new(AtomicUsize::new(0));
            let counted = calls.clone();
            let (resolver, resolved) = resolver(([127, 0, 0, 1], 0).into(), false);
            let (release, released) = tokio::sync::oneshot::channel();
            let mut connector = ObservationLayer.layer(OneCall(Some(move |value: u32| {
                counted.fetch_add(1, Ordering::Relaxed);
                async move {
                    released.await.unwrap();
                    // Resolution begins AFTER moving to a task with another scope.
                    let _addrs = resolver
                        .resolve("moved.invalid".parse().unwrap())
                        .await
                        .unwrap();
                    Ok::<_, std::io::Error>(value + 1)
                }
            })));
            let mut future =
                Box::pin(SEND_SCOPE.sync_scope(Some(owner.clone()), || connector.call(41)));
            std::future::poll_fn(|cx| {
                assert!(future.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            let task = tokio::spawn(SEND_SCOPE.scope(Some(unrelated.clone()), future));
            release.send(()).unwrap();
            assert_eq!(task.await.unwrap().unwrap(), 42);
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            assert_eq!(resolved.load(Ordering::Relaxed), 1);
            let totals = owner.lock().unwrap();
            assert_eq!(totals.connection_attempts, Some(1));
            assert_eq!(totals.resolution_calls, Some(1));
            assert!(!totals.partial && totals.in_flight == 0);
            assert!(unrelated.lock().unwrap().connection_attempts.is_none());
            assert!(unrelated.lock().unwrap().resolution_calls.is_none());
        }

        // Minimal FnOnce service for controlled futures; response/error unchanged.
        struct OneCall<F>(Option<F>);
        impl<F, Fut> Service<u32> for OneCall<F>
        where
            F: FnOnce(u32) -> Fut,
            Fut: Future<Output = Result<u32, std::io::Error>>,
        {
            type Response = u32;
            type Error = std::io::Error;
            type Future = Fut;
            fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
                Poll::Ready(Ok(()))
            }
            fn call(&mut self, value: u32) -> Self::Future {
                self.0.take().unwrap()(value)
            }
        }

        #[tokio::test]
        async fn diagnostics_network_connector_drop_and_late_completion_are_partial_and_frozen() {
            let calls = Arc::new(AtomicUsize::new(0));
            let counted = calls.clone();
            let mut connector = ObservedConnector(tower::service_fn(move |_: ()| {
                counted.fetch_add(1, Ordering::Relaxed);
                std::future::pending::<Result<(), std::io::Error>>()
            }));
            let mut observation = UpstreamObservation::default();
            let mut send = Box::pin(observe_send(&mut observation, async {
                connector.call(()).await
            }));
            std::future::poll_fn(|cx| {
                assert!(send.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(send);
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            assert!(
                observation.connection_setup_ms.is_none()
                    && observation.connection_attempts.is_none()
            );
            assert_eq!(
                observation.connection_observation.as_deref(),
                Some("partial")
            );

            let (release, released) = tokio::sync::oneshot::channel();
            let mut connector = ObservedConnector(OneCall(Some(move |value| async move {
                released.await.unwrap();
                Ok(value)
            })));
            let mut observation = UpstreamObservation::default();
            let future = observe_send(&mut observation, async { connector.call(7) }).await;
            assert_eq!(
                observation.connection_observation.as_deref(),
                Some("partial")
            );
            release.send(()).unwrap();
            assert_eq!(tokio::spawn(future).await.unwrap().unwrap(), 7);
            assert!(
                observation.connection_setup_ms.is_none()
                    && observation.connection_attempts.is_none()
            );
        }

        #[tokio::test]
        async fn diagnostics_network_gai_adapter_preserves_numeric_address_and_zero_port() {
            let addresses: Vec<_> = DefaultGaiResolver
                .resolve("127.0.0.1".parse().unwrap())
                .await
                .unwrap()
                .collect();
            assert_eq!(addresses, vec![SocketAddr::from(([127, 0, 0, 1], 0))]);
        }

        #[tokio::test]
        async fn diagnostics_network_nested_sends_and_unscoped_connector_cannot_contaminate_outer_send()
         {
            let mut outer = UpstreamObservation::default();
            let mut inner = UpstreamObservation::default();
            observe_send(&mut outer, async {
                observe_send(&mut inner, async {
                    Event::start(EventKind::Resolution).complete();
                })
                .await;
            })
            .await;
            assert_eq!(
                outer.connection_observation.as_deref(),
                Some("not_observed")
            );
            assert!(outer.name_resolution_ms.is_none());
            assert_eq!(inner.name_resolution_calls, Some(1));
            let mut connector = ObservationLayer.layer(OneCall(Some(|value| async move {
                Event::start(EventKind::Resolution).complete();
                Ok(value)
            })));
            let unscoped = connector.call(1);
            observe_send(&mut outer, unscoped).await.unwrap();
            assert!(outer.connection_attempts.is_none() && outer.name_resolution_calls.is_none());
        }

        #[tokio::test]
        async fn diagnostics_network_operation_collects_each_send_once_and_keeps_other_fields_local()
         {
            let mut operation = UpstreamObservation::default();
            observe_operation(&mut operation, async {
                for _ in 0..2 {
                    let mut local = UpstreamObservation::default();
                    observe_send(&mut local, async {
                        Event::start(EventKind::Resolution).complete();
                        Event::start(EventKind::Connection).complete();
                    })
                    .await;
                    assert_eq!(local.connection_attempts, Some(1));
                    local.status = Some(503);
                    local.error_kind = Some("upstream_http".into());
                }
            })
            .await;
            assert_eq!(operation.name_resolution_calls, Some(2));
            assert_eq!(operation.connection_attempts, Some(2));
            assert_eq!(
                operation.connection_observation.as_deref(),
                Some("observed")
            );
            assert!(operation.status.is_none() && operation.error_kind.is_none());
            let mut timing = DiagnosisTiming::default();
            merge_into_timing(&mut operation, &mut timing);
            merge_into_timing(&mut operation, &mut timing);
            assert_eq!(timing.connection_attempts, Some(2));
        }

        #[tokio::test]
        async fn diagnostics_network_operation_cancellation_freezes_completed_send_summaries() {
            let mut operation = UpstreamObservation::default();
            let (release, released) = tokio::sync::oneshot::channel();
            let mut connector = ObservedConnector(OneCall(Some(move |value| async move {
                released.await.unwrap();
                Ok(value)
            })));
            let mut background = None;
            let mut future = Box::pin(observe_operation(&mut operation, async {
                observe_send(&mut UpstreamObservation::default(), async {
                    Event::start(EventKind::Resolution).complete();
                    Event::start(EventKind::Connection).complete();
                })
                .await;
                observe_send(&mut UpstreamObservation::default(), async {
                    background = Some(connector.call(1));
                    std::future::pending::<()>().await;
                })
                .await;
            }));
            std::future::poll_fn(|cx| {
                assert!(future.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            drop(future);
            assert_eq!(operation.name_resolution_calls, Some(1));
            assert_eq!(operation.connection_attempts, Some(1));
            assert_eq!(operation.connection_observation.as_deref(), Some("partial"));
            release.send(()).unwrap();
            assert_eq!(tokio::spawn(background.unwrap()).await.unwrap().unwrap(), 1);
            assert_eq!(operation.connection_attempts, Some(1));
            assert_eq!(operation.connection_observation.as_deref(), Some("partial"));
        }

        #[tokio::test]
        async fn diagnostics_network_concurrent_operations_remain_isolated() {
            let barrier = Arc::new(tokio::sync::Barrier::new(2));
            let mut first = UpstreamObservation::default();
            let mut second = UpstreamObservation::default();
            tokio::join!(
                observe_operation(&mut first, async {
                    observe_send(&mut UpstreamObservation::default(), async {
                        barrier.wait().await;
                        Event::start(EventKind::Resolution).complete();
                    })
                    .await;
                }),
                observe_operation(&mut second, async {
                    observe_send(&mut UpstreamObservation::default(), async {
                        barrier.wait().await;
                        Event::start(EventKind::Connection).complete();
                        Event::start(EventKind::Connection).complete();
                    })
                    .await;
                }),
            );
            assert_eq!(first.name_resolution_calls, Some(1));
            assert!(first.connection_attempts.is_none());
            assert_eq!(second.connection_attempts, Some(2));
            assert!(second.name_resolution_calls.is_none());
        }

        #[tokio::test]
        async fn diagnostics_network_connector_error_and_unpolled_drop_preserve_results() {
            let mut connector = ObservationLayer.layer(OneCall(Some(|_| async {
                Err(std::io::Error::other("synthetic"))
            })));
            let mut failed = UpstreamObservation::default();
            let error = observe_send(&mut failed, async { connector.call(0).await })
                .await
                .unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::Other);
            assert_eq!(failed.connection_attempts, Some(1));
            assert_eq!(failed.connection_observation.as_deref(), Some("observed"));
            let mut connector =
                ObservationLayer.layer(OneCall(Some(|value| std::future::ready(Ok(value)))));
            let mut dropped = UpstreamObservation::default();
            observe_send(&mut dropped, async {
                drop(connector.call(0));
            })
            .await;
            assert!(dropped.connection_attempts.is_none());
            assert_eq!(dropped.connection_observation.as_deref(), Some("partial"));
        }

        #[tokio::test]
        async fn diagnostics_network_totals_saturate_and_merge_only_once() {
            let mut observation = UpstreamObservation::default();
            observe_send(&mut observation, async {
                let scope = current_scope().unwrap();
                let mut totals = scope.lock().unwrap();
                totals.resolution_ms = Some(u64::MAX);
                totals.resolution_calls = Some(u32::MAX);
                totals.connection_ms = Some(u64::MAX);
                totals.connection_attempts = Some(u32::MAX);
                drop(totals);
                Event::start(EventKind::Resolution).complete();
                Event::start(EventKind::Connection).complete();
            })
            .await;
            let mut timing = DiagnosisTiming {
                name_resolution_ms: Some(1),
                name_resolution_calls: Some(1),
                connection_setup_ms: Some(1),
                connection_attempts: Some(1),
                ..Default::default()
            };
            merge_into_timing(&mut observation, &mut timing);
            merge_into_timing(&mut observation, &mut timing);
            assert_eq!(timing.name_resolution_ms, Some(u64::MAX));
            assert_eq!(timing.name_resolution_calls, Some(u32::MAX));
            assert_eq!(timing.connection_setup_ms, Some(u64::MAX));
            assert_eq!(timing.connection_attempts, Some(u32::MAX));
            assert_eq!(timing.connection_observation.as_deref(), Some("observed"));
        }
    }
}

#[cfg(test)]
thread_local! {
    // Parallel cargo tests share the process; isolate the global collector per test thread.
    static TEST_COLLECTOR: DiagnosisCollector = DiagnosisCollector::new(DIAGNOSTICS_CAPACITY);
    static TEST_BEFORE_BUFFER_LOCK: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}

fn with_active_collector<T>(f: impl FnOnce(&DiagnosisCollector) -> T) -> Option<T> {
    #[cfg(test)]
    {
        return Some(TEST_COLLECTOR.with(f));
    }
    #[cfg(not(test))]
    {
        COLLECTOR.get().map(f)
    }
}

pub fn init_diagnostics() {
    #[cfg(test)]
    {
        TEST_COLLECTOR.with(|_| ());
    }
    let _ = COLLECTOR.set(DiagnosisCollector::new(DIAGNOSTICS_CAPACITY));
}

/// Attach the existing bounded writer after database initialization.
pub(crate) fn enable_persistence(queue: crate::db::TelemetryWriteQueue) {
    let _ = with_active_collector(|collector| {
        *collector.persistence.lock().unwrap() = Some(queue);
    });
}

pub async fn record_diagnosis(record: DiagnosisRecord) {
    record_diagnosis_now(record);
}

/// Record a diagnosis synchronously. This is used by cancellation/Drop paths where
/// awaiting is impossible, while retaining the same collector-edge sanitization.
pub fn record_diagnosis_now(record: DiagnosisRecord) {
    if let Some(collector) = with_active_collector(DiagnosisCollector::clone) {
        collector.ingest(record);
    }
}

pub fn get_diagnosis_records() -> Vec<DiagnosisRecord> {
    with_active_collector(DiagnosisCollector::get_records).unwrap_or_default()
}

pub fn clean_diagnostics() {
    let _ = with_active_collector(DiagnosisCollector::clear);
}

/// Build a diagnosis record and run it through the security sanitizer.
///
/// Future playback-chain hook (proxy/main — do **not** pass raw secrets):
/// ```ignore
/// let record = diagnostics::create_diagnosis_record(
///     server_id,   // already an id, not a URL
///     media_id,
///     ua,          // user-agent only
///     steps,       // details may contain URLs; sanitizer redacts secrets
///     cache_hit,
///     failure_node,
/// );
/// diagnostics::record_diagnosis(record).await;
/// // Or: diagnostics::sanitize_diagnostic_text(raw_detail) before building steps.
/// ```
pub fn create_diagnosis_record(
    server_id: &str,
    media_id: &str,
    ua: &str,
    steps: Vec<DiagnosisStep>,
    cache_hit: bool,
    failure_node: Option<String>,
) -> DiagnosisRecord {
    let request_id = current_request_id().unwrap_or_else(new_request_id);
    sanitize_diagnosis_record(DiagnosisRecord {
        request_id,
        server_id: server_id.to_string(),
        media_id: media_id.to_string(),
        ua: ua.to_string(),
        steps,
        cache_hit,
        failure_node,
        sanitized_errors: Vec::new(),
        ..Default::default()
    })
}

/// Full-record sanitizer used at construction, ingest, and API egress.
pub fn sanitize_diagnosis_record(mut record: DiagnosisRecord) -> DiagnosisRecord {
    record.request_id = normalize_id_like(&record.request_id);
    record.server_id = normalize_id_like(&record.server_id);
    record.media_id = normalize_id_like(&record.media_id);
    record.ua = truncate_chars(&sanitize_diagnostic_text(&record.ua), MAX_UA_CHARS);
    record.failure_node = record
        .failure_node
        .map(|node| normalize_id_like(&node))
        .filter(|node| !node.is_empty());
    record.steps = record
        .steps
        .into_iter()
        .map(sanitize_diagnosis_step)
        .collect();
    let context = &mut record.context;
    for value in [
        &mut context.server_name,
        &mut context.media_name,
        &mut context.series_name,
        &mut context.media_source_id,
        &mut context.user_name,
        &mut context.client_name,
        &mut context.device_name,
        &mut context.device_id,
        &mut context.source_ip,
        &mut context.method,
        &mut context.range,
        &mut record.result.reason,
        &mut record.result.target_host,
        &mut record.cache.reason,
        &mut record.cache.target_host,
    ] {
        *value = value.take().map(|text| sanitize_diagnostic_text(&text));
    }
    context.request_path = context.request_path.take().map(|path| sanitize_path(&path));
    for value in [
        &mut record.paths.original,
        &mut record.paths.mapped,
        &mut record.paths.resolved,
        &mut record.paths.final_url,
    ] {
        *value = value.take().map(|path| sanitize_path(&path));
    }
    record.result.outcome = allowed_value(
        record.result.outcome,
        &["redirect", "proxy", "failed", "cancelled", "blocked"],
    );
    record.cache.status = allowed_value(
        record.cache.status,
        &["hit", "miss", "disabled", "not_checked"],
    );
    record.result.error_kind = record
        .result
        .error_kind
        .map(|kind| normalize_id_like(&kind));
    record.timing.connection_observation = allowed_value(
        record.timing.connection_observation,
        &["not_observed", "observed", "partial"],
    );
    record.timing.slowest_step = record
        .timing
        .slowest_step
        .map(|name| normalize_step_name(&name));
    record.sanitized_errors = record
        .failure_node
        .as_deref()
        .and_then(|failure_node| record.steps.iter().find(|step| step.name == failure_node))
        .map(|step| vec![format!("{}: {}", step.name, step.details)])
        .unwrap_or_default();
    record
}

/// Sanitize a single step (name + details).
pub fn sanitize_diagnosis_step(mut step: DiagnosisStep) -> DiagnosisStep {
    step.name = normalize_step_name(&step.name);
    step.details = sanitize_diagnostic_text(&step.details);
    step.state = allowed_value(step.state, &["recorded", "skipped", "failed", "cancelled"]);
    step.reason = step.reason.map(|reason| sanitize_diagnostic_text(&reason));
    step.error_kind = step.error_kind.map(|kind| normalize_id_like(&kind));
    step.connection_observation = allowed_value(
        step.connection_observation,
        &["not_observed", "observed", "partial"],
    );
    step
}

/// Case-insensitive, structure-aware redaction for free-form diagnostic text.
///
/// Rules:
/// - Strip control characters.
/// - Sanitize embedded `http(s)` URLs: drop userinfo, redact all but numeric quality hints.
/// - Redact header-style and key/value credentials (Authorization, Cookie, api_key, …).
/// - Redact Bearer/Basic schemes and JSON string values for sensitive keys.
/// - Preserve non-sensitive media identifiers and surrounding context.
/// - Enforce max detail length.
/// Percent-decodes the path of a pure http(s) URL so encoded file names read as
/// plain text. The correlation marker the frontend strips is kept as-is.
/// Mixed text, other schemes, malformed escapes and segments that would decode
/// into a path separator, a query marker or a credential separator stay encoded.
pub(crate) fn decode_log_detail_url(detail: &str) -> String {
    let (prefix, rest) = split_correlation_prefix(detail);
    let decoded = decode_url_path(rest);
    format!("{prefix}{decoded}")
}

/// Splits the leading `request_id=diag-<id> ` marker so the remainder can be
/// treated as a URL on its own. The marker itself is returned unchanged.
fn split_correlation_prefix(detail: &str) -> (&str, &str) {
    let Some(rest) = detail.strip_prefix("request_id=diag-") else {
        return ("", detail);
    };
    let id_len = rest
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '-'))
        .unwrap_or(rest.len());
    if id_len == 0 || id_len > 120 {
        return ("", detail);
    }
    let after_id = &rest[id_len..];
    let remainder = after_id.trim_start();
    if remainder.len() == after_id.len() {
        // The marker is not followed by whitespace, so it is not a marker.
        return ("", detail);
    }
    let prefix_len = detail.len() - remainder.len();
    (&detail[..prefix_len], remainder)
}

fn decode_url_path(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() || trimmed.split_whitespace().count() != 1 {
        return url.to_string();
    }
    let lowered = trimmed.to_ascii_lowercase();
    if !(lowered.starts_with("http://") || lowered.starts_with("https://")) {
        return url.to_string();
    }
    let Some(scheme_end) = trimmed.find("://") else {
        return url.to_string();
    };
    let Some(path_start) = trimmed[scheme_end + 3..]
        .find('/')
        .map(|offset| scheme_end + 3 + offset)
    else {
        return url.to_string();
    };

    let origin = &trimmed[..path_start];
    let rest = &trimmed[path_start..];
    let (path, suffix) = match rest.find(['?', '#']) {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, ""),
    };
    if !path.contains('%') {
        return url.to_string();
    }

    let decoded = path
        .split('/')
        .map(decode_percent_segment)
        .collect::<Vec<_>>()
        .join("/");
    format!("{origin}{decoded}{suffix}")
}

fn decode_percent_segment(segment: &str) -> String {
    if !segment.contains('%') {
        return segment.to_string();
    }
    if has_invalid_percent_escape(segment) {
        return segment.to_string();
    }
    match urlencoding::decode(segment) {
        Ok(decoded)
            if !decoded.contains(['/', '?', '#', '=', ':'])
                && !contains_control_chars(&decoded) =>
        {
            decoded.into_owned()
        }
        _ => segment.to_string(),
    }
}

/// Detects a `%` that is not followed by two hex digits. Such a segment stays
/// encoded so it matches the frontend rule.
fn has_invalid_percent_escape(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'%' {
            continue;
        }
        let valid = bytes
            .get(index + 1)
            .zip(bytes.get(index + 2))
            .map(|(high, low)| high.is_ascii_hexdigit() && low.is_ascii_hexdigit())
            .unwrap_or(false);
        if !valid {
            return true;
        }
    }
    false
}

/// Control characters would break the single-line CSV layout, so a segment that
/// decodes into one stays percent-encoded.
fn contains_control_chars(value: &str) -> bool {
    value.chars().any(char::is_control)
}

pub fn sanitize_diagnostic_text(input: &str) -> String {
    let stripped: String = input.chars().filter(|ch| !ch.is_control()).collect();
    if stripped.is_empty() {
        return String::new();
    }
    let with_urls = sanitize_urls_in_text(&stripped);
    let with_creds = redact_credential_patterns(&with_urls);
    truncate_chars(&with_creds, MAX_DETAIL_CHARS)
}

/// Item Name/SeriesName are labels, not authorization text. Preserve bare auth
/// scheme words and ambiguous subtitle keys, but retain explicit credential syntax.
pub(crate) fn sanitize_media_label(input: &str) -> String {
    let stripped: String = input.chars().filter(|ch| !ch.is_control()).collect();
    let urls = sanitize_urls_in_text(&stripped);
    let json = replace_json_sensitive_strings(&urls);
    let cleaned = replace_header_or_kv_secrets(&json, true);
    truncate_chars(cleaned.trim(), 256)
}

/// Backward-compatible step mutator; prefer [`sanitize_diagnosis_step`].
pub fn redact_sensitive_info(steps: &mut Vec<DiagnosisStep>) {
    for step in steps.iter_mut() {
        *step = sanitize_diagnosis_step(std::mem::take(step));
    }
}

fn allowed_value(value: Option<String>, allowed: &[&str]) -> Option<String> {
    value.filter(|value| allowed.contains(&value.as_str()))
}

/// Structured paths never retain arbitrary query parameters or URL userinfo/fragments.
pub(crate) fn sanitize_path(value: &str) -> String {
    if let Ok(mut url) = url::Url::parse(value) {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        return sanitize_diagnostic_text(url.as_str());
    }
    sanitize_diagnostic_text(value.split(['?', '#']).next().unwrap_or_default())
}

pub(crate) fn target_host(value: &str) -> Option<String> {
    url::Url::parse(value).ok()?.host_str().map(str::to_string)
}

fn normalize_id_like(value: &str) -> String {
    // IDs normally come from trusted configuration or Emby paths, but the
    // collector/API boundary must not leak a credential if a caller supplies
    // a URL-like or otherwise untrusted identifier.
    let cleaned = sanitize_diagnostic_text(value).trim().to_string();
    truncate_chars(&cleaned, MAX_ID_CHARS)
}

fn normalize_step_name(value: &str) -> String {
    let cleaned: String = value
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    let cleaned = if cleaned.is_empty() {
        "step".to_string()
    } else {
        cleaned
    };
    truncate_chars(&cleaned, MAX_STEP_NAME_CHARS)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

fn is_sensitive_key(key: &str) -> bool {
    SENSITIVE_KEYS
        .iter()
        .any(|sensitive| key.eq_ignore_ascii_case(sensitive))
}

fn sanitize_urls_in_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut idx = 0;
    while idx < input.len() {
        if let Some(rel) = find_url_start(&input[idx..]) {
            let abs = idx + rel;
            out.push_str(&input[idx..abs]);
            let rest = &input[abs..];
            let url_len = rest
                .find(|ch: char| {
                    ch.is_whitespace() || matches!(ch, ')' | '"' | '\'' | '<' | '>' | ',')
                })
                .unwrap_or(rest.len());
            let url_part = &rest[..url_len];
            if let Some(redacted) = sanitize_url_str(url_part) {
                out.push_str(&redacted);
            } else {
                out.push_str(url_part);
            }
            idx = abs + url_len;
        } else {
            out.push_str(&input[idx..]);
            break;
        }
    }
    out
}

fn find_url_start(input: &str) -> Option<usize> {
    let lower = input.to_ascii_lowercase();
    let http = lower.find("http://");
    let https = lower.find("https://");
    match (http, https) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

fn sanitize_url_str(value: &str) -> Option<String> {
    let mut parsed = url::Url::parse(value).ok()?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return None;
    }
    let _ = parsed.set_username("");
    let _ = parsed.set_password(None);
    if let Some(query) = parsed.query() {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for (key, val) in url::form_urlencoded::parse(query.as_bytes()) {
            // Unknown query keys can contain provider-specific signatures. Only
            // retain bounded numeric quality hints; redact all other values.
            let value = if key.eq_ignore_ascii_case("quality")
                && val.chars().all(|ch| ch.is_ascii_digit())
            {
                truncate_chars(&val, MAX_QUERY_VALUE_CHARS)
            } else {
                REDACTED_PLACEHOLDER.to_string()
            };
            pairs.push((key.into_owned(), value));
        }
        if pairs.is_empty() {
            parsed.set_query(None);
        } else {
            // Build manually so the placeholder stays readable.
            let rebuilt = pairs
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&");
            parsed.set_query(Some(&rebuilt));
        }
    }
    parsed.set_fragment(None);
    Some(parsed.to_string())
}

fn redact_credential_patterns(input: &str) -> String {
    let mut out = replace_json_sensitive_strings(input);
    out = replace_header_or_kv_secrets(&out, false);
    replace_auth_schemes(&out)
}

fn replace_json_sensitive_strings(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '"' {
            if let Some((key, key_end)) = scan_json_string(&chars, i) {
                let mut j = key_end;
                while j < chars.len() && chars[j].is_whitespace() {
                    j += 1;
                }
                if j < chars.len() && chars[j] == ':' {
                    j += 1;
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if j < chars.len() && chars[j] == '"' {
                        if let Some((_val, val_end)) = scan_json_string(&chars, j) {
                            out.push('"');
                            out.push_str(&key);
                            out.push('"');
                            out.push(':');
                            if is_sensitive_key(&key) {
                                out.push('"');
                                out.push_str(REDACTED_PLACEHOLDER);
                                out.push('"');
                            } else {
                                for ch in &chars[j..val_end] {
                                    out.push(*ch);
                                }
                            }
                            i = val_end;
                            continue;
                        }
                    }
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

fn scan_json_string(chars: &[char], start: usize) -> Option<(String, usize)> {
    if start >= chars.len() || chars[start] != '"' {
        return None;
    }
    let mut i = start + 1;
    let mut content = String::new();
    while i < chars.len() {
        match chars[i] {
            '\\' if i + 1 < chars.len() => {
                content.push(chars[i + 1]);
                i += 2;
            }
            '"' => return Some((content, i + 1)),
            ch => {
                content.push(ch);
                i += 1;
            }
        }
    }
    None
}

fn replace_header_or_kv_secrets(input: &str, media_label: bool) -> String {
    let mut out = String::with_capacity(input.len());
    let lower = input.to_ascii_lowercase();
    let mut idx = 0;
    while idx < input.len() {
        if let Some(key_len) = match_sensitive_key_at(&lower, idx) {
            if idx > 0 {
                let prev = lower.as_bytes()[idx - 1];
                if prev.is_ascii_alphanumeric() || prev == b'_' || prev == b'-' {
                    let ch = input[idx..].chars().next().unwrap();
                    out.push(ch);
                    idx += ch.len_utf8();
                    continue;
                }
            }
            let after_key = idx + key_len;
            let rest = &input[after_key..];
            let trimmed = rest.trim_start();
            let ws = rest.len() - trimmed.len();
            // These ordinary nouns can introduce subtitles in a structured label.
            // Their '=' assignments and quoted JSON fields are still credentials.
            if let Some(sep) = trimmed.chars().next().filter(|c| {
                *c == '='
                    || (*c == ':'
                        && (!media_label
                            || !matches!(
                                &lower[idx..after_key],
                                "secret" | "credential" | "sign" | "signature" | "sig"
                            )))
            }) {
                let after_sep = &trimmed[sep.len_utf8()..];
                let value_ws_len = after_sep.len() - after_sep.trim_start().len();
                let value = after_sep.trim_start();
                if !value.is_empty() {
                    let value_len = value_len_for(sep, value);
                    // Preserve original key casing.
                    out.push_str(&input[idx..after_key]);
                    out.push_str(&input[after_key..after_key + ws]);
                    out.push(sep);
                    out.push_str(&after_sep[..value_ws_len]);
                    out.push_str(REDACTED_PLACEHOLDER);
                    idx = after_key + ws + sep.len_utf8() + value_ws_len + value_len;
                    continue;
                }
            }
        }
        let ch = input[idx..].chars().next().unwrap();
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

fn match_sensitive_key_at(lower: &str, idx: usize) -> Option<usize> {
    let slice = &lower[idx..];
    let mut best: Option<usize> = None;
    for key in SENSITIVE_KEYS {
        if slice.starts_with(key) {
            let len = key.len();
            let ok_right = slice
                .as_bytes()
                .get(len)
                .map(|b| {
                    let c = *b as char;
                    !(c.is_ascii_alphanumeric() || c == '_' || c == '-')
                })
                .unwrap_or(true);
            if ok_right && best.map(|l| len > l).unwrap_or(true) {
                best = Some(len);
            }
        }
    }
    best
}

fn value_len_for(sep: char, value: &str) -> usize {
    if value.starts_with(REDACTED_PLACEHOLDER) {
        return REDACTED_PLACEHOLDER.len();
    }
    if let Some(quote @ ('\'' | '"')) = value.chars().next() {
        // This is already a sensitive key's value, not an arbitrary title.
        // Consume quoted whitespace/separators and escaped quotes as one value;
        // an unterminated value conservatively consumes the remaining text.
        let mut escaped = false;
        for (idx, ch) in value.char_indices().skip(1) {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == quote {
                return idx + ch.len_utf8();
            }
        }
        return value.len();
    }
    if sep == ':' {
        // Header-style values may contain spaces (Bearer xxx). Stop at list separators.
        value
            .char_indices()
            .find(|(_, ch)| *ch == ';' || *ch == '\n' || *ch == '\r')
            .map(|(i, _)| i)
            .unwrap_or(value.len())
    } else if let Some(label_len) = auth_scheme_label_len(value) {
        // `Authorization=Bearer <token>` must redact scheme+token, not just "Bearer".
        let after = &value[label_len..];
        let ws = after.len() - after.trim_start().len();
        let token = after.trim_start();
        if token.is_empty() {
            value_token_len(value)
        } else {
            label_len
                + ws
                + if token.starts_with(['\'', '"']) {
                    value_len_for('=', token)
                } else {
                    value_token_len(token)
                }
        }
    } else {
        value_token_len(value)
    }
}

fn auth_scheme_label_len(value: &str) -> Option<usize> {
    let lower = value.to_ascii_lowercase();
    if lower.starts_with("bearer ") {
        Some(6)
    } else if lower.starts_with("basic ") {
        Some(5)
    } else {
        None
    }
}

fn value_token_len(value: &str) -> usize {
    // Keep an already-substituted placeholder intact so `]` is not treated as a
    // value terminator (`token=[REDACTED]` must stay idempotent).
    if value.starts_with(REDACTED_PLACEHOLDER) {
        return REDACTED_PLACEHOLDER.len();
    }
    value
        .char_indices()
        .find(|(_, ch)| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    ',' | ';' | '&' | '"' | '\'' | '<' | '>' | ')' | ']' | '{' | '}'
                )
        })
        .map(|(i, _)| i)
        .unwrap_or(value.len())
}

fn replace_auth_schemes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let lower = input.to_ascii_lowercase();
    let mut idx = 0;
    while idx < input.len() {
        let rest_lower = &lower[idx..];
        let scheme = if rest_lower.starts_with("bearer ") {
            Some(6usize)
        } else if rest_lower.starts_with("basic ") {
            Some(5usize)
        } else {
            None
        };
        if let Some(label_len) = scheme {
            let boundary_ok = idx == 0 || {
                let prev = lower.as_bytes()[idx - 1];
                !prev.is_ascii_alphanumeric() && prev != b'_'
            };
            if boundary_ok {
                let after = &input[idx + label_len..];
                let after_ws = after.trim_start();
                let ws = after.len() - after_ws.len();
                if !after_ws.is_empty() {
                    let token_len = value_token_len(after_ws);
                    out.push_str(&input[idx..idx + label_len]);
                    out.push_str(&after[..ws]);
                    out.push_str(REDACTED_PLACEHOLDER);
                    idx = idx + label_len + ws + token_len;
                    continue;
                }
            }
        }
        let ch = input[idx..].chars().next().unwrap();
        out.push(ch);
        idx += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record(id: &str, server: &str) -> DiagnosisRecord {
        DiagnosisRecord {
            request_id: id.to_string(),
            server_id: server.to_string(),
            media_id: "m1".to_string(),
            ua: "ua".to_string(),
            steps: vec![DiagnosisStep {
                name: "openlist".to_string(),
                duration_ms: 1,
                details: "ok".to_string(),
                ..Default::default()
            }],
            cache_hit: false,
            failure_node: None,
            sanitized_errors: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn nested_diagnosis_fields_are_sanitized_at_every_boundary() {
        let unsafe_url = "https://user:synthetic-pass@cdn.example.test/a.mkv?provider-ticket=synthetic-signature#synthetic-fragment";
        let mut record = sample_record("nested", "s");
        record.context = DiagnosisContext {
            media_name: Some("Movie token=synthetic-token".to_string()),
            request_path: Some("/videos/movie/stream?opaque=synthetic-query".to_string()),
            device_name: Some("Device\u{0000}".to_string()),
            ..Default::default()
        };
        record.paths = DiagnosisPaths {
            original: Some(unsafe_url.to_string()),
            mapped: Some(unsafe_url.to_string()),
            resolved: Some(unsafe_url.to_string()),
            final_url: Some(unsafe_url.to_string()),
            matched_rules: vec![2],
        };
        record.result.reason = Some(format!("fallback {unsafe_url}"));
        record.cache.reason = Some("password=synthetic-password".to_string());
        record.steps[0].reason = Some("Authorization: Bearer synthetic-auth".to_string());
        record.steps[0].details = unsafe_url.to_string();
        let once = sanitize_diagnosis_record(record);
        let twice = sanitize_diagnosis_record(once.clone());
        let json = serde_json::to_string(&twice).unwrap();
        for secret in [
            "synthetic-pass",
            "synthetic-signature",
            "synthetic-fragment",
            "synthetic-token",
            "synthetic-query",
            "synthetic-password",
            "synthetic-auth",
        ] {
            assert!(!json.contains(secret), "unexpected unsafe nested value");
        }
        assert_eq!(
            once.paths.final_url.as_deref(),
            Some("https://cdn.example.test/a.mkv")
        );
        assert_eq!(
            once.context.request_path.as_deref(),
            Some("/videos/movie/stream")
        );
        assert_eq!(
            serde_json::to_value(&once).unwrap(),
            serde_json::to_value(&twice).unwrap()
        );
    }

    #[test]
    fn request_ids_use_random_128_bit_identity_before_record_construction() {
        let ids: std::collections::HashSet<_> = (0..1000).map(|_| new_request_id()).collect();
        assert_eq!(ids.len(), 1000);
        assert!(
            ids.iter()
                .all(|id| id.len() == 37 && id.starts_with("diag-"))
        );
    }

    #[test]
    fn creates_diagnosis_record() {
        let steps = vec![DiagnosisStep {
            name: "openlist".to_string(),
            duration_ms: 10,
            details: "ok".to_string(),
            ..Default::default()
        }];
        let record = create_diagnosis_record("server1", "media1", "test-ua", steps, false, None);
        assert!(record.request_id.starts_with("diag-"));
        assert_eq!(record.server_id, "server1");
        assert_eq!(record.media_id, "media1");
        assert_eq!(record.steps[0].details, "ok");
    }

    #[test]
    fn legacy_diagnosis_does_not_invent_timestamps_or_total_duration() {
        let record: DiagnosisRecord = serde_json::from_value(serde_json::json!({
            "request_id": "legacy", "server_id": "s", "media_id": "m", "ua": "UA",
            "steps": [{ "name": "Emby Path", "duration_ms": 7, "details": "resolved" }],
            "cache_hit": false, "failure_node": null, "sanitized_errors": []
        }))
        .unwrap();
        let record = sanitize_diagnosis_record(record);
        assert_eq!(record.started_at_ms, None);
        assert_eq!(record.finished_at_ms, None);
        assert_eq!(record.duration_ms, None);
        assert_eq!(record.steps[0].started_at_ms, None);
        assert_eq!(record.steps[0].finished_at_ms, None);
        assert_eq!(record.steps[0].duration_ms, 7);
        assert!(record.timing.name_resolution_ms.is_none());
        assert!(record.timing.name_resolution_calls.is_none());
        assert!(record.timing.connection_setup_ms.is_none());
        assert!(record.timing.connection_attempts.is_none());
        assert!(record.timing.connection_observation.is_none());
        assert!(record.steps[0].name_resolution_ms.is_none());
        assert!(record.steps[0].name_resolution_calls.is_none());
        assert!(record.steps[0].connection_setup_ms.is_none());
        assert!(record.steps[0].connection_attempts.is_none());
        assert!(record.steps[0].connection_observation.is_none());
    }

    #[tokio::test]
    async fn diagnosis_timing_survives_collection_and_serialization() {
        let mut record = sample_record("timed", "s");
        record.started_at_ms = Some(1_700_000_000_123);
        record.finished_at_ms = Some(1_700_000_000_169);
        record.duration_ms = Some(46);
        record.steps[0].started_at_ms = Some(1_700_000_000_124);
        record.steps[0].finished_at_ms = Some(1_700_000_000_125);
        let collector = DiagnosisCollector::new(1);
        collector.record(record).await;
        let json = serde_json::to_value(&collector.get_records()[0]).unwrap();
        assert_eq!(json["started_at_ms"], 1_700_000_000_123_i64);
        assert_eq!(json["finished_at_ms"], 1_700_000_000_169_i64);
        assert_eq!(json["duration_ms"], 46);
        assert_eq!(json["steps"][0]["started_at_ms"], 1_700_000_000_124_i64);
        assert_eq!(json["steps"][0]["finished_at_ms"], 1_700_000_000_125_i64);
    }

    #[test]
    fn diagnosis_persistent_collector_clear_cannot_resurrect_queued_memory_write() {
        let path = std::env::temp_dir().join(format!(
            "embypanel-collector-clear-{:032x}.db",
            rand::random::<u128>()
        ));
        let store = crate::db::SettingsStore::open_for_test(path).unwrap();
        let queue = crate::db::TelemetryWriteQueue::start(store.clone()).unwrap();
        let collector = DiagnosisCollector::new(1000);
        *collector.persistence.lock().unwrap() = Some(queue.clone());
        let (reached, ready) = std::sync::mpsc::sync_channel(1);
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let buffer = collector.buf.lock().unwrap();
        let write = {
            let collector = collector.clone();
            std::thread::spawn(move || {
                TEST_BEFORE_BUFFER_LOCK.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        reached.send(()).unwrap();
                        released.recv().unwrap();
                    }));
                });
                collector.ingest(sample_record("before-clear", "s"));
            })
        };
        ready.recv().unwrap();
        // Pause exactly before the memory lock, then drain every accepted queue
        // message. An enqueue-before-push implementation deterministically leaves
        // a persisted row here, regardless of thread scheduling or SQLite speed.
        queue.shutdown().unwrap();
        let before_memory_commit = store.list_diagnosis_records_scoped(None).unwrap();
        let queue = crate::db::TelemetryWriteQueue::start(store.clone()).unwrap();
        *collector.persistence.lock().unwrap() = Some(queue.clone());
        drop(buffer);
        release.send(()).unwrap();
        write.join().unwrap();
        assert!(
            before_memory_commit.is_empty(),
            "a record must not reach persistence before committing memory"
        );
        queue.clear_diagnosis_records().unwrap();
        collector.clear();
        assert!(collector.get_records().is_empty());
        assert!(
            store
                .list_diagnosis_records_scoped(None)
                .unwrap()
                .is_empty()
        );
        collector.ingest(sample_record("after-clear", "s"));
        queue.shutdown().unwrap();
        assert_eq!(collector.get_records()[0].request_id, "after-clear");
        assert_eq!(
            store.list_diagnosis_records_scoped(None).unwrap()[0].request_id,
            "after-clear"
        );
    }

    #[test]
    fn step_order_is_correct() {
        let steps = vec![
            DiagnosisStep {
                name: "openlist".to_string(),
                duration_ms: 10,
                details: "ok".to_string(),
                ..Default::default()
            },
            DiagnosisStep {
                name: "cache".to_string(),
                duration_ms: 5,
                details: "miss".to_string(),
                ..Default::default()
            },
        ];
        let record = create_diagnosis_record("s", "m", "u", steps, false, None);
        assert_eq!(record.steps[0].name, "openlist");
        assert_eq!(record.steps[1].name, "cache");
    }

    #[test]
    fn redacts_case_variants_and_preserves_context() {
        let mut steps = vec![
            DiagnosisStep {
                name: "openlist".to_string(),
                duration_ms: 10,
                details: "API_KEY=secret123 item=abc".to_string(),
                ..Default::default()
            },
            DiagnosisStep {
                name: "cache".to_string(),
                duration_ms: 5,
                details: "status=ok media_id=42".to_string(),
                ..Default::default()
            },
        ];
        redact_sensitive_info(&mut steps);
        assert!(steps[0].details.contains(REDACTED_PLACEHOLDER));
        assert!(!steps[0].details.contains("secret123"));
        assert!(steps[0].details.contains("item=abc"));
        assert_eq!(steps[1].details, "status=ok media_id=42");
    }

    #[test]
    fn redacts_authorization_bearer_basic_cookie_and_json() {
        let details = concat!(
            "Authorization: Bearer super-secret-token; ",
            "Cookie: session=abc123def; ",
            "Basic dXNlcjpwYXNz; ",
            r#"{"api_key":"nested-secret","media":"ok"}"#
        );
        let record = create_diagnosis_record(
            "s1",
            "m1",
            "ua",
            vec![DiagnosisStep {
                name: "auth".to_string(),
                duration_ms: 1,
                details: details.to_string(),
                ..Default::default()
            }],
            false,
            Some("auth".to_string()),
        );
        let json = serde_json::to_string(&record).expect("serialize non-empty dto");
        assert!(!json.contains("super-secret-token"), "{json}");
        assert!(!json.contains("session=abc123def"), "{json}");
        assert!(!json.contains("dXNlcjpwYXNz"), "{json}");
        assert!(!json.contains("nested-secret"), "{json}");
        assert!(json.contains(REDACTED_PLACEHOLDER), "{json}");
        assert!(json.contains("media"), "{json}");
        assert!(!record.sanitized_errors.is_empty());
    }

    #[test]
    fn redacts_url_userinfo_and_query_tokens() {
        let details = "fetch https://user:pass@cdn.example.com/play.m3u8?api_key=leak&quality=1080 token=also-leak";
        let sanitized = sanitize_diagnostic_text(details);
        assert!(
            !sanitized.contains("user:pass"),
            "userinfo leaked: {sanitized}"
        );
        assert!(!sanitized.contains("leak"), "secret leaked: {sanitized}");
        assert!(!sanitized.contains("also-leak"), "{sanitized}");
        assert!(
            sanitized.contains("quality=1080") || sanitized.contains("cdn.example.com"),
            "over-redacted non-sensitive context: {sanitized}"
        );
        assert!(sanitized.contains(REDACTED_PLACEHOLDER), "{sanitized}");
    }

    #[test]
    fn combined_sensitive_text_serializes_without_secrets() {
        let steps = vec![DiagnosisStep {
            name: "proxy".to_string(),
            duration_ms: 3,
            details: concat!(
                "PASSWD=p@ss; Set-Cookie: cookieval; ",
                "https://alice:bob@host/item?Token=T0PSECRET&x-emby-token=E1SECRET&path=/Movies/1.mkv ",
                r#"Authorization=Bearer ZZZSECRET json={"Password":"hunter2"}"#
            )
            .to_string(),
            ..Default::default()
        }];
        let record = create_diagnosis_record("srv", "media-9", "Emby/1.0", steps, true, None);
        let json = serde_json::to_string_pretty(&record).unwrap();
        for secret in [
            "p@ss",
            "alice:bob",
            "T0PSECRET",
            "E1SECRET",
            "ZZZSECRET",
            "hunter2",
            "cookieval",
        ] {
            assert!(!json.contains(secret), "secret `{secret}` in {json}");
        }
        assert!(json.contains("media-9"));
        assert!(json.contains("cache_hit"));
    }

    #[test]
    fn sanitizes_identifier_fields_and_failure_node() {
        let record = create_diagnosis_record(
            "https://diag.example.test/?api_key=request-secret",
            "https://media.example.test/item?token=media-secret",
            "UA",
            vec![],
            false,
            Some("https://node.example.test/?cookie=node-secret".into()),
        );
        let json = serde_json::to_string(&record).unwrap();
        for secret in ["request-secret", "media-secret", "node-secret"] {
            assert!(!json.contains(secret), "secret `{secret}` in {json}");
        }
        assert!(json.contains(REDACTED_PLACEHOLDER), "{json}");
    }

    #[test]
    fn strips_control_chars_and_clamps_length() {
        let dirty = format!("ok\u{0000}id\u{0007} {}", "x".repeat(MAX_DETAIL_CHARS + 50));
        let cleaned = sanitize_diagnostic_text(&dirty);
        assert!(!cleaned.contains('\0'));
        assert!(!cleaned.chars().any(|c| c.is_control()));
        assert!(cleaned.chars().count() <= MAX_DETAIL_CHARS);

        let record = create_diagnosis_record(
            "  server\u{0001} ",
            &"m".repeat(MAX_ID_CHARS + 10),
            "ua",
            vec![DiagnosisStep {
                name: format!("n\u{0008}"),
                duration_ms: 1,
                details: "fine".into(),
                ..Default::default()
            }],
            false,
            Some(" node\u{0009} ".into()),
        );
        assert!(!record.server_id.contains('\u{0001}'));
        assert!(record.media_id.chars().count() <= MAX_ID_CHARS);
        assert!(!record.steps[0].name.contains('\u{0008}'));
    }

    #[test]
    fn quoted_media_credentials_are_consumed_and_redaction_is_idempotent() {
        for input in [
            r#"Authorization="Bearer quote_canary_42" title"#,
            "Authorization='Basic quote_canary_42' title",
            r#"Token = "quote_canary_42 with spaces" title"#,
            r#"api_key="escaped\"quote_canary_42" title"#,
            r"password='escaped\'quote_canary_42' title",
            r#"Authorization="Bearer quote_canary_42 unfinished"#,
            "Authorization='Basic quote_canary_42 unfinished",
            r#"Token="quote_canary_42\""#,
            r#"Authorization=Bearer "quote_canary_42 with spaces" title"#,
            "Authorization=Basic 'quote_canary_42 with spaces' title",
            r#"Authorization = "Bearer quote_canary_42"; api_key='quote_canary_42'"#,
            r#"Token="电影\"quote_canary_42" title"#,
            r#"Authorization: "Bearer quote_canary_42; more" title"#,
        ] {
            let safe = sanitize_media_label(input);
            assert!(!safe.contains("quote_canary_42"), "{safe}");
            assert_eq!(sanitize_media_label(&safe), safe);
            let strict = sanitize_diagnostic_text(input);
            assert!(!strict.contains("quote_canary_42"), "{strict}");
            assert_eq!(sanitize_diagnostic_text(&strict), strict);
        }
        for input in [
            r#"Token="" title"#,
            "Token='' title",
            r#"Token= "   " title"#,
            r#"Authorization="Bearer quote_canary_42" title"#,
        ] {
            let safe = sanitize_media_label(input);
            assert!(safe.ends_with(" title"), "{safe}");
            assert_eq!(sanitize_media_label(&safe), safe);
        }
    }

    #[test]
    fn review_media_label_preserves_titles_but_redacts_explicit_credentials() {
        for title in [
            "Basic Instinct",
            "The Secret: Dare to Dream",
            "Secret: A Subtitle",
            "Signature: A Story",
            "Bearer of Hope",
        ] {
            assert_eq!(sanitize_media_label(title), title);
        }
        for input in [
            "Authorization: Basic synthetic-credential",
            "Authorization=Bearer synthetic-credential",
            "Token=synthetic-credential",
            "api_key=synthetic-credential",
            "password=synthetic-credential",
            "password: synthetic-credential",
            "secret=synthetic-credential",
            "Cookie: synthetic-credential",
            r#"{"secret":"synthetic-credential","api_key":"synthetic-credential"}"#,
            "https://fixture:synthetic-credential@example.invalid/movie?token=synthetic-credential&other=synthetic-credential#synthetic-credential",
        ] {
            let safe = sanitize_media_label(input);
            assert!(!safe.contains("synthetic-credential"), "{safe}");
            assert_eq!(sanitize_media_label(&safe), safe);
        }
        for key in SENSITIVE_KEYS {
            let json = format!(r#"{{"{key}":"fixture_canary_42"}}"#);
            let kv = format!("{key}=fixture_canary_42");
            assert!(!sanitize_media_label(&json).contains("fixture_canary_42"));
            assert!(!sanitize_media_label(&kv).contains("fixture_canary_42"));
        }
        assert_eq!(sanitize_media_label("Basic\n Instinct\0"), "Basic Instinct");
        assert_eq!(sanitize_media_label(&"名".repeat(300)).chars().count(), 256);
        // Diagnostic/free-form callers retain their original strict behavior.
        assert_eq!(
            sanitize_diagnostic_text("Basic Instinct"),
            "Basic [REDACTED]"
        );
        assert_eq!(
            sanitize_diagnostic_text("Secret: subtitle"),
            "Secret: [REDACTED]"
        );
    }

    #[test]
    fn does_not_over_redact_normal_media_context() {
        let text = "resolved item_id=122 path=/Movies/Foo.mkv cache=miss status=200";
        assert_eq!(sanitize_diagnostic_text(text), text);
    }

    #[tokio::test]
    async fn capacity_retains_newest_records_in_stable_order() {
        let collector = DiagnosisCollector::new(2);
        collector.record(sample_record("1", "s")).await;
        collector.record(sample_record("2", "s")).await;
        collector.record(sample_record("3", "s")).await;
        let snapshot = collector.get_records();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].request_id, "2");
        assert_eq!(snapshot[1].request_id, "3");
    }

    #[test]
    fn concurrent_snapshot_is_safe() {
        let collector = DiagnosisCollector::new(4);
        let snap1 = collector.get_records();
        let snap2 = collector.get_records();
        assert_eq!(snap1.len(), snap2.len());
        collector.clear();
        let snap3 = collector.get_records();
        assert!(snap3.is_empty());
    }

    #[test]
    fn sanitize_on_record_edge_is_idempotent() {
        let once = sanitize_diagnostic_text("token=abc Authorization: Bearer zzz");
        let twice = sanitize_diagnostic_text(&once);
        assert_eq!(once, twice);
        assert!(!twice.contains("abc"));
        assert!(!twice.contains("zzz"));
    }

    #[test]
    fn url_redaction_is_idempotent_across_record_boundaries() {
        let once = sanitize_diagnostic_text(
            "resolved https://cdn.example.test/movie.mkv?token=secret&quality=2160",
        );
        let twice = sanitize_diagnostic_text(&once);
        let thrice = sanitize_diagnostic_text(&twice);
        assert_eq!(once, twice);
        assert_eq!(twice, thrice);
        assert!(thrice.contains("token=[REDACTED]&quality=2160"));
        assert!(!thrice.contains("]]"));
        assert!(!thrice.contains("secret"));
    }

    #[test]
    fn successful_redacted_steps_are_not_reported_as_errors() {
        let record = create_diagnosis_record(
            "server",
            "media",
            "ua token=secret",
            vec![DiagnosisStep {
                name: "raw_url".to_string(),
                duration_ms: 1,
                details: "https://cdn.example.test/movie.mkv?token=secret".to_string(),
                ..Default::default()
            }],
            false,
            None,
        );
        assert!(record.sanitized_errors.is_empty());
        assert!(record.steps[0].details.contains(REDACTED_PLACEHOLDER));
    }
}
