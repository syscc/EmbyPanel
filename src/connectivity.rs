use std::{collections::HashMap, net::SocketAddr, time::Duration};

use serde::Serialize;
use tokio::sync::Mutex;

use crate::{
    AppState,
    activity_log::{ActivityKind, ActivityLevel},
    config::Config,
    error::AppResult,
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
                statuses
                    .get(&server.id)
                    .cloned()
                    .unwrap_or_else(|| ConnectivityCheckStatus {
                        server_id: server.id.clone(),
                        server_name: server.name.clone(),
                        port: server.port,
                        enabled: server.enabled,
                        ok: false,
                        emby_ok: false,
                        openlist_ok: config.openlist_addr.as_ref().map(|_| false),
                        proxy_ok: false,
                        checked_at_ms: 0,
                        duration_ms: 0,
                        failed_since_ms: None,
                        auto_restarted_at_ms: None,
                        last_error: Some("尚未巡检".to_string()),
                    })
            })
            .collect()
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
    let interval = config.connectivity_check_interval_seconds.max(10);
    if !config.connectivity_check_enabled {
        return interval;
    }

    let timeout = Duration::from_secs(config.connectivity_check_timeout_seconds.max(1));
    let auto_restart_seconds = config.connectivity_auto_restart_seconds;
    let configs = config.proxy_configs();
    let mut statuses = Vec::new();
    for server_config in configs {
        let mut status = check_server(state, &server_config, timeout).await;
        handle_status_change(state, &status).await;
        if let Some(restarted_at_ms) =
            handle_auto_restart(state, &status, auto_restart_seconds).await
        {
            status.auto_restarted_at_ms = Some(restarted_at_ms);
        }
        statuses.push(status);
    }
    state.connectivity.replace_statuses(statuses).await;
    interval
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
    .and_then(|result| result.map_err(|err| err.to_string()));
    let proxy_result = check_proxy_port(config.port, timeout).await;
    let openlist_result = if config.openlist_addr.is_some() {
        Some(
            tokio::time::timeout(
                timeout,
                crate::openlist::validate_connection(&state.client, config),
            )
            .await
            .map_err(|_| "OpenList 连接超时".to_string())
            .and_then(|result| result.map_err(|err| err.to_string())),
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
                err.to_string(),
            );
            None
        }
    }
}
