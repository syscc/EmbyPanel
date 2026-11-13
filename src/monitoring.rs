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
    db::PlaybackAnalyticsSampleWrite,
    diagnostics::decode_log_detail_url,
    emby::{MediaOverview, PlaybackSession},
    error::{AppError, AppResult, safe_error_message},
    management_listen_addr, tz_offset_seconds,
};

const SERVER_QUERY_CONCURRENCY_LIMIT: usize = 4;
const MEDIA_OVERVIEW_QUERY_BUDGET: Duration = Duration::from_secs(10);
const SERVER_HEALTH_CACHE_TTL: Duration = Duration::from_secs(5);
const MONITORING_READ_PERMISSION: &str = "monitoring.read";

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

#[derive(Debug, Default, Deserialize)]
pub struct ServerFilterQuery {
    #[serde(alias = "serverId")]
    server_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PlaybackImageQuery {
    server_id: String,
    item_id: String,
}

#[derive(Debug, Serialize)]
pub struct ServerOption {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct ServerOptionsResponse {
    pub servers: Vec<ServerOption>,
    pub can_control_sessions: bool,
}

pub async fn server_options(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<ServerOptionsResponse>> {
    let context = auth::require_auth_context(&state, &headers).await?;
    let config = state.config.read().await;
    Ok(Json(server_options_for_context(&config, &context)?))
}

fn server_options_for_context(
    config: &Config,
    context: &auth::AuthContext,
) -> AppResult<ServerOptionsResponse> {
    if ![
        "settings.read",
        "monitoring.read",
        "admin.security.read",
        "library.tasks.read",
        "cache.write",
        "users.read",
    ]
    .iter()
    .any(|permission| auth::has_permission(context, permission))
    {
        return Err(AppError::Forbidden(
            "missing server options read permission".to_string(),
        ));
    }
    // Read configured servers directly so disabled servers remain editable.
    let servers = config
        .servers
        .iter()
        .filter(|server| auth::has_server_access(context, &server.id))
        .map(|server| ServerOption {
            id: server.id.clone(),
            name: server.name.clone(),
        })
        .collect();
    Ok(ServerOptionsResponse {
        servers,
        can_control_sessions: auth::has_permission(context, "sessions.control"),
    })
}

pub async fn media_overview(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ServerFilterQuery>,
) -> AppResult<Json<Vec<MediaOverview>>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;
    let requested_server_id =
        auth::require_server_filter(&auth_context, query.server_id.as_deref())?;
    let config = state.config.read().await.clone();
    let configs = authorized_playback_configs(&config, &auth_context, requested_server_id);
    let mut overviews = Vec::new();
    let client = state.client.clone();
    let queries = configs.into_iter().map(move |config| {
        let client = client.clone();
        async move { query_media_overview(client, config).await }
    });
    for (config, result) in collect_server_queries(queries).await {
        match result {
            Ok(mut overview) => {
                let (server_id, server_name) = config.server_label();
                overview.server_id = server_id;
                overview.server_name = server_name;
                overviews.push(overview);
            }
            Err(err) => {
                let (server_id, server_name) = config.server_label();
                state.activity_log.record(
                    ActivityKind::General,
                    ActivityLevel::Error,
                    Some(&server_id),
                    &server_name,
                    "读取媒体库总览失败",
                    safe_error_message(&err),
                );
                tracing::warn!(
                    server = %server_name,
                    error = %safe_error_message(&err),
                    "failed to fetch media overview"
                );
            }
        }
    }
    Ok(Json(overviews))
}

async fn query_media_overview(
    client: reqwest::Client,
    config: Config,
) -> (Config, AppResult<MediaOverview>) {
    let result = tokio::time::timeout(
        MEDIA_OVERVIEW_QUERY_BUDGET,
        crate::emby::get_media_overview(&client, &config),
    )
    .await
    .unwrap_or_else(|_| {
        Err(AppError::BadGateway(
            "media overview query timed out".to_string(),
        ))
    });
    (config, result)
}

pub async fn list_playback_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ServerFilterQuery>,
) -> AppResult<Response> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, MONITORING_READ_PERMISSION)?;
    let requested_server_id =
        auth::require_server_filter(&auth_context, query.server_id.as_deref())?;
    let config = state.config.read().await.clone();
    let configs = authorized_playback_configs(&config, &auth_context, requested_server_id);
    let expected_servers = configs.len();
    let query_state = state.clone();
    let queries = configs.into_iter().map(move |config| {
        let state = query_state.clone();
        async move {
            // The budget includes all supplementary media-path lookups for this server.
            let result = tokio::time::timeout(Duration::from_secs(10), async {
                let sessions =
                    crate::emby::get_active_playback_sessions(&state.client, &config).await?;
                let queries = sessions
                    .into_iter()
                    .map(|session| enrich_playback_session(state.clone(), config.clone(), session));
                Ok::<_, AppError>(collect_server_queries(queries).await)
            })
            .await
            .unwrap_or_else(|_| {
                Err(AppError::BadGateway(
                    "playback session query timed out".to_string(),
                ))
            });
            (config, result)
        }
    });
    let mut sessions = Vec::new();
    let mut failed_server_ids = Vec::new();
    let mut successful_servers = 0;
    let results = collect_server_queries(queries).await;
    if results.len() != expected_servers {
        return Err(AppError::BadGateway(
            "playback session query task failed".to_string(),
        ));
    }
    for (config, result) in results {
        match result {
            Ok(server_sessions) => {
                successful_servers += 1;
                // Keep the legacy client-config read/modify/write path sequential.
                for session in &server_sessions {
                    if let Err(err) = crate::client_control::record_client_event(
                        &state,
                        session.client.clone(),
                        session.device_name.clone(),
                        session.user_name.clone(),
                        session.user_agent.clone(),
                    ) {
                        tracing::warn!(error = %safe_error_message(&err), "failed to record playback session client");
                    }
                }
                let analytics_samples = server_sessions
                    .iter()
                    .filter_map(|session| {
                        let session_key = sanitize_analytics_value(&session.id);
                        (!session_key.is_empty()).then(|| PlaybackAnalyticsSampleWrite {
                            timestamp_ms: now_ms(),
                            server_id: sanitize_analytics_value(&session.server_id),
                            server_name: sanitize_analytics_value(&session.server_name),
                            session_key,
                            media_id: sanitize_analytics_value(&session.item_id),
                            playback_user: sanitize_analytics_value(&session.user_name),
                            client: sanitize_analytics_value(&session.client),
                            transcoding: session.transcoding,
                            bandwidth: session.bandwidth,
                        })
                    })
                    .collect();
                state
                    .telemetry_writes
                    .playback_analytics_samples(analytics_samples);
                sessions.extend(server_sessions);
            }
            Err(err) => {
                let (server_id, server_name) = config.server_label();
                failed_server_ids.push(server_id.clone());
                state.activity_log.record(
                    ActivityKind::General,
                    ActivityLevel::Error,
                    Some(&server_id),
                    &server_name,
                    "读取播放会话失败",
                    safe_error_message(&err),
                );
                tracing::warn!(error = %safe_error_message(&err), "failed to fetch playback sessions");
            }
        }
    }
    playback_sessions_response(sessions, successful_servers, failed_server_ids)
}

fn playback_sessions_response(
    sessions: Vec<PlaybackSession>,
    successful_servers: usize,
    failed_server_ids: Vec<String>,
) -> AppResult<Response> {
    if successful_servers == 0 && !failed_server_ids.is_empty() {
        return Err(AppError::BadGateway(
            "all playback session queries failed".to_string(),
        ));
    }
    let mut response = Json(sessions).into_response();
    if !failed_server_ids.is_empty() {
        // Never put URLs, arbitrary configuration strings or control characters in headers.
        let safe_ids: Vec<_> = failed_server_ids
            .iter()
            .map(|id| {
                if !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|ch| ch.is_ascii_alphanumeric() || b"-_.".contains(&ch))
                {
                    id.as_str()
                } else {
                    "unknown"
                }
            })
            .collect();
        let value = serde_json::json!({
            "failed_count": failed_server_ids.len(),
            "server_ids": safe_ids,
        })
        .to_string();
        response.headers_mut().insert(
            "x-embypanel-server-errors",
            value
                .parse()
                .map_err(|_| AppError::Internal("invalid playback warning header".to_string()))?,
        );
    }
    Ok(response)
}

async fn enrich_playback_session(
    state: AppState,
    config: Config,
    mut session: PlaybackSession,
) -> PlaybackSession {
    if !session.transcoding {
        let actual_mode = state.playback_paths.lock().await.lookup(
            &session.server_id,
            &session.item_id,
            session.device_id.as_deref(),
            session.media_source_id.as_deref(),
        );
        if let Some(mode) = actual_mode {
            session.playback_mode = mode.to_string();
        } else if matches!(
            session.playback_mode.as_str(),
            "emby_direct_play" | "emby_direct_stream"
        ) && let Some(mode) = tokio::time::timeout(
            Duration::from_secs(2),
            infer_untracked_playback_mode(&state.client, &config, &session),
        )
        .await
        .ok()
        .flatten()
        {
            state.playback_paths.lock().await.record(
                &session.server_id,
                &session.item_id,
                session.device_id.as_deref(),
                session.media_source_id.as_deref(),
                mode,
            );
            session.playback_mode = mode.to_string();
        }
    }
    if let Some(ip) = session.playback_ip.as_deref() {
        session.ip_location = state.ip_location.lookup(ip).await;
    }
    session
}

fn sanitize_analytics_value(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .take(128)
        .collect()
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
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
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;
    auth::require_server_access(&auth_context, &query.server_id)?;
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
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;
    let allowed_servers = auth::allowed_server_ids(&auth_context);
    let requested_server_id =
        auth::require_server_filter(&auth_context, query.server_id.as_deref())?;
    if matches!(query.kind.as_deref(), None | Some("general")) {
        record_runtime_info(&state).await;
    }
    let kind = match query.kind.as_deref() {
        Some("playback") => Some(ActivityKind::Playback),
        Some("general") => Some(ActivityKind::General),
        _ => None,
    };
    let mut entries = state.activity_log.list_filtered_scoped(
        ActivityLogFilter {
            server_id: requested_server_id,
            kind,
            level: query.level.as_deref(),
            keyword: query.keyword.as_deref(),
            since_ms: query.since_ms,
            until_ms: query.until_ms,
            limit: query.limit.unwrap_or(120),
        },
        allowed_servers.as_ref(),
    );
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
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;
    let allowed_servers = auth::allowed_server_ids(&auth_context);
    let requested_server_id =
        auth::require_server_filter(&auth_context, query.server_id.as_deref())?;
    let kind = match query.kind.as_deref() {
        Some("playback") => Some(ActivityKind::Playback),
        Some("general") => Some(ActivityKind::General),
        _ => None,
    };
    let logs = state.activity_log.list_filtered_scoped(
        ActivityLogFilter {
            server_id: requested_server_id,
            kind,
            level: query.level.as_deref(),
            keyword: query.keyword.as_deref(),
            since_ms: query.since_ms,
            until_ms: query.until_ms,
            limit: query.limit.unwrap_or(500),
        },
        allowed_servers.as_ref(),
    );
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
            csv_escape(&decode_log_detail_url(&entry.detail)),
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
    Query(query): Query<ServerFilterQuery>,
) -> AppResult<Json<Vec<crate::db::RequestStatsDaily>>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;
    let requested_server_id =
        auth::require_server_filter(&auth_context, query.server_id.as_deref())?;
    let mut rows = state.settings_store.today_request_stats()?;
    let config = state.config.read().await.clone();
    rows.retain(|row| {
        requested_server_id.is_none_or(|requested| requested == row.server_id)
            && auth::has_server_access(&auth_context, &row.server_id)
    });
    let existing = rows
        .iter()
        .map(|row| row.server_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let today = local_date();
    for server in &config.servers {
        if !requested_server_id.is_none_or(|requested| requested == server.id)
            || !auth::has_server_access(&auth_context, &server.id)
        {
            continue;
        }
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
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;
    let config = state.config.read().await.clone();
    let mut statuses = state.connectivity.statuses(&config).await;
    statuses.retain(|status| auth::has_server_access(&auth_context, &status.server_id));
    Ok(Json(statuses))
}

pub async fn request_details(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RequestDetailQuery>,
) -> AppResult<Json<Vec<crate::db::ProxyRequestDetail>>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;
    let allowed_servers = auth::allowed_server_ids(&auth_context);
    let requested_server_id =
        auth::require_server_filter(&auth_context, query.server_id.as_deref())?;
    let mut rows = state.settings_store.list_proxy_request_details_scoped(
        crate::db::ProxyRequestDetailFilter {
            server_id: requested_server_id,
            path_type: query.path_type.as_deref().filter(|value| *value != "all"),
            keyword: query.keyword.as_deref(),
            since_ms: query.since_ms,
            until_ms: query.until_ms,
            limit: query.limit.unwrap_or(200),
        },
        allowed_servers.as_ref(),
    )?;
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
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;
    let allowed_servers = auth::allowed_server_ids(&auth_context);
    let requested_server_id =
        auth::require_server_filter(&auth_context, query.server_id.as_deref())?;
    let mut rows = state.block_log.list_scoped(
        crate::block_log::BlockLogFilter {
            server_id: requested_server_id,
            path_type: query.path_type.as_deref().filter(|value| *value != "all"),
            level: query.level.as_deref().filter(|value| *value != "all"),
            keyword: query.keyword.as_deref(),
            since_ms: query.since_ms,
            until_ms: query.until_ms,
            limit: query.limit.unwrap_or(200),
        },
        allowed_servers.as_ref(),
    )?;
    for row in &mut rows {
        row.ip_location = state.ip_location.lookup(&row.playback_ip).await;
    }
    Ok(Json(rows))
}

pub async fn proxy_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<crate::ProxyStatusEntry>>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;
    let config = state.config.read().await.clone();
    let mut statuses = if let Some(manager) = state.proxy_manager.as_ref() {
        manager.statuses(&config).await
    } else {
        Vec::new()
    };
    statuses.retain(|status| auth::has_server_access(&auth_context, &status.server_id));
    Ok(Json(statuses))
}

pub async fn detailed_healthz(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<serde_json::Value>> {
    let context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&context, MONITORING_READ_PERMISSION)?;
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
    let context = auth::require_auth_context(&state, &headers).await?;
    auth::require_super_admin_session(&context)?;
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
        let (server_id, server_name) = config.server_label();
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
    Query(query): Query<ServerFilterQuery>,
) -> AppResult<Json<ServerHealth>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;
    let _ = auth::require_server_filter(&auth_context, query.server_id.as_deref())?;
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

fn authorized_playback_configs(
    config: &Config,
    auth_context: &auth::AuthContext,
    requested_server_id: Option<&str>,
) -> Vec<Config> {
    playback_configs(config)
        .into_iter()
        .filter(|config| {
            let (server_id, _) = config.server_label();
            requested_server_id.is_none_or(|requested| requested == server_id)
                && auth::has_server_access(auth_context, &server_id)
        })
        .collect()
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
    use super::{
        MONITORING_READ_PERMISSION, authorized_playback_configs, csv_escape, decode_log_detail_url,
        is_valid_item_id, server_options_for_context,
    };
    use crate::{
        auth::{self, AuthContext, AuthKind},
        config::Config,
        db::AdminServerGrant,
        error::AppError,
    };

    #[tokio::test]
    async fn overview_budget_isolates_slow_server_and_preserves_success_order() {
        use axum::{Json, Router, routing::get};
        use std::time::Duration;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let slow_seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mock_slow_seen = slow_seen.clone();
        let app = Router::new().fallback(get(move |uri: axum::http::Uri| {
            let slow_seen = mock_slow_seen.clone();
            async move {
                // Emby endpoints replace the base path; identify synthetic servers by API key.
                if url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
                    .any(|(key, value)| key == "api_key" && value == "slow")
                {
                    slow_seen.store(true, std::sync::atomic::Ordering::SeqCst);
                    std::future::pending::<()>().await;
                }
                Json(serde_json::json!({}))
            }
        }));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let queries = ["first", "slow", "last"].into_iter().map(|id| {
            let config: Config = serde_json::from_value(serde_json::json!({
                "emby_host": format!("http://{addr}"),
                "emby_api_key": id,
            }))
            .unwrap();
            let client = client.clone();
            async move {
                let (_, result) = super::query_media_overview(client, config).await;
                (id, result)
            }
        });
        let results = tokio::time::timeout(
            super::MEDIA_OVERVIEW_QUERY_BUDGET + Duration::from_secs(3),
            super::collect_server_queries(queries),
        )
        .await
        .expect("server budget must finish before the 30 second client timeout");
        assert_eq!(
            results.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            ["first", "slow", "last"]
        );
        assert!(slow_seen.load(std::sync::atomic::Ordering::SeqCst));
        assert!(results[0].1.is_ok());
        assert!(
            matches!(&results[1].1, Err(crate::error::AppError::BadGateway(message))
            if message == "media overview query timed out")
        );
        assert!(results[2].1.is_ok());
        assert_eq!(
            results
                .into_iter()
                .filter_map(|(id, result)| result.ok().map(|_| id))
                .collect::<Vec<_>>(),
            ["first", "last"]
        );
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn playback_response_preserves_array_and_distinguishes_partial_failure() {
        use axum::{body::to_bytes, http::StatusCode};
        let response = super::playback_sessions_response(vec![], 1, vec!["srv-a".into()]).unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let warning: serde_json::Value = serde_json::from_str(
            response.headers()["x-embypanel-server-errors"]
                .to_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(warning["failed_count"], 1);
        assert_eq!(warning["server_ids"][0], "srv-a");
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
            b"[]"
        );
        assert!(matches!(
            super::playback_sessions_response(vec![], 0, vec!["srv-a".into()]),
            Err(crate::error::AppError::BadGateway(_))
        ));
        let empty = super::playback_sessions_response(vec![], 0, vec![]).unwrap();
        assert!(!empty.headers().contains_key("x-embypanel-server-errors"));
        let unsafe_id = super::playback_sessions_response(
            vec![],
            1,
            vec!["https://host/?api_key=canary".into()],
        )
        .unwrap();
        assert!(
            !unsafe_id.headers()["x-embypanel-server-errors"]
                .to_str()
                .unwrap()
                .contains("canary")
        );
    }

    #[tokio::test]
    async fn playback_queries_are_bounded_and_preserve_input_order() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let active = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let queries = (0..12).map(|index| {
            let active = active.clone();
            let peak = peak.clone();
            async move {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(count, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis((12 - index) * 2)).await;
                active.fetch_sub(1, Ordering::SeqCst);
                index
            }
        });
        let result = super::collect_server_queries(queries).await;
        assert_eq!(result, (0..12).collect::<Vec<_>>());
        assert_eq!(
            peak.load(Ordering::SeqCst),
            super::SERVER_QUERY_CONCURRENCY_LIMIT
        );
    }

    #[tokio::test]
    async fn timed_out_query_batch_cancels_supplementary_work() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct Active(Arc<AtomicUsize>);
        impl Drop for Active {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        let active = Arc::new(AtomicUsize::new(0));
        let queries = (0..20).map(|_| {
            let active = active.clone();
            async move {
                active.fetch_add(1, Ordering::SeqCst);
                let _active = Active(active);
                std::future::pending::<()>().await;
            }
        });
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                super::collect_server_queries(queries)
            )
            .await
            .is_err()
        );
        // Dropping the collector aborts all its bounded child tasks.
        for _ in 0..20 {
            if active.load(Ordering::SeqCst) == 0 {
                return;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(active.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn overview_config_selection_filters_restricted_servers_and_preserves_order() {
        let config: Config = serde_json::from_value(serde_json::json!({
            "emby_host": "http://localhost:8096",
            "emby_api_key": "root-key",
            "servers": [
                {"id": "srv-a", "name": "A", "emby_host": "http://a:8096", "emby_api_key": "a-key", "port": 8096},
                {"id": "srv-b", "name": "B", "emby_host": "http://b:8096", "emby_api_key": "b-key", "port": 8097}
            ]
        }))
        .unwrap();
        let restricted = AuthContext {
            user_id: 1,
            kind: AuthKind::Session,
            scopes: None,
            server_ids: None,
            role_keys: vec!["operator".to_string()],
            permission_keys: vec![],
            server_grants: vec![AdminServerGrant {
                server_id: "srv-a".to_string(),
                scope: "full".to_string(),
                granted_at: 0,
            }],
        };
        let ids: Vec<_> = authorized_playback_configs(&config, &restricted, None)
            .into_iter()
            .map(|item| item.server_label().0)
            .collect();
        assert_eq!(ids, vec!["srv-a"]);
        assert!(authorized_playback_configs(&config, &restricted, Some("srv-b")).is_empty());

        let super_admin = AuthContext {
            role_keys: vec!["super_admin".to_string()],
            ..restricted.clone()
        };
        let ids: Vec<_> = authorized_playback_configs(&config, &super_admin, None)
            .into_iter()
            .map(|item| item.server_label().0)
            .collect();
        assert_eq!(ids, vec!["srv-a", "srv-b"]);
    }

    #[test]
    fn server_options_enforce_permissions_scopes_and_include_disabled_grants() {
        let config: Config = serde_json::from_value(serde_json::json!({
            "emby_host": "http://127.0.0.1:1", "emby_api_key": "synthetic",
            "servers": [
                {"id": "a", "name": "A", "enabled": false, "emby_host": "http://127.0.0.1:1", "emby_api_key": "synthetic", "port": 8096},
                {"id": "b", "name": "B", "emby_host": "http://127.0.0.1:1", "emby_api_key": "synthetic", "port": 8097}
            ]
        })).unwrap();
        let mut ctx = AuthContext {
            user_id: 1,
            kind: AuthKind::Session,
            scopes: None,
            server_ids: None,
            role_keys: vec![],
            permission_keys: vec![],
            server_grants: vec![AdminServerGrant {
                server_id: "a".into(),
                scope: "full".into(),
                granted_at: 0,
            }],
        };
        assert!(matches!(
            server_options_for_context(&config, &ctx),
            Err(AppError::Forbidden(_))
        ));
        for permission in [
            "settings.read",
            "monitoring.read",
            "admin.security.read",
            "library.tasks.read",
            "cache.write",
            "users.read",
        ] {
            ctx.permission_keys = vec![permission.into()];
            let response = server_options_for_context(&config, &ctx).unwrap();
            assert_eq!(
                serde_json::to_value(response).unwrap(),
                serde_json::json!({
                    "servers": [{"id": "a", "name": "A"}], "can_control_sessions": false
                })
            );
        }
        ctx.permission_keys.push("sessions.control".into());
        assert!(
            server_options_for_context(&config, &ctx)
                .unwrap()
                .can_control_sessions
        );
        ctx.kind = AuthKind::ApiToken;
        ctx.scopes = Some(vec!["users.read".into()]);
        assert!(
            !server_options_for_context(&config, &ctx)
                .unwrap()
                .can_control_sessions
        );
        ctx.scopes.as_mut().unwrap().push("sessions.control".into());
        assert!(
            server_options_for_context(&config, &ctx)
                .unwrap()
                .can_control_sessions
        );
        ctx.server_ids = Some(vec!["b".into()]);
        assert!(
            server_options_for_context(&config, &ctx)
                .unwrap()
                .servers
                .is_empty()
        );
        ctx.scopes = Some(vec!["sessions.control".into()]);
        assert!(matches!(
            server_options_for_context(&config, &ctx),
            Err(AppError::Forbidden(_))
        ));

        ctx.permission_keys = vec!["cache.write".into()];
        ctx.scopes = Some(vec!["cache.write".into()]);
        ctx.server_ids = Some(vec!["a".into()]);
        assert_eq!(
            serde_json::to_value(server_options_for_context(&config, &ctx).unwrap()).unwrap(),
            serde_json::json!({
                "servers": [{"id": "a", "name": "A"}], "can_control_sessions": false
            })
        );
        ctx.server_ids = Some(vec!["b".into()]);
        assert!(
            server_options_for_context(&config, &ctx)
                .unwrap()
                .servers
                .is_empty()
        );
        ctx.role_keys = vec!["super_admin".into()];
        assert_eq!(
            serde_json::to_value(server_options_for_context(&config, &ctx).unwrap()).unwrap(),
            serde_json::json!({
                "servers": [{"id": "b", "name": "B"}], "can_control_sessions": false
            })
        );
        ctx.server_ids = None;
        assert_eq!(
            server_options_for_context(&config, &ctx)
                .unwrap()
                .servers
                .len(),
            2
        );
    }

    #[test]
    fn playback_read_rejects_context_without_monitoring_permission() {
        let context = AuthContext {
            user_id: 42,
            kind: AuthKind::Session,
            scopes: None,
            server_ids: None,
            role_keys: vec!["operator".to_string()],
            permission_keys: vec!["users.read".to_string()],
            server_grants: vec![AdminServerGrant {
                server_id: "srv-a".to_string(),
                scope: "full".to_string(),
                granted_at: 0,
            }],
        };
        assert!(auth::require_permission(&context, MONITORING_READ_PERMISSION).is_err());
    }

    #[test]
    fn playback_image_item_id_validation_is_strict() {
        assert!(is_valid_item_id("item-123_ABC"));
        assert!(is_valid_item_id(&"a".repeat(128)));
        assert!(!is_valid_item_id(""));
        assert!(!is_valid_item_id(&"a".repeat(129)));
        assert!(!is_valid_item_id("item/123"));
        assert!(!is_valid_item_id("媒体-123"));
    }

    #[test]
    fn log_detail_csv_row_carries_the_decoded_detail() {
        let detail = "https://cdn.example.com/%E9%87%8D%E5%99%A8.S01E21.mkv";
        assert_eq!(
            csv_escape(&decode_log_detail_url(detail)),
            "\"https://cdn.example.com/重器.S01E21.mkv\""
        );
    }
}
