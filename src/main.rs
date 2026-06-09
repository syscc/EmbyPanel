mod activity_log;
mod auth;
mod block_log;
mod cache;
mod client_control;
mod config;
mod connectivity;
mod crypto_api;
mod db;
mod emby;
mod error;
mod file_log;
mod internal_redirect;
mod ip_location;
mod monitoring;
mod openlist;
mod proxy;
mod rewrite;
mod settings_api;
mod url_mapping;

use std::{
    collections::{HashMap, VecDeque},
    env, fmt,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::{ConnectInfo, Query, Request, State},
    http::{Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{any, get},
};
use bytes::Bytes;
use config::Config;
use crypto_api::CryptoKeys;
use db::{ProxyRequestDetailInsert, RequestStatKind, SettingsStore};
use error::{AppError, AppResult, safe_error_message};
use serde::Serialize;
use tokio::{
    sync::{Mutex, RwLock, oneshot},
    task::JoinHandle,
};
use tower_http::services::{ServeDir, ServeFile};
use tracing_subscriber::{
    EnvFilter,
    fmt::{format::Writer, time::FormatTime},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

use crate::cache::DirectLinkCache;
use activity_log::{ActivityKind, ActivityLevel, ActivityLogStore, PlaybackLogRecord};
use block_log::BlockLogStore;
use file_log::FileLogStore;

const MAX_PROXY_BODY_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const PROJECT_NAME: &str = "EmbyPanel";
const PROJECT_URL: &str = "https://github.com/syscc/EmbyPanel";

#[derive(Serialize)]
struct AppInfo {
    name: &'static str,
    version: String,
    project_url: &'static str,
    ui_path: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct UpdateCheckResponse {
    current_version: String,
    latest_version: String,
    release_url: String,
    has_update: bool,
    checked_at_ms: u128,
}

#[derive(Debug, Default, serde::Deserialize)]
struct UpdateCheckQuery {
    force: Option<String>,
}

async fn app_info() -> Json<AppInfo> {
    Json(AppInfo {
        name: PROJECT_NAME,
        version: app_version(),
        project_url: PROJECT_URL,
        ui_path: "/ui/",
    })
}

pub(crate) fn app_version() -> String {
    env::var("EMBYPANEL_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            option_env!("EMBYPANEL_BUILD_VERSION")
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("v{}", env!("CARGO_PKG_VERSION")))
}

#[derive(Clone)]
struct TzTimer {
    offset_seconds: i32,
}

impl TzTimer {
    fn from_env() -> Self {
        Self {
            offset_seconds: tz_offset_seconds(&env::var("TZ").unwrap_or_default()),
        }
    }
}

impl FormatTime for TzTimer {
    fn format_time(&self, writer: &mut Writer<'_>) -> fmt::Result {
        let now = chrono::Utc::now() + chrono::Duration::seconds(self.offset_seconds.into());
        write!(writer, "{}", now.format("%Y-%m-%d %H:%M:%S%.3f"))
    }
}

pub(crate) fn tz_offset_seconds(tz: &str) -> i32 {
    let tz = tz.trim();
    if tz.eq_ignore_ascii_case("Asia/Shanghai")
        || tz.eq_ignore_ascii_case("Asia/Chongqing")
        || tz.eq_ignore_ascii_case("Asia/Harbin")
        || tz.eq_ignore_ascii_case("Asia/Urumqi")
        || tz.eq_ignore_ascii_case("PRC")
    {
        return 8 * 3600;
    }
    if tz.eq_ignore_ascii_case("UTC") || tz.eq_ignore_ascii_case("Etc/UTC") || tz == "Z" {
        return 0;
    }
    parse_utc_offset(tz).unwrap_or(0)
}

fn parse_utc_offset(value: &str) -> Option<i32> {
    let value = value
        .strip_prefix("UTC")
        .or_else(|| value.strip_prefix("GMT"))
        .unwrap_or(value)
        .trim();
    let sign = match value.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let value = &value[1..];
    let (hours, minutes) = if let Some((hours, minutes)) = value.split_once(':') {
        (hours.parse::<i32>().ok()?, minutes.parse::<i32>().ok()?)
    } else {
        (value.parse::<i32>().ok()?, 0)
    };
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

#[derive(Clone)]
struct AppState {
    config: Arc<RwLock<Config>>,
    client: reqwest::Client,
    cache: Arc<RwLock<DirectLinkCache>>,
    settings_store: SettingsStore,
    crypto_keys: CryptoKeys,
    activity_log: Arc<ActivityLogStore>,
    file_log: Arc<FileLogStore>,
    block_log: Arc<BlockLogStore>,
    ip_location: Arc<ip_location::IpLocationStore>,
    connectivity: Arc<connectivity::ConnectivityMonitor>,
    proxy_manager: Option<Arc<ProxyManager>>,
    proxy_server_id: Option<String>,
    playback_users: Arc<Mutex<HashMap<String, String>>>,
    playback_rate_hits: Arc<Mutex<HashMap<String, VecDeque<client_control::PlaybackRateHit>>>>,
    playback_rate_recent_events: Arc<Mutex<HashMap<String, u64>>>,
    playback_rate_bans: Arc<Mutex<HashMap<String, u64>>>,
    playback_rate_ip_bans: Arc<Mutex<HashMap<String, u64>>>,
    login_failures: Arc<Mutex<HashMap<String, VecDeque<i64>>>>,
    update_check: Arc<Mutex<Option<(u128, UpdateCheckResponse)>>>,
}

pub(crate) struct ProxyManager {
    running: Mutex<HashMap<String, ProxyTask>>,
}

struct ProxyTask {
    port: u16,
    server_name: String,
    started_at_ms: u128,
    last_request_ms: Option<u128>,
    last_error: Option<String>,
    shutdown: oneshot::Sender<()>,
    handle: JoinHandle<()>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProxyStatusEntry {
    pub(crate) server_id: String,
    pub(crate) server_name: String,
    pub(crate) enabled: bool,
    pub(crate) port: u16,
    pub(crate) listening: bool,
    pub(crate) started_at_ms: Option<u128>,
    pub(crate) last_request_ms: Option<u128>,
    pub(crate) last_error: Option<String>,
}

impl ProxyManager {
    fn new() -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
        }
    }

    async fn ensure_running(&self, state: AppState) -> AppResult<()> {
        let servers = state.config.read().await.proxy_configs();
        let mut running = self.running.lock().await;
        let desired_ids = servers
            .iter()
            .filter_map(|server| server.servers.first().map(|entry| entry.id.clone()))
            .collect::<std::collections::HashSet<_>>();
        let stale_ids = running
            .keys()
            .filter(|id| !desired_ids.contains(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in stale_ids {
            if let Some(task) = running.remove(&id) {
                let _ = task.shutdown.send(());
                task.handle.abort();
            }
        }

        for config in servers {
            let Some(server) = config.servers.first() else {
                continue;
            };
            let server_id = server.id.clone();
            let port = config.port;
            if running
                .get(&server_id)
                .is_some_and(|task| task.port == port)
            {
                continue;
            }

            let addr = SocketAddr::from(([0, 0, 0, 0], port));
            let listener = tokio::net::TcpListener::bind(addr).await.map_err(|err| {
                AppError::Config(format!("failed to bind Emby proxy port {port}: {err}"))
            })?;

            if let Some(task) = running.remove(&server_id) {
                let _ = task.shutdown.send(());
                task.handle.abort();
            }

            let mut proxy_state = state.clone();
            proxy_state.proxy_server_id = Some(server_id.clone());
            let (shutdown, receiver) = oneshot::channel();
            let handle = tokio::spawn(run_proxy_server(proxy_state, listener, receiver));
            running.insert(
                server_id,
                ProxyTask {
                    port,
                    server_name: server.name.clone(),
                    started_at_ms: now_ms(),
                    last_request_ms: None,
                    last_error: None,
                    shutdown,
                    handle,
                },
            );
        }

        Ok(())
    }

    async fn restart_all(&self, state: AppState) -> AppResult<()> {
        let tasks = self.running.lock().await.drain().collect::<Vec<_>>();
        for (_, task) in tasks {
            let _ = task.shutdown.send(());
            task.handle.abort();
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        self.ensure_running(state).await
    }

    pub(crate) async fn restart_server(&self, state: AppState, server_id: &str) -> AppResult<()> {
        let config = state
            .config
            .read()
            .await
            .proxy_configs()
            .into_iter()
            .find(|config| {
                config
                    .servers
                    .first()
                    .is_some_and(|server| server.id == server_id)
            })
            .ok_or_else(|| {
                AppError::Config(format!(
                    "server {server_id} is not enabled or does not exist"
                ))
            })?;

        if let Some(task) = self.running.lock().await.remove(server_id) {
            let _ = task.shutdown.send(());
            task.handle.abort();
            tokio::time::sleep(Duration::from_millis(150)).await;
        }

        let Some(server) = config.servers.first() else {
            return Err(AppError::Config(format!("server {server_id} is invalid")));
        };
        let port = config.port;
        let addr = SocketAddr::from(([0, 0, 0, 0], port));
        let listener = tokio::net::TcpListener::bind(addr).await.map_err(|err| {
            AppError::Config(format!("failed to bind Emby proxy port {port}: {err}"))
        })?;

        let mut proxy_state = state.clone();
        proxy_state.proxy_server_id = Some(server.id.clone());
        let (shutdown, receiver) = oneshot::channel();
        let handle = tokio::spawn(run_proxy_server(proxy_state, listener, receiver));
        self.running.lock().await.insert(
            server.id.clone(),
            ProxyTask {
                port,
                server_name: server.name.clone(),
                started_at_ms: now_ms(),
                last_request_ms: None,
                last_error: None,
                shutdown,
                handle,
            },
        );
        Ok(())
    }

    pub(crate) async fn stop_server(&self, server_id: &str) {
        if let Some(task) = self.running.lock().await.remove(server_id) {
            let _ = task.shutdown.send(());
            task.handle.abort();
        }
    }

    async fn record_request(&self, server_id: &str) {
        if let Some(task) = self.running.lock().await.get_mut(server_id) {
            task.last_request_ms = Some(now_ms());
        }
    }

    async fn record_error(&self, server_id: &str, error: String) {
        if let Some(task) = self.running.lock().await.get_mut(server_id) {
            task.last_error = Some(error);
        }
    }

    pub(crate) async fn statuses(&self, config: &Config) -> Vec<ProxyStatusEntry> {
        let running = self.running.lock().await;
        config
            .servers
            .iter()
            .map(|server| {
                let task = running.get(&server.id);
                ProxyStatusEntry {
                    server_id: server.id.clone(),
                    server_name: task
                        .map(|task| task.server_name.clone())
                        .unwrap_or_else(|| server.name.clone()),
                    enabled: server.enabled,
                    port: server.port,
                    listening: task.is_some_and(|task| task.port == server.port),
                    started_at_ms: task.map(|task| task.started_at_ms),
                    last_request_ms: task.and_then(|task| task.last_request_ms),
                    last_error: task.and_then(|task| task.last_error.clone()),
                }
            })
            .collect()
    }

    async fn shutdown(&self) {
        for (_, task) in self.running.lock().await.drain() {
            let _ = task.shutdown.send(());
            task.handle.abort();
        }
    }
}

#[tokio::main]
async fn main() -> AppResult<()> {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| "emby302gateway_rs=info".into()))
        .with(tracing_subscriber::fmt::layer().with_timer(TzTimer::from_env()))
        .init();

    if env::args().any(|arg| arg == "--healthcheck") {
        return healthcheck_command().await;
    }

    let settings_store = SettingsStore::open_default()?;
    let config = settings_store.load_or_default_config()?;
    let file_log = Arc::new(FileLogStore::new(&settings_store));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let block_log = Arc::new(BlockLogStore::new());
    block_log.bootstrap_from_proxy_logs(&settings_store);
    let ip_location = Arc::new(ip_location::IpLocationStore::new());
    if let Err(err) = ip_location.initialize(&client).await {
        tracing::warn!(error = %err, "failed to initialize IP database");
        file_log.write("warning", "IP 数据库初始化失败", &err.safe_log_message());
    }
    let cache = DirectLinkCache::new(config.cache_ttl_seconds, config.cache_max_capacity);
    let proxy_manager = Arc::new(ProxyManager::new());
    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        client,
        cache: Arc::new(RwLock::new(cache)),
        settings_store,
        crypto_keys: CryptoKeys::generate()?,
        activity_log: Arc::new(ActivityLogStore::new(800)),
        file_log,
        block_log,
        ip_location,
        connectivity: Arc::new(connectivity::ConnectivityMonitor::new()),
        proxy_manager: Some(proxy_manager.clone()),
        proxy_server_id: None,
        playback_users: Arc::new(Mutex::new(HashMap::new())),
        playback_rate_hits: Arc::new(Mutex::new(HashMap::new())),
        playback_rate_recent_events: Arc::new(Mutex::new(HashMap::new())),
        playback_rate_bans: Arc::new(Mutex::new(HashMap::new())),
        playback_rate_ip_bans: Arc::new(Mutex::new(HashMap::new())),
        login_failures: Arc::new(Mutex::new(HashMap::new())),
        update_check: Arc::new(Mutex::new(None)),
    };
    state.activity_log.record(
        ActivityKind::General,
        ActivityLevel::Info,
        None,
        PROJECT_NAME,
        "面板服务初始化",
        format!("数据库 {}", db::database_path().display()),
    );
    tracing::info!("项目名称: {}", PROJECT_NAME);
    tracing::info!("项目版本: {}", app_version());
    tracing::info!("项目地址: {}", PROJECT_URL);
    state.file_log.write("info", "项目名称", PROJECT_NAME);
    state.file_log.write("info", "项目版本", &app_version());
    state.file_log.write("info", "项目地址", PROJECT_URL);

    if state.settings_store.has_admin()? && !state.config.read().await.proxy_configs().is_empty() {
        proxy_manager.ensure_running(state.clone()).await?;
    }
    let connectivity_task = connectivity::start_connectivity_monitor(state.clone());

    let listen_addr = management_listen_addr()?;
    let app = build_management_app(state.clone());
    let listener = tokio::net::TcpListener::bind(listen_addr).await?;
    state.activity_log.record(
        ActivityKind::General,
        ActivityLevel::Info,
        None,
        PROJECT_NAME,
        "管理 API 运行中",
        format!("http://{}", listener.local_addr()?),
    );
    tracing::info!("管理 UI: http://{}/ui/", listener.local_addr()?);
    state.file_log.write(
        "info",
        "管理 UI",
        &format!("http://{}/ui/", listener.local_addr()?),
    );

    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(|err| AppError::Internal(format!("server error: {err}")));
    connectivity_task.abort();
    proxy_manager.shutdown().await;
    result
}

async fn healthcheck_command() -> AppResult<()> {
    let addr = management_listen_addr()?;
    let url = format!("http://127.0.0.1:{}/healthz", addr.port());
    let response = reqwest::get(url).await?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(AppError::Internal(format!(
            "healthcheck failed with status {}",
            response.status()
        )))
    }
}

async fn run_proxy_server(
    state: AppState,
    listener: tokio::net::TcpListener,
    shutdown: oneshot::Receiver<()>,
) {
    let addr = listener
        .local_addr()
        .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
    let port = addr.port();
    let app = build_proxy_app(state.clone());
    record_proxy_request(
        &state,
        ActivityLevel::Info,
        "反代服务运行中",
        format!("http://{addr}"),
    )
    .await;
    tracing::info!("Emby reverse proxy listening on http://{}", addr);
    state
        .file_log
        .write("info", "反代服务运行中", &format!("http://{addr}"));
    if let Err(err) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = shutdown.await;
    })
    .await
    {
        tracing::error!(port, error = %err, "Emby proxy server stopped with error");
    }
}

pub(crate) fn management_listen_addr() -> AppResult<SocketAddr> {
    env::var("EMBYPANEL_API_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8090".to_string())
        .parse()
        .map_err(|err| AppError::Config(format!("EMBYPANEL_API_ADDR must be host:port: {err}")))
}

async fn handle_request(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    request: Request,
) -> AppResult<Response> {
    let timing = RequestTiming {
        started_at_ms: now_ms(),
        started: Instant::now(),
    };
    let (parts, body) = request.into_parts();
    let method = parts.method;
    let uri = parts.uri;
    let headers = parts.headers;
    let body = axum::body::to_bytes(body, MAX_PROXY_BODY_BYTES)
        .await
        .map_err(|err| {
            if err
                .to_string()
                .to_ascii_lowercase()
                .contains("length limit")
            {
                AppError::PayloadTooLarge("request body is too large".to_string())
            } else {
                AppError::Internal(format!("failed to read request body: {err}"))
            }
        })?;
    let path = uri.path();

    if !state.settings_store.has_admin()? {
        return Ok(proxy::redirect_response("/ui/".to_string()));
    }

    let client_control_path = rewrite::is_playback_info(path) || rewrite::is_video_stream(path);
    if client_control_path {
        if let Err(err) = client_control::record_from_headers(&state, &headers, None) {
            tracing::warn!(error = %err, "failed to record client control event");
        }
        if let Some(rule) = client_control::matched_block_rule(&state, &headers)? {
            let config = proxy_config(&state).await;
            let (server_id, server_name) = config_server_label(&config);
            let playback_ip = proxy::real_ip_for_log(&config, &headers)
                .or_else(|| Some(peer_addr.ip().to_string()))
                .unwrap_or_else(|| "--".to_string());
            client_control::notify_client_rule_hit(
                &state,
                &rule,
                &server_id,
                &server_name,
                &playback_ip,
                method.as_str(),
                path,
            )
            .await;
            record_proxy_request(
                &state,
                ActivityLevel::Warn,
                "客户端播放已拦截",
                format!("{} {} 命中 UA 拦截规则", method, path),
            )
            .await;
            record_stat(&state, &config, RequestStatKind::Block).await;
            record_proxy_request_detail(
                &state,
                &config,
                ProxyRequestDetailInput {
                    method: method.as_str(),
                    uri: &uri,
                    path_type: request_path_type(path),
                    status_code: StatusCode::FORBIDDEN.as_u16(),
                    outcome: "UA 拦截",
                    started_at_ms: timing.started_at_ms,
                    duration_ms: timing.started.elapsed().as_millis(),
                    playback_user: &rule.user_name,
                    playback_ip: &playback_ip,
                    cache_hit: false,
                    blocked: true,
                    detail: &rule.user_agent,
                },
            )
            .await;
            return Ok((StatusCode::FORBIDDEN, "client disabled by EmbyPanel").into_response());
        }
    }

    if rewrite::is_base_html_player(path) {
        let result =
            handle_base_html_player(&state, method.clone(), uri.clone(), headers, body).await;
        if let Ok(response) = &result {
            let status = response.status();
            let config = proxy_config(&state).await;
            record_proxy_request_detail(
                &state,
                &config,
                ProxyRequestDetailInput {
                    method: method.as_str(),
                    uri: &uri,
                    path_type: "base_html_player",
                    status_code: status.as_u16(),
                    outcome: response_outcome(status),
                    started_at_ms: timing.started_at_ms,
                    duration_ms: timing.started.elapsed().as_millis(),
                    playback_user: "--",
                    playback_ip: "--",
                    cache_hit: false,
                    blocked: false,
                    detail: "",
                },
            )
            .await;
        }
        return result;
    }

    if rewrite::is_system_info(path) {
        let result = handle_system_info(&state, method.clone(), uri.clone(), headers, body).await;
        if let Ok(response) = &result {
            let status = response.status();
            let config = proxy_config(&state).await;
            record_proxy_request_detail(
                &state,
                &config,
                ProxyRequestDetailInput {
                    method: method.as_str(),
                    uri: &uri,
                    path_type: "system_info",
                    status_code: status.as_u16(),
                    outcome: response_outcome(status),
                    started_at_ms: timing.started_at_ms,
                    duration_ms: timing.started.elapsed().as_millis(),
                    playback_user: "--",
                    playback_ip: "--",
                    cache_hit: false,
                    blocked: false,
                    detail: "",
                },
            )
            .await;
        }
        return result;
    }

    if rewrite::is_playback_info(path) {
        return handle_playback_info(&state, method, uri, headers, body, Some(peer_addr), timing)
            .await;
    }

    if rewrite::is_video_stream(path) {
        return handle_video_stream(&state, method, uri, headers, body, Some(peer_addr), timing)
            .await;
    }

    let config = proxy_config(&state).await;
    proxy_to_emby_recorded(
        &state,
        &config,
        method,
        &uri,
        &headers,
        body,
        Some(ProxyRequestTrace {
            started_at_ms: timing.started_at_ms,
            started: timing.started,
            path_type: request_path_type(path),
            playback_user: "--".to_string(),
            playback_ip: proxy::real_ip_for_log(&config, &headers)
                .or_else(|| Some(peer_addr.ip().to_string()))
                .unwrap_or_else(|| "--".to_string()),
        }),
    )
    .await
}

fn build_management_app(state: AppState) -> Router {
    Router::new()
        .route(
            "/",
            get(|| async { proxy::redirect_response("/ui/".to_string()) }),
        )
        .route("/api/setup-status", get(auth::setup_status))
        .route("/api/app-info", get(app_info))
        .route("/api/app-info/update-check", get(update_check))
        .route("/healthz", get(public_healthz))
        .route("/api/public-key", get(crypto_api::public_key))
        .route("/api/setup", axum::routing::post(auth::setup))
        .route("/api/login", axum::routing::post(auth::login))
        .route(
            "/api/change-password",
            axum::routing::post(auth::change_password),
        )
        .route(
            "/api/profile",
            get(auth::get_profile).put(auth::update_profile),
        )
        .route(
            "/api/monitoring/plays",
            get(monitoring::list_playback_sessions),
        )
        .route("/api/monitoring/overview", get(monitoring::media_overview))
        .route("/api/monitoring/health", get(monitoring::server_health))
        .route("/api/monitoring/healthz", get(monitoring::detailed_healthz))
        .route(
            "/api/monitoring/proxy-status",
            get(monitoring::proxy_status),
        )
        .route("/api/monitoring/stats", get(monitoring::request_stats))
        .route(
            "/api/monitoring/connectivity",
            get(monitoring::connectivity_status),
        )
        .route(
            "/api/monitoring/request-details",
            get(monitoring::request_details),
        )
        .route("/api/monitoring/block-logs", get(monitoring::block_logs))
        .route("/api/monitoring/audit-logs", get(monitoring::audit_logs))
        .route("/api/monitoring/logs", get(monitoring::list_activity_logs))
        .route(
            "/api/monitoring/logs/export",
            get(monitoring::export_activity_logs),
        )
        .route(
            "/api/client-control",
            get(client_control::get_client_control).put(client_control::update_client_control),
        )
        .route(
            "/api/client-control/rules",
            axum::routing::post(client_control::add_user_agent_rule)
                .delete(client_control::delete_client_rule),
        )
        .route(
            "/api/client-control/rules/toggle",
            axum::routing::put(client_control::toggle_client_rule),
        )
        .route(
            "/api/client-control/rate-blocks/unblock",
            axum::routing::post(client_control::unblock_rate_limit),
        )
        .route(
            "/api/client-control/rate-limit/status",
            get(client_control::rate_limit_status),
        )
        .route(
            "/api/client-control/webhook/test",
            axum::routing::post(client_control::test_webhook),
        )
        .route(
            "/api/settings",
            get(settings_api::get_settings).put(settings_api::update_settings),
        )
        .route(
            "/api/settings/restart-proxy",
            axum::routing::post(settings_api::restart_proxy_server),
        )
        .route(
            "/api/settings/toggle-proxy",
            axum::routing::post(settings_api::toggle_proxy_server),
        )
        .route(
            "/api/settings/validate",
            axum::routing::post(settings_api::validate_settings),
        )
        .route(
            "/api/settings/log-config",
            get(settings_api::get_log_config).put(settings_api::update_log_config),
        )
        .route(
            "/api/settings/backup/export",
            axum::routing::post(settings_api::export_backup),
        )
        .route(
            "/api/settings/backup/import",
            axum::routing::post(settings_api::import_backup),
        )
        .nest_service(
            "/ui",
            ServeDir::new("frontend/dist")
                .not_found_service(ServeFile::new("frontend/dist/index.html")),
        )
        .with_state(state)
}

fn build_proxy_app(state: AppState) -> Router {
    Router::new()
        .fallback(any(handle_request))
        .with_state(state)
}

#[cfg(test)]
fn build_app(state: AppState) -> Router {
    build_proxy_app(state).layer(axum::Extension(ConnectInfo(SocketAddr::from((
        [127, 0, 0, 1],
        50000,
    )))))
}

async fn proxy_config(state: &AppState) -> Config {
    state
        .config
        .read()
        .await
        .proxy_config_for_server(state.proxy_server_id.as_deref())
}

async fn record_proxy_request(
    state: &AppState,
    level: ActivityLevel,
    message: impl Into<String>,
    detail: impl Into<String>,
) {
    let config = proxy_config(state).await;
    let (server_id, server_name) = config_server_label(&config);
    let message = message.into();
    let detail = detail.into();
    state.activity_log.record(
        ActivityKind::General,
        level,
        Some(&server_id),
        &server_name,
        &message,
        &detail,
    );
    if let Some(proxy_manager) = state.proxy_manager.as_ref() {
        proxy_manager.record_request(&server_id).await;
    }
    state.file_log.write(
        match level {
            ActivityLevel::Success | ActivityLevel::Info => "info",
            ActivityLevel::Warn => "warning",
            ActivityLevel::Error => "error",
        },
        &message,
        &detail,
    );
    record_stat(state, &config, RequestStatKind::Request).await;
}

async fn record_stat(state: &AppState, config: &Config, kind: RequestStatKind) {
    let (server_id, server_name) = config_server_label(config);
    let _ =
        state
            .settings_store
            .increment_request_stats(&server_id, &server_name, config.port, kind);
}

struct ProxyRequestTrace {
    started_at_ms: u128,
    started: Instant,
    path_type: &'static str,
    playback_user: String,
    playback_ip: String,
}

#[derive(Clone, Copy)]
struct RequestTiming {
    started_at_ms: u128,
    started: Instant,
}

struct ProxyRequestDetailInput<'a> {
    method: &'a str,
    uri: &'a Uri,
    path_type: &'a str,
    status_code: u16,
    outcome: &'a str,
    started_at_ms: u128,
    duration_ms: u128,
    playback_user: &'a str,
    playback_ip: &'a str,
    cache_hit: bool,
    blocked: bool,
    detail: &'a str,
}

async fn record_proxy_request_detail(
    state: &AppState,
    config: &Config,
    input: ProxyRequestDetailInput<'_>,
) {
    let (server_id, server_name) = config_server_label(config);
    let path = sanitized_uri_for_log(input.uri);
    let detail = sanitized_detail_for_log(input.detail);
    let _ = state
        .settings_store
        .record_proxy_request_detail(ProxyRequestDetailInsert {
            timestamp_ms: input.started_at_ms,
            server_id: &server_id,
            server_name: &server_name,
            port: config.port,
            method: input.method,
            path: &path,
            path_type: input.path_type,
            status_code: input.status_code,
            outcome: input.outcome,
            duration_ms: input.duration_ms,
            playback_user: input.playback_user,
            playback_ip: input.playback_ip,
            cache_hit: input.cache_hit,
            blocked: input.blocked,
            detail: &detail,
        });
    if input.blocked {
        let ip_location_text = state
            .ip_location
            .lookup(input.playback_ip)
            .await
            .map(|location| location.display_text())
            .unwrap_or_default();
        state.block_log.record(block_log::BlockLogInsert {
            event_type: "request",
            timestamp_ms: input.started_at_ms,
            server_id: &server_id,
            server_name: &server_name,
            port: config.port,
            method: input.method,
            path: &path,
            path_type: input.path_type,
            status_code: input.status_code,
            outcome: input.outcome,
            duration_ms: input.duration_ms,
            playback_user: input.playback_user,
            playback_ip: input.playback_ip,
            ip_location_text: &ip_location_text,
            cache_hit: input.cache_hit,
            detail: &detail,
        });
    }
}

fn request_path_type(path: &str) -> &'static str {
    if rewrite::is_playback_info(path) {
        "playback_info"
    } else if rewrite::is_video_stream(path) {
        "video_stream"
    } else if rewrite::is_system_info(path) {
        "system_info"
    } else if rewrite::is_base_html_player(path) {
        "base_html_player"
    } else {
        "proxy"
    }
}

fn response_outcome(status: StatusCode) -> &'static str {
    if status.is_success() {
        "代理成功"
    } else if status.is_redirection() {
        "重定向"
    } else if status.is_client_error() {
        "客户端错误"
    } else if status.is_server_error() {
        "服务错误"
    } else {
        "代理响应"
    }
}

async fn record_significant_proxy_result(
    state: &AppState,
    config: &Config,
    method: &str,
    uri: &Uri,
    path_type: &str,
    status: StatusCode,
    duration_ms: u128,
    playback_user: &str,
    playback_ip: &str,
) {
    if status.is_success() && duration_ms < 3000 {
        return;
    }
    let (level, message) = if status.is_server_error() {
        (ActivityLevel::Error, "代理服务错误")
    } else if status.is_client_error() {
        (ActivityLevel::Warn, "代理客户端错误")
    } else if duration_ms >= 3000 {
        (ActivityLevel::Warn, "代理慢请求")
    } else {
        return;
    };
    let (server_id, server_name) = config_server_label(config);
    let detail = format!(
        "{} {} status={} duration={}ms type={} user={} ip={}",
        method,
        sanitized_uri_for_log(uri),
        status.as_u16(),
        duration_ms,
        path_type,
        empty_log_value(playback_user),
        empty_log_value(playback_ip)
    );
    state.activity_log.record(
        ActivityKind::General,
        level,
        Some(&server_id),
        &server_name,
        message,
        &detail,
    );
    state.file_log.write(
        match level {
            ActivityLevel::Success | ActivityLevel::Info => "info",
            ActivityLevel::Warn => "warning",
            ActivityLevel::Error => "error",
        },
        message,
        &detail,
    );
}

fn sanitized_uri_for_log(uri: &Uri) -> String {
    const SENSITIVE_QUERY_KEYS: &[&str] = &[
        "api_key",
        "apikey",
        "access_token",
        "token",
        "x-emby-token",
        "x-mediabrowser-token",
    ];
    let path = uri.path();
    let Some(query) = uri.query() else {
        return path.to_string();
    };
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        let value = if SENSITIVE_QUERY_KEYS
            .iter()
            .any(|sensitive| key.eq_ignore_ascii_case(sensitive))
        {
            "***".into()
        } else {
            value
        };
        serializer.append_pair(&key, &value);
    }
    let query = serializer.finish();
    if query.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{query}")
    }
}

fn sanitized_detail_for_log(detail: &str) -> String {
    let trimmed = detail.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(redacted) = sanitized_url_for_log(trimmed) {
        return redacted;
    }
    detail
        .split_whitespace()
        .map(|part| sanitized_url_token_for_log(part).unwrap_or_else(|| part.to_string()))
        .collect::<Vec<_>>()
        .join(" ")
}

fn sanitized_url_token_for_log(token: &str) -> Option<String> {
    let prefix_len = token.find("http://").or_else(|| token.find("https://"))?;
    let (prefix, url_with_suffix) = token.split_at(prefix_len);
    let url_len = url_with_suffix
        .find(|ch: char| ch.is_whitespace() || matches!(ch, ')' | '"' | '\'' | '>' | '<'))
        .unwrap_or(url_with_suffix.len());
    let (url_part, suffix) = url_with_suffix.split_at(url_len);
    sanitized_url_for_log(url_part).map(|url| format!("{prefix}{url}{suffix}"))
}

fn sanitized_url_for_log(value: &str) -> Option<String> {
    let mut url = url::Url::parse(value).ok()?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return None;
    }
    if url.query().is_some() {
        url.set_query(Some("***"));
    }
    url.set_fragment(None);
    Some(url.to_string())
}

fn empty_log_value(value: &str) -> &str {
    if value.trim().is_empty() { "--" } else { value }
}

async fn proxy_to_emby_recorded(
    state: &AppState,
    config: &Config,
    method: Method,
    uri: &Uri,
    headers: &axum::http::HeaderMap,
    body: Bytes,
    trace: Option<ProxyRequestTrace>,
) -> AppResult<Response> {
    let method_label = method.as_str().to_string();
    let result = proxy::proxy_to_emby(&state.client, config, method, uri, headers, body).await;
    match &result {
        Ok(response) => {
            if let Some(trace) = trace {
                let status = response.status();
                let duration_ms = trace.started.elapsed().as_millis();
                record_proxy_request_detail(
                    state,
                    config,
                    ProxyRequestDetailInput {
                        method: &method_label,
                        uri,
                        path_type: trace.path_type,
                        status_code: status.as_u16(),
                        outcome: response_outcome(status),
                        started_at_ms: trace.started_at_ms,
                        duration_ms,
                        playback_user: &trace.playback_user,
                        playback_ip: &trace.playback_ip,
                        cache_hit: false,
                        blocked: false,
                        detail: "",
                    },
                )
                .await;
                record_significant_proxy_result(
                    state,
                    config,
                    &method_label,
                    uri,
                    trace.path_type,
                    status,
                    duration_ms,
                    &trace.playback_user,
                    &trace.playback_ip,
                )
                .await;
            }
        }
        Err(err) => {
            let (server_id, server_name) = config_server_label(config);
            let error_detail = sanitized_detail_for_log(&err.safe_log_message());
            record_stat(state, config, RequestStatKind::Error).await;
            if let Some(proxy_manager) = state.proxy_manager.as_ref() {
                proxy_manager
                    .record_error(&server_id, error_detail.clone())
                    .await;
            }
            state.activity_log.record(
                ActivityKind::General,
                ActivityLevel::Error,
                Some(&server_id),
                &server_name,
                "代理请求失败",
                error_detail.clone(),
            );
            state.file_log.write("error", "代理请求失败", &error_detail);
            if let Some(trace) = trace {
                record_proxy_request_detail(
                    state,
                    config,
                    ProxyRequestDetailInput {
                        method: &method_label,
                        uri,
                        path_type: trace.path_type,
                        status_code: StatusCode::BAD_GATEWAY.as_u16(),
                        outcome: "代理失败",
                        started_at_ms: trace.started_at_ms,
                        duration_ms: trace.started.elapsed().as_millis(),
                        playback_user: &trace.playback_user,
                        playback_ip: &trace.playback_ip,
                        cache_hit: false,
                        blocked: false,
                        detail: &error_detail,
                    },
                )
                .await;
            }
        }
    }
    result
}

async fn public_healthz(State(state): State<AppState>) -> Json<serde_json::Value> {
    let database_ok = state.settings_store.has_admin().is_ok();
    Json(serde_json::json!({
        "status": if database_ok { "ok" } else { "degraded" },
        "name": PROJECT_NAME,
        "version": app_version(),
        "database": if database_ok { "ok" } else { "error" },
        "management": "ok",
    }))
}

async fn update_check(
    State(state): State<AppState>,
    Query(query): Query<UpdateCheckQuery>,
    headers: axum::http::HeaderMap,
) -> AppResult<Json<UpdateCheckResponse>> {
    auth::require_auth(&state, &headers).await?;
    let now = now_ms();
    if !is_force_update_check(&query)
        && let Some((checked_at, cached)) = state.update_check.lock().await.as_ref()
        && now.saturating_sub(*checked_at) < 6 * 3600 * 1000
    {
        return Ok(Json(cached.clone()));
    }
    #[derive(serde::Deserialize)]
    struct GithubRelease {
        tag_name: String,
        html_url: String,
    }
    let current = app_version();
    let release = state
        .client
        .get("https://api.github.com/repos/syscc/EmbyPanel/releases/latest")
        .header(header::USER_AGENT, "EmbyPanel")
        .send()
        .await?
        .json::<GithubRelease>()
        .await?;
    let response = UpdateCheckResponse {
        current_version: current.clone(),
        latest_version: release.tag_name.clone(),
        release_url: release.html_url,
        has_update: normalize_version(&release.tag_name) > normalize_version(&current),
        checked_at_ms: now,
    };
    *state.update_check.lock().await = Some((now, response.clone()));
    Ok(Json(response))
}

fn is_force_update_check(query: &UpdateCheckQuery) -> bool {
    query.force.as_deref().map(str::trim).is_some_and(|value| {
        matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn normalize_version(value: &str) -> Vec<u64> {
    value
        .trim_start_matches('v')
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or_default())
        .collect()
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn config_server_label(config: &Config) -> (String, String) {
    config
        .servers
        .first()
        .map(|server| (server.id.clone(), server.name.clone()))
        .unwrap_or_else(|| ("default".to_string(), "默认服务器".to_string()))
}

async fn handle_base_html_player(
    state: &AppState,
    method: Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let config = proxy_config(state).await;
    let (status, response_headers, text) =
        proxy::proxy_text(&state.client, &config, method, &uri, &headers, body).await?;
    if !status.is_success() {
        return proxy::body_response(status, response_headers, text);
    }

    let modified = rewrite::patch_base_html_player(&text);
    tracing::info!(
        path = uri.path(),
        patched = modified != text,
        "patched basehtmlplayer.js"
    );
    proxy::body_response(
        status,
        rewrite::text_headers("application/javascript"),
        modified,
    )
}

async fn handle_system_info(
    state: &AppState,
    method: Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let config = proxy_config(state).await;
    let (status, response_headers, text) =
        proxy::proxy_text(&state.client, &config, method, &uri, &headers, body).await?;
    if !status.is_success() {
        return proxy::body_response(status, response_headers, text);
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return proxy::body_response(status, response_headers, text);
    };
    let gateway_url = proxy_base_url(&headers, config.port);
    let modified = rewrite::patch_system_info(value, config.port, &gateway_url);

    proxy::body_response(
        status,
        rewrite::json_headers(),
        serde_json::to_string(&modified)?,
    )
}

async fn handle_playback_info(
    state: &AppState,
    method: Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: Bytes,
    peer_addr: Option<SocketAddr>,
    timing: RequestTiming,
) -> AppResult<Response> {
    let config = proxy_config(state).await;
    let path = uri.path().to_string();
    let query = uri.query().map(str::to_string);
    let method_label = method.as_str().to_string();
    let playback_user = playback_user_name(
        &state.client,
        &config,
        query.as_deref().unwrap_or(""),
        &headers,
        state,
    )
    .await;
    let playback_ip = proxy::real_ip_for_log(&config, &headers)
        .or_else(|| peer_addr.map(|addr| addr.ip().to_string()))
        .unwrap_or_else(|| "--".to_string());
    remember_playback_user(state, &config, query.as_deref().unwrap_or(""), &headers).await;
    let (status, response_headers, text) =
        proxy::proxy_text(&state.client, &config, method, &uri, &headers, body).await?;
    if !status.is_success() {
        record_proxy_request_detail(
            state,
            &config,
            ProxyRequestDetailInput {
                method: &method_label,
                uri: &uri,
                path_type: "playback_info",
                status_code: status.as_u16(),
                outcome: response_outcome(status),
                started_at_ms: timing.started_at_ms,
                duration_ms: timing.started.elapsed().as_millis(),
                playback_user: &playback_user,
                playback_ip: &playback_ip,
                cache_hit: false,
                blocked: false,
                detail: "",
            },
        )
        .await;
        return proxy::body_response(status, response_headers, text);
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        record_proxy_request_detail(
            state,
            &config,
            ProxyRequestDetailInput {
                method: &method_label,
                uri: &uri,
                path_type: "playback_info",
                status_code: status.as_u16(),
                outcome: "代理成功",
                started_at_ms: timing.started_at_ms,
                duration_ms: timing.started.elapsed().as_millis(),
                playback_user: &playback_user,
                playback_ip: &playback_ip,
                cache_hit: false,
                blocked: false,
                detail: "非 JSON 响应",
            },
        )
        .await;
        return proxy::body_response(status, response_headers, text);
    };
    let stream_query = playback_stream_query(query.as_deref().unwrap_or(""), &headers);
    let modified = rewrite::patch_playback_info(value, &path, stream_query.as_deref());

    record_proxy_request_detail(
        state,
        &config,
        ProxyRequestDetailInput {
            method: &method_label,
            uri: &uri,
            path_type: "playback_info",
            status_code: status.as_u16(),
            outcome: "播放信息改写",
            started_at_ms: timing.started_at_ms,
            duration_ms: timing.started.elapsed().as_millis(),
            playback_user: &playback_user,
            playback_ip: &playback_ip,
            cache_hit: false,
            blocked: false,
            detail: "",
        },
    )
    .await;
    proxy::body_response(
        status,
        rewrite::json_headers(),
        serde_json::to_string(&modified)?,
    )
}

async fn handle_video_stream(
    state: &AppState,
    method: Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: Bytes,
    peer_addr: Option<SocketAddr>,
    timing: RequestTiming,
) -> AppResult<Response> {
    let config = proxy_config(state).await;
    let Some(item_id) = rewrite::parse_item_id(uri.path()) else {
        record_proxy_request_detail(
            state,
            &config,
            ProxyRequestDetailInput {
                method: method.as_str(),
                uri: &uri,
                path_type: "video_stream",
                status_code: StatusCode::BAD_REQUEST.as_u16(),
                outcome: "请求错误",
                started_at_ms: timing.started_at_ms,
                duration_ms: timing.started.elapsed().as_millis(),
                playback_user: "--",
                playback_ip: "--",
                cache_hit: false,
                blocked: false,
                detail: "缺少媒体项目 ID",
            },
        )
        .await;
        return Ok((StatusCode::BAD_REQUEST, "Bad Request").into_response());
    };

    let method_label = method.as_str().to_string();
    let query = uri.query().unwrap_or("");
    let media_source_id = query_param(query, "MediaSourceId");
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let playback_user = playback_user_name(&state.client, &config, query, &headers, state).await;
    let playback_ip = proxy::real_ip_for_log(&config, &headers)
        .or_else(|| peer_addr.map(|addr| addr.ip().to_string()))
        .unwrap_or_else(|| "--".to_string());
    let (server_id, server_name) = config_server_label(&config);
    let concurrent_limited = method != Method::HEAD
        && client_control::enforce_concurrent_playback_limit(
            state,
            client_control::ConcurrentPlaybackLimitInput {
                runtime_config: &config,
                server_id: &server_id,
                server_name: &server_name,
                playback_user: &playback_user,
                playback_ip: &playback_ip,
                item_id: &item_id,
            },
        )
        .await?;
    if concurrent_limited {
        record_stat(state, &config, RequestStatKind::Block).await;
        record_proxy_request_detail(
            state,
            &config,
            ProxyRequestDetailInput {
                method: &method_label,
                uri: &uri,
                path_type: "video_stream",
                status_code: StatusCode::TOO_MANY_REQUESTS.as_u16(),
                outcome: "同时播放限制",
                started_at_ms: timing.started_at_ms,
                duration_ms: timing.started.elapsed().as_millis(),
                playback_user: &playback_user,
                playback_ip: &playback_ip,
                cache_hit: false,
                blocked: true,
                detail: &item_id,
            },
        )
        .await;
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            "concurrent playback limit exceeded",
        )
            .into_response());
    }
    let rate_limited = method != Method::HEAD
        && client_control::enforce_playback_rate_limit(
            state,
            client_control::PlaybackRateLimitInput {
                runtime_config: &config,
                client: &state.client,
                server_id: &server_id,
                server_name: &server_name,
                playback_user: &playback_user,
                playback_ip: &playback_ip,
                playback_event: &item_id,
                skip_recent_event: false,
                record_recent_event: false,
            },
        )
        .await?;
    if rate_limited {
        record_stat(state, &config, RequestStatKind::Block).await;
        record_proxy_request_detail(
            state,
            &config,
            ProxyRequestDetailInput {
                method: &method_label,
                uri: &uri,
                path_type: "video_stream",
                status_code: StatusCode::TOO_MANY_REQUESTS.as_u16(),
                outcome: "频率限制",
                started_at_ms: timing.started_at_ms,
                duration_ms: timing.started.elapsed().as_millis(),
                playback_user: &playback_user,
                playback_ip: &playback_ip,
                cache_hit: false,
                blocked: true,
                detail: &item_id,
            },
        )
        .await;
        return Ok((
            StatusCode::TOO_MANY_REQUESTS,
            "playback rate limit exceeded",
        )
            .into_response());
    }
    let cache_key = format!(
        "{}:{}",
        state.proxy_server_id.as_deref().unwrap_or("default"),
        openlist::cache_key(&item_id, media_source_id.as_deref(), user_agent)
    );

    let cache = state.cache.read().await.clone();
    if let Some(cached) = cache.get(&cache_key).await
        && should_cache_direct_link(&config, &cached)
    {
        record_playback_redirect(
            state,
            &config,
            &playback_user,
            &playback_ip,
            "直链缓存命中",
            &cached,
        );
        tracing::info!(
            item = item_id,
            source = rewrite::source_label(media_source_id.as_deref()),
            "direct link cache hit"
        );
        record_proxy_request_detail(
            state,
            &config,
            ProxyRequestDetailInput {
                method: &method_label,
                uri: &uri,
                path_type: "video_stream",
                status_code: StatusCode::FOUND.as_u16(),
                outcome: "直链缓存命中",
                started_at_ms: timing.started_at_ms,
                duration_ms: timing.started.elapsed().as_millis(),
                playback_user: &playback_user,
                playback_ip: &playback_ip,
                cache_hit: true,
                blocked: false,
                detail: &cached,
            },
        )
        .await;
        return Ok(proxy::redirect_response(cached));
    }

    let media_path =
        match emby::get_media_path(&state.client, &config, &item_id, media_source_id.as_deref())
            .await
        {
            Ok(Some(path)) => path,
            Ok(None) => {
                tracing::info!(item = item_id, "media source not found; proxying to Emby");
                return proxy_to_emby_recorded(
                    state,
                    &config,
                    method,
                    &uri,
                    &headers,
                    body,
                    Some(ProxyRequestTrace {
                        started_at_ms: timing.started_at_ms,
                        started: timing.started,
                        path_type: "video_stream",
                        playback_user,
                        playback_ip,
                    }),
                )
                .await;
            }
            Err(err) => {
                record_proxy_request_detail(
                    state,
                    &config,
                    ProxyRequestDetailInput {
                        method: &method_label,
                        uri: &uri,
                        path_type: "video_stream",
                        status_code: StatusCode::BAD_GATEWAY.as_u16(),
                        outcome: "读取媒体失败",
                        started_at_ms: timing.started_at_ms,
                        duration_ms: timing.started.elapsed().as_millis(),
                        playback_user: &playback_user,
                        playback_ip: &playback_ip,
                        cache_hit: false,
                        blocked: false,
                        detail: &err.safe_log_message(),
                    },
                )
                .await;
                return Err(err);
            }
        };
    let media_path = apply_strm_url_mappings(&config, &media_path);

    if is_direct_url(&media_path) && config.openlist_addr.is_none() {
        let redirect_url = maybe_resolve_internal_redirect(&config, &media_path, user_agent).await;
        let should_cache = should_cache_direct_link(&config, &redirect_url);
        if should_cache {
            let cache = state.cache.read().await.clone();
            cache.set(cache_key, redirect_url.clone()).await;
        }
        record_playback_redirect(
            state,
            &config,
            &playback_user,
            &playback_ip,
            redirect_log_message(&config, should_cache),
            &redirect_url,
        );
        tracing::info!(
            item = item_id,
            source = rewrite::source_label(media_source_id.as_deref()),
            "redirecting to direct media URL"
        );
        record_proxy_request_detail(
            state,
            &config,
            ProxyRequestDetailInput {
                method: &method_label,
                uri: &uri,
                path_type: "video_stream",
                status_code: StatusCode::FOUND.as_u16(),
                outcome: if should_cache {
                    "重定向"
                } else {
                    "重定向不缓存"
                },
                started_at_ms: timing.started_at_ms,
                duration_ms: timing.started.elapsed().as_millis(),
                playback_user: &playback_user,
                playback_ip: &playback_ip,
                cache_hit: false,
                blocked: false,
                detail: &redirect_url,
            },
        )
        .await;
        return Ok(proxy::redirect_response(redirect_url));
    }

    let Some(openlist_path) = openlist::extract_openlist_path(&media_path) else {
        tracing::info!(
            item = item_id,
            path = media_path,
            "media path is not a direct URL or OpenList /d URL; proxying to Emby"
        );
        return proxy_to_emby_recorded(
            state,
            &config,
            method,
            &uri,
            &headers,
            body,
            Some(ProxyRequestTrace {
                started_at_ms: timing.started_at_ms,
                started: timing.started,
                path_type: "video_stream",
                playback_user,
                playback_ip,
            }),
        )
        .await;
    };

    let mut raw_url = openlist::ensure_raw_url(
        openlist::fs_get(&state.client, &config, &openlist_path, user_agent).await?,
    )?;
    raw_url = maybe_resolve_internal_redirect(&config, &raw_url, user_agent).await;
    let should_cache = should_cache_direct_link(&config, &raw_url);
    if should_cache {
        let cache = state.cache.read().await.clone();
        cache.set(cache_key, raw_url.clone()).await;
    }
    record_playback_redirect(
        state,
        &config,
        &playback_user,
        &playback_ip,
        redirect_log_message(&config, should_cache),
        &raw_url,
    );

    tracing::info!(
        item = item_id,
        source = rewrite::source_label(media_source_id.as_deref()),
        "redirecting to OpenList raw_url"
    );
    record_proxy_request_detail(
        state,
        &config,
        ProxyRequestDetailInput {
            method: &method_label,
            uri: &uri,
            path_type: "video_stream",
            status_code: StatusCode::FOUND.as_u16(),
            outcome: if should_cache {
                "重定向"
            } else {
                "重定向不缓存"
            },
            started_at_ms: timing.started_at_ms,
            duration_ms: timing.started.elapsed().as_millis(),
            playback_user: &playback_user,
            playback_ip: &playback_ip,
            cache_hit: false,
            blocked: false,
            detail: &raw_url,
        },
    )
    .await;
    Ok(proxy::redirect_response(raw_url))
}

fn query_param(query: &str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.into_owned())
}

fn is_direct_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn record_playback_redirect(
    state: &AppState,
    config: &Config,
    playback_user: &str,
    playback_ip: &str,
    message: impl Into<String>,
    redirect_url: &str,
) {
    let (server_id, server_name) = config_server_label(config);
    let message = message.into();
    let log_detail = sanitized_detail_for_log(redirect_url);
    state.activity_log.record_playback(PlaybackLogRecord {
        level: ActivityLevel::Success,
        server_id: Some(&server_id),
        server_name: &server_name,
        playback_user,
        playback_ip,
        message: message.clone(),
        detail: log_detail.clone(),
    });
    state
        .file_log
        .write("info", &message, &format!("{playback_user} {log_detail}"));
    let kind = if message.contains("缓存命中") {
        RequestStatKind::CacheHit
    } else {
        RequestStatKind::Redirect
    };
    let _ =
        state
            .settings_store
            .increment_request_stats(&server_id, &server_name, config.port, kind);
}

fn cache_ttl_label(seconds: u64) -> String {
    if seconds >= 60 && seconds.is_multiple_of(60) {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

fn redirect_log_message(config: &Config, should_cache: bool) -> String {
    if !should_cache {
        "重定向 strm".to_string()
    } else {
        format!(
            "重定向 strm(缓存{})",
            cache_ttl_label(config.cache_ttl_seconds)
        )
    }
}

async fn playback_user_name(
    client: &reqwest::Client,
    config: &Config,
    query: &str,
    headers: &axum::http::HeaderMap,
    state: &AppState,
) -> String {
    if let Some(user_name) = query_param(query, "UserName").filter(|value| !value.trim().is_empty())
    {
        return user_name;
    }

    if let Some(user_name) = user_name_by_user_id(client, config, query, headers).await {
        return user_name;
    }

    if let Some(user_name) = user_name_by_token(client, config, query, headers).await {
        return user_name;
    }

    let Some(device_id) = playback_device_id(query, headers) else {
        return "--".to_string();
    };

    if let Some(user_name) = remembered_playback_user(state, config, &device_id).await {
        return user_name;
    }

    match emby::get_user_name_by_device_id(client, config, &device_id).await {
        Ok(Some(user_name)) => user_name,
        Ok(None) => "--".to_string(),
        Err(err) => {
            tracing::warn!(
                device_id,
                error = %err.safe_log_message(),
                "failed to resolve playback user name"
            );
            "--".to_string()
        }
    }
}

async fn remember_playback_user(
    state: &AppState,
    config: &Config,
    query: &str,
    headers: &axum::http::HeaderMap,
) {
    let Some(device_id) = playback_device_id(query, headers) else {
        return;
    };
    let user_name = if let Some(user_name) =
        query_param(query, "UserName").filter(|value| !value.trim().is_empty())
    {
        user_name
    } else if let Some(user_name) =
        user_name_by_user_id(&state.client, config, query, headers).await
    {
        user_name
    } else if let Some(user_name) = user_name_by_token(&state.client, config, query, headers).await
    {
        user_name
    } else {
        match emby::get_user_name_by_device_id(&state.client, config, &device_id).await {
            Ok(Some(user_name)) => user_name,
            Ok(None) => return,
            Err(err) => {
                tracing::warn!(
                    device_id,
                    error = %err.safe_log_message(),
                    "failed to remember playback user name"
                );
                return;
            }
        }
    };

    state
        .playback_users
        .lock()
        .await
        .insert(playback_user_key(config, &device_id), user_name);
}

async fn remembered_playback_user(
    state: &AppState,
    config: &Config,
    device_id: &str,
) -> Option<String> {
    state
        .playback_users
        .lock()
        .await
        .get(&playback_user_key(config, device_id))
        .cloned()
}

fn playback_user_key(config: &Config, device_id: &str) -> String {
    let server_id = config
        .servers
        .first()
        .map(|server| server.id.as_str())
        .unwrap_or("default");
    format!("{server_id}:{}", device_id.trim())
}

async fn user_name_by_user_id(
    client: &reqwest::Client,
    config: &Config,
    query: &str,
    headers: &axum::http::HeaderMap,
) -> Option<String> {
    let user_id = query_param(query, "UserId")
        .or_else(|| header_value(headers, "X-Emby-User-Id"))
        .or_else(|| emby_authorization_value(headers, "UserId"))?;
    match emby::get_user_name_by_user_id(client, config, &user_id).await {
        Ok(Some(user_name)) => Some(user_name),
        Ok(None) => None,
        Err(err) => {
            tracing::warn!(
                user_id,
                error = %err.safe_log_message(),
                "failed to resolve playback user by user id"
            );
            None
        }
    }
}

async fn user_name_by_token(
    client: &reqwest::Client,
    config: &Config,
    query: &str,
    headers: &axum::http::HeaderMap,
) -> Option<String> {
    let token = query_param(query, "X-Emby-Token")
        .or_else(|| query_param(query, "api_key"))
        .or_else(|| header_value(headers, "X-Emby-Token"))
        .or_else(|| header_value(headers, "X-MediaBrowser-Token"))
        .or_else(|| emby_authorization_value(headers, "Token"))
        .or_else(|| emby_token_from_authorization(headers));
    let token = token?.trim().to_string();
    if token.is_empty() || token == config.emby_api_key {
        return None;
    }

    match emby::get_user_name_by_token(client, config, &token).await {
        Ok(Some(user_name)) => Some(user_name),
        Ok(None) => None,
        Err(err) => {
            tracing::warn!(
                error = %err.safe_log_message(),
                "failed to resolve playback user by token"
            );
            None
        }
    }
}

fn playback_device_id(query: &str, headers: &axum::http::HeaderMap) -> Option<String> {
    query_param(query, "DeviceId")
        .or_else(|| header_value(headers, "X-Emby-Device-Id"))
        .or_else(|| emby_authorization_value(headers, "DeviceId"))
}

fn playback_stream_query(query: &str, headers: &axum::http::HeaderMap) -> Option<String> {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        serializer.append_pair(&key, &value);
    }

    if query_param(query, "UserId").is_none()
        && let Some(user_id) = header_value(headers, "X-Emby-User-Id")
            .or_else(|| emby_authorization_value(headers, "UserId"))
    {
        serializer.append_pair("UserId", &user_id);
    }

    if query_param(query, "DeviceId").is_none()
        && let Some(device_id) = header_value(headers, "X-Emby-Device-Id")
            .or_else(|| emby_authorization_value(headers, "DeviceId"))
    {
        serializer.append_pair("DeviceId", &device_id);
    }

    let query = serializer.finish();
    (!query.is_empty()).then_some(query)
}

fn emby_token_from_authorization(headers: &axum::http::HeaderMap) -> Option<String> {
    let value = header_value(headers, "Authorization")?;
    if let Some(token) = value.strip_prefix("Bearer ") {
        return Some(token.trim().to_string()).filter(|token| !token.is_empty());
    }
    value
        .split(',')
        .map(str::trim)
        .find_map(|part| {
            part.strip_prefix("Token=")
                .or_else(|| part.strip_prefix("token="))
                .map(|token| token.trim_matches('"').trim().to_string())
        })
        .filter(|token| !token.is_empty())
}

fn emby_authorization_value(headers: &axum::http::HeaderMap, key: &str) -> Option<String> {
    ["X-Emby-Authorization", "X-MediaBrowser-Authorization"]
        .into_iter()
        .filter_map(|header| header_value(headers, header))
        .find_map(|value| media_browser_auth_value(&value, key))
}

fn media_browser_auth_value(value: &str, key: &str) -> Option<String> {
    value
        .split(',')
        .map(str::trim)
        .find_map(|part| {
            let (name, value) = part.split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case(key)
                .then(|| value.trim().trim_matches('"').to_string())
        })
        .filter(|value| !value.is_empty())
}

fn header_value(headers: &axum::http::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn should_cache_direct_link(config: &Config, redirect_url: &str) -> bool {
    if config.cache_ttl_seconds == 0 {
        return false;
    }
    let Some(host) = url::Url::parse(redirect_url)
        .ok()
        .and_then(|url| url.host_str().map(|host| host.to_ascii_lowercase()))
    else {
        return false;
    };

    let rules = cache_filter_entries(&config.cache_domain_whitelist);
    match config.cache_domain_filter_mode.as_str() {
        "whitelist" => {
            !rules.is_empty()
                && rules
                    .iter()
                    .any(|pattern| domain_filter_matches(&host, pattern))
        }
        "blacklist" => !rules
            .iter()
            .any(|pattern| domain_filter_matches(&host, pattern)),
        _ => true,
    }
}

fn cache_filter_entries(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            url::Url::parse(line)
                .ok()
                .and_then(|url| url.host_str().map(str::to_string))
                .unwrap_or_else(|| line.to_string())
                .to_ascii_lowercase()
        })
        .collect()
}

fn domain_filter_matches(host: &str, pattern: &str) -> bool {
    if pattern.contains('*') {
        wildcard_match(host, pattern)
    } else {
        host.contains(pattern)
    }
}

fn wildcard_match(value: &str, pattern: &str) -> bool {
    let parts = pattern.split('*').collect::<Vec<_>>();
    if parts.len() == 1 {
        return value == pattern;
    }

    let mut remaining = value;
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        let Some(position) = remaining.find(part) else {
            return false;
        };
        if index == 0 && !pattern.starts_with('*') && position != 0 {
            return false;
        }
        remaining = &remaining[position + part.len()..];
    }

    pattern.ends_with('*') || parts.last().is_none_or(|last| value.ends_with(last))
}

fn proxy_base_url(headers: &axum::http::HeaderMap, fallback_port: u16) -> String {
    let proto = forwarded_header_value(headers, "x-forwarded-proto").unwrap_or("http");
    let host = forwarded_header_value(headers, "x-forwarded-host")
        .or_else(|| forwarded_header_value(headers, "host"))
        .map(str::to_string)
        .unwrap_or_else(|| format!("127.0.0.1:{fallback_port}"));

    format!("{proto}://{host}")
}

fn forwarded_header_value<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn apply_strm_url_mappings(config: &Config, media_path: &str) -> String {
    let mapped = url_mapping::apply_rules(media_path, &config.strm_url_mapping_rules);
    if mapped != media_path {
        tracing::info!(
            from = sanitized_detail_for_log(media_path),
            to = sanitized_detail_for_log(&mapped),
            "mapped STRM URL"
        );
    }
    mapped
}

async fn maybe_resolve_internal_redirect(config: &Config, url: &str, user_agent: &str) -> String {
    if !config.enable_internal_redirect {
        return url.to_string();
    }

    match internal_redirect::resolve_redirect_location(
        url,
        config.internal_redirect_timeout_seconds,
        user_agent,
    )
    .await
    {
        Ok(resolved) => {
            tracing::info!(
                from = sanitized_detail_for_log(url),
                to = sanitized_detail_for_log(&resolved),
                "internal redirect resolved final URL"
            );
            resolved
        }
        Err(err) => {
            tracing::warn!(
                url = sanitized_detail_for_log(url),
                error = %safe_error_message(&err),
                "internal redirect failed; using original URL"
            );
            url.to_string()
        }
    }
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use axum::{
        body::{Body, to_bytes},
        http::Request as HttpRequest,
    };
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    };
    use tokio::net::TcpListener;
    use tower::ServiceExt;

    use super::*;

    static TEST_DB_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_config(emby_host: &str, openlist_addr: &str, port: u16) -> Config {
        Config {
            emby_host: emby_host.to_string(),
            emby_api_key: "emby-key".to_string(),
            servers: Vec::new(),
            openlist_addr: Some(openlist_addr.to_string()),
            openlist_token: Some("openlist-token".to_string()),
            port,
            cache_ttl_seconds: 180,
            cache_max_capacity: 10_000,
            cache_domain_filter_mode: "off".to_string(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect: false,
            internal_redirect_timeout_seconds: 15,
            strm_url_mappings: String::new(),
            connectivity_check_enabled: true,
            connectivity_check_interval_seconds: 60,
            connectivity_check_timeout_seconds: 5,
            connectivity_auto_restart_seconds: 180,
            strm_url_mapping_rules: Vec::new(),
        }
    }

    fn test_state(config: Config, settings_store: SettingsStore) -> AppState {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        AppState {
            config: Arc::new(RwLock::new(config)),
            client,
            cache: Arc::new(RwLock::new(DirectLinkCache::new(180, 10_000))),
            file_log: Arc::new(FileLogStore::new(&settings_store)),
            settings_store,
            crypto_keys: CryptoKeys::generate().unwrap(),
            activity_log: Arc::new(ActivityLogStore::new(100)),
            block_log: Arc::new(BlockLogStore::new()),
            ip_location: Arc::new(ip_location::IpLocationStore::new()),
            connectivity: Arc::new(connectivity::ConnectivityMonitor::new()),
            proxy_manager: None,
            proxy_server_id: None,
            playback_users: Arc::new(Mutex::new(HashMap::new())),
            playback_rate_hits: Arc::new(Mutex::new(HashMap::new())),
            playback_rate_recent_events: Arc::new(Mutex::new(HashMap::new())),
            playback_rate_bans: Arc::new(Mutex::new(HashMap::new())),
            playback_rate_ip_bans: Arc::new(Mutex::new(HashMap::new())),
            login_failures: Arc::new(Mutex::new(HashMap::new())),
            update_check: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn query_param_finds_case_insensitive_keys() {
        assert_eq!(
            query_param("MediaSourceId=abc&DeviceId=d", "mediasourceid").as_deref(),
            Some("abc")
        );
    }

    #[test]
    fn media_browser_auth_value_extracts_token_fields() {
        let value = r#"MediaBrowser Client="Emby Web", DeviceId="device-1", Token="token-1", UserId="user-1""#;
        assert_eq!(
            media_browser_auth_value(value, "deviceid").as_deref(),
            Some("device-1")
        );
        assert_eq!(
            media_browser_auth_value(value, "Token").as_deref(),
            Some("token-1")
        );
        assert_eq!(
            media_browser_auth_value(value, "UserId").as_deref(),
            Some("user-1")
        );
    }

    #[test]
    fn playback_stream_query_carries_auth_context_from_headers() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-emby-authorization",
            axum::http::HeaderValue::from_static(
                r#"MediaBrowser Client="Emby Web", DeviceId="device-1", UserId="user-1""#,
            ),
        );

        let query = playback_stream_query("MediaSourceId=source-1", &headers).unwrap();
        assert_eq!(query_param(&query, "UserId").as_deref(), Some("user-1"));
        assert_eq!(query_param(&query, "DeviceId").as_deref(), Some("device-1"));
    }

    #[test]
    fn direct_url_detection_accepts_http_and_https_only() {
        assert!(is_direct_url("http://cdn.example.test/a.mkv"));
        assert!(is_direct_url("https://cdn.example.test/a.mkv"));
        assert!(!is_direct_url("/media/a.mkv"));
    }

    #[tokio::test]
    async fn playback_rate_limit_counts_video_stream_once_per_playback() {
        let store = test_store();
        store
            .save_setting_json(
                "client_control",
                &client_control::ClientControlConfig {
                    enabled: false,
                    notify_enabled: false,
                    playback_rate_limit_enabled: true,
                    playback_rate_limit_window_seconds: 30,
                    playback_rate_limit_max_requests: 5,
                    playback_rate_limit_block_seconds: 1800,
                    playback_rate_limit_action: "block_ip".to_string(),
                    concurrent_playback_limit_enabled: false,
                    concurrent_playback_limit_max: 3,
                    rate_limit_blocks: Vec::new(),
                    webhook: client_control::WebhookNotifyConfig::default(),
                    webhooks: Vec::new(),
                    records: Vec::new(),
                },
            )
            .unwrap();
        let config = test_config("http://127.0.0.1:8096", "http://127.0.0.1:5244", 8096);
        let state = test_state(config.clone(), store);

        for _ in 0..5 {
            let blocked = client_control::enforce_playback_rate_limit(
                &state,
                client_control::PlaybackRateLimitInput {
                    runtime_config: &config,
                    client: &state.client,
                    server_id: "server-a",
                    server_name: "a",
                    playback_user: "test",
                    playback_ip: "10.0.0.74",
                    playback_event: "item-1",
                    skip_recent_event: false,
                    record_recent_event: false,
                },
            )
            .await
            .unwrap();
            assert!(!blocked);
        }

        let blocked = client_control::enforce_playback_rate_limit(
            &state,
            client_control::PlaybackRateLimitInput {
                runtime_config: &config,
                client: &state.client,
                server_id: "server-a",
                server_name: "a",
                playback_user: "test",
                playback_ip: "10.0.0.74",
                playback_event: "item-1",
                skip_recent_event: false,
                record_recent_event: false,
            },
        )
        .await
        .unwrap();
        assert!(blocked);
    }

    #[tokio::test]
    async fn playback_rate_limit_counts_stream_only_requests() {
        let store = test_store();
        store
            .save_setting_json(
                "client_control",
                &client_control::ClientControlConfig {
                    enabled: false,
                    notify_enabled: false,
                    playback_rate_limit_enabled: true,
                    playback_rate_limit_window_seconds: 30,
                    playback_rate_limit_max_requests: 5,
                    playback_rate_limit_block_seconds: 1800,
                    playback_rate_limit_action: "block_ip".to_string(),
                    concurrent_playback_limit_enabled: false,
                    concurrent_playback_limit_max: 3,
                    rate_limit_blocks: Vec::new(),
                    webhook: client_control::WebhookNotifyConfig::default(),
                    webhooks: Vec::new(),
                    records: Vec::new(),
                },
            )
            .unwrap();
        let config = test_config("http://127.0.0.1:8096", "http://127.0.0.1:5244", 8096);
        let state = test_state(config.clone(), store);

        for _ in 0..5 {
            let blocked = client_control::enforce_playback_rate_limit(
                &state,
                client_control::PlaybackRateLimitInput {
                    runtime_config: &config,
                    client: &state.client,
                    server_id: "server-a",
                    server_name: "a",
                    playback_user: "test",
                    playback_ip: "10.0.0.67",
                    playback_event: "item-1",
                    skip_recent_event: false,
                    record_recent_event: false,
                },
            )
            .await
            .unwrap();
            assert!(!blocked);
        }

        let blocked = client_control::enforce_playback_rate_limit(
            &state,
            client_control::PlaybackRateLimitInput {
                runtime_config: &config,
                client: &state.client,
                server_id: "server-a",
                server_name: "a",
                playback_user: "test",
                playback_ip: "10.0.0.67",
                playback_event: "item-1",
                skip_recent_event: false,
                record_recent_event: false,
            },
        )
        .await
        .unwrap();
        assert!(blocked);
    }

    #[tokio::test]
    async fn concurrent_playback_limit_blocks_next_distinct_video() {
        let store = test_store();
        store
            .save_setting_json(
                "client_control",
                &client_control::ClientControlConfig {
                    enabled: false,
                    notify_enabled: false,
                    playback_rate_limit_enabled: false,
                    playback_rate_limit_window_seconds: 30,
                    playback_rate_limit_max_requests: 20,
                    playback_rate_limit_block_seconds: 1800,
                    playback_rate_limit_action: "block_ip".to_string(),
                    concurrent_playback_limit_enabled: true,
                    concurrent_playback_limit_max: 3,
                    rate_limit_blocks: Vec::new(),
                    webhook: client_control::WebhookNotifyConfig::default(),
                    webhooks: Vec::new(),
                    records: Vec::new(),
                },
            )
            .unwrap();
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Sessions" {
                return response_json(serde_json::json!([
                    {"Id": "s1", "UserName": "test", "NowPlayingItem": {"Id": "item-1", "Name": "A"}},
                    {"Id": "s2", "UserName": "test", "NowPlayingItem": {"Id": "item-2", "Name": "B"}},
                    {"Id": "s3", "UserName": "test", "NowPlayingItem": {"Id": "item-3", "Name": "C"}}
                ]));
            }
            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let config = test_config(&emby, "http://127.0.0.1:5244", 8096);
        let state = test_state(config.clone(), store);

        let blocked = client_control::enforce_concurrent_playback_limit(
            &state,
            client_control::ConcurrentPlaybackLimitInput {
                runtime_config: &config,
                server_id: "default",
                server_name: "default",
                playback_user: "test",
                playback_ip: "10.0.0.88",
                item_id: "item-4",
            },
        )
        .await
        .unwrap();

        assert!(blocked);
        assert_eq!(
            state
                .playback_rate_ip_bans
                .lock()
                .await
                .get("10.0.0.88")
                .is_some(),
            true
        );
    }

    #[tokio::test]
    async fn concurrent_playback_limit_allows_same_video_refresh() {
        let store = test_store();
        store
            .save_setting_json(
                "client_control",
                &client_control::ClientControlConfig {
                    enabled: false,
                    notify_enabled: false,
                    playback_rate_limit_enabled: false,
                    playback_rate_limit_window_seconds: 30,
                    playback_rate_limit_max_requests: 20,
                    playback_rate_limit_block_seconds: 1800,
                    playback_rate_limit_action: "block_ip".to_string(),
                    concurrent_playback_limit_enabled: true,
                    concurrent_playback_limit_max: 3,
                    rate_limit_blocks: Vec::new(),
                    webhook: client_control::WebhookNotifyConfig::default(),
                    webhooks: Vec::new(),
                    records: Vec::new(),
                },
            )
            .unwrap();
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Sessions" {
                return response_json(serde_json::json!([
                    {"Id": "s1", "UserName": "test", "NowPlayingItem": {"Id": "item-1", "Name": "A"}},
                    {"Id": "s2", "UserName": "test", "NowPlayingItem": {"Id": "item-2", "Name": "B"}},
                    {"Id": "s3", "UserName": "test", "NowPlayingItem": {"Id": "item-3", "Name": "C"}}
                ]));
            }
            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let config = test_config(&emby, "http://127.0.0.1:5244", 8096);
        let state = test_state(config.clone(), store);

        let blocked = client_control::enforce_concurrent_playback_limit(
            &state,
            client_control::ConcurrentPlaybackLimitInput {
                runtime_config: &config,
                server_id: "default",
                server_name: "default",
                playback_user: "test",
                playback_ip: "10.0.0.89",
                item_id: "item-1",
            },
        )
        .await
        .unwrap();

        assert!(!blocked);
    }

    #[test]
    fn cache_domain_filters_match_host_only() {
        let mut config = test_config("http://emby.test", "http://openlist.test", 18096);
        config.cache_domain_filter_mode = "whitelist".to_string();
        config.cache_domain_whitelist = "*.115cdn.*".to_string();
        assert!(should_cache_direct_link(
            &config,
            "https://video.115cdn.com/path/file.mkv"
        ));
        assert!(!should_cache_direct_link(
            &config,
            "https://example.com/path/115cdn/file.mkv"
        ));

        config.cache_domain_filter_mode = "blacklist".to_string();
        config.cache_domain_whitelist = "sharepoint\n115".to_string();
        assert!(!should_cache_direct_link(
            &config,
            "https://hope.sharepoint.cn/download.aspx"
        ));
        assert!(should_cache_direct_link(
            &config,
            "https://cdn.example.com/a.mkv"
        ));
    }

    #[test]
    fn sanitized_detail_for_log_removes_direct_link_query() {
        assert_eq!(
            sanitized_detail_for_log(
                "https://cdn.example.test/movie.mkv?t=1780801682&u=2401236&k=secret"
            ),
            "https://cdn.example.test/movie.mkv?***"
        );
        assert_eq!(
            sanitized_detail_for_log(
                "alice https://cdn.example.test/movie.mkv?t=1780801682&u=2401236"
            ),
            "alice https://cdn.example.test/movie.mkv?***"
        );
    }

    #[test]
    fn sanitized_uri_for_log_masks_sensitive_query_keys() {
        let uri: Uri = "/videos/item1/stream?DeviceId=d1&api_key=secret&Static=true"
            .parse()
            .unwrap();
        assert_eq!(
            sanitized_uri_for_log(&uri),
            "/videos/item1/stream?DeviceId=d1&api_key=***&Static=true"
        );
    }

    #[test]
    fn proxy_base_url_uses_forwarded_headers_first() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "host",
            axum::http::HeaderValue::from_static("10.0.0.74:8096"),
        );
        headers.insert(
            "x-forwarded-host",
            axum::http::HeaderValue::from_static("media.example.com"),
        );
        headers.insert(
            "x-forwarded-proto",
            axum::http::HeaderValue::from_static("https"),
        );

        assert_eq!(proxy_base_url(&headers, 8096), "https://media.example.com");
    }

    #[test]
    fn proxy_base_url_falls_back_to_host_header() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "host",
            axum::http::HeaderValue::from_static("10.0.0.74:8096"),
        );

        assert_eq!(proxy_base_url(&headers, 8096), "http://10.0.0.74:8096");
    }

    #[tokio::test]
    async fn playback_info_rewrites_strm_sources() {
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items/item1/PlaybackInfo" {
                return response_json(json!({
                    "MediaSources": [{
                        "Id": "source1",
                        "Name": "OpenList",
                        "IsRemote": true,
                        "IsInfiniteStream": false,
                        "SupportsTranscoding": true,
                        "TranscodingUrl": "/transcode"
                    }]
                }));
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let openlist =
            spawn_mock_server(|_| async { response_text(StatusCode::NOT_FOUND, "not found") })
                .await;
        let app = test_app(&emby, &openlist, 18096);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/Items/item1/PlaybackInfo?DeviceId=d1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        let source = &body["MediaSources"][0];
        assert_eq!(source["SupportsDirectPlay"], true);
        assert_eq!(source["SupportsDirectStream"], true);
        assert_eq!(source["SupportsTranscoding"], false);
        assert!(source.get("TranscodingUrl").is_none());
        assert_eq!(
            source["DirectStreamUrl"].as_str(),
            Some("/videos/item1/stream?DeviceId=d1&MediaSourceId=source1&Static=true&api_key=")
        );
    }

    #[tokio::test]
    async fn video_stream_redirects_to_openlist_raw_url() {
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{
                        "MediaSources": [{
                            "Id": "source1",
                            "Path": "http://openlist.local/d/movie/%E6%B5%8B%E8%AF%95.mkv"
                        }]
                    }]
                }));
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let openlist = spawn_mock_server(|request| async move {
            if request.uri().path() == "/api/fs/get" {
                return response_json(json!({
                    "code": 200,
                    "data": { "raw_url": "https://cdn.example.test/movie.mkv" }
                }));
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let app = test_app(&emby, &openlist, 18096);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/videos/item1/stream?MediaSourceId=source1")
                    .header(header::USER_AGENT, "test-player")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "https://cdn.example.test/movie.mkv"
        );
    }

    #[tokio::test]
    async fn video_stream_redirects_direct_media_url_without_openlist() {
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{
                        "MediaSources": [{
                            "Id": "source1",
                            "Path": "https://cdn.example.test/direct.mkv"
                        }]
                    }]
                }));
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let app = test_app_without_openlist(&emby, 8096);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/videos/item1/stream?MediaSourceId=source1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "https://cdn.example.test/direct.mkv"
        );
    }

    #[tokio::test]
    async fn head_video_stream_resolves_direct_media_url() {
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{
                        "MediaSources": [{
                            "Id": "source1",
                            "Path": "https://cdn.example.test/direct.mkv"
                        }]
                    }]
                }));
            }

            if request.uri().path() == "/videos/item1/stream" {
                return response_text(StatusCode::OK, "emby stream");
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let app = test_app_without_openlist(&emby, 8096);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .method(Method::HEAD)
                    .uri("/videos/item1/stream?MediaSourceId=source1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "https://cdn.example.test/direct.mkv"
        );
    }

    #[tokio::test]
    async fn video_stream_redirects_openlist_download_url_without_openlist_api() {
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{
                        "MediaSources": [{
                            "Id": "source1",
                            "Path": "https://openlist.example.test/d/videos/movie.mkv"
                        }]
                    }]
                }));
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let app = test_app_without_openlist(&emby, 8096);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/emby/videos/item1/stream?MediaSourceId=source1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "https://openlist.example.test/d/videos/movie.mkv"
        );
    }

    #[tokio::test]
    async fn video_stream_applies_strm_url_mapping_before_redirect() {
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{
                        "MediaSources": [{
                            "Id": "source1",
                            "Path": "https://openlist.example.test/d/videos/movie.mkv"
                        }]
                    }]
                }));
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let app = test_app_without_openlist_with_mappings(
            &emby,
            8096,
            "https://openlist.example.test => http://localhost:5244",
        );

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/videos/item1/stream?MediaSourceId=source1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "http://localhost:5244/d/videos/movie.mkv"
        );
    }

    #[tokio::test]
    async fn video_stream_resolves_openlist_raw_url_when_openlist_api_is_configured() {
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{
                        "MediaSources": [{
                            "Id": "source1",
                            "Path": "https://openlist.example.test/d/videos/movie.mkv"
                        }]
                    }]
                }));
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let openlist = spawn_mock_server(|request| async move {
            if request.uri().path() == "/api/fs/get" {
                return response_json(json!({
                    "code": 200,
                    "data": { "raw_url": "https://cdn.example.test/movie.mkv" }
                }));
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let app = test_app(&emby, &openlist, 8096);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/emby/videos/item1/stream?MediaSourceId=source1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "https://cdn.example.test/movie.mkv"
        );
    }

    #[tokio::test]
    async fn internal_redirect_resolves_first_redirect_location() {
        let final_server = spawn_mock_server(|request| async move {
            if request.uri().path() == "/final.mkv" {
                return response_text(StatusCode::OK, "");
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let redirect_target = format!("{final_server}/final.mkv");
        let redirect_target_for_server = redirect_target.clone();
        let media_server = spawn_mock_server(move |request| {
            let redirect_target = redirect_target_for_server.clone();
            async move {
                if request.uri().path() == "/direct.mkv" {
                    return (
                        StatusCode::FOUND,
                        [(header::LOCATION, redirect_target.as_str())],
                    )
                        .into_response();
                }

                response_text(StatusCode::NOT_FOUND, "not found")
            }
        })
        .await;
        let media_url = format!("{media_server}/direct.mkv");
        let emby = spawn_mock_server(move |request| {
            let media_url = media_url.clone();
            async move {
                if request.uri().path() == "/Items" {
                    return response_json(json!({
                        "Items": [{
                            "MediaSources": [{
                                "Id": "source1",
                                "Path": media_url
                            }]
                        }]
                    }));
                }

                response_text(StatusCode::NOT_FOUND, "not found")
            }
        })
        .await;
        let app = test_app_without_openlist_with_internal_redirect(&emby, 8096, true);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/videos/item1/stream?MediaSourceId=source1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            redirect_target.as_str()
        );
    }

    #[tokio::test]
    async fn internal_redirect_uses_location_without_requesting_final_url() {
        let final_hits = Arc::new(AtomicUsize::new(0));
        let final_hits_for_server = final_hits.clone();
        let final_server = spawn_mock_server(move |request| {
            let final_hits = final_hits_for_server.clone();
            async move {
                if request.uri().path() == "/final.mkv" {
                    final_hits.fetch_add(1, Ordering::Relaxed);
                    return response_text(StatusCode::FORBIDDEN, "forbidden");
                }

                response_text(StatusCode::NOT_FOUND, "not found")
            }
        })
        .await;
        let redirect_target = format!("{final_server}/final.mkv");
        let redirect_target_for_server = redirect_target.clone();
        let media_server = spawn_mock_server(move |request| {
            let redirect_target = redirect_target_for_server.clone();
            async move {
                if request.uri().path() == "/direct.mkv" {
                    return (
                        StatusCode::FOUND,
                        [(header::LOCATION, redirect_target.as_str())],
                    )
                        .into_response();
                }

                response_text(StatusCode::NOT_FOUND, "not found")
            }
        })
        .await;
        let media_url = format!("{media_server}/direct.mkv");

        assert_eq!(
            internal_redirect::resolve_redirect_location(&media_url, 15, "test-player")
                .await
                .unwrap(),
            redirect_target
        );
        assert_eq!(final_hits.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn internal_redirect_forwards_playback_user_agent() {
        let seen_user_agent = Arc::new(std::sync::Mutex::new(String::new()));
        let seen_user_agent_for_server = seen_user_agent.clone();
        let media_server = spawn_mock_server(move |request| {
            let seen_user_agent = seen_user_agent_for_server.clone();
            async move {
                if request.uri().path() == "/direct.mkv" {
                    if let Some(user_agent) = request
                        .headers()
                        .get(header::USER_AGENT)
                        .and_then(|value| value.to_str().ok())
                    {
                        *seen_user_agent.lock().unwrap() = user_agent.to_string();
                    }
                    return (
                        StatusCode::FOUND,
                        [(header::LOCATION, "https://cdn.example.test/final.mkv")],
                    )
                        .into_response();
                }

                response_text(StatusCode::NOT_FOUND, "not found")
            }
        })
        .await;
        let media_url = format!("{media_server}/direct.mkv");

        let resolved =
            internal_redirect::resolve_redirect_location(&media_url, 15, "Emby Web Test/1.0")
                .await
                .unwrap();

        assert_eq!(resolved, "https://cdn.example.test/final.mkv");
        assert_eq!(
            seen_user_agent.lock().unwrap().as_str(),
            "Emby Web Test/1.0"
        );
    }

    #[tokio::test]
    async fn head_video_stream_resolves_internal_redirect_final_url() {
        let final_server = spawn_mock_server(|request| async move {
            if request.uri().path() == "/final.mkv" {
                return response_text(StatusCode::OK, "");
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let redirect_target = format!("{final_server}/final.mkv");
        let redirect_target_for_server = redirect_target.clone();
        let media_server = spawn_mock_server(move |request| {
            let redirect_target = redirect_target_for_server.clone();
            async move {
                if request.uri().path() == "/openlist/d/movie.mkv" {
                    return (
                        StatusCode::FOUND,
                        [(header::LOCATION, redirect_target.as_str())],
                        format!(r#"<a href="{redirect_target}">Found</a>."#),
                    )
                        .into_response();
                }

                response_text(StatusCode::NOT_FOUND, "not found")
            }
        })
        .await;
        let media_url = format!("{media_server}/openlist/d/movie.mkv");
        let emby = spawn_mock_server(move |request| {
            let media_url = media_url.clone();
            async move {
                if request.uri().path() == "/Items" {
                    return response_json(json!({
                        "Items": [{
                            "MediaSources": [{
                                "Id": "source1",
                                "Path": media_url
                            }]
                        }]
                    }));
                }

                if request.uri().path() == "/videos/item1/stream" {
                    return response_text(StatusCode::OK, "emby stream");
                }

                response_text(StatusCode::NOT_FOUND, "not found")
            }
        })
        .await;
        let app = test_app_without_openlist_with_internal_redirect(&emby, 8096, true);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .method(Method::HEAD)
                    .uri("/videos/item1/stream?MediaSourceId=source1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            redirect_target.as_str()
        );
    }

    #[tokio::test]
    async fn video_stream_returns_bad_gateway_when_openlist_fails() {
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{
                        "MediaSources": [{
                            "Id": "source1",
                            "Path": "http://openlist.local/d/movie/test.mkv"
                        }]
                    }]
                }));
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let openlist = spawn_mock_server(|request| async move {
            if request.uri().path() == "/api/fs/get" {
                return response_json(json!({
                    "code": 500,
                    "message": "failed"
                }));
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let app = test_app(&emby, &openlist, 18096);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/videos/item1/stream?MediaSourceId=source1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn video_stream_falls_back_to_emby_for_non_openlist_paths() {
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{
                        "MediaSources": [{
                            "Id": "source1",
                            "Path": "/media/movie/test.mkv"
                        }]
                    }]
                }));
            }

            if request.uri().path() == "/videos/item1/stream" {
                return response_text(StatusCode::OK, "emby stream");
            }

            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let openlist =
            spawn_mock_server(|_| async { response_text(StatusCode::NOT_FOUND, "not found") })
                .await;
        let app = test_app(&emby, &openlist, 18096);

        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/videos/item1/stream?MediaSourceId=source1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(read_text(response).await, "emby stream");
    }

    fn test_app(emby_host: &str, openlist_addr: &str, port: u16) -> Router {
        let config = Config {
            emby_host: emby_host.to_string(),
            emby_api_key: "emby-key".to_string(),
            servers: Vec::new(),
            openlist_addr: Some(openlist_addr.to_string()),
            openlist_token: Some("openlist-token".to_string()),
            port,
            cache_ttl_seconds: 180,
            cache_max_capacity: 10_000,
            cache_domain_filter_mode: "off".to_string(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect: false,
            internal_redirect_timeout_seconds: 15,
            strm_url_mappings: String::new(),
            connectivity_check_enabled: true,
            connectivity_check_interval_seconds: 60,
            connectivity_check_timeout_seconds: 5,
            connectivity_auto_restart_seconds: 180,
            strm_url_mapping_rules: Vec::new(),
        };
        build_app(test_state(config, test_store()))
    }

    fn test_app_without_openlist(emby_host: &str, port: u16) -> Router {
        let config = Config {
            emby_host: emby_host.to_string(),
            emby_api_key: "emby-key".to_string(),
            servers: Vec::new(),
            openlist_addr: None,
            openlist_token: None,
            port,
            cache_ttl_seconds: 180,
            cache_max_capacity: 10_000,
            cache_domain_filter_mode: "off".to_string(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect: false,
            internal_redirect_timeout_seconds: 15,
            strm_url_mappings: String::new(),
            connectivity_check_enabled: true,
            connectivity_check_interval_seconds: 60,
            connectivity_check_timeout_seconds: 5,
            connectivity_auto_restart_seconds: 180,
            strm_url_mapping_rules: Vec::new(),
        };
        build_app(test_state(config, test_store()))
    }

    fn test_app_without_openlist_with_internal_redirect(
        emby_host: &str,
        port: u16,
        enable_internal_redirect: bool,
    ) -> Router {
        let config = Config {
            emby_host: emby_host.to_string(),
            emby_api_key: "emby-key".to_string(),
            servers: Vec::new(),
            openlist_addr: None,
            openlist_token: None,
            port,
            cache_ttl_seconds: 180,
            cache_max_capacity: 10_000,
            cache_domain_filter_mode: "off".to_string(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect,
            internal_redirect_timeout_seconds: 15,
            strm_url_mappings: String::new(),
            connectivity_check_enabled: true,
            connectivity_check_interval_seconds: 60,
            connectivity_check_timeout_seconds: 5,
            connectivity_auto_restart_seconds: 180,
            strm_url_mapping_rules: Vec::new(),
        };
        build_app(test_state(config, test_store()))
    }

    fn test_app_without_openlist_with_mappings(
        emby_host: &str,
        port: u16,
        mappings: &str,
    ) -> Router {
        let config = Config {
            emby_host: emby_host.to_string(),
            emby_api_key: "emby-key".to_string(),
            servers: Vec::new(),
            openlist_addr: None,
            openlist_token: None,
            port,
            cache_ttl_seconds: 180,
            cache_max_capacity: 10_000,
            cache_domain_filter_mode: "off".to_string(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect: false,
            internal_redirect_timeout_seconds: 15,
            strm_url_mappings: mappings.to_string(),
            connectivity_check_enabled: true,
            connectivity_check_interval_seconds: 60,
            connectivity_check_timeout_seconds: 5,
            connectivity_auto_restart_seconds: 180,
            strm_url_mapping_rules: url_mapping::parse_rules(mappings).unwrap(),
        };
        build_app(test_state(config, test_store()))
    }

    fn test_store() -> SettingsStore {
        let path = std::env::temp_dir().join(format!(
            "embypanel-test-{}-{}.db",
            uuid_like_timestamp(),
            TEST_DB_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let store = SettingsStore::open_for_test(path).unwrap();
        store.create_admin("admin", "test-password-hash").unwrap();
        store
    }

    fn uuid_like_timestamp() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    #[test]
    fn log_timer_respects_common_tz_values() {
        assert_eq!(tz_offset_seconds("Asia/Shanghai"), 8 * 3600);
        assert_eq!(tz_offset_seconds("UTC+08:00"), 8 * 3600);
        assert_eq!(tz_offset_seconds("UTC"), 0);
    }

    async fn spawn_mock_server<F, Fut>(handler: F) -> String
    where
        F: Fn(Request) -> Fut + Clone + Send + Sync + 'static,
        Fut: Future<Output = Response> + Send + 'static,
    {
        let app = Router::new().fallback(any(handler));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    fn response_json(value: serde_json::Value) -> Response {
        (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "application/json")],
            serde_json::to_string(&value).unwrap(),
        )
            .into_response()
    }

    fn response_text(status: StatusCode, text: &'static str) -> Response {
        (status, text).into_response()
    }

    async fn read_json(response: Response) -> serde_json::Value {
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
    }

    async fn read_text(response: Response) -> String {
        String::from_utf8(
            to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap()
                .to_vec(),
        )
        .unwrap()
    }
}
