use std::{
    collections::{HashMap, HashSet},
    future::Future,
    net::SocketAddr,
    time::Duration,
};

use serde::Serialize;
use tokio::{sync::Mutex, task::JoinSet};

use crate::{
    AppState,
    activity_log::{ActivityKind, ActivityLevel},
    config::Config,
    error::{AppResult, safe_error_message},
};

#[derive(Debug, Clone, Serialize)]
pub struct ConnectivityCheckStatus {
    pub server_id: String,
    pub server_name: String,
    pub port: u16,
    pub enabled: bool,
    pub ok: bool,
    pub emby_ok: bool,
    pub openlist_ok: Option<bool>,
    pub proxy_ok: bool,
    pub checked_at_ms: u128,
    pub duration_ms: u128,
    pub failed_since_ms: Option<u128>,
    pub auto_restarted_at_ms: Option<u128>,
    pub last_error: Option<String>,
}

#[derive(Default)]
pub struct ConnectivityMonitor {
    statuses: Mutex<HashMap<String, ConnectivityCheckStatus>>,
    last_checked: Mutex<HashMap<String, u128>>,
    proxy_failed_since: Mutex<HashMap<String, u128>>,
    last_error: Mutex<HashMap<String, String>>,
    last_restart: Mutex<HashMap<String, u128>>,
}

impl ConnectivityMonitor {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn statuses(&self, config: &Config) -> Vec<ConnectivityCheckStatus> {
        let statuses = self.statuses.lock().await;
        config
            .servers
            .iter()
            .map(|server| {
                let effective = config.for_server_for_validation(server);
                let enabled = server.enabled
                    && !server.emby_api_key.trim().is_empty()
                    && effective.connectivity_check_enabled;
                if enabled {
                    statuses
                        .get(&server.id)
                        .cloned()
                        .unwrap_or_else(|| initial_status(&effective, true))
                } else {
                    initial_status(&effective, false)
                }
            })
            .collect()
    }

    async fn status_snapshot(&self) -> HashMap<String, ConnectivityCheckStatus> {
        self.statuses.lock().await.clone()
    }

    async fn last_checked_snapshot(&self) -> HashMap<String, u128> {
        self.last_checked.lock().await.clone()
    }

    async fn mark_checked(&self, server_id: &str, checked_at_ms: u128) {
        self.last_checked
            .lock()
            .await
            .insert(server_id.to_string(), checked_at_ms);
    }

    async fn mark_disabled(&self, server_id: &str) {
        self.statuses.lock().await.remove(server_id);
        self.last_checked.lock().await.remove(server_id);
        self.proxy_failed_since.lock().await.remove(server_id);
        self.last_error.lock().await.remove(server_id);
        self.last_restart.lock().await.remove(server_id);
    }

    async fn clear_inactive(&self, active_server_ids: &HashSet<String>) {
        self.statuses
            .lock()
            .await
            .retain(|server_id, _| active_server_ids.contains(server_id));
        self.last_checked
            .lock()
            .await
            .retain(|server_id, _| active_server_ids.contains(server_id));
        self.proxy_failed_since
            .lock()
            .await
            .retain(|server_id, _| active_server_ids.contains(server_id));
        self.last_error
            .lock()
            .await
            .retain(|server_id, _| active_server_ids.contains(server_id));
        self.last_restart
            .lock()
            .await
            .retain(|server_id, _| active_server_ids.contains(server_id));
    }

    async fn replace_statuses(&self, statuses: Vec<ConnectivityCheckStatus>) {
        let mut current = self.statuses.lock().await;
        current.clear();
        for status in statuses {
            current.insert(status.server_id.clone(), status);
        }
    }

    async fn update_restart_time(&self, server_id: &str, restarted_at_ms: u128) {
        if let Some(status) = self.statuses.lock().await.get_mut(server_id) {
            status.auto_restarted_at_ms = Some(restarted_at_ms);
        }
        self.last_restart
            .lock()
            .await
            .insert(server_id.to_string(), restarted_at_ms);
        self.proxy_failed_since.lock().await.remove(server_id);
    }
}

pub fn start_connectivity_monitor(state: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let interval = run_connectivity_check_once(&state).await;
            tokio::time::sleep(Duration::from_secs(interval.max(10))).await;
        }
    })
}

async fn run_connectivity_check_once(state: &AppState) -> u64 {
    let config = state.config.read().await.clone();
    let configs = config.proxy_configs();
    let active_server_ids = active_server_ids(&configs);
    state.connectivity.clear_inactive(&active_server_ids).await;
    let interval = minimum_enabled_interval(&configs)
        .unwrap_or_else(|| config.connectivity_check_interval_seconds.max(10));
    let previous_statuses = state.connectivity.status_snapshot().await;
    let last_checked = state.connectivity.last_checked_snapshot().await;
    let now = crate::now_ms();
    let mut statuses = (0..configs.len())
        .map(|_| None)
        .collect::<Vec<Option<ConnectivityCheckStatus>>>();
    let mut due_checks = Vec::new();

    for (index, server_config) in configs.iter().enumerate() {
        let policy = ConnectivityPolicy::from_config(server_config);
        let server_id = server_config
            .servers
            .first()
            .map(|server| server.id.as_str())
            .unwrap_or("default");
        if !policy.enabled {
            state.connectivity.mark_disabled(server_id).await;
            statuses[index] = Some(initial_status(server_config, false));
            continue;
        }
        if !is_check_due(
            last_checked.get(server_id).copied(),
            now,
            policy.interval_seconds,
        ) {
            statuses[index] = Some(
                previous_statuses
                    .get(server_id)
                    .cloned()
                    .unwrap_or_else(|| initial_status(server_config, true)),
            );
            continue;
        }

        due_checks.push((
            index,
            server_config.clone(),
            Duration::from_secs(policy.timeout_seconds),
            policy.auto_restart_seconds,
        ));
    }

    let due_probe_inputs = due_checks
        .iter()
        .enumerate()
        .map(|(due_index, (_, server_config, timeout, _))| {
            (due_index, server_config.clone(), *timeout)
        })
        .collect::<Vec<_>>();
    let collected_results = collect_connectivity_checks(due_probe_inputs.into_iter().map(
        |(due_index, server_config, timeout)| {
            let state = (*state).clone();
            (due_index, async move {
                check_server(&state, &server_config, timeout).await
            })
        },
    ))
    .await;
    let mut ordered_results = (0..due_checks.len())
        .map(|_| None)
        .collect::<Vec<Option<Result<ConnectivityCheckStatus, tokio::task::JoinError>>>>();
    for (due_index, result) in collected_results {
        let Some(slot) = ordered_results.get_mut(due_index) else {
            tracing::error!(due_index, "connectivity check result index out of range");
            continue;
        };
        if slot.is_some() {
            tracing::error!(due_index, "duplicate connectivity check result index");
            continue;
        }
        *slot = Some(result);
    }

    for (due_index, (index, server_config, _, auto_restart_seconds)) in
        due_checks.iter().enumerate()
    {
        let server_id = server_config
            .servers
            .first()
            .map(|server| server.id.as_str())
            .unwrap_or("default");
        let mut status = match ordered_results[due_index].take() {
            Some(Ok(status)) => status,
            Some(Err(err)) => {
                tracing::error!(
                    server_id,
                    error = %err,
                    "connectivity check task failed; using safe failure status"
                );
                failed_probe_status(server_config, previous_statuses.get(server_id))
            }
            None => {
                tracing::error!(
                    server_id,
                    "connectivity check result missing; using safe failure status"
                );
                failed_probe_status(server_config, previous_statuses.get(server_id))
            }
        };
        handle_status_change(state, &status).await;
        if let Some(restarted_at_ms) =
            handle_auto_restart(state, &status, *auto_restart_seconds).await
        {
            status.auto_restarted_at_ms = Some(restarted_at_ms);
        }
        state
            .connectivity
            .mark_checked(server_id, crate::now_ms())
            .await;
        statuses[*index] = Some(status);
    }

    let statuses = statuses
        .into_iter()
        .enumerate()
        .map(|(index, status)| {
            status.unwrap_or_else(|| {
                let server_config = &configs[index];
                let server_id = server_config
                    .servers
                    .first()
                    .map(|server| server.id.as_str())
                    .unwrap_or("default");
                tracing::error!(server_id, "connectivity check status missing");
                if ConnectivityPolicy::from_config(server_config).enabled {
                    failed_probe_status(server_config, previous_statuses.get(server_id))
                } else {
                    initial_status(server_config, false)
                }
            })
        })
        .collect();
    state.connectivity.replace_statuses(statuses).await;
    interval
}

const CONNECTIVITY_CHECK_CONCURRENCY_LIMIT: usize = 4;

async fn collect_connectivity_checks<I, F, T>(
    checks: I,
) -> Vec<(usize, Result<T, tokio::task::JoinError>)>
where
    I: IntoIterator<Item = (usize, F)>,
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let mut pending = checks.into_iter();
    let mut tasks = JoinSet::new();
    let mut task_indexes = HashMap::new();

    for _ in 0..CONNECTIVITY_CHECK_CONCURRENCY_LIMIT {
        let Some((index, future)) = pending.next() else {
            break;
        };
        let task_id = tasks.spawn(async move { (index, future.await) }).id();
        task_indexes.insert(task_id, index);
    }

    let mut results = Vec::new();
    while let Some(result) = tasks.join_next_with_id().await {
        match result {
            Ok((task_id, (index, result))) => {
                if task_indexes.remove(&task_id).is_none() {
                    tracing::error!(task_id = ?task_id, index, "connectivity check task index missing");
                }
                results.push((index, Ok(result)));
            }
            Err(err) => {
                let task_id = err.id();
                let Some(index) = task_indexes.remove(&task_id) else {
                    tracing::error!(
                        task_id = ?task_id,
                        error = %err,
                        "connectivity check task failed without an index"
                    );
                    continue;
                };
                results.push((index, Err(err)));
            }
        }

        let Some((index, future)) = pending.next() else {
            continue;
        };
        let task_id = tasks.spawn(async move { (index, future.await) }).id();
        task_indexes.insert(task_id, index);
    }

    results.sort_unstable_by_key(|(index, _)| *index);
    results
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ConnectivityPolicy {
    enabled: bool,
    interval_seconds: u64,
    timeout_seconds: u64,
    auto_restart_seconds: u64,
}

impl ConnectivityPolicy {
    fn from_config(config: &Config) -> Self {
        Self {
            enabled: config.connectivity_check_enabled,
            interval_seconds: config.connectivity_check_interval_seconds.max(10),
            timeout_seconds: config.connectivity_check_timeout_seconds.max(1),
            auto_restart_seconds: config.connectivity_auto_restart_seconds,
        }
    }
}

fn minimum_enabled_interval(configs: &[Config]) -> Option<u64> {
    configs
        .iter()
        .map(ConnectivityPolicy::from_config)
        .filter(|policy| policy.enabled)
        .map(|policy| policy.interval_seconds)
        .min()
}

fn active_server_ids(configs: &[Config]) -> HashSet<String> {
    configs
        .iter()
        .filter_map(|server_config| {
            server_config
                .servers
                .first()
                .map(|server| server.id.clone())
        })
        .collect()
}

fn is_check_due(last_checked_ms: Option<u128>, now_ms: u128, interval_seconds: u64) -> bool {
    last_checked_ms.is_none_or(|last_checked_ms| {
        now_ms.saturating_sub(last_checked_ms) >= u128::from(interval_seconds.max(10)) * 1000
    })
}

fn initial_status(config: &Config, enabled: bool) -> ConnectivityCheckStatus {
    let (server_id, server_name) = config
        .servers
        .first()
        .map(|server| (server.id.clone(), server.name.clone()))
        .unwrap_or_else(|| ("default".to_string(), "默认服务器".to_string()));
    ConnectivityCheckStatus {
        server_id,
        server_name,
        port: config.port,
        enabled,
        ok: false,
        emby_ok: false,
        openlist_ok: config.openlist_addr.as_ref().map(|_| false),
        proxy_ok: false,
        checked_at_ms: 0,
        duration_ms: 0,
        failed_since_ms: None,
        auto_restarted_at_ms: None,
        last_error: Some(if enabled {
            "尚未巡检".to_string()
        } else {
            "连通性巡检未启用".to_string()
        }),
    }
}

fn failed_probe_status(
    config: &Config,
    previous: Option<&ConnectivityCheckStatus>,
) -> ConnectivityCheckStatus {
    let mut status = initial_status(config, true);
    status.auto_restarted_at_ms = previous.and_then(|status| status.auto_restarted_at_ms);
    status.checked_at_ms = crate::now_ms();
    status.proxy_ok = true;
    status.failed_since_ms = None;
    status.last_error = Some("连通性巡检任务异常".to_string());
    status
}

async fn check_server(
    state: &AppState,
    config: &Config,
    timeout: Duration,
) -> ConnectivityCheckStatus {
    let started_at = crate::now_ms();
    let started = std::time::Instant::now();
    let (server_id, server_name) = config
        .servers
        .first()
        .map(|server| (server.id.clone(), server.name.clone()))
        .unwrap_or_else(|| ("default".to_string(), "默认服务器".to_string()));

    let emby_result = tokio::time::timeout(
        timeout,
        crate::emby::validate_connection(&state.client, config),
    )
    .await
    .map_err(|_| "Emby 连接超时".to_string())
    .and_then(|result| result.map_err(|err| safe_error_message(&err)));
    let proxy_result = check_proxy_port(config.port, timeout).await;
    let openlist_result = if config.openlist_addr.is_some() {
        Some(
            tokio::time::timeout(
                timeout,
                crate::openlist::validate_connection(&state.client, config),
            )
            .await
            .map_err(|_| "OpenList 连接超时".to_string())
            .and_then(|result| result.map_err(|err| safe_error_message(&err))),
        )
    } else {
        None
    };

    let mut errors = Vec::new();
    if let Err(err) = &emby_result {
        errors.push(format!("Emby: {err}"));
    }
    if let Err(err) = &proxy_result {
        errors.push(format!("反代端口: {err}"));
    }
    if let Some(Err(err)) = &openlist_result {
        errors.push(format!("OpenList: {err}"));
    }

    let now = crate::now_ms();
    let proxy_ok = proxy_result.is_ok();
    let failed_since_ms = update_proxy_failure_since(state, &server_id, proxy_ok, now).await;
    let openlist_ok = openlist_result.as_ref().map(Result::is_ok);
    let ok = emby_result.is_ok() && proxy_ok && openlist_ok.unwrap_or(true);

    ConnectivityCheckStatus {
        server_id,
        server_name,
        port: config.port,
        enabled: true,
        ok,
        emby_ok: emby_result.is_ok(),
        openlist_ok,
        proxy_ok,
        checked_at_ms: started_at,
        duration_ms: started.elapsed().as_millis(),
        failed_since_ms,
        auto_restarted_at_ms: state
            .connectivity
            .last_restart
            .lock()
            .await
            .get(
                config
                    .servers
                    .first()
                    .map(|server| server.id.as_str())
                    .unwrap_or("default"),
            )
            .copied(),
        last_error: if errors.is_empty() {
            None
        } else {
            Some(errors.join("；"))
        },
    }
}

async fn check_proxy_port(port: u16, timeout: Duration) -> Result<(), String> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr))
        .await
        .map_err(|_| format!("连接 127.0.0.1:{port} 超时"))?
        .map(|_| ())
        .map_err(|err| format!("连接 127.0.0.1:{port} 失败: {err}"))
}

async fn update_proxy_failure_since(
    state: &AppState,
    server_id: &str,
    proxy_ok: bool,
    now: u128,
) -> Option<u128> {
    let mut failures = state.connectivity.proxy_failed_since.lock().await;
    if proxy_ok {
        failures.remove(server_id);
        None
    } else {
        Some(*failures.entry(server_id.to_string()).or_insert(now))
    }
}

async fn handle_status_change(state: &AppState, status: &ConnectivityCheckStatus) {
    let error_key = status.last_error.clone().unwrap_or_default();
    let mut last_errors = state.connectivity.last_error.lock().await;
    let previous = last_errors
        .get(&status.server_id)
        .cloned()
        .unwrap_or_default();
    if status.ok {
        if !previous.is_empty() {
            last_errors.remove(&status.server_id);
            state.activity_log.record(
                ActivityKind::General,
                ActivityLevel::Info,
                Some(&status.server_id),
                &status.server_name,
                "服务器连通性恢复",
                "Emby、OpenList 和反代端口巡检通过",
            );
            state
                .file_log
                .write("info", "服务器连通性恢复", &status.server_name);
        }
        return;
    }
    if previous == error_key {
        return;
    }
    last_errors.insert(status.server_id.clone(), error_key.clone());
    drop(last_errors);

    state.activity_log.record(
        ActivityKind::General,
        ActivityLevel::Warn,
        Some(&status.server_id),
        &status.server_name,
        "服务器连通性异常",
        error_key.clone(),
    );
    state
        .file_log
        .write("warning", "服务器连通性异常", &error_key);
    crate::client_control::notify_connectivity_issue(
        state,
        &status.server_id,
        &status.server_name,
        &format!("{} 连通性异常", status.server_name),
        &format!(
            "服务器：{}\n端口：{}\n检查时间：{}\n异常：{}",
            status.server_name, status.port, status.checked_at_ms, error_key
        ),
    )
    .await;
}

async fn handle_auto_restart(
    state: &AppState,
    status: &ConnectivityCheckStatus,
    auto_restart_seconds: u64,
) -> Option<u128> {
    if auto_restart_seconds == 0 || status.proxy_ok {
        return None;
    }
    let failed_since_ms = status.failed_since_ms?;
    let now = crate::now_ms();
    if now.saturating_sub(failed_since_ms) < u128::from(auto_restart_seconds) * 1000 {
        return None;
    }
    let last_restart = state
        .connectivity
        .last_restart
        .lock()
        .await
        .get(&status.server_id)
        .copied()
        .unwrap_or_default();
    if now.saturating_sub(last_restart) < u128::from(auto_restart_seconds) * 1000 {
        return None;
    }

    let result: AppResult<()> = if let Some(proxy_manager) = state.proxy_manager.as_ref() {
        proxy_manager
            .restart_server(state.clone(), &status.server_id)
            .await
    } else {
        Ok(())
    };
    match result {
        Ok(()) => {
            state
                .connectivity
                .update_restart_time(&status.server_id, now)
                .await;
            state.activity_log.record(
                ActivityKind::General,
                ActivityLevel::Warn,
                Some(&status.server_id),
                &status.server_name,
                "反代端口无响应，已自动重启",
                format!("连续无响应 {} 秒", auto_restart_seconds),
            );
            state.file_log.write(
                "warning",
                "反代端口无响应，已自动重启",
                &format!("{} :{}", status.server_name, status.port),
            );
            Some(now)
        }
        Err(err) => {
            state.activity_log.record(
                ActivityKind::General,
                ActivityLevel::Error,
                Some(&status.server_id),
                &status.server_name,
                "反代自动重启失败",
                err.safe_log_message(),
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::config::EmbyServerConfig;

    use super::*;

    fn server(id: &str) -> EmbyServerConfig {
        EmbyServerConfig {
            id: id.to_string(),
            name: id.to_string(),
            emby_host: "http://127.0.0.1:8096".to_string(),
            emby_api_key: "key".to_string(),
            port: 18096,
            enabled: true,
            ..EmbyServerConfig::default()
        }
    }

    fn status(server_id: &str) -> ConnectivityCheckStatus {
        ConnectivityCheckStatus {
            server_id: server_id.to_string(),
            server_name: server_id.to_string(),
            port: 18096,
            enabled: true,
            ok: false,
            emby_ok: false,
            openlist_ok: None,
            proxy_ok: false,
            checked_at_ms: 1,
            duration_ms: 1,
            failed_since_ms: Some(1),
            auto_restarted_at_ms: Some(1),
            last_error: Some("stale".to_string()),
        }
    }

    async fn seed_history(monitor: &ConnectivityMonitor, server_id: &str) {
        monitor
            .statuses
            .lock()
            .await
            .insert(server_id.to_string(), status(server_id));
        monitor
            .last_checked
            .lock()
            .await
            .insert(server_id.to_string(), 1);
        monitor
            .proxy_failed_since
            .lock()
            .await
            .insert(server_id.to_string(), 1);
        monitor
            .last_error
            .lock()
            .await
            .insert(server_id.to_string(), "stale".to_string());
        monitor
            .last_restart
            .lock()
            .await
            .insert(server_id.to_string(), 1);
    }

    async fn assert_history_absent(monitor: &ConnectivityMonitor, server_id: &str) {
        assert!(!monitor.statuses.lock().await.contains_key(server_id));
        assert!(!monitor.last_checked.lock().await.contains_key(server_id));
        assert!(
            !monitor
                .proxy_failed_since
                .lock()
                .await
                .contains_key(server_id)
        );
        assert!(!monitor.last_error.lock().await.contains_key(server_id));
        assert!(!monitor.last_restart.lock().await.contains_key(server_id));
    }

    async fn gated_probe(
        value: usize,
        release: tokio::sync::oneshot::Receiver<()>,
        started: tokio::sync::mpsc::UnboundedSender<usize>,
        active: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        max_active: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) -> usize {
        let active_now = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        max_active.fetch_max(active_now, std::sync::atomic::Ordering::SeqCst);
        started
            .send(value)
            .expect("collector test receiver is alive");
        let _ = release.await;
        active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        value
    }

    async fn maybe_panics(value: usize, should_panic: bool) -> usize {
        if should_panic {
            panic!("collector test task panic");
        }
        value
    }

    #[tokio::test]
    async fn connectivity_check_collector_is_bounded_and_keeps_input_order() {
        let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut releases = Vec::new();
        let mut checks = Vec::new();
        for value in 0..6 {
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            releases.push(Some(release_tx));
            checks.push((
                value,
                gated_probe(
                    value,
                    release_rx,
                    started_tx.clone(),
                    active.clone(),
                    max_active.clone(),
                ),
            ));
        }
        drop(started_tx);

        let collector = tokio::spawn(collect_connectivity_checks(checks));
        let mut initially_started = Vec::new();
        for _ in 0..CONNECTIVITY_CHECK_CONCURRENCY_LIMIT {
            initially_started.push(started_rx.recv().await.expect("probe should start"));
        }
        initially_started.sort_unstable();
        assert_eq!(
            initially_started,
            (0..CONNECTIVITY_CHECK_CONCURRENCY_LIMIT).collect::<Vec<_>>()
        );
        assert_eq!(
            max_active.load(std::sync::atomic::Ordering::SeqCst),
            CONNECTIVITY_CHECK_CONCURRENCY_LIMIT
        );
        assert!(started_rx.try_recv().is_err());

        releases[0]
            .take()
            .expect("release 0 exists")
            .send(())
            .unwrap();
        assert_eq!(
            started_rx.recv().await,
            Some(CONNECTIVITY_CHECK_CONCURRENCY_LIMIT)
        );
        for release in releases.iter_mut().skip(1) {
            release.take().expect("release exists").send(()).unwrap();
        }

        let results = collector.await.expect("collector task did not panic");
        let ordered = results
            .into_iter()
            .map(|(index, result)| (index, result.expect("probe should succeed")))
            .collect::<Vec<_>>();
        assert_eq!(
            ordered,
            (0..6).map(|value| (value, value)).collect::<Vec<_>>()
        );
        assert_eq!(
            max_active.load(std::sync::atomic::Ordering::SeqCst),
            CONNECTIVITY_CHECK_CONCURRENCY_LIMIT
        );
        assert_eq!(active.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn connectivity_check_collector_preserves_a_join_error_slot() {
        let results = collect_connectivity_checks(vec![
            (2, maybe_panics(2, false)),
            (0, maybe_panics(0, false)),
            (1, maybe_panics(1, true)),
        ])
        .await;

        assert_eq!(results.len(), 3);
        assert_eq!(
            results.iter().map(|(index, _)| *index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(results[1].1.is_err());
    }

    #[test]
    fn server_override_can_enable_check_when_global_default_is_disabled() {
        let mut config = Config::default_runtime();
        config.connectivity_check_enabled = false;
        config.connectivity_check_interval_seconds = 60;
        config.connectivity_check_timeout_seconds = 5;
        config.connectivity_auto_restart_seconds = 180;
        config.servers = vec![EmbyServerConfig {
            connectivity_check_enabled_override: Some(true),
            connectivity_check_interval_seconds_override: Some(20),
            connectivity_check_timeout_seconds_override: Some(2),
            connectivity_auto_restart_seconds_override: Some(45),
            ..server("enabled-override")
        }];

        let configs = config.proxy_configs();
        let policy = ConnectivityPolicy::from_config(&configs[0]);

        assert_eq!(
            policy,
            ConnectivityPolicy {
                enabled: true,
                interval_seconds: 20,
                timeout_seconds: 2,
                auto_restart_seconds: 45,
            }
        );
        assert_eq!(minimum_enabled_interval(&configs), Some(20));
    }

    #[test]
    fn per_server_intervals_control_wakeup_without_making_slow_server_due_early() {
        let mut config = Config::default_runtime();
        config.servers = vec![
            EmbyServerConfig {
                connectivity_check_interval_seconds_override: Some(10),
                connectivity_check_timeout_seconds_override: Some(1),
                connectivity_auto_restart_seconds_override: Some(0),
                ..server("fast")
            },
            EmbyServerConfig {
                connectivity_check_interval_seconds_override: Some(60),
                connectivity_check_timeout_seconds_override: Some(9),
                connectivity_auto_restart_seconds_override: Some(300),
                ..server("slow")
            },
        ];

        let configs = config.proxy_configs();
        let slow_policy = ConnectivityPolicy::from_config(&configs[1]);

        assert_eq!(minimum_enabled_interval(&configs), Some(10));
        assert_eq!(slow_policy.timeout_seconds, 9);
        assert_eq!(slow_policy.auto_restart_seconds, 300);
        assert!(is_check_due(None, 10_000, slow_policy.interval_seconds));
        assert!(!is_check_due(
            Some(10_000),
            20_000,
            slow_policy.interval_seconds
        ));
        assert!(is_check_due(
            Some(10_000),
            70_000,
            slow_policy.interval_seconds
        ));
    }

    #[test]
    fn initial_status_uses_effective_openlist_and_disabled_policy() {
        let mut config = Config::default_runtime();
        config.openlist_addr = Some("http://global-openlist.test".to_string());
        config.openlist_token = Some("token".to_string());
        config.servers = vec![EmbyServerConfig {
            openlist_addr_override: Some(String::new()),
            openlist_token_override: Some(String::new()),
            connectivity_check_enabled_override: Some(false),
            ..server("disabled")
        }];
        let effective = config.proxy_configs().remove(0);

        let status = initial_status(&effective, false);

        assert!(!status.enabled);
        assert_eq!(status.openlist_ok, None);
        assert_eq!(status.checked_at_ms, 0);
        assert_eq!(status.last_error.as_deref(), Some("连通性巡检未启用"));
    }

    #[tokio::test]
    async fn policy_disabled_clears_restart_and_all_other_history() {
        let monitor = ConnectivityMonitor::new();
        seed_history(&monitor, "disabled").await;

        monitor.mark_disabled("disabled").await;

        assert_history_absent(&monitor, "disabled").await;
    }

    #[tokio::test]
    async fn inactive_server_ids_are_evicted_from_all_history_maps() {
        let mut config = Config::default_runtime();
        config.servers = vec![
            server("active"),
            EmbyServerConfig {
                enabled: false,
                ..server("disabled")
            },
            EmbyServerConfig {
                emby_api_key: String::new(),
                ..server("missing-key")
            },
        ];
        let configs = config.proxy_configs();
        assert_eq!(
            active_server_ids(&configs),
            HashSet::from(["active".to_string()])
        );

        let monitor = ConnectivityMonitor::new();
        seed_history(&monitor, "active").await;
        seed_history(&monitor, "disabled").await;
        seed_history(&monitor, "missing-key").await;
        seed_history(&monitor, "removed").await;

        monitor.clear_inactive(&active_server_ids(&configs)).await;

        assert_history_absent(&monitor, "disabled").await;
        assert_history_absent(&monitor, "missing-key").await;
        assert_history_absent(&monitor, "removed").await;
        assert!(monitor.statuses.lock().await.contains_key("active"));
        assert!(monitor.last_checked.lock().await.contains_key("active"));
        assert!(
            monitor
                .proxy_failed_since
                .lock()
                .await
                .contains_key("active")
        );
        assert!(monitor.last_error.lock().await.contains_key("active"));
        assert!(monitor.last_restart.lock().await.contains_key("active"));
    }

    #[tokio::test]
    async fn reenabled_server_is_due_without_inheriting_old_failure_state() {
        let monitor = ConnectivityMonitor::new();
        seed_history(&monitor, "reenabled").await;

        monitor.mark_disabled("reenabled").await;

        assert!(is_check_due(None, 2_000, 60));
        assert_history_absent(&monitor, "reenabled").await;
    }
}
