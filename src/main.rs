mod activity_log;
mod alerts;
mod alerts_api;
mod alerts_runtime;
mod analytics_api;
mod auth;
mod block_log;
mod cache;
mod cache_api;
mod client_control;
mod config;
mod connectivity;
mod crypto_api;
mod db;
mod diagnostics;
mod diagnostics_api;
mod emby;
mod error;
mod file_log;
mod internal_redirect;
mod ip_location;
mod library_tasks_api;
mod monitoring;
mod openlist;
mod proxy;
mod rewrite;
mod sessions_api;
mod settings_api;
mod url_mapping;
mod users_api;

use std::{
    collections::{HashMap, VecDeque},
    env, fmt,
    net::{IpAddr, SocketAddr},
    sync::{
        Arc, Mutex as StdMutex, RwLock as StdRwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Query, Request, State},
    http::{Method, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, delete, get, patch, post, put},
};
use bytes::Bytes;
use config::Config;
use crypto_api::CryptoKeys;
use db::{ProxyRequestDetailWrite, RequestStatKind, SettingsStore, TelemetryWriteQueue};
use error::{AppError, AppResult, safe_error_message};
use serde::Serialize;
use tokio::{
    sync::{Mutex, RwLock, Semaphore, oneshot},
    task::{JoinHandle, JoinSet},
};
use tower_http::services::{ServeDir, ServeFile};
use tracing_subscriber::{
    EnvFilter,
    fmt::{format::Writer, time::FormatTime},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

use crate::cache::{DirectLinkCache, DirectLinkCachePolicy};
use activity_log::{ActivityKind, ActivityLevel, ActivityLogStore, PlaybackLogRecord};
use alerts::Alerts;
use alerts_runtime::AlertRuntime;
use block_log::BlockLogStore;
use file_log::FileLogStore;

const MAX_PROXY_BODY_BYTES: usize = 64 * 1024 * 1024;
const MAX_BACKUP_IMPORT_REQUEST_BYTES: usize = 32 * 1024 * 1024;
const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_POOL_MAX_IDLE_PER_HOST: usize = 16;
const PROXY_RESPONSE_BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const PROXY_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const PROXY_REBIND_TIMEOUT: Duration = Duration::from_secs(2);
const PASSWORD_TASK_CONCURRENCY_LIMIT: usize = 4;
const USER_PASSWORD_RESET_CONCURRENCY_LIMIT: usize = 2;
const PLAYBACK_PATH_STATE_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const PLAYBACK_PATH_STATE_CAPACITY: usize = 256;
const PLAYBACK_USER_CACHE_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const PLAYBACK_USER_CACHE_CAPACITY: u64 = 1024;
pub(crate) const PLAYBACK_MODE_DIRECT_LINK: &str = "direct_link";
pub(crate) const PLAYBACK_MODE_SERVER_PROXY: &str = "server_proxy";
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
    config_updates: Arc<Mutex<()>>,
    client: reqwest::Client,
    proxy_client: reqwest::Client,
    cache: Arc<RwLock<DirectLinkCache>>,
    settings_store: SettingsStore,
    telemetry_writes: TelemetryWriteQueue,
    alerts: Alerts,
    alert_runtime: AlertRuntime,
    client_control_cache: Arc<StdRwLock<Option<(u64, client_control::ClientControlConfig)>>>,
    user_policies_cache: Arc<StdRwLock<Option<(u64, users_api::UserPoliciesDocument)>>>,
    rate_limit_updates: Arc<Mutex<()>>,
    playback_sessions_cache: Arc<Mutex<HashMap<String, (Instant, Vec<emby::PlaybackSession>)>>>,
    password_tasks: Arc<Semaphore>,
    user_password_tasks: Arc<Semaphore>,
    crypto_keys: CryptoKeys,
    activity_log: Arc<ActivityLogStore>,
    file_log: Arc<FileLogStore>,
    block_log: Arc<BlockLogStore>,
    ip_location: Arc<ip_location::IpLocationStore>,
    connectivity: Arc<connectivity::ConnectivityMonitor>,
    proxy_manager: Option<Arc<ProxyManager>>,
    proxy_server_id: Option<String>,
    proxy_metrics: Option<Arc<ProxyRuntimeMetrics>>,
    playback_paths: Arc<Mutex<PlaybackPathState>>,
    playback_users: moka::future::Cache<String, String>,
    playback_rate_hits: Arc<Mutex<HashMap<String, VecDeque<client_control::PlaybackRateHit>>>>,
    playback_rate_recent_events: Arc<Mutex<HashMap<String, u64>>>,
    playback_rate_bans: Arc<Mutex<HashMap<String, u64>>>,
    playback_rate_ip_bans: Arc<Mutex<HashMap<String, u64>>>,
    login_failures: Arc<Mutex<HashMap<String, VecDeque<i64>>>>,
    update_check: Arc<Mutex<Option<(u128, UpdateCheckResponse)>>>,
}

#[derive(Debug, Clone, Eq, Hash, PartialEq)]
struct PlaybackPathKey {
    server_id: String,
    item_id: String,
    device_id: Option<String>,
    media_source_id: Option<String>,
}

#[derive(Debug, Clone)]
struct PlaybackPathEntry {
    mode: &'static str,
    recorded_at: Instant,
    sequence: u64,
}

#[derive(Debug, Default)]
struct PlaybackPathState {
    entries: HashMap<PlaybackPathKey, PlaybackPathEntry>,
    next_sequence: u64,
}

impl PlaybackPathState {
    fn record(
        &mut self,
        server_id: &str,
        item_id: &str,
        device_id: Option<&str>,
        media_source_id: Option<&str>,
        mode: &'static str,
    ) {
        let now = Instant::now();
        self.prune_expired(now);
        let key = playback_path_key(server_id, item_id, device_id, media_source_id);
        if !self.entries.contains_key(&key) && self.entries.len() >= PLAYBACK_PATH_STATE_CAPACITY {
            if let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.sequence)
                .map(|(key, _)| key.clone())
            {
                self.entries.remove(&oldest);
            }
        }
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.entries.insert(
            key,
            PlaybackPathEntry {
                mode,
                recorded_at: now,
                sequence: self.next_sequence,
            },
        );
    }

    fn lookup(
        &mut self,
        server_id: &str,
        item_id: &str,
        device_id: Option<&str>,
        media_source_id: Option<&str>,
    ) -> Option<&'static str> {
        let now = Instant::now();
        self.prune_expired(now);
        let exact_key = playback_path_key(server_id, item_id, device_id, media_source_id);
        if self.entries.contains_key(&exact_key) {
            return self.touch(&exact_key, now);
        }

        let mut mode = None;
        let mut latest = None;
        for (key, entry) in &self.entries {
            if key.server_id != server_id || key.item_id != item_id {
                continue;
            }
            if device_id.is_some() && key.device_id.as_deref() != device_id {
                continue;
            }
            if media_source_id.is_some() && key.media_source_id.as_deref() != media_source_id {
                continue;
            }
            if mode.is_some_and(|candidate| candidate != entry.mode) {
                return None;
            }
            mode = Some(entry.mode);
            if latest
                .as_ref()
                .is_none_or(|(_, sequence)| entry.sequence > *sequence)
            {
                latest = Some((key.clone(), entry.sequence));
            }
        }

        self.touch(&latest?.0, now)
    }

    fn touch(&mut self, key: &PlaybackPathKey, now: Instant) -> Option<&'static str> {
        let next_sequence = self.next_sequence.wrapping_add(1);
        let entry = self.entries.get_mut(key)?;
        self.next_sequence = next_sequence;
        entry.recorded_at = now;
        entry.sequence = next_sequence;
        Some(entry.mode)
    }

    fn prune_expired(&mut self, now: Instant) {
        self.entries
            .retain(|_, entry| now.duration_since(entry.recorded_at) <= PLAYBACK_PATH_STATE_TTL);
    }
}

fn playback_path_key(
    server_id: &str,
    item_id: &str,
    device_id: Option<&str>,
    media_source_id: Option<&str>,
) -> PlaybackPathKey {
    PlaybackPathKey {
        server_id: server_id.to_string(),
        item_id: item_id.to_string(),
        device_id: device_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        media_source_id: media_source_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    }
}

pub(crate) struct ProxyManager {
    running: Mutex<HashMap<String, ProxyTask>>,
    draining: Mutex<JoinSet<()>>,
    lifecycle: Mutex<()>,
}

struct ProxyTask {
    port: u16,
    server_name: String,
    started_at_ms: u128,
    metrics: Arc<ProxyRuntimeMetrics>,
    shutdown: oneshot::Sender<()>,
    handle: JoinHandle<()>,
}

#[derive(Default)]
struct ProxyRuntimeMetrics {
    last_request_ms: AtomicU64,
    last_error: StdMutex<Option<String>>,
}

impl ProxyRuntimeMetrics {
    fn record_request(&self) {
        self.last_request_ms
            .store(now_ms().min(u128::from(u64::MAX)) as u64, Ordering::Relaxed);
    }

    fn record_error(&self, error: String) {
        *self
            .last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(error);
    }

    fn last_request_ms(&self) -> Option<u128> {
        let value = self.last_request_ms.load(Ordering::Relaxed);
        (value != 0).then_some(u128::from(value))
    }

    fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
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
            draining: Mutex::new(JoinSet::new()),
            lifecycle: Mutex::new(()),
        }
    }

    pub(crate) async fn ensure_running(&self, state: AppState) -> AppResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
        let servers = state.config.read().await.proxy_configs();
        let desired_ids = servers
            .iter()
            .filter_map(|server| server.servers.first().map(|entry| entry.id.clone()))
            .collect::<std::collections::HashSet<_>>();
        let desired_ports = servers
            .iter()
            .filter_map(|config| {
                config
                    .servers
                    .first()
                    .map(|server| (server.id.clone(), config.port))
            })
            .collect::<HashMap<_, _>>();
        let tasks_to_drain = {
            let mut running = self.running.lock().await;
            let stale_ids = running
                .iter()
                .filter_map(|(id, task)| {
                    let desired_port = desired_ports.get(id);
                    (!desired_ids.contains(id)
                        || desired_port.is_some_and(|port| *port != task.port)
                        || task.handle.is_finished())
                    .then_some(id.clone())
                })
                .collect::<Vec<_>>();
            stale_ids
                .into_iter()
                .filter_map(|id| running.remove(&id).map(|task| (id, task)))
                .collect::<Vec<_>>()
        };
        for (server_id, task) in tasks_to_drain {
            self.begin_proxy_drain(server_id, task).await;
        }

        let mut first_error = None;
        for config in servers {
            let Some(server) = config.servers.first() else {
                continue;
            };
            let server_id = server.id.clone();
            let (already_running, finished_task) = {
                let mut running = self.running.lock().await;
                if running
                    .get(&server_id)
                    .is_some_and(|task| task.handle.is_finished())
                {
                    (false, running.remove(&server_id))
                } else if let Some(task) = running.get_mut(&server_id) {
                    task.server_name = server.name.clone();
                    (true, None)
                } else {
                    (false, None)
                }
            };
            if let Some(task) = finished_task {
                self.begin_proxy_drain(server_id.clone(), task).await;
            }
            if already_running {
                continue;
            }

            if let Err(err) = self.start_server(state.clone(), server).await {
                tracing::error!(
                    server_id,
                    port = server.port,
                    error = %err.safe_log_message(),
                    "failed to start Emby proxy"
                );
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }

        if let Some(err) = first_error {
            return Err(err);
        }
        Ok(())
    }

    pub(crate) async fn restart_server(&self, state: AppState, server_id: &str) -> AppResult<()> {
        let _lifecycle = self.lifecycle.lock().await;
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
            self.begin_proxy_drain(server_id.to_string(), task).await;
        }

        let Some(server) = config.servers.first() else {
            return Err(AppError::Config(format!("server {server_id} is invalid")));
        };
        self.start_server(state, server).await
    }

    async fn start_server(
        &self,
        state: AppState,
        server: &config::EmbyServerConfig,
    ) -> AppResult<()> {
        let port = server.port;
        let listener = bind_proxy_listener(port).await?;
        let mut proxy_state = state.clone();
        proxy_state.proxy_server_id = Some(server.id.clone());
        let metrics = Arc::new(ProxyRuntimeMetrics::default());
        proxy_state.proxy_metrics = Some(metrics.clone());
        let (shutdown, receiver) = oneshot::channel();
        let handle = tokio::spawn(run_proxy_server(proxy_state, listener, receiver));
        self.running.lock().await.insert(
            server.id.clone(),
            ProxyTask {
                port,
                server_name: server.name.clone(),
                started_at_ms: now_ms(),
                metrics,
                shutdown,
                handle,
            },
        );
        Ok(())
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
                    listening: task
                        .is_some_and(|task| task.port == server.port && !task.handle.is_finished()),
                    started_at_ms: task.map(|task| task.started_at_ms),
                    last_request_ms: task.and_then(|task| task.metrics.last_request_ms()),
                    last_error: task.and_then(|task| task.metrics.last_error()),
                }
            })
            .collect()
    }

    async fn shutdown(&self) {
        let _lifecycle = self.lifecycle.lock().await;
        let tasks = self.running.lock().await.drain().collect::<Vec<_>>();
        let mut drains = self.draining.lock().await;
        reap_completed_proxy_drains(&mut drains);
        for (server_id, task) in tasks {
            let _ = task.shutdown.send(());
            drains.spawn(wait_for_proxy_drain(server_id, task.port, task.handle));
        }
        // A deadline belongs to process shutdown, not to a live config reload.
        let drained = tokio::time::timeout(PROXY_DRAIN_TIMEOUT, async {
            while let Some(result) = drains.join_next().await {
                if let Err(err) = result {
                    tracing::warn!(error = %err, "proxy drain task failed");
                }
            }
        })
        .await;
        if drained.is_err() {
            tracing::warn!("proxy shutdown drain timed out");
            drains.abort_all();
            while drains.join_next().await.is_some() {}
        }
    }

    async fn begin_proxy_drain(&self, server_id: String, task: ProxyTask) {
        let _ = task.shutdown.send(());
        let mut drains = self.draining.lock().await;
        reap_completed_proxy_drains(&mut drains);
        drains.spawn(wait_for_proxy_drain(server_id, task.port, task.handle));
    }
}

async fn bind_proxy_listener(port: u16) -> AppResult<tokio::net::TcpListener> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    let started = Instant::now();
    loop {
        match tokio::net::TcpListener::bind(addr).await {
            Ok(listener) => return Ok(listener),
            Err(err)
                if err.kind() == std::io::ErrorKind::AddrInUse
                    && started.elapsed() < PROXY_REBIND_TIMEOUT =>
            {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            Err(err) => {
                return Err(AppError::Config(format!(
                    "failed to bind Emby proxy port {port}: {err}"
                )));
            }
        }
    }
}

fn reap_completed_proxy_drains(drains: &mut JoinSet<()>) {
    while let Some(result) = drains.try_join_next() {
        if let Err(err) = result {
            tracing::warn!(error = %err, "proxy drain task failed");
        }
    }
}

fn direct_link_cache_policy(config: &Config) -> DirectLinkCachePolicy {
    DirectLinkCachePolicy::new(
        config.cache_enabled,
        config.cache_ttl_seconds,
        config.cache_max_capacity,
    )
}

pub(crate) fn direct_link_cache_from_config(config: &Config) -> DirectLinkCache {
    let default_policy = direct_link_cache_policy(config);
    let server_policies = config.servers.iter().filter_map(|server| {
        let server_id = server.id.trim();
        if server_id.is_empty() {
            return None;
        }
        let effective_config = config.for_server_for_validation(server);
        Some((
            server_id.to_string(),
            direct_link_cache_policy(&effective_config),
        ))
    });
    DirectLinkCache::new_with_server_policies(default_policy, server_policies)
}

fn direct_link_cache_is_effective(config: &Config) -> bool {
    config.cache_enabled && config.cache_ttl_seconds > 0 && config.cache_max_capacity > 0
}

struct DrainingProxyTask(JoinHandle<()>);

impl Drop for DrainingProxyTask {
    fn drop(&mut self) {
        // JoinHandle normally detaches on drop; canceled drain supervisors must
        // not leave their server tasks detached during process shutdown.
        self.0.abort();
    }
}

async fn wait_for_proxy_drain(server_id: String, port: u16, handle: JoinHandle<()>) {
    let mut task = DrainingProxyTask(handle);
    match (&mut task.0).await {
        Ok(()) => {}
        Err(err) if err.is_cancelled() => {}
        Err(err) => {
            tracing::warn!(server_id, port, error = %err, "proxy task exited during drain");
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

    diagnostics::init_diagnostics();
    let settings_store = SettingsStore::open_default()?;
    let config = settings_store.load_or_default_config()?;
    let file_log = Arc::new(FileLogStore::new(&settings_store));
    let client = build_http_client()?;
    let proxy_client = build_proxy_http_client()?;
    let block_log = Arc::new(BlockLogStore::new());
    block_log.bootstrap_from_proxy_logs(&settings_store);
    let ip_location = Arc::new(ip_location::IpLocationStore::new());
    let cache = direct_link_cache_from_config(&config);
    let proxy_manager = Arc::new(ProxyManager::new());
    let telemetry_writes = TelemetryWriteQueue::start(settings_store.clone())?;
    diagnostics::enable_persistence(telemetry_writes.clone());
    let alerts = Alerts::new(db::database_path())?;
    let alert_runtime = AlertRuntime::start(alerts.clone(), client.clone());
    let state = AppState {
        config: Arc::new(RwLock::new(config)),
        config_updates: Arc::new(Mutex::new(())),
        client,
        proxy_client,
        cache: Arc::new(RwLock::new(cache)),
        settings_store,
        telemetry_writes,
        alerts,
        alert_runtime,
        client_control_cache: Arc::new(StdRwLock::new(None)),
        user_policies_cache: Arc::new(StdRwLock::new(None)),
        rate_limit_updates: Arc::new(Mutex::new(())),
        playback_sessions_cache: Arc::new(Mutex::new(HashMap::new())),
        password_tasks: Arc::new(Semaphore::new(PASSWORD_TASK_CONCURRENCY_LIMIT)),
        user_password_tasks: Arc::new(Semaphore::new(USER_PASSWORD_RESET_CONCURRENCY_LIMIT)),
        crypto_keys: CryptoKeys::generate()?,
        activity_log: Arc::new(ActivityLogStore::new(800)),
        file_log,
        block_log,
        ip_location,
        connectivity: Arc::new(connectivity::ConnectivityMonitor::new()),
        proxy_manager: Some(proxy_manager.clone()),
        proxy_server_id: None,
        proxy_metrics: None,
        playback_paths: Arc::new(Mutex::new(PlaybackPathState::default())),
        playback_users: playback_user_cache(),
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

    let listen_addr = management_listen_addr()?;
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

    let ip_location_task = {
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(err) = state.ip_location.initialize(&state.client).await {
                tracing::warn!(error = %err, "failed to initialize IP database");
                state
                    .file_log
                    .write("warning", "IP 数据库初始化失败", &err.safe_log_message());
            }
        })
    };

    if state.settings_store.has_admin()? && !state.config.read().await.proxy_configs().is_empty() {
        if let Err(err) = proxy_manager.ensure_running(state.clone()).await {
            let detail = err.safe_log_message();
            tracing::error!(error = %detail, "one or more Emby proxy listeners failed to start");
            state.activity_log.record(
                ActivityKind::General,
                ActivityLevel::Error,
                None,
                PROJECT_NAME,
                "部分反代监听器启动失败",
                &detail,
            );
            state
                .file_log
                .write("error", "部分反代监听器启动失败", &detail);
        }
    }
    let connectivity_task = connectivity::start_connectivity_monitor(state.clone());

    let app = build_management_app(state.clone());
    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await
    .map_err(|err| AppError::Internal(format!("server error: {err}")));
    ip_location_task.abort();
    let _ = ip_location_task.await;
    connectivity_task.abort();
    let _ = connectivity_task.await;
    proxy_manager.shutdown().await;
    state.alert_runtime.shutdown().await;
    let telemetry_writes = state.telemetry_writes.clone();
    let telemetry_result = tokio::task::spawn_blocking(move || telemetry_writes.shutdown())
        .await
        .map_err(|err| AppError::Internal(format!("telemetry shutdown task failed: {err}")))
        .and_then(|result| result);
    if let Err(err) = &telemetry_result {
        tracing::error!(error = %err.safe_log_message(), "failed to flush telemetry during shutdown");
    }
    result.and(telemetry_result)
}

async fn healthcheck_command() -> AppResult<()> {
    let addr = management_listen_addr()?;
    let url = format!("http://127.0.0.1:{}/healthz", addr.port());
    let response = diagnostics::network::send(build_http_client()?.get(url)).await?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(AppError::Internal(format!(
            "healthcheck failed with status {}",
            response.status()
        )))
    }
}

fn build_http_client() -> AppResult<reqwest::Client> {
    diagnostics::network::instrument_client(reqwest::Client::builder())
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .timeout(HTTP_REQUEST_TIMEOUT)
        .pool_max_idle_per_host(HTTP_POOL_MAX_IDLE_PER_HOST)
        .tcp_nodelay(true)
        .build()
        .map_err(Into::into)
}

fn build_proxy_http_client() -> AppResult<reqwest::Client> {
    diagnostics::network::instrument_client(reqwest::Client::builder())
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .read_timeout(PROXY_RESPONSE_BODY_IDLE_TIMEOUT)
        .pool_max_idle_per_host(HTTP_POOL_MAX_IDLE_PER_HOST)
        .tcp_nodelay(true)
        .build()
        .map_err(Into::into)
}

fn playback_user_cache() -> moka::future::Cache<String, String> {
    moka::future::Cache::builder()
        .max_capacity(PLAYBACK_USER_CACHE_CAPACITY)
        .time_to_idle(PLAYBACK_USER_CACHE_TTL)
        .build()
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
    let request_id = diagnostics::new_request_id();
    let span = tracing::info_span!("proxy_request", request_id = %request_id);
    tracing::Instrument::instrument(
        diagnostics::REQUEST_ID.scope(request_id, handle_request_scoped(state, peer_addr, request)),
        span,
    )
    .await
}

async fn handle_request_scoped(
    state: AppState,
    peer_addr: SocketAddr,
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
    let path = uri.path();

    if !state.settings_store.has_admin()? {
        return Ok(proxy::redirect_response("/ui/".to_string()));
    }
    let request_config = proxy_config(&state).await?;

    if should_block_web_ui(&request_config, path)? {
        record_incoming_proxy_request(&state, &request_config);
        let client_ip = proxy::real_ip_for_log(&request_config, &headers, peer_addr.ip());
        record_proxy_request(
            &state,
            ActivityLevel::Warn,
            "Emby Web UI 已拦截",
            format!("{} {}", method, path),
        )
        .await;
        record_stat(&state, &request_config, RequestStatKind::Block);
        record_proxy_request_detail(
            &state,
            &request_config,
            ProxyRequestDetailInput {
                method: method.as_str(),
                uri: &uri,
                path_type: "proxy",
                status_code: request_config
                    .servers
                    .first()
                    .map(|server| server.block_web_ui_status_code)
                    .unwrap_or(403),
                outcome: "Web UI 拦截",
                started_at_ms: timing.started_at_ms,
                duration_ms: timing.started.elapsed().as_millis(),
                playback_user: "--",
                playback_ip: &client_ip,
                cache_hit: false,
                blocked: true,
                detail: "Emby Web UI",
            },
        )
        .await;
        let block_status_code = request_config
            .servers
            .first()
            .map(|server| server.block_web_ui_status_code)
            .unwrap_or(403);
        let block_message = request_config
            .servers
            .first()
            .map(|server| server.block_web_ui_message.clone())
            .unwrap_or_else(|| "Emby Web UI is disabled by EmbyPanel".to_string());
        let status = StatusCode::from_u16(block_status_code)
            .map_err(|_| AppError::Config("invalid Emby Web UI block status code".to_string()))?;
        return Ok((status, block_message).into_response());
    }

    let body = if method == Method::GET || method == Method::HEAD {
        Bytes::new()
    } else {
        axum::body::to_bytes(body, MAX_PROXY_BODY_BYTES)
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
            })?
    };
    record_incoming_proxy_request(&state, &request_config);

    let client_control_path = rewrite::is_playback_info(path) || rewrite::is_video_stream(path);
    if client_control_path {
        if let Err(err) = client_control::record_from_headers(&state, &headers, None) {
            tracing::warn!(error = %err, "failed to record client control event");
        }
        if let Some(rule) = client_control::matched_block_rule(&state, &headers)? {
            let (server_id, server_name) = config_server_label(&request_config);
            let playback_ip = proxy::real_ip_for_log(&request_config, &headers, peer_addr.ip());
            let mut diagnosis = if rewrite::is_video_stream(path) {
                rewrite::parse_item_id(path).map(|item_id| {
                    let ua = header_value(&headers, "User-Agent").unwrap_or_default();
                    let mut trace = proxy::PlaybackDiagnosisTrace::new(&server_id, &item_id, &ua);
                    trace.context = playback_diagnosis_context(
                        &server_name,
                        &method,
                        &uri,
                        &headers,
                        &playback_ip,
                    );
                    // Rule metadata describes an earlier observation, not this request's user.
                    trace.context.user_name = query_param(uri.query().unwrap_or(""), "UserName")
                        .filter(|name| !name.trim().is_empty() && name != "--");
                    attach_diagnosis_failure_log(&mut trace, &state, &request_config, timing);
                    trace.begin_step(proxy::STEP_POLICY, "client policy block notification");
                    trace
                })
            } else {
                None
            };
            let mut untraced_observation = diagnostics::UpstreamObservation::default();
            let observation = diagnosis
                .as_mut()
                .map(|trace| &mut trace.observation)
                .unwrap_or(&mut untraced_observation);
            diagnostics::network::observe_operation(
                observation,
                client_control::notify_client_rule_hit(
                    &state,
                    &rule,
                    &server_id,
                    &server_name,
                    &playback_ip,
                    method.as_str(),
                    path,
                ),
            )
            .await;
            record_proxy_request(
                &state,
                ActivityLevel::Warn,
                "客户端播放已拦截",
                format!("{} {} 命中 UA 拦截规则", method, path),
            )
            .await;
            record_stat(&state, &request_config, RequestStatKind::Block);
            if let Some(mut trace) = diagnosis.take() {
                trace.complete_step("client policy notification completed");
                trace.result = diagnostics::DiagnosisResult {
                    outcome: Some("blocked".to_string()),
                    status_code: Some(403),
                    reason: Some("client policy blocked request".to_string()),
                    ..Default::default()
                };
                trace.finish().await;
            }
            record_proxy_request_detail(
                &state,
                &request_config,
                ProxyRequestDetailInput {
                    method: method.as_str(),
                    uri: &uri,
                    path_type: request_path_type(path),
                    status_code: StatusCode::FORBIDDEN.as_u16(),
                    outcome: "UA 拦截",
                    started_at_ms: timing.started_at_ms,
                    duration_ms: timing.started.elapsed().as_millis(),
                    playback_user: query_param(uri.query().unwrap_or(""), "UserName")
                        .as_deref()
                        .unwrap_or("--"),
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
        let result = handle_base_html_player(
            &state,
            &request_config,
            method.clone(),
            uri.clone(),
            headers,
            body,
            peer_addr.ip(),
        )
        .await;
        if let Ok(response) = &result {
            let status = response.status();
            record_proxy_request_detail(
                &state,
                &request_config,
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
        let result = handle_system_info(
            &state,
            &request_config,
            method.clone(),
            uri.clone(),
            headers,
            body,
            peer_addr.ip(),
        )
        .await;
        if let Ok(response) = &result {
            let status = response.status();
            record_proxy_request_detail(
                &state,
                &request_config,
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
        return handle_playback_info(
            &state,
            &request_config,
            method,
            uri,
            headers,
            body,
            peer_addr,
            timing,
        )
        .await;
    }

    if rewrite::is_video_stream(path) {
        return handle_video_stream(
            &state,
            &request_config,
            method,
            uri,
            headers,
            body,
            peer_addr,
            timing,
        )
        .await;
    }

    proxy_to_emby_recorded(
        &state,
        &request_config,
        method,
        &uri,
        &headers,
        body,
        peer_addr.ip(),
        Some(ProxyRequestTrace {
            started_at_ms: timing.started_at_ms,
            started: timing.started,
            path_type: request_path_type(path),
            playback_user: "--".to_string(),
            playback_ip: proxy::real_ip_for_log(&request_config, &headers, peer_addr.ip()),
        }),
        None,
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
        .route("/api/logout", axum::routing::post(auth::logout))
        .route(
            "/api/change-password",
            axum::routing::post(auth::change_password),
        )
        .route(
            "/api/profile",
            get(auth::get_profile).put(auth::update_profile),
        )
        .route(
            "/api/admins",
            get(settings_api::list_admins).post(settings_api::create_admin),
        )
        .route(
            "/api/admins/{admin_user_id}/roles",
            put(settings_api::update_admin_roles),
        )
        .route(
            "/api/admins/{admin_user_id}/server-grants",
            put(settings_api::update_admin_server_grants),
        )
        .route(
            "/api/admins/{admin_user_id}/status",
            patch(settings_api::set_admin_status),
        )
        .route(
            "/api/admins/{admin_user_id}/password",
            post(settings_api::reset_admin_password),
        )
        .route(
            "/api/admins/{admin_user_id}",
            delete(settings_api::delete_admin),
        )
        .route("/api/admin/tokens", get(settings_api::list_admin_tokens))
        .route("/api/admin/token", post(settings_api::create_admin_token))
        .route(
            "/api/admin/token/{token_id}",
            delete(settings_api::revoke_admin_token),
        )
        .route(
            "/api/admin/sessions",
            get(settings_api::list_admin_sessions),
        )
        .route(
            "/api/admin/session/{session_id}",
            delete(settings_api::revoke_admin_session),
        )
        .route(
            "/api/admin/2fa/status",
            get(settings_api::get_admin_2fa_status),
        )
        .route("/api/admin/2fa/setup", post(settings_api::setup_admin_2fa))
        .route(
            "/api/admin/2fa/confirm",
            post(settings_api::confirm_admin_2fa),
        )
        .route(
            "/api/admin/2fa/disable",
            post(settings_api::disable_admin_2fa),
        )
        .route(
            "/api/admin/2fa/recovery",
            post(settings_api::regenerate_admin_recovery_codes),
        )
        .route(
            "/api/admin/2fa/rollback",
            post(settings_api::rollback_admin_2fa_setup),
        )
        .route("/api/users", get(users_api::list_users))
        .route(
            "/api/users/{server_id}",
            axum::routing::post(users_api::create_user),
        )
        .route(
            "/api/users/{server_id}/{user_id}",
            get(users_api::get_user).delete(users_api::delete_user),
        )
        .route(
            "/api/users/{server_id}/{user_id}/policy",
            axum::routing::put(users_api::update_user_policy),
        )
        .route(
            "/api/users/{server_id}/{user_id}/password",
            axum::routing::post(users_api::reset_user_password),
        )
        .route("/api/user-templates", get(users_api::list_user_templates))
        .route(
            "/api/user-templates/{server_id}",
            axum::routing::post(users_api::save_user_template),
        )
        .route(
            "/api/user-templates/{server_id}/{template_id}",
            axum::routing::delete(users_api::delete_user_template),
        )
        .route(
            "/api/monitoring/plays",
            get(monitoring::list_playback_sessions),
        )
        .route(
            "/api/monitoring/playback-image",
            get(monitoring::playback_image),
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
            "/api/settings/servers/{server_id}/api-key",
            axum::routing::post(settings_api::reveal_emby_api_key),
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
            "/api/library/libraries",
            get(library_tasks_api::list_library_libraries),
        )
        .route(
            "/api/library/libraries/validate",
            post(library_tasks_api::validate_library_paths),
        )
        .route(
            "/api/library/tasks",
            get(library_tasks_api::list_library_tasks),
        )
        .route(
            "/api/library/tasks/{task_id}/failure",
            get(library_tasks_api::get_library_task_failure),
        )
        .route(
            "/api/library/tasks/{task_id}/start",
            post(library_tasks_api::start_library_task),
        )
        .route(
            "/api/library/tasks/{task_id}/cancel",
            post(library_tasks_api::cancel_library_task),
        )
        .route(
            "/api/sessions/{session_id}/stop",
            post(sessions_api::stop_session),
        )
        .route(
            "/api/sessions/{session_id}/message",
            post(sessions_api::send_session_message),
        )
        .route("/api/history", get(analytics_api::get_history))
        .route("/api/servers/options", get(monitoring::server_options))
        .route("/api/cache/stats", get(cache_api::get_cache_stats))
        .route("/api/cache/entries", get(cache_api::list_cache_entries))
        .route("/api/cache/cleanup", post(cache_api::cleanup_cache))
        .route("/api/diagnostics", get(diagnostics_api::get_diagnostics))
        .route(
            "/api/diagnostics/clean",
            post(diagnostics_api::clean_diagnostics),
        )
        .route("/api/alert-rules", get(alerts_api::list_alert_rules))
        .route("/api/alert-rules", post(alerts_api::create_alert_rule))
        .route("/api/alert-rules/{id}", get(alerts_api::get_alert_rule))
        .route("/api/alert-rules/{id}", put(alerts_api::update_alert_rule))
        .route(
            "/api/alert-rules/{id}",
            delete(alerts_api::delete_alert_rule),
        )
        .route(
            "/api/alert-rules/{id}/toggle",
            patch(alerts_api::toggle_alert_rule),
        )
        .route(
            "/api/alert-rules/{id}/silent",
            patch(alerts_api::set_alert_rule_silent),
        )
        .route(
            "/api/alert-rules/{id}/history",
            get(alerts_api::list_alert_rule_history),
        )
        .route(
            "/api/alert-rules/{id}/test",
            post(alerts_api::test_alert_rule),
        )
        .route(
            "/api/alert-deliveries",
            get(alerts_api::list_alert_deliveries),
        )
        .route(
            "/api/settings/backup/export",
            axum::routing::post(settings_api::export_backup),
        )
        .route(
            "/api/settings/backup/import",
            axum::routing::post(settings_api::import_backup)
                .layer(DefaultBodyLimit::max(MAX_BACKUP_IMPORT_REQUEST_BYTES)),
        )
        .nest_service(
            "/ui",
            ServeDir::new("frontend/dist")
                .not_found_service(ServeFile::new("frontend/dist/index.html")),
        )
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            authenticate_management_request,
        ))
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

async fn proxy_config(state: &AppState) -> AppResult<Config> {
    let config = state.config.read().await;
    let Some(server_id) = state.proxy_server_id.as_deref() else {
        return Ok(config.proxy_config_for_server(None));
    };
    let server = config
        .servers
        .iter()
        .find(|server| server.id == server_id)
        .ok_or_else(|| {
            AppError::BadGateway(format!(
                "proxy server configuration {server_id} is no longer active"
            ))
        })?;
    Ok(config.for_server_for_validation(server))
}

async fn record_proxy_request(
    state: &AppState,
    level: ActivityLevel,
    message: impl Into<String>,
    detail: impl Into<String>,
) {
    let Ok(config) = proxy_config(state).await else {
        return;
    };
    let (server_id, server_name) = config_server_label(&config);
    let message = message.into();
    let detail = correlated_detail(&detail.into());
    state.activity_log.record(
        ActivityKind::General,
        level,
        Some(&server_id),
        &server_name,
        &message,
        &detail,
    );
    state.file_log.write(
        match level {
            ActivityLevel::Success | ActivityLevel::Info => "info",
            ActivityLevel::Warn => "warning",
            ActivityLevel::Error => "error",
        },
        &message,
        &detail,
    );
}

fn record_incoming_proxy_request(state: &AppState, config: &Config) {
    if let Some(metrics) = state.proxy_metrics.as_ref() {
        metrics.record_request();
    }
    record_stat(state, config, RequestStatKind::Request);
}

fn record_stat(state: &AppState, config: &Config, kind: RequestStatKind) {
    let (server_id, server_name) = config_server_label(config);
    state.alert_runtime.record_stat(server_id.clone(), kind);
    state
        .telemetry_writes
        .request_stat(server_id, server_name, config.port, kind);
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
    let detail = correlated_detail(input.detail);
    state
        .telemetry_writes
        .proxy_request_detail(ProxyRequestDetailWrite {
            timestamp_ms: input.started_at_ms,
            server_id: server_id.clone(),
            server_name: server_name.clone(),
            port: config.port,
            method: input.method.to_string(),
            path: path.clone(),
            path_type: input.path_type.to_string(),
            status_code: input.status_code,
            outcome: input.outcome.to_string(),
            duration_ms: input.duration_ms,
            playback_user: input.playback_user.to_string(),
            playback_ip: input.playback_ip.to_string(),
            cache_hit: input.cache_hit,
            blocked: input.blocked,
            detail: detail.clone(),
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

fn should_block_web_ui(config: &Config, path: &str) -> AppResult<bool> {
    if !config
        .servers
        .first()
        .is_some_and(|server| server.block_web_ui)
    {
        return Ok(false);
    }
    let upstream_url = config.emby_url(path)?;
    if rewrite::is_web_ui_path(upstream_url.path()) {
        return Ok(true);
    }
    let Ok(decoded_path) = urlencoding::decode(upstream_url.path()) else {
        return Ok(false);
    };
    if decoded_path.as_ref() == upstream_url.path() {
        return Ok(false);
    }
    Ok(rewrite::is_web_ui_path(
        config.emby_url(decoded_path.as_ref())?.path(),
    ))
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
    let detail = correlated_detail(&format!(
        "{} {} status={} duration={}ms type={} user={} ip={}",
        method,
        sanitized_uri_for_log(uri),
        status.as_u16(),
        duration_ms,
        path_type,
        empty_log_value(playback_user),
        empty_log_value(playback_ip)
    ));
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

fn correlated_detail(detail: &str) -> String {
    let detail = diagnostics::sanitize_diagnostic_text(&sanitized_detail_for_log(detail));
    match diagnostics::current_request_id() {
        Some(id) => format!("request_id={id} {detail}").trim_end().to_string(),
        None => detail,
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
    let _ = url.set_username("");
    let _ = url.set_password(None);
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
    peer_ip: IpAddr,
    trace: Option<ProxyRequestTrace>,
    mut diagnosis: Option<&mut proxy::PlaybackDiagnosisTrace>,
) -> AppResult<Response> {
    let method_label = method.as_str().to_string();
    let mut untraced_observation = diagnostics::UpstreamObservation::default();
    if let Some(diagnosis) = diagnosis.as_deref_mut() {
        diagnosis.begin_step(
            proxy::STEP_PROXY,
            "forward request through response headers; stream not measured",
        );
    }
    let observation = match diagnosis.as_deref_mut() {
        Some(diagnosis) => &mut diagnosis.observation,
        None => &mut untraced_observation,
    };
    let result = proxy::proxy_to_emby_observed(
        &state.proxy_client,
        config,
        method,
        uri,
        headers,
        body,
        peer_ip,
        observation,
    )
    .await;
    if let Some(diagnosis) = diagnosis.as_deref_mut() {
        match &result {
            Ok(response) => {
                diagnosis
                    .complete_step("upstream response headers received; media stream not measured");
                let status = response.status().as_u16();
                diagnosis.result.outcome =
                    Some(if status >= 400 { "failed" } else { "proxy" }.to_string());
                diagnosis.result.status_code = Some(status);
                diagnosis.result.upstream_status = Some(status);
                diagnosis.result.target_host = diagnostics::target_host(&config.emby_host);
                if status >= 400 {
                    let reason = diagnosis.result.reason.clone().unwrap_or_default();
                    diagnosis.result.reason =
                        Some(format!("{reason}; upstream response status {status}"));
                    diagnosis.result.error_kind = Some("upstream_http".to_string());
                    diagnosis.mark_upstream_status_failure();
                }
            }
            Err(error) => diagnosis.fail_error(proxy::STEP_PROXY, error),
        }
    }
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
                if diagnosis.is_some()
                    && !status.is_client_error()
                    && !status.is_server_error()
                    && duration_ms < 3000
                {
                    let (server_id, server_name) = config_server_label(config);
                    state.activity_log.record(
                        ActivityKind::General,
                        ActivityLevel::Info,
                        Some(&server_id),
                        &server_name,
                        "回源响应头已返回",
                        correlated_detail(&format!(
                            "status={} media_stream_not_measured",
                            status.as_u16()
                        )),
                    );
                }
            }
        }
        Err(err) => {
            let (server_id, server_name) = config_server_label(config);
            let error_detail = correlated_detail(&err.safe_log_message());
            record_stat(state, config, RequestStatKind::Error);
            if let Some(metrics) = state.proxy_metrics.as_ref() {
                metrics.record_error(error_detail.clone());
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
    if let Some(diagnosis) = diagnosis.as_deref_mut() {
        diagnosis.failure_logged();
    }
    result
}

fn is_public_management_path(path: &str) -> bool {
    matches!(
        path,
        "/" | "/healthz"
            | "/api/setup-status"
            | "/api/app-info"
            | "/api/public-key"
            | "/api/setup"
            | "/api/login"
    ) || path == "/ui"
        || path.starts_with("/ui/")
}

async fn authenticate_management_request(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let is_write = matches!(
        *request.method(),
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    );
    if !is_write || is_public_management_path(request.uri().path()) {
        return next.run(request).await;
    }

    match auth::require_auth(&state, request.headers()).await {
        Ok(()) => next.run(request).await,
        Err(error) => error.into_response(),
    }
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
    let release = diagnostics::network::send(
        state
            .client
            .get("https://api.github.com/repos/syscc/EmbyPanel/releases/latest")
            .header(header::USER_AGENT, "EmbyPanel"),
    )
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
    config: &Config,
    method: Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: Bytes,
    peer_ip: IpAddr,
) -> AppResult<Response> {
    let (status, response_headers, text) = proxy::proxy_text(
        &state.client,
        &config,
        method,
        &uri,
        &headers,
        body,
        peer_ip,
    )
    .await?;
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
    config: &Config,
    method: Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: Bytes,
    peer_ip: IpAddr,
) -> AppResult<Response> {
    let (status, response_headers, text) = proxy::proxy_text(
        &state.client,
        &config,
        method,
        &uri,
        &headers,
        body,
        peer_ip,
    )
    .await?;
    if !status.is_success() {
        return proxy::body_response(status, response_headers, text);
    }

    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return proxy::body_response(status, response_headers, text);
    };
    let gateway_url = proxy_base_url(&config, &headers, peer_ip);
    let modified = rewrite::patch_system_info(value, config.port, &gateway_url);

    proxy::body_response(
        status,
        rewrite::json_headers(),
        serde_json::to_string(&modified)?,
    )
}

async fn handle_playback_info(
    state: &AppState,
    config: &Config,
    method: Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: Bytes,
    peer_addr: SocketAddr,
    timing: RequestTiming,
) -> AppResult<Response> {
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
    let playback_ip = proxy::real_ip_for_log(&config, &headers, peer_addr.ip());
    remember_playback_user(state, &config, query.as_deref().unwrap_or(""), &headers).await;
    let (status, response_headers, text) = proxy::proxy_text(
        &state.client,
        &config,
        method,
        &uri,
        &headers,
        body,
        peer_addr.ip(),
    )
    .await?;
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
    config: &Config,
    method: Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: Bytes,
    peer_addr: SocketAddr,
    timing: RequestTiming,
) -> AppResult<Response> {
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
    let device_id = playback_device_id(query, &headers);
    let media_source_id = query_param(query, "MediaSourceId");
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let (server_id, server_name) = config_server_label(&config);
    let mut trace = proxy::PlaybackDiagnosisTrace::new(&server_id, &item_id, user_agent);
    let playback_ip = proxy::real_ip_for_log(&config, &headers, peer_addr.ip());
    trace.context = playback_diagnosis_context(&server_name, &method, &uri, &headers, &playback_ip);
    attach_diagnosis_failure_log(&mut trace, state, config, timing);
    trace.begin_step(proxy::STEP_CONTEXT, "resolve playback user");
    let playback_user = diagnostics::network::observe_operation(
        &mut trace.observation,
        playback_user_name(&state.client, &config, query, &headers, state),
    )
    .await;
    trace.context.user_name = (playback_user != "--").then(|| playback_user.clone());
    trace.complete_step("playback user resolution completed");
    if method == Method::HEAD {
        trace.skip_step(
            proxy::STEP_POLICY,
            "HEAD request does not enforce playback limits",
        );
    } else {
        trace.begin_step(proxy::STEP_POLICY, "playback limits");
    }
    let concurrent_limited = method != Method::HEAD
        && diagnostics::network::observe_operation(
            &mut trace.observation,
            client_control::enforce_concurrent_playback_limit(
                state,
                client_control::ConcurrentPlaybackLimitInput {
                    runtime_config: &config,
                    server_id: &server_id,
                    server_name: &server_name,
                    playback_user: &playback_user,
                    playback_ip: &playback_ip,
                    item_id: &item_id,
                    device_id: device_id.as_deref(),
                },
            ),
        )
        .await
        .map_err(|error| {
            trace.fail_error(proxy::STEP_POLICY, &error);
            error
        })?;
    if concurrent_limited {
        trace.complete_step("concurrent playback limit");
        trace.result = diagnostics::DiagnosisResult {
            outcome: Some("blocked".to_string()),
            status_code: Some(429),
            reason: Some("concurrent playback limit".to_string()),
            ..Default::default()
        };
        trace.finish().await;
        record_stat(state, &config, RequestStatKind::Block);
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
        && diagnostics::network::observe_operation(
            &mut trace.observation,
            client_control::enforce_playback_rate_limit(
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
            ),
        )
        .await
        .map_err(|error| {
            trace.fail_error(proxy::STEP_POLICY, &error);
            error
        })?;
    if rate_limited {
        trace.complete_step("playback rate limit");
        trace.result = diagnostics::DiagnosisResult {
            outcome: Some("blocked".to_string()),
            status_code: Some(429),
            reason: Some("playback rate limit".to_string()),
            ..Default::default()
        };
        trace.finish().await;
        record_stat(state, &config, RequestStatKind::Block);
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
    trace.complete_step("playback limits passed");
    let cache_key = DirectLinkCache::key(
        state.proxy_server_id.as_deref().unwrap_or("default"),
        &item_id,
        media_source_id.as_deref().unwrap_or("default"),
        user_agent,
    );

    trace.begin_step(proxy::STEP_CACHE, "lookup");
    let cache = state.cache.read().await.clone();
    let lookup = cache.get_observed(&cache_key).await;
    let valid_hit = lookup
        .value
        .as_ref()
        .is_some_and(|value| should_cache_direct_link(&config, value));
    trace.cache = diagnostics::DiagnosisCache {
        status: Some(
            if !lookup.enabled {
                "disabled"
            } else if valid_hit {
                "hit"
            } else {
                "miss"
            }
            .to_string(),
        ),
        stored: None,
        reason: Some(
            if !lookup.enabled {
                "cache policy disabled"
            } else if valid_hit {
                "cached target accepted"
            } else if lookup.value.is_some() {
                "cached target rejected by domain policy"
            } else {
                "no cached entry observed"
            }
            .to_string(),
        ),
        ttl_seconds: Some(lookup.ttl_seconds),
        remaining_ttl_ms: valid_hit.then_some(lookup.remaining_ttl_ms).flatten(),
        target_host: lookup.value.as_deref().and_then(diagnostics::target_host),
    };
    trace.set_cache_hit(valid_hit);
    trace.complete_step(if valid_hit { "hit" } else { "miss" });
    let cached = lookup.value.filter(|_| valid_hit);
    if let Some(cached) = cached {
        for step in [
            proxy::STEP_EMBY_PATH,
            proxy::STEP_MAPPING,
            proxy::STEP_OPENLIST,
            proxy::STEP_RAW_URL,
        ] {
            trace.skip_step(step, "cache hit; media path resolution not needed");
        }
        trace.skip_step(
            proxy::STEP_INTERNAL_REDIRECT,
            "cache hit; cached final URL reused",
        );
        trace.append_cache_step("hit");
        let response =
            playback_redirect_response(&mut trace, &cached, "cache hit; redirect response only")?;
        trace.push_step(
            proxy::STEP_REDIRECT,
            Instant::now(),
            &redirect_diagnosis_detail(&cached),
        );
        trace.finish().await;
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
        record_playback_path(
            state,
            &server_id,
            &item_id,
            device_id.as_deref(),
            media_source_id.as_deref(),
            PLAYBACK_MODE_DIRECT_LINK,
        )
        .await;
        return Ok(response);
    }

    trace.begin_step(proxy::STEP_EMBY_PATH, "query");
    let media_resolution = emby::get_media_path_observed(
        &state.client,
        &config,
        &item_id,
        media_source_id.as_deref(),
        user_agent,
        &mut trace.observation,
    )
    .await;
    if let Ok(resolution) = &media_resolution {
        trace.context.media_name = resolution.media_name.clone();
        trace.context.series_name = resolution.series_name.clone();
        trace.context.season_number = resolution.season_number;
        trace.context.episode_number = resolution.episode_number;
        trace.context.media_source_id = resolution
            .media_source_id
            .clone()
            .or(trace.context.media_source_id.take());
    }
    let media_path = match media_resolution.map(|resolution| resolution.path) {
        Ok(Some(path)) => {
            trace.paths.original = Some(diagnostics::sanitize_path(&path));
            trace.complete_step(&diagnostics::sanitize_diagnostic_text(&format!(
                "resolved path={path}"
            )));
            path
        }
        Ok(None) => {
            tracing::info!(item = item_id, "media source not found; proxying to Emby");
            trace.complete_step("not found");
            for step in [
                proxy::STEP_MAPPING,
                proxy::STEP_OPENLIST,
                proxy::STEP_RAW_URL,
                proxy::STEP_INTERNAL_REDIRECT,
            ] {
                trace.skip_step(step, "media source path not found; using Emby proxy");
            }
            trace.cache.stored = Some(false);
            trace.cache.reason =
                Some("media source path not found; no cache insertion".to_string());
            trace.append_cache_step("miss");
            trace.skip_step(proxy::STEP_REDIRECT, "source not found; proxy to Emby");
            trace.result.reason = Some(
                "requested media source path not found; proxy response headers only".to_string(),
            );
            let result = proxy_to_emby_recorded(
                state,
                &config,
                method,
                &uri,
                &headers,
                body,
                peer_addr.ip(),
                Some(ProxyRequestTrace {
                    started_at_ms: timing.started_at_ms,
                    started: timing.started,
                    path_type: "video_stream",
                    playback_user,
                    playback_ip,
                }),
                Some(&mut trace),
            )
            .await;
            trace.finish().await;
            if let Ok(response) = &result
                && let Some(mode) = playback_mode_for_proxy_response(response)
            {
                record_playback_path(
                    state,
                    &server_id,
                    &item_id,
                    device_id.as_deref(),
                    media_source_id.as_deref(),
                    mode,
                )
                .await;
            }
            return result;
        }
        Err(err) => {
            trace.fail_error(proxy::STEP_EMBY_PATH, &err);
            trace.finish().await;
            return Err(err);
        }
    };
    let mapping_started = Instant::now();
    let (media_path, mapping_details, matched_rules) =
        apply_strm_url_mappings_observed(&config, &media_path);
    trace.paths.mapped = Some(diagnostics::sanitize_path(&media_path));
    trace.paths.matched_rules = matched_rules;
    if config.strm_url_mapping_enabled {
        trace.push_step(proxy::STEP_MAPPING, mapping_started, &mapping_details);
    } else {
        trace.skip_step(proxy::STEP_MAPPING, "URL mapping disabled");
    }

    if is_direct_url(&media_path) && config.openlist_addr.is_none() {
        let redirect_url = maybe_resolve_internal_redirect(
            &state.proxy_client,
            &config,
            &media_path,
            user_agent,
            &mut trace,
        )
        .await;
        let should_cache = should_cache_direct_link(&config, &redirect_url);
        if should_cache {
            trace.begin_step(proxy::STEP_CACHE, "set");
            let cache = state.cache.read().await.clone();
            let stored = cache.set_observed(cache_key, redirect_url.clone()).await;
            trace.cache.stored = Some(stored);
            trace.complete_step("miss");
        }
        trace.skip_step(
            proxy::STEP_OPENLIST,
            "direct URL selected; OpenList API not configured",
        );
        trace.skip_step(
            proxy::STEP_RAW_URL,
            "direct URL selected; no OpenList raw URL needed",
        );
        complete_cache_decision(&mut trace, &config, should_cache, &redirect_url);
        trace.append_cache_step(cache_miss_detail(&config, should_cache));
        let response = playback_redirect_response(
            &mut trace,
            &redirect_url,
            "direct media URL; redirect response only",
        )?;
        trace.set_cache_hit(false);
        trace.push_step(
            proxy::STEP_REDIRECT,
            Instant::now(),
            &redirect_diagnosis_detail(&redirect_url),
        );
        trace.finish().await;
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
                outcome: if should_cache && direct_link_cache_is_effective(&config) {
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
        record_playback_path(
            state,
            &server_id,
            &item_id,
            device_id.as_deref(),
            media_source_id.as_deref(),
            PLAYBACK_MODE_DIRECT_LINK,
        )
        .await;
        return Ok(response);
    }

    let Some(openlist_path) = openlist::extract_openlist_path(&media_path) else {
        tracing::info!(
            item = item_id,
            path = diagnostics::sanitize_path(&media_path),
            "media path is not a direct URL or OpenList /d URL; proxying to Emby"
        );
        for step in [
            proxy::STEP_OPENLIST,
            proxy::STEP_RAW_URL,
            proxy::STEP_INTERNAL_REDIRECT,
        ] {
            trace.skip_step(step, "path requires Emby proxy, not direct URL resolution");
        }
        trace.cache.stored = Some(false);
        trace.cache.reason = Some("path requires Emby proxy; no direct URL cached".to_string());
        trace.append_cache_step("miss");
        trace.set_cache_hit(false);
        trace.skip_step(
            proxy::STEP_REDIRECT,
            "path is neither a direct URL nor OpenList /d path",
        );
        trace.result.reason = Some(
            "path is neither a direct URL nor OpenList /d path; proxy response headers only"
                .to_string(),
        );
        let result = proxy_to_emby_recorded(
            state,
            &config,
            method,
            &uri,
            &headers,
            body,
            peer_addr.ip(),
            Some(ProxyRequestTrace {
                started_at_ms: timing.started_at_ms,
                started: timing.started,
                path_type: "video_stream",
                playback_user,
                playback_ip,
            }),
            Some(&mut trace),
        )
        .await;
        trace.finish().await;
        if let Ok(response) = &result
            && let Some(mode) = playback_mode_for_proxy_response(response)
        {
            record_playback_path(
                state,
                &server_id,
                &item_id,
                device_id.as_deref(),
                media_source_id.as_deref(),
                mode,
            )
            .await;
        }
        return result;
    };

    trace.begin_step(
        proxy::STEP_OPENLIST,
        &diagnostics::sanitize_diagnostic_text(&format!("fs_get path={openlist_path}")),
    );
    let raw_url = match openlist::fs_get_observed(
        &state.client,
        &config,
        &openlist_path,
        user_agent,
        &mut trace.observation,
    )
    .await
    {
        Ok(fs) => {
            trace.complete_step(&diagnostics::sanitize_diagnostic_text(&format!(
                "fs_get path={openlist_path}; matched"
            )));
            trace.begin_step(proxy::STEP_RAW_URL, "validate and resolve");
            let missing_raw_url = fs.is_none();
            match openlist::ensure_raw_url(fs) {
                Ok(raw) => {
                    trace.complete_step(&diagnostics::sanitize_diagnostic_text(&format!(
                        "resolved raw_url={raw}"
                    )));
                    let raw = maybe_resolve_internal_redirect(
                        &state.proxy_client,
                        &config,
                        &raw,
                        user_agent,
                        &mut trace,
                    )
                    .await;
                    raw
                }
                Err(err) => {
                    trace.observation.error_kind = Some(
                        if missing_raw_url {
                            "missing_raw_url"
                        } else {
                            "invalid_url"
                        }
                        .to_string(),
                    );
                    trace.fail_error(proxy::STEP_RAW_URL, &err);
                    trace.finish().await;
                    return Err(err);
                }
            }
        }
        Err(err) => {
            trace.fail_error(proxy::STEP_OPENLIST, &err);
            trace.finish().await;
            return Err(err);
        }
    };
    let should_cache = should_cache_direct_link(&config, &raw_url);
    if should_cache {
        trace.begin_step(proxy::STEP_CACHE, "set");
        let cache = state.cache.read().await.clone();
        let stored = cache.set_observed(cache_key, raw_url.clone()).await;
        trace.cache.stored = Some(stored);
        trace.complete_step("miss");
    }
    complete_cache_decision(&mut trace, &config, should_cache, &raw_url);
    trace.append_cache_step(cache_miss_detail(&config, should_cache));
    trace.set_cache_hit(false);
    let response = playback_redirect_response(
        &mut trace,
        &raw_url,
        "OpenList raw URL; redirect response only",
    )?;
    trace.push_step(
        proxy::STEP_REDIRECT,
        Instant::now(),
        &redirect_diagnosis_detail(&raw_url),
    );
    trace.finish().await;
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
            outcome: if should_cache && direct_link_cache_is_effective(&config) {
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
    record_playback_path(
        state,
        &server_id,
        &item_id,
        device_id.as_deref(),
        media_source_id.as_deref(),
        PLAYBACK_MODE_DIRECT_LINK,
    )
    .await;
    Ok(response)
}

async fn record_playback_path(
    state: &AppState,
    server_id: &str,
    item_id: &str,
    device_id: Option<&str>,
    media_source_id: Option<&str>,
    mode: &'static str,
) {
    state
        .playback_paths
        .lock()
        .await
        .record(server_id, item_id, device_id, media_source_id, mode);
}

fn playback_mode_for_proxy_response(response: &Response) -> Option<&'static str> {
    if response.status().is_success() {
        return Some(PLAYBACK_MODE_SERVER_PROXY);
    }
    response
        .status()
        .is_redirection()
        .then_some(PLAYBACK_MODE_DIRECT_LINK)
}

fn query_param(query: &str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(name, _)| name.eq_ignore_ascii_case(key))
        .map(|(_, value)| value.into_owned())
}

fn non_empty_query_param(query: &str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(name, value)| name.eq_ignore_ascii_case(key) && !value.trim().is_empty())
        .map(|(_, value)| value.into_owned())
}

fn is_direct_url(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

fn attach_diagnosis_failure_log(
    trace: &mut proxy::PlaybackDiagnosisTrace,
    state: &AppState,
    config: &Config,
    timing: RequestTiming,
) {
    let writes = state.telemetry_writes.clone();
    let activity = state.activity_log.clone();
    let port = config.port;
    trace.on_unlogged_failure(move |record| {
        // Only fixed safe fields, never error text, request query, headers or upstream bodies.
        let cancelled = record.result.outcome.as_deref() == Some("cancelled");
        let outcome = if cancelled {
            "请求取消"
        } else {
            "播放请求处理失败"
        };
        let detail = format!(
            "request_id={} outcome={}",
            record.request_id,
            record.result.outcome.as_deref().unwrap_or("failed")
        );
        let server_name = record.context.server_name.as_deref().unwrap_or("");
        writes.proxy_request_detail(ProxyRequestDetailWrite {
            timestamp_ms: timing.started_at_ms,
            server_id: record.server_id.clone(),
            server_name: server_name.to_string(),
            port,
            method: record.context.method.clone().unwrap_or_default(),
            path: record.context.request_path.clone().unwrap_or_default(),
            path_type: "video_stream".to_string(),
            // 0 is an internal log sentinel: cancellation observed, no HTTP response sent.
            status_code: record.result.status_code.unwrap_or(0),
            outcome: outcome.to_string(),
            duration_ms: record.duration_ms.unwrap_or(0) as u128,
            playback_user: record
                .context
                .user_name
                .clone()
                .unwrap_or_else(|| "--".to_string()),
            playback_ip: record
                .context
                .source_ip
                .clone()
                .unwrap_or_else(|| "--".to_string()),
            cache_hit: record.cache_hit,
            blocked: false,
            detail: detail.clone(),
        });
        activity.record(
            ActivityKind::General,
            if cancelled {
                ActivityLevel::Warn
            } else {
                ActivityLevel::Error
            },
            Some(&record.server_id),
            server_name,
            outcome,
            detail,
        );
    });
}

fn playback_diagnosis_context(
    server_name: &str,
    method: &Method,
    uri: &Uri,
    headers: &axum::http::HeaderMap,
    source_ip: &str,
) -> diagnostics::DiagnosisContext {
    let query = uri.query().unwrap_or("");
    diagnostics::DiagnosisContext {
        server_name: Some(server_name.to_string()),
        media_source_id: non_empty_query_param(query, "MediaSourceId"),
        client_name: playback_context_value(query, headers, "Client", "X-Emby-Client"),
        device_name: playback_device_name(query, headers),
        device_id: playback_device_id(query, headers),
        source_ip: Some(source_ip.to_string()),
        method: Some(method.to_string()),
        request_path: Some(uri.path().to_string()),
        range: header_value(headers, "Range"),
        ..Default::default()
    }
}

fn complete_cache_decision(
    trace: &mut proxy::PlaybackDiagnosisTrace,
    config: &Config,
    eligible: bool,
    url: &str,
) {
    trace.cache.stored.get_or_insert(false);
    trace.cache.target_host = diagnostics::target_host(url);
    trace.cache.reason = Some(
        if trace.cache.status.as_deref() == Some("disabled") {
            "cache policy disabled"
        } else if !eligible {
            match config.cache_domain_filter_mode.as_str() {
                "whitelist" => "target not accepted by domain whitelist",
                "blacklist" => "target rejected by domain blacklist",
                _ => "target URL is not cacheable",
            }
        } else if trace.cache.stored == Some(true) {
            "cache miss; entry inserted"
        } else {
            "cache insertion disabled by effective bucket policy"
        }
        .to_string(),
    );
}

fn cache_miss_detail(config: &Config, cache_eligible: bool) -> &'static str {
    if !direct_link_cache_is_effective(config) {
        "disabled"
    } else if cache_eligible {
        "miss; stored"
    } else {
        "miss; not stored"
    }
}

fn playback_redirect_response(
    trace: &mut proxy::PlaybackDiagnosisTrace,
    location: &str,
    reason: &str,
) -> AppResult<Response> {
    let response = proxy::redirect_response(location.to_string());
    if response.status() != StatusCode::FOUND {
        let error =
            AppError::BadGateway("upstream redirect location could not be returned".to_string());
        trace.begin_step(proxy::STEP_REDIRECT, "construct redirect response");
        trace.observation.error_kind = Some("invalid_redirect_location".to_string());
        trace.fail_error(proxy::STEP_REDIRECT, &error);
        return Err(error);
    }
    trace.set_redirect(location, reason);
    Ok(response)
}

fn redirect_diagnosis_detail(location: &str) -> String {
    diagnostics::sanitize_diagnostic_text(&format!("redirect location={location}"))
}

pub(crate) fn media_path_uses_direct_link(config: &Config, media_path: &str) -> bool {
    let media_path = apply_strm_url_mappings(config, media_path);
    (is_direct_url(&media_path) && config.openlist_addr.is_none())
        || openlist::extract_openlist_path(&media_path).is_some()
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
    let log_detail = correlated_detail(redirect_url);
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
    state.alert_runtime.record_stat(server_id.clone(), kind);
    state
        .telemetry_writes
        .request_stat(server_id, server_name, config.port, kind);
}

fn cache_ttl_label(seconds: u64) -> String {
    if seconds >= 60 && seconds.is_multiple_of(60) {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

fn redirect_log_message(config: &Config, should_cache: bool) -> String {
    if !should_cache || !direct_link_cache_is_effective(config) {
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
        .insert(playback_user_key(config, &device_id), user_name)
        .await;
}

async fn remembered_playback_user(
    state: &AppState,
    config: &Config,
    device_id: &str,
) -> Option<String> {
    state
        .playback_users
        .get(&playback_user_key(config, device_id))
        .await
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
    let user_id = playback_context_value(query, headers, "UserId", "X-Emby-User-Id")?;
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

fn playback_context_value(
    query: &str,
    headers: &axum::http::HeaderMap,
    key: &str,
    header: &str,
) -> Option<String> {
    non_empty_query_param(query, key)
        .or_else(|| non_empty_query_param(query, header))
        .or_else(|| header_value(headers, header))
        .or_else(|| emby_authorization_value(headers, key))
}

fn playback_device_id(query: &str, headers: &axum::http::HeaderMap) -> Option<String> {
    playback_context_value(query, headers, "DeviceId", "X-Emby-Device-Id")
}

fn playback_device_name(query: &str, headers: &axum::http::HeaderMap) -> Option<String> {
    non_empty_query_param(query, "Device")
        .or_else(|| non_empty_query_param(query, "DeviceName"))
        .or_else(|| playback_context_value(query, headers, "DeviceName", "X-Emby-Device-Name"))
        .or_else(|| emby_authorization_value(headers, "Device"))
}

fn playback_stream_query(query: &str, headers: &axum::http::HeaderMap) -> Option<String> {
    let fields = [
        (
            "UserId",
            playback_context_value(query, headers, "UserId", "X-Emby-User-Id"),
        ),
        ("DeviceId", playback_device_id(query, headers)),
        (
            "Client",
            playback_context_value(query, headers, "Client", "X-Emby-Client"),
        ),
        ("Device", playback_device_name(query, headers)),
    ];
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if !fields
            .iter()
            .any(|(name, _)| key.eq_ignore_ascii_case(name))
            && !key.eq_ignore_ascii_case("DeviceName")
        {
            serializer.append_pair(&key, &value);
        }
    }
    for (key, value) in fields {
        if let Some(value) = value {
            serializer.append_pair(key, &value);
        }
    }
    let query = serializer.finish();
    (!query.is_empty()).then_some(query)
}

fn emby_token_from_authorization(headers: &axum::http::HeaderMap) -> Option<String> {
    let value = header_value(headers, "Authorization")?;
    if let Some((scheme, token)) = value.split_once(char::is_whitespace)
        && scheme.eq_ignore_ascii_case("Bearer")
    {
        return Some(token.trim().to_string()).filter(|token| !token.is_empty());
    }
    media_browser_auth_value(&value, "Token")
}

fn emby_authorization_value(headers: &axum::http::HeaderMap, key: &str) -> Option<String> {
    [
        "X-Emby-Authorization",
        "X-MediaBrowser-Authorization",
        "Authorization",
    ]
    .into_iter()
    .filter_map(|header| header_value(headers, header))
    .find_map(|value| media_browser_auth_value(&value, key))
}

fn media_browser_auth_value(value: &str, key: &str) -> Option<String> {
    let value = value.trim();
    let attributes = value
        .split_once(char::is_whitespace)
        .filter(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("MediaBrowser") || scheme.eq_ignore_ascii_case("Emby")
        })
        .map(|(_, attributes)| attributes.trim_start())
        .unwrap_or(value);
    let mut quoted = false;
    let mut escaped = false;
    for part in attributes.split(|ch| {
        if escaped {
            escaped = false;
        } else if ch == '\\' && quoted {
            escaped = true;
        } else if ch == '"' {
            quoted = !quoted;
        } else if ch == ',' && !quoted {
            return true;
        }
        false
    }) {
        let Some((name, raw)) = part.split_once('=') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case(key) {
            continue;
        }
        let raw = raw.trim();
        let value = if let Some(inner) = raw.strip_prefix('"') {
            let mut chars = inner.chars();
            let mut value = String::new();
            loop {
                match chars.next()? {
                    '\\' => value.push(chars.next()?),
                    '"' => {
                        if !chars.as_str().trim().is_empty() {
                            return None;
                        }
                        break;
                    }
                    ch => value.push(ch),
                }
            }
            value
        } else {
            if !raw
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            {
                return None;
            }
            raw.to_string()
        };
        if !value.trim().is_empty() {
            return Some(value);
        }
    }
    None
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

fn proxy_base_url(config: &Config, headers: &axum::http::HeaderMap, peer_ip: IpAddr) -> String {
    let trusted_forwarder = config
        .servers
        .first()
        .is_some_and(|server| server.is_trusted_proxy(peer_ip));
    let proto = trusted_forwarder
        .then(|| forwarded_header_value(headers, "x-forwarded-proto"))
        .flatten()
        .and_then(normalized_proxy_scheme)
        .unwrap_or("http");
    let host = trusted_forwarder
        .then(|| authority_header_value(headers, "x-forwarded-host"))
        .flatten()
        .or_else(|| authority_header_value(headers, "host"))
        .map(str::to_string)
        .unwrap_or_else(|| format!("127.0.0.1:{}", config.port));

    format!("{proto}://{host}")
}

fn normalized_proxy_scheme(value: &str) -> Option<&'static str> {
    if value.eq_ignore_ascii_case("https") {
        Some("https")
    } else if value.eq_ignore_ascii_case("http") {
        Some("http")
    } else {
        None
    }
}

fn authority_header_value<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    forwarded_header_value(headers, name)
        .filter(|value| value.parse::<axum::http::uri::Authority>().is_ok())
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
    apply_strm_url_mappings_with_details(config, media_path).0
}

fn apply_strm_url_mappings_with_details(config: &Config, media_path: &str) -> (String, String) {
    let (mapped, details, _) = apply_strm_url_mappings_observed(config, media_path);
    (mapped, details)
}

fn apply_strm_url_mappings_observed(
    config: &Config,
    media_path: &str,
) -> (String, String, Vec<usize>) {
    if !config.strm_url_mapping_enabled {
        return (
            media_path.to_string(),
            diagnostics::sanitize_diagnostic_text(&format!("disabled; result={media_path}")),
            Vec::new(),
        );
    }

    let mapped = url_mapping::apply_rules(media_path, &config.strm_url_mapping_rules);
    let mut current = media_path.to_string();
    let mut matched_rules = Vec::new();
    let mut matched_rule_numbers = Vec::new();
    for (index, line) in config.strm_url_mappings.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Reuse the canonical parser one rule at a time so diagnosis can identify
        // the actual configured rule without duplicating regex semantics.
        let Ok(rules) = url_mapping::parse_rules(line) else {
            continue;
        };
        let next = url_mapping::apply_rules(&current, &rules);
        if next != current {
            matched_rule_numbers.push(index + 1);
            matched_rules.push(format!(
                "rule#{} {}",
                index + 1,
                diagnostics::sanitize_diagnostic_text(line)
            ));
        }
        current = next;
    }

    if mapped != media_path {
        tracing::info!(
            from = sanitized_detail_for_log(media_path),
            to = sanitized_detail_for_log(&mapped),
            "mapped STRM URL"
        );
    }
    let details = if matched_rules.is_empty() {
        diagnostics::sanitize_diagnostic_text(&format!("unmatched; result={mapped}"))
    } else {
        diagnostics::sanitize_diagnostic_text(&format!(
            "matched {}; result={mapped}",
            matched_rules.join(", ")
        ))
    };
    (mapped, details, matched_rule_numbers)
}

async fn maybe_resolve_internal_redirect(
    client: &reqwest::Client,
    config: &Config,
    url: &str,
    user_agent: &str,
    trace: &mut proxy::PlaybackDiagnosisTrace,
) -> String {
    if !config.enable_internal_redirect {
        trace.skip_step(proxy::STEP_INTERNAL_REDIRECT, "internal redirect disabled");
        trace.paths.resolved = Some(diagnostics::sanitize_path(url));
        return url.to_string();
    }
    trace.begin_step(
        proxy::STEP_INTERNAL_REDIRECT,
        "resolve Location from existing probe",
    );
    match internal_redirect::resolve_redirect_observed(
        client,
        url,
        config.internal_redirect_timeout_seconds,
        user_agent,
        &mut trace.observation,
    )
    .await
    {
        Ok(resolution) => {
            let resolved = resolution.url;
            trace.timing.redirect_count = resolution.redirect_count;
            trace.paths.resolved = Some(diagnostics::sanitize_path(&resolved));
            trace.complete_step("internal redirect response headers received");
            tracing::info!(
                from = sanitized_detail_for_log(url),
                to = sanitized_detail_for_log(&resolved),
                "internal redirect resolved final URL"
            );
            resolved
        }
        Err(err) => {
            trace.complete_redirect_fallback(&err);
            trace.paths.resolved = Some(diagnostics::sanitize_path(url));
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
        http::{Request as HttpRequest, StatusCode},
    };
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    };
    use tokio::net::TcpListener;
    use tower::{Service, ServiceExt};

    use super::*;

    static TEST_DB_COUNTER: AtomicU64 = AtomicU64::new(0);

    include!("test-fixtures/review_authorization.rs");
    include!("test-fixtures/review_api_fixes.rs");
    include!("test-fixtures/diagnostics_expansion.rs");
    include!("test-fixtures/network_timing_api.rs");

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
            cache_enabled: true,
            cache_domain_filter_mode: "off".to_string(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect: false,
            internal_redirect_timeout_seconds: 15,
            strm_url_mappings: String::new(),
            strm_url_mapping_enabled: true,
            connectivity_check_enabled: true,
            connectivity_check_interval_seconds: 60,
            connectivity_check_timeout_seconds: 5,
            connectivity_auto_restart_seconds: 180,
            strm_url_mapping_rules: Vec::new(),
        }
    }

    fn with_trusted_proxy(mut config: Config) -> Config {
        config.servers = vec![config::EmbyServerConfig {
            id: "server-1".to_string(),
            name: "Server 1".to_string(),
            emby_host: config.emby_host.clone(),
            emby_api_key: config.emby_api_key.clone(),
            port: config.port,
            enabled: true,
            block_web_ui: false,
            block_web_ui_status_code: 403,
            block_web_ui_message: "Emby Web UI is disabled by EmbyPanel".to_string(),
            real_ip_mode: "auto".to_string(),
            real_ip_header: String::new(),
            trusted_proxy_cidrs: "192.0.2.0/24".to_string(),
            openlist_addr_override: None,
            openlist_token_override: None,
            openlist_token_override_configured: false,
            strm_url_mappings_override: None,
            strm_url_mapping_enabled_override: None,
            cache_ttl_seconds_override: None,
            cache_max_capacity_override: None,
            cache_enabled_override: None,
            cache_domain_filter_mode_override: None,
            cache_domain_whitelist_override: None,
            enable_internal_redirect_override: None,
            internal_redirect_timeout_seconds_override: None,
            connectivity_check_enabled_override: None,
            connectivity_check_interval_seconds_override: None,
            connectivity_check_timeout_seconds_override: None,
            connectivity_auto_restart_seconds_override: None,
            trusted_proxy_networks: vec!["192.0.2.0/24".parse().unwrap()],
        }];
        config
    }

    fn test_state(config: Config, settings_store: SettingsStore) -> AppState {
        let client = build_http_client().unwrap();
        let proxy_client = build_proxy_http_client().unwrap();
        let telemetry_writes = TelemetryWriteQueue::start(settings_store.clone()).unwrap();
        let alerts_path = std::env::temp_dir().join(format!(
            "embypanel-main-alerts-test-{}-{}.db",
            std::process::id(),
            TEST_DB_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let alerts = Alerts::new(alerts_path).unwrap();
        AppState {
            config: Arc::new(RwLock::new(config)),
            config_updates: Arc::new(Mutex::new(())),
            client,
            proxy_client,
            cache: Arc::new(RwLock::new(DirectLinkCache::new(true, 180, 10_000))),
            file_log: Arc::new(FileLogStore::new(&settings_store)),
            settings_store,
            telemetry_writes,
            alerts,
            alert_runtime: AlertRuntime::disabled(),
            client_control_cache: Arc::new(StdRwLock::new(None)),
            user_policies_cache: Arc::new(StdRwLock::new(None)),
            rate_limit_updates: Arc::new(Mutex::new(())),
            playback_sessions_cache: Arc::new(Mutex::new(HashMap::new())),
            password_tasks: Arc::new(Semaphore::new(PASSWORD_TASK_CONCURRENCY_LIMIT)),
            user_password_tasks: Arc::new(Semaphore::new(USER_PASSWORD_RESET_CONCURRENCY_LIMIT)),
            crypto_keys: CryptoKeys::generate().unwrap(),
            activity_log: Arc::new(ActivityLogStore::new(100)),
            block_log: Arc::new(BlockLogStore::new()),
            ip_location: Arc::new(ip_location::IpLocationStore::new()),
            connectivity: Arc::new(connectivity::ConnectivityMonitor::new()),
            proxy_manager: None,
            proxy_server_id: None,
            proxy_metrics: None,
            playback_paths: Arc::new(Mutex::new(PlaybackPathState::default())),
            playback_users: playback_user_cache(),
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
    fn playback_path_state_isolates_devices_and_conflicting_sources() {
        let mut state = PlaybackPathState::default();
        state.record(
            "server-1",
            "item-1",
            Some("device-1"),
            Some("source-1"),
            PLAYBACK_MODE_DIRECT_LINK,
        );
        state.record(
            "server-1",
            "item-1",
            Some("device-2"),
            Some("source-2"),
            PLAYBACK_MODE_SERVER_PROXY,
        );

        let direct_key =
            playback_path_key("server-1", "item-1", Some("device-1"), Some("source-1"));
        let sequence_before_lookup = state.entries[&direct_key].sequence;
        assert_eq!(
            state.lookup("server-1", "item-1", Some("device-1"), Some("source-1")),
            Some(PLAYBACK_MODE_DIRECT_LINK)
        );
        assert!(state.entries[&direct_key].sequence > sequence_before_lookup);
        assert_eq!(
            state.lookup("server-1", "item-1", Some("device-1"), None),
            Some(PLAYBACK_MODE_DIRECT_LINK)
        );
        assert_eq!(state.lookup("server-1", "item-1", None, None), None);
        assert_eq!(
            state.lookup(
                "server-1",
                "item-1",
                Some("device-1"),
                Some("missing-source")
            ),
            None
        );
        assert_eq!(state.lookup("server-2", "item-1", None, None), None);
    }

    #[test]
    fn proxy_playback_mode_classifies_success_and_redirects() {
        let success = Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            playback_mode_for_proxy_response(&success),
            Some(PLAYBACK_MODE_SERVER_PROXY)
        );

        let redirect = Response::builder()
            .status(StatusCode::FOUND)
            .header(header::LOCATION, "https://media.example.test/video.mkv")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            playback_mode_for_proxy_response(&redirect),
            Some(PLAYBACK_MODE_DIRECT_LINK)
        );

        let redirect_without_location = Response::builder()
            .status(StatusCode::FOUND)
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            playback_mode_for_proxy_response(&redirect_without_location),
            Some(PLAYBACK_MODE_DIRECT_LINK)
        );

        let failure = Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::empty())
            .unwrap();
        assert_eq!(playback_mode_for_proxy_response(&failure), None);
    }

    #[test]
    fn media_browser_auth_value_extracts_token_fields_after_scheme() {
        let value = r#"MediaBrowser Client="Emby Web", Device="Chrome", DeviceName="Chromium", DeviceId="device-1", Token="token-1", UserId="user-1""#;
        assert_eq!(
            media_browser_auth_value(value, "Client").as_deref(),
            Some("Emby Web")
        );
        assert_eq!(
            media_browser_auth_value(value, "Device").as_deref(),
            Some("Chrome")
        );
        assert_eq!(
            media_browser_auth_value(value, "DeviceName").as_deref(),
            Some("Chromium")
        );
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
        assert_eq!(media_browser_auth_value("Bearer token-1", "Client"), None);
    }

    #[test]
    fn media_browser_auth_value_preserves_quoted_device_names_and_emby_scheme() {
        for scheme in ["MediaBrowser", "Emby", "eMbY", ""] {
            let value = format!(
                r#"{scheme} Client="Emby Web", DeviceName="Office, \"Chrome\" \\ Mac", DeviceId="browser-device", Token="test-token""#
            );
            assert_eq!(
                media_browser_auth_value(&value, "Client").as_deref(),
                Some("Emby Web")
            );
            assert_eq!(
                media_browser_auth_value(&value, "DeviceName").as_deref(),
                Some(r#"Office, "Chrome" \ Mac"#)
            );
            assert_eq!(
                media_browser_auth_value(&value, "DeviceId").as_deref(),
                Some("browser-device")
            );
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(header::AUTHORIZATION, value.trim().parse().unwrap());
            assert_eq!(
                emby_token_from_authorization(&headers).as_deref(),
                Some("test-token")
            );
        }
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, "bEaReR test-token".parse().unwrap());
        assert_eq!(
            emby_token_from_authorization(&headers).as_deref(),
            Some("test-token")
        );
        assert_eq!(
            media_browser_auth_value(r#"Emby DeviceName="unterminated"#, "DeviceName"),
            None
        );
    }

    #[test]
    fn media_browser_auth_value_accepts_unquoted_http_tokens() {
        for scheme in ["MediaBrowser", "Emby", "eMbY", ""] {
            for token in ["Chrome-Web", "Mac_1.2", "AZaz09!#$%&'*+-.^_`|~"] {
                let authorization =
                    format!("{scheme} Client={token}, Device={token}, Token={token}");
                for key in ["Client", "Device", "Token"] {
                    assert_eq!(
                        media_browser_auth_value(&authorization, key).as_deref(),
                        Some(token)
                    );
                }
            }
        }
    }

    #[test]
    fn playback_context_rejects_unquoted_credentials_without_leaking_into_stream_urls() {
        let secret = "synthetic-unquoted-credential";
        let original_query = "api_key=query-key&X-Emby-Token=query-token";
        for key in ["Client", "Device", "DeviceName", "DeviceId", "UserId"] {
            for malformed in [
                format!("Chrome Token={secret}"),
                format!("Chrome ApiKey={secret}"),
                format!("Chrome\tToken={secret}"),
                format!("Chrome;Token={secret}"),
                format!("Chrome=Token={secret}"),
                format!("Chrome/Token={secret}"),
            ] {
                for scheme in ["MediaBrowser", "Emby", ""] {
                    for header in [
                        "Authorization",
                        "X-Emby-Authorization",
                        "X-MediaBrowser-Authorization",
                    ] {
                        let authorization = format!("{scheme} {key}={malformed}");
                        assert!(
                            media_browser_auth_value(&authorization, key).is_none(),
                            "malformed unquoted {key} must be rejected"
                        );
                        let mut headers = axum::http::HeaderMap::new();
                        headers.insert(
                            header.parse::<axum::http::HeaderName>().unwrap(),
                            authorization.trim().parse().unwrap(),
                        );
                        let query = playback_stream_query(original_query, &headers).unwrap();
                        let body = rewrite::patch_playback_info(
                            json!({"MediaSources":[{"Id":"source", "IsRemote":true}]}),
                            "/Items/item/PlaybackInfo",
                            Some(&query),
                        );
                        let url = body["MediaSources"][0]["DirectStreamUrl"].as_str().unwrap();
                        let stream_query = url.split_once('?').unwrap().1;
                        assert!(
                            url::form_urlencoded::parse(stream_query.as_bytes())
                                .all(|(_, value)| !value.contains(secret)),
                            "header credential must not enter any decoded stream parameter"
                        );
                        let context = playback_diagnosis_context(
                            "Synthetic",
                            &Method::GET,
                            &url.parse().unwrap(),
                            &headers,
                            "127.0.0.1",
                        );
                        assert!(context.client_name.is_none());
                        assert!(context.device_name.is_none());
                        assert!(context.device_id.is_none());
                        assert!(!serde_json::to_string(&context).unwrap().contains(secret));
                        assert_eq!(query_param(stream_query, "UserId"), None);
                        assert_eq!(
                            query_param(stream_query, "api_key").as_deref(),
                            Some("query-key")
                        );
                        assert_eq!(
                            query_param(stream_query, "X-Emby-Token").as_deref(),
                            Some("query-token")
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn playback_context_rejects_malformed_quotes_without_copying_header_credentials() {
        for key in ["Client", "Device", "DeviceName", "DeviceId", "UserId"] {
            for raw in [
                r#""Chrome, Token="synthetic-secret""#,
                r#""Chrome" trailing, Token="synthetic-secret""#,
                r#"Chrome", Token="synthetic-secret""#,
                r#""Chrome\", Token=synthetic-secret"#,
                r#""Chrome, ApiKey="synthetic-secret""#,
            ] {
                for header in [
                    "Authorization",
                    "X-Emby-Authorization",
                    "X-MediaBrowser-Authorization",
                ] {
                    let authorization = format!("Emby {key}={raw}");
                    assert_eq!(
                        media_browser_auth_value(&authorization, key),
                        None,
                        "invalid {key} must be rejected"
                    );
                    let mut headers = axum::http::HeaderMap::new();
                    headers.insert(
                        header.parse::<axum::http::HeaderName>().unwrap(),
                        authorization.parse().unwrap(),
                    );
                    let query = playback_stream_query("api_key=query-key", &headers).unwrap();
                    let body = rewrite::patch_playback_info(
                        json!({"MediaSources":[{"Id":"source", "IsRemote":true}]}),
                        "/Items/item/PlaybackInfo",
                        Some(&query),
                    );
                    let url = body["MediaSources"][0]["DirectStreamUrl"].as_str().unwrap();
                    assert!(
                        !url.contains("synthetic-secret"),
                        "header credentials must not enter stream URL"
                    );
                    let context = playback_diagnosis_context(
                        "Synthetic",
                        &Method::GET,
                        &url.parse().unwrap(),
                        &headers,
                        "127.0.0.1",
                    );
                    assert!(
                        !serde_json::to_string(&context)
                            .unwrap()
                            .contains("synthetic-secret")
                    );
                    assert_eq!(query_param(&query, "api_key").as_deref(), Some("query-key"));
                }
            }
        }
    }

    #[test]
    fn playback_context_preserves_emby_query_metadata_without_inventing_ua_identity() {
        let query = "Client=&X-Emby-Client=Emby+Web&DeviceName=&X-Emby-Device-Name=Office%2C+Chrome&DeviceId=&X-Emby-Device-Id=browser-device&UserId=&X-Emby-User-Id=browser-user";
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            "Mozilla/5.0 Chrome/156.0 Safari/537.36".parse().unwrap(),
        );
        let stream_query = playback_stream_query(query, &headers).unwrap();
        let uri = format!("/videos/item/stream?{stream_query}")
            .parse()
            .unwrap();
        let context =
            playback_diagnosis_context("Synthetic", &Method::GET, &uri, &headers, "127.0.0.1");
        assert_eq!(context.client_name.as_deref(), Some("Emby Web"));
        assert_eq!(context.device_name.as_deref(), Some("Office, Chrome"));
        assert_eq!(context.device_id.as_deref(), Some("browser-device"));
        assert_eq!(
            query_param(&stream_query, "UserId").as_deref(),
            Some("browser-user")
        );
        assert_eq!(
            playback_device_id("DeviceId=&deviceid=explicit-device", &headers).as_deref(),
            Some("explicit-device")
        );
        let context = playback_diagnosis_context(
            "Synthetic",
            &Method::GET,
            &"/videos/item/stream".parse().unwrap(),
            &headers,
            "127.0.0.1",
        );
        assert!(context.client_name.is_none());
        assert!(context.device_name.is_none());
        assert!(context.device_id.is_none());
    }

    #[test]
    fn playback_context_reads_standard_authorization_and_ignores_empty_query_values() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, axum::http::HeaderValue::from_static(
            r#"MediaBrowser Client="Emby Web", Device="Chrome", DeviceName="Chromium", DeviceId="device-1""#,
        ));
        let method = Method::GET;
        let uri: Uri = "/videos/item/stream?Client=&DeviceName=&DeviceId="
            .parse()
            .unwrap();
        let context = playback_diagnosis_context("Synthetic", &method, &uri, &headers, "127.0.0.1");
        assert_eq!(context.client_name.as_deref(), Some("Emby Web"));
        assert_eq!(context.device_name.as_deref(), Some("Chromium"));
        assert_eq!(context.device_id.as_deref(), Some("device-1"));
        assert_eq!(
            playback_device_id("DeviceId=", &axum::http::HeaderMap::new()),
            None
        );
    }

    #[test]
    fn playback_stream_query_propagates_context_and_preserves_original_credentials() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(header::AUTHORIZATION, axum::http::HeaderValue::from_static(
            r#"MediaBrowser Client="Emby Web", Device="Chrome", DeviceName="Chromium", DeviceId="device-1", UserId="user-1", Token="synthetic-token""#,
        ));
        let query = playback_stream_query(
            "MediaSourceId=source-1&DeviceId=&X-Emby-Token=query-token&api_key=query-key",
            &headers,
        )
        .unwrap();
        assert_eq!(query_param(&query, "Client").as_deref(), Some("Emby Web"));
        assert_eq!(query_param(&query, "Device").as_deref(), Some("Chromium"));
        assert_eq!(query_param(&query, "DeviceId").as_deref(), Some("device-1"));
        assert_eq!(query_param(&query, "UserId").as_deref(), Some("user-1"));
        assert_eq!(
            query_param(&query, "X-Emby-Token").as_deref(),
            Some("query-token")
        );
        assert_eq!(query_param(&query, "api_key").as_deref(), Some("query-key"));
        assert_eq!(
            query_param(&query, "MediaSourceId").as_deref(),
            Some("source-1")
        );
        assert!(!query.contains(&emby_authorization_value(&headers, "Token").unwrap()));
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
        assert_eq!(query_param(&query, "Client").as_deref(), Some("Emby Web"));
        assert_eq!(query_param(&query, "UserId").as_deref(), Some("user-1"));
        assert_eq!(query_param(&query, "DeviceId").as_deref(), Some("device-1"));
    }

    #[test]
    fn playback_device_id_falls_back_to_emby_headers() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-emby-device-id",
            axum::http::HeaderValue::from_static("device-header"),
        );
        assert_eq!(
            playback_device_id("", &headers).as_deref(),
            Some("device-header")
        );

        headers.remove("x-emby-device-id");
        headers.insert(
            "x-emby-authorization",
            axum::http::HeaderValue::from_static(
                r#"MediaBrowser Client="Emby Web", DeviceId="device-auth""#,
            ),
        );
        assert_eq!(
            playback_device_id("", &headers).as_deref(),
            Some("device-auth")
        );
    }

    #[test]
    fn direct_url_detection_accepts_http_and_https_only() {
        assert!(is_direct_url("http://cdn.example.test/a.mkv"));
        assert!(is_direct_url("https://cdn.example.test/a.mkv"));
        assert!(!is_direct_url("/media/a.mkv"));
    }

    #[test]
    fn media_path_direct_link_inference_matches_stream_routing() {
        let config = test_config("http://127.0.0.1:8096", "http://127.0.0.1:5244", 8096);
        assert!(media_path_uses_direct_link(
            &config,
            "http://openlist.example.test/d/videos/movie.mkv"
        ));
        assert!(!media_path_uses_direct_link(
            &config,
            "https://cdn.example.test/movie.mkv"
        ));

        let mut without_openlist = config;
        without_openlist.openlist_addr = None;
        without_openlist.openlist_token = None;
        assert!(media_path_uses_direct_link(
            &without_openlist,
            "https://cdn.example.test/movie.mkv"
        ));
        assert!(!media_path_uses_direct_link(
            &without_openlist,
            "/media/movie.mkv"
        ));
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
                device_id: None,
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
                .get("default:10.0.0.88")
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
                device_id: None,
            },
        )
        .await
        .unwrap();

        assert!(!blocked);
    }

    #[tokio::test]
    async fn direct_link_cache_from_config_applies_global_and_server_policies() {
        let mut config = test_config("http://emby.test", "http://openlist.test", 18096);
        config.cache_enabled = false;
        config.cache_ttl_seconds = 60;
        config.cache_max_capacity = 100;
        config.servers.push(config::EmbyServerConfig {
            id: "enabled-server".to_string(),
            cache_enabled_override: Some(true),
            cache_ttl_seconds_override: Some(30),
            cache_max_capacity_override: Some(7),
            ..config::EmbyServerConfig::default()
        });

        let cache = direct_link_cache_from_config(&config);
        let enabled_key = "enabled-server:item:source:ua".to_string();
        let unknown_key = "unknown-server:item:source:ua".to_string();
        cache
            .set(enabled_key.clone(), "enabled-url".to_string())
            .await;
        cache
            .set(unknown_key.clone(), "unknown-url".to_string())
            .await;
        cache
            .set("legacy-key".to_string(), "legacy-url".to_string())
            .await;
        assert_eq!(
            cache.get(&enabled_key).await.as_deref(),
            Some("enabled-url")
        );
        assert_eq!(cache.get(&unknown_key).await, None);
        assert_eq!(cache.get("legacy-key").await, None);
        assert_eq!(cache.get_stats().await.max_capacity, 7);

        let mut config = test_config("http://emby.test", "http://openlist.test", 18096);
        config.cache_enabled = true;
        config.cache_ttl_seconds = 60;
        config.cache_max_capacity = 100;
        config.servers.push(config::EmbyServerConfig {
            id: "disabled-server".to_string(),
            cache_enabled_override: Some(false),
            ..config::EmbyServerConfig::default()
        });
        let cache = direct_link_cache_from_config(&config);
        let disabled_key = "disabled-server:item:source:ua".to_string();
        let unknown_key = "unknown-server:item:source:ua".to_string();
        cache
            .set(disabled_key.clone(), "disabled-url".to_string())
            .await;
        cache
            .set(unknown_key.clone(), "unknown-url".to_string())
            .await;
        cache
            .set("legacy-key".to_string(), "legacy-url".to_string())
            .await;
        assert_eq!(cache.get(&disabled_key).await, None);
        assert_eq!(
            cache.get(&unknown_key).await.as_deref(),
            Some("unknown-url")
        );
        assert_eq!(cache.get("legacy-key").await.as_deref(), Some("legacy-url"));
        assert_eq!(cache.get_stats().await.max_capacity, 100);

        let mut config = test_config("http://emby.test", "http://openlist.test", 18096);
        config.cache_enabled = true;
        config.cache_ttl_seconds = 60;
        config.cache_max_capacity = 100;
        config.servers.push(config::EmbyServerConfig {
            id: "overridden-server".to_string(),
            cache_ttl_seconds_override: Some(1),
            cache_max_capacity_override: Some(1),
            ..config::EmbyServerConfig::default()
        });
        let cache = direct_link_cache_from_config(&config);
        assert_eq!(cache.get_stats().await.max_capacity, 101);
        let overridden_key = "overridden-server:item:source:ua".to_string();
        let second_overridden_key = "overridden-server:item-2:source:ua".to_string();
        cache
            .set(overridden_key.clone(), "overridden-url".to_string())
            .await;
        cache
            .set(second_overridden_key, "overridden-url-2".to_string())
            .await;
        assert_eq!(cache.get_stats().await.capacity, 1);
        cache
            .set(overridden_key.clone(), "overridden-url".to_string())
            .await;
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert_eq!(cache.get(&overridden_key).await, None);
    }

    #[test]
    fn cache_domain_filters_match_host_only() {
        let mut config = test_config("http://emby.test", "http://openlist.test", 18096);
        config.cache_domain_filter_mode = "whitelist".to_string();
        config.cache_domain_whitelist = "*.115cdn.*".to_string();
        assert!(should_cache_direct_link(
            &config,
            "https://video.115cdn.example.test/path/file.mkv"
        ));
        assert!(!should_cache_direct_link(
            &config,
            "https://example.com/path/115cdn/file.mkv"
        ));

        config.cache_domain_filter_mode = "blacklist".to_string();
        config.cache_domain_whitelist = "sharepoint\n115".to_string();
        assert!(!should_cache_direct_link(
            &config,
            "https://files.sharepoint.example.test/download.aspx"
        ));
        assert!(should_cache_direct_link(
            &config,
            "https://cdn.example.com/a.mkv"
        ));

        config.cache_enabled = false;
        assert!(should_cache_direct_link(
            &config,
            "https://cdn.example.com/a.mkv"
        ));
    }

    #[test]
    fn disabled_strm_url_mapping_preserves_media_path() {
        let mut config = test_config("http://emby.test", "http://openlist.test", 18096);
        config.strm_url_mapping_rules =
            url_mapping::parse_rules("https://source.example.test => http://openlist.test/d")
                .unwrap();
        config.strm_url_mapping_enabled = false;

        assert_eq!(
            apply_strm_url_mappings(&config, "https://source.example.test/movie.mkv"),
            "https://source.example.test/movie.mkv"
        );
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
    fn web_ui_block_uses_upstream_path_normalization() {
        let mut config = test_config(
            "http://emby.example.test",
            "http://openlist.example.test",
            18096,
        );
        config.validate_for_storage().unwrap();
        config.servers[0].block_web_ui = true;

        for path in [
            "/web",
            "/WEB/index.html",
            "//web/index.html",
            "/x/../web/index.html",
            "/x/%2e%2e/web/index.html",
            "/%77eb/index.html",
            "/web%2Findex.html",
            "/x%2F..%2Fweb/index.html",
        ] {
            assert!(should_block_web_ui(&config, path).unwrap(), "{path}");
        }
        for path in [
            "/webhook",
            "/emby/web",
            "/web/../Items",
            "/web/%2e%2e/Items",
            "/web%2F..%2FItems",
        ] {
            assert!(!should_block_web_ui(&config, path).unwrap(), "{path}");
        }
    }

    #[test]
    fn proxy_base_url_uses_forwarded_headers_from_trusted_proxy() {
        let config = with_trusted_proxy(test_config(
            "http://emby.example.test",
            "http://openlist.example.test",
            8096,
        ));
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

        assert_eq!(
            proxy_base_url(&config, &headers, "192.0.2.10".parse().unwrap()),
            "https://media.example.com"
        );
    }

    #[test]
    fn proxy_base_url_ignores_forwarded_headers_from_untrusted_peer() {
        let config = with_trusted_proxy(test_config(
            "http://emby.example.test",
            "http://openlist.example.test",
            8096,
        ));
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

        assert_eq!(
            proxy_base_url(&config, &headers, "198.51.100.10".parse().unwrap()),
            "http://10.0.0.74:8096"
        );
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
                    .header(
                        header::AUTHORIZATION,
                        r#"MediaBrowser Client="Chrome Desktop", Device="Chrome", DeviceName="Chromium", DeviceId="d1", UserId="u1", Token="synthetic-browser-token""#,
                    )
                    .body(Body::empty())
                    .unwrap()
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
            Some(
                "/videos/item1/stream?UserId=u1&DeviceId=d1&Client=Chrome+Desktop&Device=Chromium&MediaSourceId=source1&Static=true&api_key="
            )
        );
        assert!(query_param(source["DirectStreamUrl"].as_str().unwrap(), "Token").is_none());
    }

    #[tokio::test]
    async fn playback_info_stream_url_carries_browser_context_into_diagnosis() {
        diagnostics::clean_diagnostics();
        let observed_paths = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let server = spawn_mock_server({
            let observed_paths = observed_paths.clone();
            move |request| {
                let observed_paths = observed_paths.clone();
                async move {
                    let path_query = request.uri().path_and_query().unwrap().as_str().to_string();
                    observed_paths.lock().unwrap().push(path_query.clone());
                    if request.uri().path() == "/Items/browser-item/PlaybackInfo" {
                        return response_json(json!({"MediaSources":[{
                            "Id":"source-browser", "Name":"STRM", "IsRemote":true,
                            "IsInfiniteStream":false, "SupportsTranscoding":true
                        }]}));
                    }
                    if request.uri().path() == "/Users/browser-user-456" {
                        return response_json(json!({"Name":"Synthetic browser user"}));
                    }
                    if request.uri().path() == "/Items" {
                        return response_json(
                            json!({"Items":[{"Name":"Synthetic browser item","MediaSources":[{
                                "Id":"source-browser", "Path":"https://unused.invalid/movie.mkv"
                            }]}]}),
                        );
                    }
                    response_text(StatusCode::NOT_FOUND, "not found")
                }
            }
        })
        .await;
        let mut config = test_config(&server, "http://unused.invalid", 18096);
        config.openlist_addr = None;
        config.openlist_token = None;
        let state = test_state(config, test_store());
        let app = build_app(state.clone());
        let authorization = r#"Emby UserId="browser-user-456", Client="Chrome Desktop", Device="Chrome", DeviceName="Chrome on macOS", DeviceId="browser-device-123", Token="synthetic-browser-token""#;
        let playback_info = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/Items/browser-item/PlaybackInfo")
                    .method(Method::POST)
                    .header(header::AUTHORIZATION, authorization)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(playback_info.status(), StatusCode::OK);
        let rewritten = read_json(playback_info).await;
        let direct_url = rewritten["MediaSources"][0]["DirectStreamUrl"]
            .as_str()
            .unwrap();
        assert!(direct_url.contains("Client=Chrome+Desktop"));
        assert!(direct_url.contains("Device=Chrome+on+macOS"));
        assert!(direct_url.contains("DeviceId=browser-device-123"));
        assert!(direct_url.contains("UserId=browser-user-456"));
        let header_token = media_browser_auth_value(authorization, "Token").unwrap();
        assert!(!direct_url.contains(&header_token));

        let stream = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri(direct_url)
                    .header(header::USER_AGENT, "Mozilla/5.0 Chrome/156.0 Safari/537.36")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stream.status(), StatusCode::FOUND);
        let record = diagnostics::get_diagnosis_records()
            .into_iter()
            .find(|record| record.media_id == "browser-item")
            .unwrap();
        assert_eq!(
            record.context.client_name.as_deref(),
            Some("Chrome Desktop")
        );
        assert_eq!(
            record.context.device_name.as_deref(),
            Some("Chrome on macOS")
        );
        assert_eq!(
            record.context.device_id.as_deref(),
            Some("browser-device-123")
        );
        assert_eq!(
            record.context.user_name.as_deref(),
            Some("Synthetic browser user")
        );
        assert!(
            !serde_json::to_string(&record)
                .unwrap()
                .contains(&header_token)
        );
        let paths = observed_paths.lock().unwrap();
        let items_query = paths
            .iter()
            .find(|path| path.starts_with("/Items?"))
            .unwrap();
        assert!(items_query.contains("api_key=emby-key"));
        assert!(!items_query.contains(&header_token));
        drop(paths);
        let cached = app
            .oneshot(
                HttpRequest::builder()
                    .uri(direct_url)
                    .header(header::USER_AGENT, "Mozilla/5.0 Chrome/156.0 Safari/537.36")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(cached.status(), StatusCode::FOUND);
        let hit = diagnostics::get_diagnosis_records().pop().unwrap();
        assert!(hit.cache_hit);
        assert_eq!(hit.context.client_name, record.context.client_name);
        assert_eq!(hit.context.device_name, record.context.device_name);
        assert_eq!(hit.context.device_id, record.context.device_id);
        assert_eq!(
            observed_paths
                .lock()
                .unwrap()
                .iter()
                .filter(|path| path.starts_with("/Items?"))
                .count(),
            1
        );
        state.telemetry_writes.shutdown().unwrap();
        diagnostics::clean_diagnostics();
    }

    fn assert_capture_correlated_logs(state: &AppState, request_id: &str) {
        state.telemetry_writes.shutdown().unwrap();
        let requests = state
            .settings_store
            .list_proxy_request_details(crate::db::ProxyRequestDetailFilter {
                server_id: None,
                path_type: Some("video_stream"),
                keyword: Some(request_id),
                since_ms: None,
                until_ms: None,
                limit: 100,
            })
            .unwrap();
        let activities = state
            .activity_log
            .list_filtered(crate::activity_log::ActivityLogFilter {
                server_id: None,
                kind: None,
                level: None,
                keyword: Some(request_id),
                since_ms: None,
                until_ms: None,
                limit: 100,
            });
        assert_eq!(requests.len(), 1, "exactly one correlated request log");
        assert_eq!(activities.len(), 1, "exactly one correlated activity log");
        assert!(requests[0].detail.contains(request_id));
        assert!(activities[0].detail.contains(request_id));
        assert!(!requests[0].detail.contains("token"));
        assert!(!activities[0].detail.contains("token"));
        if requests[0].outcome == "请求取消" {
            assert_eq!(
                requests[0].status_code, 0,
                "no response, not an HTTP success"
            );
            assert_eq!(activities[0].level, "warn");
            assert!(requests[0].detail.contains("outcome=cancelled"));
        }
    }

    #[tokio::test]
    async fn capture_proxy_result_waits_for_real_headers_without_reading_media_stream() {
        diagnostics::clean_diagnostics();
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(
                    json!({"Items": [{"Name": "Synthetic movie", "MediaSources": []}]}),
                );
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
            response_text(StatusCode::PARTIAL_CONTENT, "synthetic media body")
        })
        .await;
        let state = test_state(
            test_config(&emby, "http://unused.example.test", 18096),
            test_store(),
        );
        let app = build_app(state.clone());
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            app.oneshot(
                HttpRequest::builder()
                    .uri("/videos/capture-proxy/stream?UserName=SyntheticUser")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let record = diagnostics::get_diagnosis_records()
            .into_iter()
            .find(|r| r.media_id == "capture-proxy")
            .unwrap();
        assert_eq!(record.result.outcome.as_deref(), Some("proxy"));
        assert_eq!(record.result.status_code, Some(206));
        assert_eq!(record.result.upstream_status, Some(206));
        assert!(
            record
                .result
                .reason
                .as_deref()
                .unwrap()
                .contains("source path not found")
        );
        let step = record
            .steps
            .iter()
            .find(|s| s.name == proxy::STEP_PROXY)
            .unwrap();
        assert_eq!(step.upstream_status, Some(206));
        assert!(step.response_headers_ms.unwrap() >= 10);
        assert!(
            step.response_body_ms.is_none(),
            "media body must not be buffered or timed"
        );
        assert!(record.duration_ms.unwrap() >= 10);
        drop(response);
        state.telemetry_writes.shutdown().unwrap();
    }

    #[tokio::test]
    async fn capture_upstream_failure_records_actual_status_and_one_correlated_log_pair() {
        diagnostics::clean_diagnostics();
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_text(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "synthetic response body must not be logged",
                );
            }
            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let state = test_state(
            test_config(&emby, "http://unused.example.test", 18096),
            test_store(),
        );
        let response = build_app(state.clone())
            .oneshot(
                HttpRequest::builder()
                    .uri("/videos/capture-failure/stream?UserName=SyntheticUser")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let record = diagnostics::get_diagnosis_records()
            .into_iter()
            .find(|r| r.media_id == "capture-failure")
            .unwrap();
        assert_eq!(record.result.outcome.as_deref(), Some("failed"));
        assert_eq!(record.result.status_code, Some(502));
        assert_eq!(record.result.upstream_status, Some(503));
        assert_eq!(record.result.error_kind.as_deref(), Some("upstream_http"));
        let step = record
            .steps
            .iter()
            .find(|s| s.name == proxy::STEP_EMBY_PATH)
            .unwrap();
        assert_eq!(step.state.as_deref(), Some("failed"));
        assert_eq!(step.upstream_status, Some(503));
        assert!(step.response_headers_ms.is_some());
        assert!(step.response_body_ms.is_none());
        assert!(
            !serde_json::to_string(&record)
                .unwrap()
                .contains("synthetic response body")
        );
        assert_capture_correlated_logs(&state, &record.request_id);
    }

    #[tokio::test]
    async fn capture_context_result_cache_hit_keeps_unknown_names_and_real_ttl() {
        diagnostics::clean_diagnostics();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = calls.clone();
        let emby = spawn_mock_server(move |request| {
            let calls = observed_calls.clone();
            async move {
                if request.uri().path() == "/Items" {
                    calls.fetch_add(1, Ordering::Relaxed);
                    return response_json(json!({"Items": [{
                        "Name": "Synthetic episode", "SeriesName": "Synthetic series", "ParentIndexNumber": 2, "IndexNumber": 3,
                        "MediaSources": [{"Id": "source1", "Path": "https://cdn.example.test/movie.mkv?opaque=synthetic-signature#fragment"}]
                    }]}));
                }
                response_text(StatusCode::NOT_FOUND, "not found")
            }
        }).await;
        let mut config = test_config(&emby, "http://unused.example.test", 18096);
        config.openlist_addr = None;
        config.openlist_token = None;
        let state = test_state(config, test_store());
        let app = build_app(state.clone());
        let request = || {
            HttpRequest::builder()
            .uri("/videos/capture-context/stream?MediaSourceId=source1&UserName=SyntheticUser&DeviceId=device1&Client=SyntheticClient&Device=SyntheticDevice")
            .header("Range", "bytes=0-1023").header(header::USER_AGENT, "SyntheticPlayer")
            .body(Body::empty()).unwrap()
        };
        assert_eq!(
            app.clone().oneshot(request()).await.unwrap().status(),
            StatusCode::FOUND
        );
        assert_eq!(
            app.oneshot(request()).await.unwrap().status(),
            StatusCode::FOUND
        );
        let records: Vec<_> = diagnostics::get_diagnosis_records()
            .into_iter()
            .filter(|r| r.media_id == "capture-context")
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(
            calls.load(Ordering::Relaxed),
            1,
            "cache hit must not query Items"
        );
        let miss = &records[0];
        assert_eq!(
            miss.context.media_name.as_deref(),
            Some("Synthetic episode")
        );
        assert_eq!(
            miss.context.series_name.as_deref(),
            Some("Synthetic series")
        );
        assert_eq!(miss.context.season_number, Some(2));
        assert_eq!(miss.context.episode_number, Some(3));
        assert_eq!(miss.context.user_name.as_deref(), Some("SyntheticUser"));
        assert_eq!(miss.context.client_name.as_deref(), Some("SyntheticClient"));
        assert_eq!(miss.context.device_name.as_deref(), Some("SyntheticDevice"));
        assert_eq!(miss.context.device_id.as_deref(), Some("device1"));
        assert_eq!(miss.context.media_source_id.as_deref(), Some("source1"));
        assert_eq!(miss.context.method.as_deref(), Some("GET"));
        assert_eq!(miss.context.range.as_deref(), Some("bytes=0-1023"));
        assert_eq!(miss.result.outcome.as_deref(), Some("redirect"));
        assert_eq!(miss.result.status_code, Some(302));
        assert_eq!(miss.result.target_host.as_deref(), Some("cdn.example.test"));
        assert_eq!(
            miss.paths.final_url.as_deref(),
            Some("https://cdn.example.test/movie.mkv")
        );
        assert_eq!(miss.cache.status.as_deref(), Some("miss"));
        assert_eq!(miss.cache.stored, Some(true));
        assert!(miss.cache.remaining_ttl_ms.is_none());
        assert!(miss.timing.unaccounted_ms.unwrap() <= miss.duration_ms.unwrap());
        let item_step = miss
            .steps
            .iter()
            .find(|s| s.name == proxy::STEP_EMBY_PATH)
            .unwrap();
        assert_eq!(item_step.upstream_status, Some(200));
        assert!(item_step.response_headers_ms.is_some() && item_step.response_body_ms.is_some());
        let hit = &records[1];
        assert_eq!(hit.cache.status.as_deref(), Some("hit"));
        assert!(
            hit.context.media_name.is_none(),
            "no ID masquerading as a name"
        );
        assert!(hit.cache.remaining_ttl_ms.unwrap() <= hit.cache.ttl_seconds.unwrap() * 1000);
        assert!(
            hit.steps
                .iter()
                .filter(|s| matches!(
                    s.name.as_str(),
                    proxy::STEP_EMBY_PATH
                        | proxy::STEP_MAPPING
                        | proxy::STEP_OPENLIST
                        | proxy::STEP_RAW_URL
                ))
                .all(|s| s.state.as_deref() == Some("skipped") && s.duration_ms == 0)
        );
        assert_ne!(miss.request_id, hit.request_id);
        assert!(
            !serde_json::to_string(&records)
                .unwrap()
                .contains("synthetic-signature")
        );
        assert_capture_correlated_logs(&state, &miss.request_id);
    }

    #[tokio::test]
    async fn video_stream_cancellation_records_real_emby_step() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return std::future::pending::<Response>().await;
            }
            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let openlist =
            spawn_mock_server(|_| async { response_text(StatusCode::NOT_FOUND, "not found") })
                .await;
        let state = test_state(test_config(&emby, &openlist, 18096), test_store());
        let app = build_app(state.clone());
        let task = tokio::spawn(
            app.oneshot(
                HttpRequest::builder()
                    .uri("/videos/item-cancel/stream?MediaSourceId=source1")
                    .header(header::USER_AGENT, "test-player token=ua-secret")
                    .body(Body::empty())
                    .unwrap(),
            ),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        task.abort();
        let _ = task.await;

        let record = diagnostics::get_diagnosis_records()
            .into_iter()
            .find(|record| record.media_id == "item-cancel")
            .expect("real handle_video_stream cancellation diagnosis");
        assert_eq!(record.failure_node.as_deref(), Some("Emby Path"));
        assert_eq!(record.result.outcome.as_deref(), Some("cancelled"));
        assert!(record.result.status_code.is_none());
        assert_eq!(
            diagnostics::get_diagnosis_records()
                .iter()
                .filter(|r| r.request_id == record.request_id)
                .count(),
            1
        );
        assert_capture_correlated_logs(&state, &record.request_id);
        assert!(
            record
                .steps
                .iter()
                .find(|step| step.name == proxy::STEP_EMBY_PATH)
                .unwrap()
                .details
                .contains("cancelled")
        );
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("ua-secret"));
    }

    #[tokio::test]
    async fn video_stream_cancellation_records_real_openlist_step() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{"MediaSources": [{"Id": "source1", "Path": "http://openlist.local/d/movie/test.mkv"}]}]
                }));
            }
            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let openlist = spawn_mock_server(|request| async move {
            if request.uri().path() == "/api/fs/get" {
                return std::future::pending::<Response>().await;
            }
            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let app = test_app(&emby, &openlist, 18096);
        let task = tokio::spawn(
            app.oneshot(
                HttpRequest::builder()
                    .uri("/videos/item-openlist-cancel/stream?MediaSourceId=source1")
                    .header(header::USER_AGENT, "test-player token=ua-secret")
                    .body(Body::empty())
                    .unwrap(),
            ),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        task.abort();
        let _ = task.await;

        let record = diagnostics::get_diagnosis_records()
            .into_iter()
            .find(|record| record.media_id == "item-openlist-cancel")
            .expect("real OpenList cancellation diagnosis");
        assert_eq!(record.failure_node.as_deref(), Some("OpenList"));
        assert!(
            record
                .steps
                .iter()
                .find(|step| step.name == proxy::STEP_OPENLIST)
                .unwrap()
                .details
                .contains("cancelled")
        );
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("ua-secret"));
    }

    #[tokio::test]
    async fn video_stream_redirects_to_openlist_raw_url() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();
        let seen_user_agent = Arc::new(std::sync::Mutex::new(String::new()));
        let seen_user_agent_for_server = seen_user_agent.clone();
        let emby = spawn_mock_server(move |request| {
            let seen_user_agent = seen_user_agent_for_server.clone();
            async move {
                if request.uri().path() == "/Items" {
                    if let Some(user_agent) = request
                        .headers()
                        .get(header::USER_AGENT)
                        .and_then(|value| value.to_str().ok())
                    {
                        *seen_user_agent.lock().unwrap() = user_agent.to_string();
                    }
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
            }
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
        assert_eq!(seen_user_agent.lock().unwrap().as_str(), "test-player");
        let record = diagnostics::get_diagnosis_records()
            .into_iter()
            .find(|record| record.media_id == "item1")
            .expect("real playback diagnosis");
        assert_eq!(record.ua, "test-player");
        assert_eq!(record.failure_node, None);
        assert_eq!(record.cache_hit, false);
        assert_eq!(
            record
                .steps
                .iter()
                .map(|step| step.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "请求上下文",
                "播放策略",
                "Emby Path",
                "映射",
                "OpenList",
                "raw_url",
                "内部跳转",
                "缓存",
                "302"
            ]
        );
        assert!(record.steps.iter().all(|step| step.duration_ms < 10_000));
    }

    #[tokio::test]
    async fn video_stream_diagnosis_records_mapped_openlist_cache_and_redaction() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{
                        "MediaSources": [{
                            "Id": "source1",
                            "Path": "https://source.example.test/d/movie/test.mkv?api_key=emby-path-secret&quality=1080"
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
                    "data": {
                        "raw_url": "https://cdn.example.test/movie.mkv?token=raw-secret&quality=2160"
                    }
                }));
            }
            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let mappings = format!("https://source.example.test => {openlist}");
        let mut config = test_config(&emby, &openlist, 18096);
        config.strm_url_mappings = mappings.clone();
        config.strm_url_mapping_rules = url_mapping::parse_rules(&mappings).unwrap();
        let app = build_app(test_state(config, test_store()));
        let request = || {
            HttpRequest::builder()
                .uri("/videos/item-chain/stream?MediaSourceId=source1&api_key=request-secret")
                .header(header::USER_AGENT, "diagnostic-player/1.0 token=ua-secret")
                .body(Body::empty())
                .unwrap()
        };

        let first = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(first.status(), StatusCode::FOUND);
        assert_eq!(
            first.headers().get(header::LOCATION).unwrap(),
            "https://cdn.example.test/movie.mkv?token=raw-secret&quality=2160"
        );
        assert_eq!(
            app.oneshot(request()).await.unwrap().status(),
            StatusCode::FOUND
        );

        let records: Vec<_> = diagnostics::get_diagnosis_records()
            .into_iter()
            .filter(|record| record.media_id == "item-chain")
            .collect();
        assert_eq!(records.len(), 2);
        let miss = &records[0];
        assert_eq!(miss.failure_node, None);
        assert!(!miss.cache_hit);
        assert!(miss.sanitized_errors.is_empty());
        assert_eq!(miss.steps.len(), 9);
        let step = |name| miss.steps.iter().find(|step| step.name == name).unwrap();
        assert!(
            step(proxy::STEP_EMBY_PATH)
                .details
                .contains("/d/movie/test.mkv")
        );
        assert!(step(proxy::STEP_MAPPING).details.contains("matched rule#1"));
        assert!(step(proxy::STEP_MAPPING).details.contains(&openlist));
        assert!(
            step(proxy::STEP_OPENLIST)
                .details
                .contains("path=/movie/test.mkv")
        );
        assert!(
            step(proxy::STEP_RAW_URL)
                .details
                .contains("cdn.example.test/movie.mkv")
        );
        assert_eq!(step(proxy::STEP_CACHE).details, "miss; stored");
        assert!(
            step(proxy::STEP_REDIRECT)
                .details
                .contains("redirect location=")
        );

        let hit = &records[1];
        assert!(hit.cache_hit);
        assert!(hit.sanitized_errors.is_empty());
        assert_eq!(
            hit.steps
                .iter()
                .find(|step| step.name == proxy::STEP_CACHE)
                .unwrap()
                .details,
            "hit"
        );
        assert!(
            hit.steps
                .iter()
                .find(|step| step.name == proxy::STEP_REDIRECT)
                .unwrap()
                .details
                .contains("redirect location=")
        );

        let json = serde_json::to_string(&records).unwrap();
        for secret in [
            "emby-path-secret",
            "raw-secret",
            "request-secret",
            "ua-secret",
        ] {
            assert!(!json.contains(secret), "secret leaked: {json}");
        }
        assert!(json.contains(diagnostics::REDACTED_PLACEHOLDER));
        assert!(!json.contains("]]"));
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
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();
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
        let record = diagnostics::get_diagnosis_records()
            .into_iter()
            .find(|record| record.media_id == "item1")
            .expect("mapping diagnosis");
        let mapping = record
            .steps
            .iter()
            .find(|step| step.name == "映射")
            .expect("mapping step");
        assert!(mapping.details.contains("matched rule#1"));
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
    async fn capture_internal_redirect_observes_one_hop_and_fallback_without_extra_requests() {
        diagnostics::clean_diagnostics();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = calls.clone();
        let media = spawn_mock_server(move |request| {
            let calls = observed_calls.clone();
            async move {
                calls.fetch_add(1, Ordering::Relaxed);
                if request.uri().path() == "/redirect" {
                    return (
                        StatusCode::FOUND,
                        [(
                            header::LOCATION,
                            "/never-requested?signature=synthetic-signature",
                        )],
                    )
                        .into_response();
                }
                response_text(StatusCode::SERVICE_UNAVAILABLE, "synthetic-private-body")
            }
        })
        .await;
        let mut config = test_config(
            "http://unused.example.test",
            "http://unused.example.test",
            18096,
        );
        config.enable_internal_redirect = true;
        let client = build_proxy_http_client().unwrap();
        for (path, status, state, count) in [
            ("redirect", 302, "recorded", Some(1)),
            ("failure", 503, "failed", None),
        ] {
            let url = format!("{media}/{path}");
            let mut trace = proxy::PlaybackDiagnosisTrace::new("s", path, "SyntheticPlayer");
            let resolved = maybe_resolve_internal_redirect(
                &client,
                &config,
                &url,
                "SyntheticPlayer",
                &mut trace,
            )
            .await;
            trace.set_redirect(&resolved, "redirect response only");
            trace.finish().await;
            let record = diagnostics::get_diagnosis_records()
                .into_iter()
                .find(|r| r.media_id == path)
                .unwrap();
            assert_eq!(record.result.outcome.as_deref(), Some("redirect"));
            assert_eq!(record.result.status_code, Some(302));
            assert_eq!(record.timing.redirect_count, count);
            let step = record
                .steps
                .iter()
                .find(|s| s.name == proxy::STEP_INTERNAL_REDIRECT)
                .unwrap();
            assert_eq!(step.upstream_status, Some(status));
            assert_eq!(step.state.as_deref(), Some(state));
            assert!(step.response_headers_ms.is_some() && step.response_body_ms.is_none());
            assert!(
                !serde_json::to_string(&record)
                    .unwrap()
                    .contains("synthetic-signature")
            );
            if path == "failure" {
                assert_eq!(resolved, url);
                assert_eq!(step.error_kind.as_deref(), Some("upstream_http"));
                assert!(step.reason.as_deref().unwrap().contains("original URL"));
                assert!(
                    record.failure_node.is_none(),
                    "fallback did not fail the overall request"
                );
            }
        }
        assert_eq!(
            calls.load(Ordering::Relaxed),
            2,
            "only the original probes; never the redirect target"
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
        let client = build_proxy_http_client().unwrap();

        assert_eq!(
            internal_redirect::resolve_redirect_location(&client, &media_url, 15, "test-player")
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
        let client = build_proxy_http_client().unwrap();

        let resolved = internal_redirect::resolve_redirect_location(
            &client,
            &media_url,
            15,
            "Emby Web Test/1.0",
        )
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
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();
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
        let record = diagnostics::get_diagnosis_records()
            .into_iter()
            .find(|record| record.media_id == "item1")
            .expect("OpenList failure diagnosis");
        assert_eq!(record.failure_node.as_deref(), Some("OpenList"));
        assert!(record.steps.iter().all(|step| step.name != "raw_url"));
        assert!(
            record
                .steps
                .iter()
                .find(|step| step.name == "OpenList")
                .is_some_and(|step| step.details.contains("code 500"))
        );
        assert_eq!(record.result.upstream_status, Some(200));
        assert_eq!(record.result.error_kind.as_deref(), Some("upstream_api"));
        let step = record
            .steps
            .iter()
            .find(|s| s.name == proxy::STEP_OPENLIST)
            .unwrap();
        assert!(step.response_headers_ms.is_some() && step.response_body_ms.is_some());
    }

    #[tokio::test]
    async fn video_stream_records_openlist_http_failure_node() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{"MediaSources": [{"Id": "source1", "Path": "http://openlist.local/d/movie/test.mkv"}]}]
                }));
            }
            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let openlist = spawn_mock_server(|request| async move {
            if request.uri().path() == "/api/fs/get" {
                return response_text(StatusCode::SERVICE_UNAVAILABLE, "token=upstream-secret");
            }
            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let app = test_app(&emby, &openlist, 18096);
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/videos/item1/stream?MediaSourceId=source1")
                    .header(header::USER_AGENT, "test-player token=ua-secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let record = diagnostics::get_diagnosis_records()
            .into_iter()
            .find(|record| record.media_id == "item1")
            .expect("HTTP OpenList failure diagnosis");
        assert_eq!(record.failure_node.as_deref(), Some("OpenList"));
        assert_eq!(record.result.outcome.as_deref(), Some("failed"));
        assert_eq!(record.result.upstream_status, Some(503));
        assert_eq!(record.result.error_kind.as_deref(), Some("upstream_http"));
        let step = record
            .steps
            .iter()
            .find(|step| step.name == proxy::STEP_OPENLIST)
            .unwrap();
        assert_eq!(step.state.as_deref(), Some("failed"));
        assert!(step.response_headers_ms.is_some());
        assert!(step.response_body_ms.is_none());
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("upstream-secret"));
        assert!(!json.contains("ua-secret"));
    }

    #[tokio::test]
    async fn video_stream_records_raw_url_missing_node() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{"MediaSources": [{"Id": "source1", "Path": "http://openlist.local/d/movie/test.mkv"}]}]
                }));
            }
            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let openlist = spawn_mock_server(|request| async move {
            if request.uri().path() == "/api/fs/get" {
                return response_json(json!({"code": 200, "data": {}}));
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
        let record = diagnostics::get_diagnosis_records()
            .into_iter()
            .find(|record| record.media_id == "item1")
            .expect("raw_url failure diagnosis");
        assert_eq!(record.failure_node.as_deref(), Some("raw_url"));
        assert!(
            record
                .steps
                .iter()
                .any(|step| step.name == "OpenList" && step.details.contains("matched"))
        );
    }

    #[tokio::test]
    async fn video_stream_colon_server_cache_policy_uses_versioned_keys() {
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{"MediaSources": [{"Id": "source1", "Path": "http://openlist.local/d/movie/test.mkv"}]}]
                }));
            }
            response_text(StatusCode::NOT_FOUND, "not found")
        }).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let mock_calls = calls.clone();
        let openlist = spawn_mock_server(move |request| {
            let calls = mock_calls.clone();
            async move {
                if request.uri().path() == "/api/fs/get" {
                    calls.fetch_add(1, Ordering::Relaxed);
                    return response_json(json!({
                        "code": 200,
                        "data": {"raw_url": "https://cdn.example.test/movie.mkv?token=private"}
                    }));
                }
                response_text(StatusCode::NOT_FOUND, "not found")
            }
        })
        .await;
        let mut config = test_config(&emby, &openlist, 18096);
        config.servers = ["a", "a:b"]
            .into_iter()
            .enumerate()
            .map(|(index, id)| config::EmbyServerConfig {
                id: id.into(),
                name: id.into(),
                enabled: true,
                emby_host: emby.clone(),
                emby_api_key: "synthetic-key".into(),
                port: 18096 + index as u16,
                cache_enabled_override: Some(id == "a"),
                ..config::EmbyServerConfig::default()
            })
            .collect();
        let cache = direct_link_cache_from_config(&config);
        let mut state = test_state(config, test_store());
        state.cache = Arc::new(RwLock::new(cache.clone()));
        for server in ["a", "a:b"] {
            state.proxy_server_id = Some(server.into());
            let app = build_app(state.clone());
            for _ in 0..2 {
                let response = app
                    .clone()
                    .oneshot(
                        HttpRequest::builder()
                            .uri("/videos/colon-cache-item/stream?MediaSourceId=source1")
                            .header(header::USER_AGENT, "private-UA:secret")
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FOUND);
            }
        }
        assert_eq!(calls.load(Ordering::Relaxed), 3);
        let expected =
            DirectLinkCache::key("a", "colon-cache-item", "source1", "private-UA:secret");
        assert!(cache.get(&expected).await.is_some());
        assert_eq!(
            cache
                .get(&DirectLinkCache::key(
                    "a:b",
                    "colon-cache-item",
                    "source1",
                    "private-UA:secret"
                ))
                .await,
            None
        );
        let views = cache.list_entry_views(None).await;
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].server_id, "a");
        let serialized = serde_json::to_string(&views).unwrap();
        assert!(!serialized.contains("private"));
        assert!(!serialized.contains("secret"));
    }

    #[tokio::test]
    async fn video_stream_diagnosis_distinguishes_cache_miss_and_hit() {
        diagnostics::init_diagnostics();
        diagnostics::clean_diagnostics();
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({
                    "Items": [{"MediaSources": [{"Id": "source1", "Path": "http://openlist.local/d/movie/test.mkv"}]}]
                }));
            }
            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let openlist = spawn_mock_server(|request| async move {
            if request.uri().path() == "/api/fs/get" {
                return response_json(json!({
                    "code": 200,
                    "data": {"raw_url": "https://cdn.example.test/movie.mkv"}
                }));
            }
            response_text(StatusCode::NOT_FOUND, "not found")
        })
        .await;
        let app = test_app(&emby, &openlist, 18096);
        let request = || {
            HttpRequest::builder()
                .uri("/videos/item1/stream?MediaSourceId=source1")
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(
            app.clone().oneshot(request()).await.unwrap().status(),
            StatusCode::FOUND
        );
        assert_eq!(
            app.oneshot(request()).await.unwrap().status(),
            StatusCode::FOUND
        );
        let records: Vec<_> = diagnostics::get_diagnosis_records()
            .into_iter()
            .filter(|record| record.media_id == "item1")
            .collect();
        assert_eq!(records.len(), 2);
        assert!(!records[0].cache_hit);
        assert!(
            records[0]
                .steps
                .last()
                .is_some_and(|step| step.details.contains("redirect"))
        );
        assert!(records[1].cache_hit);
        assert_eq!(
            records[1]
                .steps
                .iter()
                .find(|step| step.name == proxy::STEP_CACHE)
                .unwrap()
                .details,
            "hit"
        );
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
            cache_enabled: true,
            cache_domain_filter_mode: "off".to_string(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect: false,
            internal_redirect_timeout_seconds: 15,
            strm_url_mappings: String::new(),
            strm_url_mapping_enabled: true,
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
            cache_enabled: true,
            cache_domain_filter_mode: "off".to_string(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect: false,
            internal_redirect_timeout_seconds: 15,
            strm_url_mappings: String::new(),
            strm_url_mapping_enabled: true,
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
            cache_enabled: true,
            cache_domain_filter_mode: "off".to_string(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect,
            internal_redirect_timeout_seconds: 15,
            strm_url_mappings: String::new(),
            strm_url_mapping_enabled: true,
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
            cache_enabled: true,
            cache_domain_filter_mode: "off".to_string(),
            cache_domain_whitelist: String::new(),
            cache_domain_blacklist: String::new(),
            enable_internal_redirect: false,
            internal_redirect_timeout_seconds: 15,
            strm_url_mappings: mappings.to_string(),
            strm_url_mapping_enabled: true,
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

    async fn unused_proxy_port() -> u16 {
        TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[tokio::test]
    async fn hot_reload_keeps_active_stream_beyond_shutdown_deadline() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("http://{}", upstream.local_addr().unwrap());
        let (finish_tx, finish_rx) = oneshot::channel();
        let upstream_task = tokio::spawn(async move {
            let (mut socket, _) = upstream.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buf = [0u8; 1024];
            while !request.windows(4).any(|value| value == b"\r\n\r\n") {
                let n = socket.read(&mut buf).await.unwrap();
                assert!(n > 0);
                request.extend_from_slice(&buf[..n]);
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\nbefore\r\n").await.unwrap();
            finish_rx.await.unwrap();
            socket.write_all(b"5\r\nafter\r\n0\r\n\r\n").await.unwrap();
        });
        let old_port = unused_proxy_port().await;
        let mut config = Config::default_runtime();
        config.servers = vec![config::EmbyServerConfig {
            id: "drain-test".into(),
            name: "Drain test".into(),
            emby_host: host,
            emby_api_key: "synthetic-key".into(),
            port: old_port,
            enabled: true,
            ..config::EmbyServerConfig::default()
        }];
        let state = test_state(config, test_store());
        let manager = ProxyManager::new();
        manager.ensure_running(state.clone()).await.unwrap();
        let client = reqwest::Client::new();
        let mut response = client
            .get(format!("http://127.0.0.1:{old_port}/qa-stream"))
            .send()
            .await
            .unwrap();
        let first = response.chunk().await.unwrap().unwrap();
        assert_eq!(first.as_ref(), b"before");

        let new_port = unused_proxy_port().await;
        state.config.write().await.servers[0].port = new_port;
        tokio::time::timeout(
            Duration::from_secs(3),
            manager.ensure_running(state.clone()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(manager.running.lock().await["drain-test"].port, new_port);
        // Cross the old cutoff using an actual open HTTP response. Hot reload
        // must return promptly without imposing the process shutdown deadline.
        tokio::time::sleep(PROXY_DRAIN_TIMEOUT + Duration::from_millis(250)).await;
        assert!(!manager.draining.lock().await.is_empty());
        finish_tx.send(()).unwrap();
        let remaining = tokio::time::timeout(Duration::from_secs(3), response.bytes())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(remaining.as_ref(), b"after");
        upstream_task.await.unwrap();
        manager.shutdown().await;
        state.telemetry_writes.shutdown().unwrap();
    }

    #[tokio::test]
    async fn proxy_reload_reuses_listener_and_failed_bind_can_roll_back() {
        let old_port = unused_proxy_port().await;
        let occupied = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let occupied_port = occupied.local_addr().unwrap().port();
        let mut config = Config::default_runtime();
        config.servers = vec![config::EmbyServerConfig {
            id: "reload-test".into(),
            name: "Reload test".into(),
            emby_host: "http://127.0.0.1:1".into(),
            emby_api_key: "synthetic-key".into(),
            port: old_port,
            enabled: true,
            ..config::EmbyServerConfig::default()
        }];
        let state = test_state(config, test_store());
        let manager = ProxyManager::new();
        manager.ensure_running(state.clone()).await.unwrap();
        let before = manager.running.lock().await["reload-test"].handle.id();
        state.config.write().await.servers[0].name = "Renamed".into();
        manager.ensure_running(state.clone()).await.unwrap();
        assert_eq!(
            manager.running.lock().await["reload-test"].handle.id(),
            before
        );
        state.config.write().await.servers[0].port = occupied_port;
        assert!(manager.ensure_running(state.clone()).await.is_err());
        state.config.write().await.servers[0].port = old_port;
        manager.ensure_running(state.clone()).await.unwrap();
        assert_eq!(manager.running.lock().await["reload-test"].port, old_port);
        manager.shutdown().await;
        state.telemetry_writes.shutdown().unwrap();
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

    #[tokio::test]
    async fn protected_json_routes_authenticate_before_body_extraction() {
        let config = test_config("http://127.0.0.1:18090", "http://127.0.0.1:8096", 18090);
        let store = test_store();
        let token = "management-auth-test-token";
        store
            .create_session(1, &auth::hash_token(token), auth::now_ts() + 3600)
            .unwrap();
        let state = test_state(config, store);
        let app = build_management_app(state).layer(axum::Extension(ConnectInfo(
            SocketAddr::from(([127, 0, 0, 1], 50000)),
        )));

        let request = |body: &'static str| {
            HttpRequest::builder()
                .method("POST")
                .uri("/api/library/tasks/task-1/start")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap()
        };

        for body in ["{}", "{", "", "[]"] {
            assert_eq!(
                app.clone().oneshot(request(body)).await.unwrap().status(),
                StatusCode::UNAUTHORIZED
            );
        }

        let authenticated = HttpRequest::builder()
            .method("POST")
            .uri("/api/library/tasks/task-1/start")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{"))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(authenticated).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );

        let health = HttpRequest::builder()
            .uri("/healthz")
            .body(Body::from("{"))
            .unwrap();
        assert_eq!(
            app.clone().oneshot(health).await.unwrap().status(),
            StatusCode::OK
        );

        for path in ["/api/setup", "/api/login"] {
            let public_request = HttpRequest::builder()
                .method("POST")
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from("{"))
                .unwrap();
            assert_eq!(
                app.clone().oneshot(public_request).await.unwrap().status(),
                StatusCode::BAD_REQUEST,
                "public route {path} must retain JSON extraction errors"
            );
        }
    }

    #[tokio::test]
    async fn p2_management_routes_are_mounted_with_expected_methods() {
        let config = test_config("http://127.0.0.1:18090", "http://127.0.0.1:8096", 18090);
        let store = test_store();
        let state = test_state(config, store);
        let mut app = build_management_app(state);
        let test_routes = vec![
            ("/api/library/libraries", "GET"),
            ("/api/library/libraries/validate", "POST"),
            ("/api/library/tasks", "GET"),
            ("/api/library/tasks/task-1/failure", "GET"),
            ("/api/library/tasks/task-1/start", "POST"),
            ("/api/library/tasks/task-1/cancel", "POST"),
            ("/api/sessions/session-1/stop", "POST"),
            ("/api/sessions/session-1/message", "POST"),
            ("/api/history", "GET"),
            ("/api/servers/options", "GET"),
            ("/api/cache/stats", "GET"),
            ("/api/cache/entries", "GET"),
            ("/api/cache/cleanup", "POST"),
            ("/api/diagnostics", "GET"),
            ("/api/diagnostics/clean", "POST"),
            ("/api/alert-rules", "GET"),
            ("/api/alert-rules", "POST"),
            ("/api/alert-rules/1", "GET"),
            ("/api/alert-rules/1", "PUT"),
            ("/api/alert-rules/1", "DELETE"),
            ("/api/alert-rules/1/toggle", "PATCH"),
            ("/api/alert-rules/1/silent", "PATCH"),
            ("/api/alert-rules/1/history", "GET"),
            ("/api/alert-rules/1/test", "POST"),
            ("/api/alert-deliveries", "GET"),
        ];
        for (uri, method) in test_routes {
            let req = HttpRequest::builder()
                .uri(uri)
                .method(method)
                .body(Body::empty())
                .unwrap();
            let res = tower::ServiceExt::<HttpRequest<Body>>::ready(&mut app)
                .await
                .unwrap()
                .call(req)
                .await
                .unwrap();
            let status = res.status();
            assert!(
                status != StatusCode::NOT_FOUND && status != StatusCode::METHOD_NOT_ALLOWED,
                "Route failed with status code: method={}, uri={}, status={}",
                method,
                uri,
                status
            );
        }
    }
}
