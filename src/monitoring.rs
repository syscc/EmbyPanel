use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use sysinfo::{Disks, System};

use crate::{
    AppState,
    activity_log::{ActivityKind, ActivityLevel, ActivityLogEntry},
    auth,
    config::Config,
    emby::{MediaOverview, PlaybackSession},
    error::AppResult,
};

#[derive(Debug, Serialize)]
pub struct ServerHealth {
    pub uptime_seconds: u64,
    pub cpu_percent: u8,
    pub cpu_name: String,
    pub cpu_cores: usize,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
    pub memory_percent: u8,
    pub disk_used_bytes: u64,
    pub disk_total_bytes: u64,
    pub disk_percent: u8,
}

#[derive(Debug, Deserialize)]
pub struct ActivityLogQuery {
    server_id: Option<String>,
    kind: Option<String>,
    limit: Option<usize>,
}

pub async fn media_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<MediaOverview>>> {
    auth::require_auth(&state, &headers).await?;
    let config = state.config.read().await.clone();
    let configs = playback_configs(&config);
    let mut overviews = Vec::new();
    for config in configs {
        match crate::emby::get_media_overview(&state.client, &config).await {
            Ok(mut overview) => {
                let (_, server_name) = server_label(&config);
                overview.server_name = server_name;
                overviews.push(overview);
            }
            Err(err) => {
                let (server_id, server_name) = server_label(&config);
                state.activity_log.record(
                    ActivityKind::General,
                    ActivityLevel::Error,
                    Some(&server_id),
                    &server_name,
                    "读取媒体库总览失败",
                    err.to_string(),
                );
                tracing::warn!(server = server_name, error = %err, "failed to fetch media overview");
            }
        }
    }
    Ok(Json(overviews))
}

pub async fn list_playback_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<PlaybackSession>>> {
    auth::require_auth(&state, &headers).await?;
    let config = state.config.read().await.clone();
    let configs = playback_configs(&config);
    let mut sessions = Vec::new();
    for config in configs {
        match crate::emby::get_active_playback_sessions(&state.client, &config).await {
            Ok(server_sessions) => {
                for session in &server_sessions {
                    if let Err(err) = crate::client_control::record_client_event(
                        &state,
                        session.client.clone(),
                        session.device_name.clone(),
                        session.user_name.clone(),
                        session.user_agent.clone(),
                    ) {
                        tracing::warn!(error = %err, "failed to record playback session client");
                    }
                }
                sessions.extend(server_sessions);
            }
            Err(err) => {
                let (server_id, server_name) = server_label(&config);
                state.activity_log.record(
                    ActivityKind::General,
                    ActivityLevel::Error,
                    Some(&server_id),
                    &server_name,
                    "读取播放会话失败",
                    err.to_string(),
                );
                tracing::warn!(server = server_name, error = %err, "failed to fetch playback sessions");
            }
        }
    }
    Ok(Json(sessions))
}

pub async fn list_activity_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ActivityLogQuery>,
) -> AppResult<Json<Vec<ActivityLogEntry>>> {
    auth::require_auth(&state, &headers).await?;
    if matches!(query.kind.as_deref(), None | Some("general")) {
        record_runtime_info(&state).await;
    }
    let kind = match query.kind.as_deref() {
        Some("playback") => Some(ActivityKind::Playback),
        Some("general") => Some(ActivityKind::General),
        _ => None,
    };
    let server_id = query.server_id.as_deref().filter(|value| *value != "all");
    Ok(Json(state.activity_log.list(
        server_id,
        kind,
        query.limit.unwrap_or(120),
    )))
}

async fn record_runtime_info(state: &AppState) {
    state.activity_log.record(
        ActivityKind::General,
        ActivityLevel::Info,
        None,
        "EmbyPanel",
        "服务正常运行",
        "管理 API 监听 0.0.0.0:8090",
    );

    let config = state.config.read().await.clone();
    for config in playback_configs(&config) {
        let (server_id, server_name) = server_label(&config);
        state.activity_log.record(
            ActivityKind::General,
            ActivityLevel::Info,
            Some(&server_id),
            &server_name,
            "反代端口正常运行",
            format!("监听端口 {}", config.port),
        );
    }
}

pub async fn server_health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<ServerHealth>> {
    auth::require_auth(&state, &headers).await?;
    let mut system = System::new_all();
    system.refresh_all();
    let disks = Disks::new_with_refreshed_list();

    let total_memory = system.total_memory();
    let used_memory = system.used_memory();
    let memory_percent = percent(used_memory, total_memory);
    let total_disk: u64 = disks.iter().map(|disk| disk.total_space()).sum();
    let available_disk: u64 = disks.iter().map(|disk| disk.available_space()).sum();
    let used_disk = total_disk.saturating_sub(available_disk);
    let cpu_percent = system.global_cpu_usage().round().clamp(0.0, 100.0) as u8;
    let cpu_name = system
        .cpus()
        .first()
        .map(|cpu| cpu.brand().to_string())
        .unwrap_or_else(|| "CPU".to_string());

    Ok(Json(ServerHealth {
        uptime_seconds: System::uptime(),
        cpu_percent,
        cpu_name,
        cpu_cores: system.cpus().len(),
        memory_used_bytes: used_memory,
        memory_total_bytes: total_memory,
        memory_percent,
        disk_used_bytes: used_disk,
        disk_total_bytes: total_disk,
        disk_percent: percent(used_disk, total_disk),
    }))
}

fn playback_configs(config: &Config) -> Vec<Config> {
    let configs = config.proxy_configs();
    if configs.is_empty() {
        vec![config.clone()]
    } else {
        configs
    }
}

fn server_label(config: &Config) -> (String, String) {
    config
        .servers
        .first()
        .map(|server| (server.id.clone(), server.name.clone()))
        .unwrap_or_else(|| ("default".to_string(), "默认服务器".to_string()))
}

fn percent(used: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    ((used as f64 / total as f64) * 100.0)
        .round()
        .clamp(0.0, 100.0) as u8
}
