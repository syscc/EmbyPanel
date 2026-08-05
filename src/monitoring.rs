use axum::{
    Json,
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::{
    future::Future,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use sysinfo::{Disks, System};
use tokio::task::JoinSet;

use crate::{
    AppState, PROJECT_NAME,
    activity_log::{ActivityKind, ActivityLevel, ActivityLogEntry, ActivityLogFilter},
    app_version, auth,
    config::Config,
    emby::{MediaOverview, PlaybackSession},
    error::{AppError, AppResult, safe_error_message},
    management_listen_addr, tz_offset_seconds,
};

const SERVER_QUERY_CONCURRENCY_LIMIT: usize = 4;
const SERVER_HEALTH_CACHE_TTL: Duration = Duration::from_secs(5);

static SERVER_HEALTH_CACHE: OnceLock<Mutex<Option<(Instant, ServerHealth)>>> = OnceLock::new();

#[derive(Debug, Clone, Serialize)]
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
    level: Option<String>,
    keyword: Option<String>,
    since_ms: Option<u128>,
    until_ms: Option<u128>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct PlaybackImageQuery {
    server_id: String,
    item_id: String,
}

pub async fn media_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<MediaOverview>>> {
    auth::require_auth(&state, &headers).await?;
    let config = state.config.read().await.clone();
    let configs = playback_configs(&config);
    let mut overviews = Vec::new();
    let client = state.client.clone();
    let queries = configs.into_iter().map(move |config| {
        let client = client.clone();
        async move {
            let result = crate::emby::get_media_overview(&client, &config).await;
            (config, result)
        }
    });
    for (config, result) in collect_server_queries(queries).await {
        match result {
            Ok(mut overview) => {
                let (server_id, server_name) = server_label(&config);
                overview.server_id = server_id;
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
                    safe_error_message(&err),
                );
                tracing::warn!(
                    server = server_name,
                    error = %safe_error_message(&err),
                    "failed to fetch media overview"
                );
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
    let client = state.client.clone();
    let query_client = client.clone();
    let queries = configs.into_iter().map(move |config| {
        let client = query_client.clone();
        async move {
            let result = crate::emby::get_active_playback_sessions(&client, &config).await;
            (config, result)
        }
    });
    for (config, result) in collect_server_queries(queries).await {
        match result {
            Ok(mut server_sessions) => {
                for session in &mut server_sessions {
                    if !session.transcoding {
                        let actual_mode = {
                            let mut playback_paths = state.playback_paths.lock().await;
                            playback_paths.lookup(
                                &session.server_id,
                                &session.item_id,
                                session.device_id.as_deref(),
                                session.media_source_id.as_deref(),
                            )
                        };
                        if let Some(actual_mode) = actual_mode {
                            session.playback_mode = actual_mode.to_string();
                        } else if matches!(
                            session.playback_mode.as_str(),
                            "emby_direct_play" | "emby_direct_stream"
                        ) && let Some(inferred_mode) =
                            infer_untracked_playback_mode(&client, &config, session).await
                        {
                            {
                                let mut playback_paths = state.playback_paths.lock().await;
                                playback_paths.record(
                                    &session.server_id,
                                    &session.item_id,
                                    session.device_id.as_deref(),
                                    session.media_source_id.as_deref(),
                                    inferred_mode,
                                );
                            }
                            session.playback_mode = inferred_mode.to_string();
                        }
                    }
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
                    safe_error_message(&err),
                );
                tracing::warn!(
                    server = server_name,
                    error = %safe_error_message(&err),
                    "failed to fetch playback sessions"
                );
            }
        }
    }
    Ok(Json(sessions))
}

async fn infer_untracked_playback_mode(
    client: &reqwest::Client,
    config: &Config,
    session: &PlaybackSession,
) -> Option<&'static str> {
    let media_path = crate::emby::get_media_path(
        client,
        config,
        &session.item_id,
        session.media_source_id.as_deref(),
        &session.user_agent,
    )
    .await
    .ok()??;

    Some(if crate::media_path_uses_direct_link(config, &media_path) {
        crate::PLAYBACK_MODE_DIRECT_LINK
    } else {
        crate::PLAYBACK_MODE_SERVER_PROXY
    })
}

pub async fn playback_image(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PlaybackImageQuery>,
) -> AppResult<Response> {
    auth::require_auth(&state, &headers).await?;
    if !is_valid_item_id(&query.item_id) {
        return Err(AppError::Validation("invalid item_id".to_string()));
    }

    let root_config = state.config.read().await.clone();
    let Some(config) = playback_config_by_server_id(&root_config, &query.server_id) else {
        return Ok(StatusCode::NOT_FOUND.into_response());
    };
    let mut url = config.emby_url(&format!("/Items/{}/Images/Primary", query.item_id))?;
    url.query_pairs_mut()
        .append_pair("maxWidth", "640")
        .append_pair("quality", "90");

    let upstream = state
        .client
        .get(url)
        .header("X-Emby-Token", &config.emby_api_key)
        .send()
        .await?;
    if upstream.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(StatusCode::NOT_FOUND.into_response());
    }
    if !upstream.status().is_success() {
        return Err(AppError::BadGateway(format!(
            "Emby playback image query failed with status {}",
            upstream.status()
        )));
    }

    let content_type = upstream
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .filter(|value| {
            value
                .to_str()
                .is_ok_and(|value| value.trim().to_ascii_lowercase().starts_with("image/"))
        })
        .cloned()
        .ok_or_else(|| {
            AppError::BadGateway(
                "Emby playback image returned a non-image content type".to_string(),
            )
        })?;
    let content_length = upstream
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .cloned();
    let etag = upstream.headers().get(reqwest::header::ETAG).cloned();

    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CACHE_CONTROL, "private, max-age=300")
        .header("x-content-type-options", "nosniff");
    if let Some(content_length) = content_length {
        builder = builder.header(header::CONTENT_LENGTH, content_length);
    }
    if let Some(etag) = etag {
        builder = builder.header(header::ETAG, etag);
    }
    builder
        .body(Body::from_stream(upstream.bytes_stream()))
        .map_err(|err| {
            AppError::Internal(format!("failed to build playback image response: {err}"))
        })
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
        level: query.level.as_deref().filter(|value| *value != "all"),
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

async fn collect_server_queries<I, F, T>(futures: I) -> Vec<T>
where
    I: IntoIterator<Item = F>,
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let mut pending = futures.into_iter().enumerate();
    let mut tasks = JoinSet::new();
    for _ in 0..SERVER_QUERY_CONCURRENCY_LIMIT {
        let Some((index, future)) = pending.next() else {
            break;
        };
        tasks.spawn(async move { (index, future.await) });
    }

    let mut results = Vec::new();
    while let Some(result) = tasks.join_next().await {
        if let Some((index, future)) = pending.next() {
            tasks.spawn(async move { (index, future.await) });
        }
        match result {
            Ok(result) => results.push(result),
            Err(err) => tracing::error!(error = %err, "monitoring server query task failed"),
        }
    }
    results.sort_unstable_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

pub async fn server_health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<ServerHealth>> {
    auth::require_auth(&state, &headers).await?;
    let cache = SERVER_HEALTH_CACHE.get_or_init(|| Mutex::new(None));
    if let Some(health) = cache
        .lock()
        .expect("server health cache mutex poisoned")
        .as_ref()
        .filter(|(updated_at, _)| updated_at.elapsed() < SERVER_HEALTH_CACHE_TTL)
        .map(|(_, health)| health.clone())
    {
        return Ok(Json(health));
    }

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

    let health = ServerHealth {
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
    };
    *cache.lock().expect("server health cache mutex poisoned") =
        Some((Instant::now(), health.clone()));
    Ok(Json(health))
}

fn playback_configs(config: &Config) -> Vec<Config> {
    let configs = config.proxy_configs();
    if configs.is_empty() {
        vec![config.clone()]
    } else {
        configs
    }
}

fn playback_config_by_server_id(config: &Config, server_id: &str) -> Option<Config> {
    if config.servers.is_empty() {
        return (server_id == "default").then(|| config.clone());
    }
    config
        .servers
        .iter()
        .find(|server| server.id == server_id)
        .map(|server| config.proxy_config_for_server(Some(&server.id)))
}

fn is_valid_item_id(item_id: &str) -> bool {
    !item_id.is_empty()
        && item_id.len() <= 128
        && item_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
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

#[cfg(test)]
mod tests {
    use super::is_valid_item_id;

    #[test]
    fn playback_image_item_id_validation_is_strict() {
        assert!(is_valid_item_id("item-123_ABC"));
        assert!(is_valid_item_id(&"a".repeat(128)));
        assert!(!is_valid_item_id(""));
        assert!(!is_valid_item_id(&"a".repeat(129)));
        assert!(!is_valid_item_id("item/123"));
        assert!(!is_valid_item_id("媒体-123"));
    }
}
