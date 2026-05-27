use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sysinfo::{Disks, System};

use crate::{
    AppState, PROJECT_NAME,
    activity_log::{ActivityKind, ActivityLevel, ActivityLogEntry, ActivityLogFilter},
    app_version, auth,
    config::Config,
    emby::{MediaOverview, PlaybackSession},
    error::AppResult,
    management_listen_addr, tz_offset_seconds,
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
    level: Option<String>,
    keyword: Option<String>,
    since_ms: Option<u128>,
    until_ms: Option<u128>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct AuditLogQuery {
    action: Option<String>,
    keyword: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct RequestDetailQuery {
    server_id: Option<String>,
    path_type: Option<String>,
    keyword: Option<String>,
    since_ms: Option<u128>,
    until_ms: Option<u128>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct BlockLogQuery {
    server_id: Option<String>,
    path_type: Option<String>,
    keyword: Option<String>,
    since_ms: Option<u128>,
    until_ms: Option<u128>,
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
            Ok(mut server_sessions) => {
                for session in &mut server_sessions {
                    if let Some(ip) = session.playback_ip.as_deref() {
                        session.ip_location = state.ip_location.lookup(ip).await;
                    }
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
    let mut entries = state.activity_log.list_filtered(ActivityLogFilter {
        server_id,
        kind,
        level: query.level.as_deref(),
        keyword: query.keyword.as_deref(),
        since_ms: query.since_ms,
        until_ms: query.until_ms,
        limit: query.limit.unwrap_or(120),
    });
    for entry in &mut entries {
        if let Some(ip) = entry.playback_ip.as_deref() {
            entry.ip_location = state.ip_location.lookup(ip).await;
        }
    }
    Ok(Json(entries))
}

pub async fn export_activity_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ActivityLogQuery>,
) -> AppResult<Response> {
    auth::require_auth(&state, &headers).await?;
    let kind = match query.kind.as_deref() {
        Some("playback") => Some(ActivityKind::Playback),
        Some("general") => Some(ActivityKind::General),
        _ => None,
    };
    let server_id = query.server_id.as_deref().filter(|value| *value != "all");
    let logs = state.activity_log.list_filtered(ActivityLogFilter {
        server_id,
        kind,
        level: query.level.as_deref(),
        keyword: query.keyword.as_deref(),
        since_ms: query.since_ms,
        until_ms: query.until_ms,
        limit: query.limit.unwrap_or(500),
    });
    let mut csv = String::from("id,time,kind,level,server,user,ip,message,detail\n");
    for entry in logs {
        csv.push_str(&format!(
            "{},{},{},{},{},{},{},{},{}\n",
            entry.id,
            entry.timestamp_ms,
            csv_escape(&entry.kind),
            csv_escape(&entry.level),
            csv_escape(&entry.server_name),
            csv_escape(entry.playback_user.as_deref().unwrap_or("")),
            csv_escape(entry.playback_ip.as_deref().unwrap_or("")),
            csv_escape(&entry.message),
            csv_escape(&entry.detail),
        ));
    }
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"embypanel-logs.csv\"",
            ),
        ],
        csv,
    )
        .into_response())
}

pub async fn request_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<crate::db::RequestStatsDaily>>> {
    auth::require_auth(&state, &headers).await?;
    let mut rows = state.settings_store.today_request_stats()?;
    let config = state.config.read().await.clone();
    let existing = rows
        .iter()
        .map(|row| row.server_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let today = local_date();
    for server in &config.servers {
        if existing.contains(&server.id) {
            continue;
        }
        rows.push(crate::db::RequestStatsDaily {
            date: today.clone(),
            server_id: server.id.clone(),
            server_name: server.name.clone(),
            port: server.port,
            requests: 0,
            redirects: 0,
            cache_hits: 0,
            blocks: 0,
            errors: 0,
            updated_at_ms: 0,
        });
    }
    rows.sort_by(|left, right| left.server_name.cmp(&right.server_name));
    Ok(Json(rows))
}

pub async fn connectivity_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<crate::connectivity::ConnectivityCheckStatus>>> {
    auth::require_auth(&state, &headers).await?;
    let config = state.config.read().await.clone();
    Ok(Json(state.connectivity.statuses(&config).await))
}

pub async fn request_details(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RequestDetailQuery>,
) -> AppResult<Json<Vec<crate::db::ProxyRequestDetail>>> {
    auth::require_auth(&state, &headers).await?;
    let mut rows =
        state
            .settings_store
            .list_proxy_request_details(crate::db::ProxyRequestDetailFilter {
                server_id: query.server_id.as_deref().filter(|value| *value != "all"),
                path_type: query.path_type.as_deref().filter(|value| *value != "all"),
                keyword: query.keyword.as_deref(),
                since_ms: query.since_ms,
                until_ms: query.until_ms,
                limit: query.limit.unwrap_or(200),
            })?;
    for row in &mut rows {
        row.ip_location = state.ip_location.lookup(&row.playback_ip).await;
    }
    Ok(Json(rows))
}

pub async fn block_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<BlockLogQuery>,
) -> AppResult<Json<Vec<crate::block_log::BlockLogEntry>>> {
    auth::require_auth(&state, &headers).await?;
    let mut rows = state.block_log.list(crate::block_log::BlockLogFilter {
        server_id: query.server_id.as_deref().filter(|value| *value != "all"),
        path_type: query.path_type.as_deref().filter(|value| *value != "all"),
        keyword: query.keyword.as_deref(),
        since_ms: query.since_ms,
        until_ms: query.until_ms,
        limit: query.limit.unwrap_or(200),
    })?;
    for row in &mut rows {
        row.ip_location = state.ip_location.lookup(&row.playback_ip).await;
    }
    Ok(Json(rows))
}

pub async fn proxy_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<crate::ProxyStatusEntry>>> {
    auth::require_auth(&state, &headers).await?;
    let config = state.config.read().await.clone();
    let statuses = if let Some(manager) = state.proxy_manager.as_ref() {
        manager.statuses(&config).await
    } else {
        Vec::new()
    };
    Ok(Json(statuses))
}

pub async fn detailed_healthz(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<serde_json::Value>> {
    auth::require_auth(&state, &headers).await?;
    let database_ok = state.settings_store.has_admin().is_ok();
    Ok(Json(serde_json::json!({
        "status": if database_ok { "ok" } else { "degraded" },
        "name": PROJECT_NAME,
        "version": app_version(),
        "database": if database_ok { "ok" } else { "error" },
        "proxy_count": state.config.read().await.proxy_configs().len(),
    })))
}

pub async fn audit_logs(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuditLogQuery>,
) -> AppResult<Json<Vec<crate::db::AuditLogEntry>>> {
    auth::require_auth(&state, &headers).await?;
    Ok(Json(state.settings_store.list_audit_logs(
        query.action.as_deref(),
        query.keyword.as_deref(),
        query.limit.unwrap_or(120),
    )?))
}

async fn record_runtime_info(state: &AppState) {
    let management = management_listen_addr()
        .map(|addr| format!("管理 API 监听 {addr}"))
        .unwrap_or_else(|_| "管理 API 监听地址读取失败".to_string());
    state.activity_log.record(
        ActivityKind::General,
        ActivityLevel::Info,
        None,
        "EmbyPanel",
        "服务正常运行",
        management,
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

fn csv_escape(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn local_date() -> String {
    let offset = std::env::var("TZ")
        .map(|tz| tz_offset_seconds(&tz))
        .unwrap_or(8 * 3600);
    let now = chrono::Utc::now() + chrono::Duration::seconds(offset.into());
    now.format("%Y-%m-%d").to_string()
}
