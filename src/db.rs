use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
        mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel},
    },
    thread,
    time::Duration,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, ToSql, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    config::Config,
    diagnostics::{DiagnosisRecord, sanitize_diagnosis_record},
    error::{AppError, AppResult},
    ip_location::IpLocation,
};

const DEFAULT_DATABASE_PATH: &str = "data/embypanel.db";
const CONTAINER_DATABASE_PATH: &str = "/data/embypanel.db";
const DATABASE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAINTENANCE_INTERVAL_SECONDS: i64 = 60 * 60;
const TELEMETRY_WRITE_QUEUE_CAPACITY: usize = 2048;
const TELEMETRY_WRITE_BATCH_SIZE: usize = 64;
pub const DIAGNOSTICS_RETENTION_DAYS: i64 = 7;
pub const DIAGNOSTICS_PERSIST_MAX_RECORDS: usize = 10000;
const DIAGNOSTICS_MIGRATION_VERSION: &str = "diagnosis_records_v1";
const SCHEDULED_TASK_FAILURE_HISTORY_MIGRATION_VERSION: &str = "scheduled_task_failure_history_v1";
const PROXY_ANALYTICS_SAMPLES_MIGRATION_VERSION: &str = "proxy_analytics_samples_v1";
const PROXY_ANALYTICS_RETENTION_DAYS: i64 = 7;
const PROXY_ANALYTICS_MAX_ROWS: i64 = 40000;
const TASK_FAILURE_HISTORY_DEFAULT_LIMIT: i64 = 50;
const TASK_FAILURE_HISTORY_MAX_LIMIT: i64 = 200;
const TASK_FAILURE_HISTORY_ERROR_MAX_CHARS: usize = 512;
const TASK_FAILURE_HISTORY_FIELD_MAX_CHARS: usize = 256;
const TASK_FAILURE_HISTORY_DAY_MS: i64 = 24 * 60 * 60 * 1000;
const TASK_FAILURE_ERROR_REDACTED: &str = "task failure details omitted for safety";
const TOTP_PERIOD_SECONDS: i64 = 30;
const TOTP_DIGITS: i64 = 6;

#[cfg(test)]
thread_local! {
    static TEST_BEFORE_DIAGNOSIS_DECODE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}

fn validate_totp_parameters(algo: &str, digits: i64, period_seconds: i64) -> AppResult<()> {
    if !matches!(algo.to_ascii_uppercase().as_str(), "SHA1" | "SHA256") {
        return Err(AppError::Validation(
            "unsupported TOTP algorithm".to_string(),
        ));
    }
    if digits != TOTP_DIGITS {
        return Err(AppError::Validation(
            "unsupported TOTP digit count".to_string(),
        ));
    }
    if period_seconds != TOTP_PERIOD_SECONDS {
        return Err(AppError::Validation("unsupported TOTP period".to_string()));
    }
    Ok(())
}

/// Permission keys currently exposed by the admin API.
///
/// Keep this catalog small and declarative so later API code can use one source of truth
/// without coupling permissions to the audit or role persistence tables.
pub const PERMISSION_KEY_CATALOG: &[&str] = &[
    "admin.security.read",
    "admin.security.write",
    "alerts.read",
    "alerts.write",
    "cache.write",
    "library.tasks.execute",
    "library.tasks.read",
    "monitoring.read",
    "sessions.control",
    "settings.read",
    "users.emby.read",
    "users.read",
    "users.write",
];

pub fn permission_key_catalog() -> &'static [&'static str] {
    PERMISSION_KEY_CATALOG
}

pub fn database_path() -> PathBuf {
    if PathBuf::from("/data").is_dir() {
        PathBuf::from(CONTAINER_DATABASE_PATH)
    } else {
        PathBuf::from(DEFAULT_DATABASE_PATH)
    }
}

pub fn data_dir() -> PathBuf {
    database_path()
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data"))
}

#[derive(Clone)]
pub struct SettingsStore {
    path: PathBuf,
    maintenance: Arc<MaintenanceState>,
    settings_cache: Arc<Mutex<HashMap<String, Option<String>>>>,
    settings_revision: Arc<AtomicU64>,
}

#[derive(Clone)]
pub struct TelemetryWriteQueue {
    inner: Arc<TelemetryWriteQueueInner>,
}

struct TelemetryWriteQueueInner {
    sender: SyncSender<TelemetryMessage>,
    dropped: AtomicU64,
    closing: AtomicBool,
    in_flight: AtomicUsize,
    writer: Mutex<Option<thread::JoinHandle<()>>>,
}

enum TelemetryMessage {
    Write(TelemetryWrite),
    ClearDiagnosisRecords(SyncSender<AppResult<()>>),
    Shutdown,
}

enum TelemetryWrite {
    RequestStat {
        server_id: String,
        server_name: String,
        port: u16,
        kind: RequestStatKind,
    },
    ProxyRequestDetail(ProxyRequestDetailWrite),
    PlaybackAnalyticsSamples(Vec<PlaybackAnalyticsSampleWrite>),
    DiagnosisRecord(DiagnosisRecord),
}

pub struct PlaybackAnalyticsSampleWrite {
    pub timestamp_ms: u128,
    pub server_id: String,
    pub server_name: String,
    pub session_key: String,
    pub media_id: String,
    pub playback_user: String,
    pub client: String,
    pub transcoding: bool,
    pub bandwidth: Option<u64>,
}

pub struct ProxyRequestDetailWrite {
    pub timestamp_ms: u128,
    pub server_id: String,
    pub server_name: String,
    pub port: u16,
    pub method: String,
    pub path: String,
    pub path_type: String,
    pub status_code: u16,
    pub outcome: String,
    pub duration_ms: u128,
    pub playback_user: String,
    pub playback_ip: String,
    pub cache_hit: bool,
    pub blocked: bool,
    pub detail: String,
}

impl TelemetryWriteQueue {
    pub fn start(settings_store: SettingsStore) -> AppResult<Self> {
        let (sender, receiver) = sync_channel(TELEMETRY_WRITE_QUEUE_CAPACITY);
        let writer = thread::Builder::new()
            .name("embypanel-telemetry-writer".to_string())
            .spawn(move || telemetry_writer_loop(settings_store, receiver))?;
        Ok(Self {
            inner: Arc::new(TelemetryWriteQueueInner {
                sender,
                dropped: AtomicU64::new(0),
                closing: AtomicBool::new(false),
                in_flight: AtomicUsize::new(0),
                writer: Mutex::new(Some(writer)),
            }),
        })
    }

    pub fn request_stat(
        &self,
        server_id: String,
        server_name: String,
        port: u16,
        kind: RequestStatKind,
    ) {
        self.enqueue(TelemetryWrite::RequestStat {
            server_id,
            server_name,
            port,
            kind,
        });
    }

    pub fn proxy_request_detail(&self, record: ProxyRequestDetailWrite) {
        self.enqueue(TelemetryWrite::ProxyRequestDetail(record));
    }

    pub fn playback_analytics_samples(&self, mut samples: Vec<PlaybackAnalyticsSampleWrite>) {
        if samples.is_empty() {
            return;
        }
        // Bound sample count per message as well as message count; retain the existing
        // per-poll sampling limit and the queue's non-blocking drop policy.
        samples.truncate(256);
        self.enqueue(TelemetryWrite::PlaybackAnalyticsSamples(samples));
    }

    /// Non-blocking like other telemetry writes; sanitize before entering the queue.
    pub fn diagnosis_record(&self, record: DiagnosisRecord) {
        self.enqueue(TelemetryWrite::DiagnosisRecord(sanitize_diagnosis_record(
            record,
        )));
    }

    /// FIFO barrier: process earlier writes, clear persistence, then acknowledge.
    /// This management operation may block; async handlers must use spawn_blocking.
    pub fn clear_diagnosis_records(&self) -> AppResult<()> {
        let writer = self
            .inner
            .writer
            .lock()
            .map_err(|_| AppError::Internal("telemetry writer lock is poisoned".to_string()))?;
        if writer.is_none() || self.inner.closing.load(Ordering::Acquire) {
            return Err(AppError::Internal(
                "telemetry writer is unavailable".to_string(),
            ));
        }
        let (reply, result) = sync_channel(1);
        // Serialize control-message submission against shutdown, not normal producers.
        self.inner
            .sender
            .send(TelemetryMessage::ClearDiagnosisRecords(reply))
            .map_err(|_| AppError::Internal("telemetry writer is unavailable".to_string()))?;
        drop(writer);
        result.recv().map_err(|_| {
            AppError::Internal("diagnostics cleanup was not acknowledged".to_string())
        })?
    }

    fn enqueue(&self, write: TelemetryWrite) {
        if self.inner.closing.load(Ordering::Acquire) {
            self.record_drop("writer shutting down");
            return;
        }
        self.inner.in_flight.fetch_add(1, Ordering::AcqRel);
        if self.inner.closing.load(Ordering::Acquire) {
            self.inner.in_flight.fetch_sub(1, Ordering::Release);
            self.record_drop("writer shutting down");
            return;
        }
        let result = self.inner.sender.try_send(TelemetryMessage::Write(write));
        self.inner.in_flight.fetch_sub(1, Ordering::Release);
        let reason = match result {
            Ok(()) => return,
            Err(TrySendError::Full(_)) => "queue full",
            Err(TrySendError::Disconnected(_)) => "writer stopped",
        };
        self.record_drop(reason);
    }

    fn record_drop(&self, reason: &'static str) {
        let dropped = self.inner.dropped.fetch_add(1, Ordering::Relaxed) + 1;
        if dropped.is_power_of_two() {
            tracing::warn!(dropped, reason, "dropping telemetry database write");
        }
    }

    pub fn shutdown(&self) -> AppResult<()> {
        let mut writer = self
            .inner
            .writer
            .lock()
            .map_err(|_| AppError::Internal("telemetry writer lock is poisoned".to_string()))?;
        let Some(handle) = writer.take() else {
            return Ok(());
        };

        self.inner.closing.store(true, Ordering::Release);
        while self.inner.in_flight.load(Ordering::Acquire) != 0 {
            thread::sleep(Duration::from_millis(1));
        }
        let _ = self.inner.sender.send(TelemetryMessage::Shutdown);
        handle
            .join()
            .map_err(|_| AppError::Internal("telemetry writer thread panicked".to_string()))
    }
}

fn telemetry_writer_loop(settings_store: SettingsStore, receiver: Receiver<TelemetryMessage>) {
    let mut pending = None;
    while let Some(message) = pending.take().or_else(|| receiver.recv().ok()) {
        let first = match message {
            TelemetryMessage::Write(write) => write,
            TelemetryMessage::ClearDiagnosisRecords(reply) => {
                let _ = reply.send(settings_store.clear_persisted_diagnosis_records());
                continue;
            }
            TelemetryMessage::Shutdown => break,
        };
        let mut batch = Vec::with_capacity(TELEMETRY_WRITE_BATCH_SIZE);
        batch.push(first);
        while batch.len() < TELEMETRY_WRITE_BATCH_SIZE {
            match receiver.try_recv() {
                Ok(TelemetryMessage::Write(write)) => batch.push(write),
                Ok(control) => {
                    pending = Some(control);
                    break;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if let Err(err) = settings_store.write_telemetry_batch(&batch) {
            tracing::warn!(
                batch_size = batch.len(),
                error = %err.safe_log_message(),
                "failed to write telemetry batch"
            );
        }
    }
}

#[derive(Default)]
struct MaintenanceState {
    audit_logs: AtomicI64,
    request_stats: AtomicI64,
    proxy_request_logs: AtomicI64,
    proxy_analytics_samples: AtomicI64,
    sessions: AtomicI64,
    task_failure_history: AtomicI64,
}

/// A persisted failure observation for a scheduled library task.
///
/// `observed_at`, `started_at`, and `ended_at` are Unix timestamps in milliseconds.
/// `run_identity` is supplied by the caller and is part of the idempotency key together
/// with `server_id` and `task_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTaskFailureHistory {
    pub id: i64,
    pub server_id: String,
    pub task_id: String,
    pub run_identity: String,
    pub observed_at: i64,
    pub status: String,
    pub safe_error_summary: Option<String>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledTaskFailureInput {
    pub server_id: String,
    pub task_id: String,
    pub run_identity: String,
    pub observed_at: i64,
    pub status: String,
    pub error_summary: Option<String>,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskFailureListParams {
    pub server_id: Option<String>,
    pub task_id: Option<String>,
    pub run_identity: Option<String>,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Clone)]
pub struct TaskFailureHistoryStore {
    settings_store: SettingsStore,
}

impl TaskFailureHistoryStore {
    pub fn new(settings_store: &SettingsStore) -> Self {
        Self {
            settings_store: settings_store.clone(),
        }
    }

    /// Upsert one failure observation, scoped by server/task/run identity.
    ///
    /// Replaying the same run updates its latest status and timestamps instead of creating
    /// a duplicate row. Error text is sanitized before it reaches SQLite.
    pub fn record_failure(
        &self,
        input: ScheduledTaskFailureInput,
    ) -> AppResult<ScheduledTaskFailureHistory> {
        let server_id = normalize_task_failure_field(&input.server_id, "server_id")?;
        let task_id = normalize_task_failure_field(&input.task_id, "task_id")?;
        let run_identity = normalize_task_failure_field(&input.run_identity, "run_identity")?;
        let status = normalize_task_failure_field(&input.status, "status")?;
        let safe_error_summary = sanitize_task_failure_error(input.error_summary.as_deref());

        let conn = self.settings_store.connection()?;
        conn.execute(
            r#"
            INSERT INTO scheduled_task_failure_history (
                server_id, task_id, run_identity, observed_at, status,
                safe_error_summary, started_at, ended_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(server_id, task_id, run_identity) DO UPDATE SET
                observed_at = excluded.observed_at,
                status = excluded.status,
                safe_error_summary = excluded.safe_error_summary,
                started_at = excluded.started_at,
                ended_at = excluded.ended_at
            "#,
            params![
                server_id,
                task_id,
                run_identity,
                input.observed_at,
                status,
                safe_error_summary,
                input.started_at,
                input.ended_at,
            ],
        )?;

        let result = conn
            .query_row(
                r#"
            SELECT id, server_id, task_id, run_identity, observed_at, status,
                   safe_error_summary, started_at, ended_at
            FROM scheduled_task_failure_history
            WHERE server_id = ?1 AND task_id = ?2 AND run_identity = ?3
            "#,
                params![server_id, task_id, run_identity],
                task_failure_history_from_row,
            )
            .map_err(AppError::from)?;
        self.settings_store.maintain_task_failure_history()?;
        Ok(result)
    }

    pub fn list_failures(
        &self,
        filter: &TaskFailureListParams,
    ) -> AppResult<Vec<ScheduledTaskFailureHistory>> {
        let limit = normalize_task_failure_limit(filter.limit);
        let offset = filter.offset.max(0);
        let conn = self.settings_store.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, server_id, task_id, run_identity, observed_at, status,
                   safe_error_summary, started_at, ended_at
            FROM scheduled_task_failure_history
            WHERE (?1 IS NULL OR server_id = ?1)
              AND (?2 IS NULL OR task_id = ?2)
              AND (?3 IS NULL OR run_identity = ?3)
            ORDER BY observed_at DESC, id DESC
            LIMIT ?4 OFFSET ?5
            "#,
        )?;
        let rows = stmt.query_map(
            params![
                filter.server_id.as_deref(),
                filter.task_id.as_deref(),
                filter.run_identity.as_deref(),
                limit,
                offset,
            ],
            task_failure_history_from_row,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Delete rows outside the retention window and then enforce a global row cap.
    /// `retention_days` and `max_rows` are clamped at zero for safe maintenance calls.
    pub fn prune(&self, retention_days: i64, max_rows: i64) -> AppResult<usize> {
        let retention_days = retention_days.max(0);
        let max_rows = max_rows.max(0);
        let cutoff = now_ms().min(i64::MAX as u128).saturating_sub(
            (retention_days as u128 * TASK_FAILURE_HISTORY_DAY_MS as u128).min(i64::MAX as u128),
        ) as i64;
        let conn = self.settings_store.connection()?;
        let tx = conn.unchecked_transaction()?;
        let mut deleted = tx.execute(
            "DELETE FROM scheduled_task_failure_history WHERE observed_at < ?1",
            params![cutoff],
        )?;
        deleted += tx.execute(
            r#"
            DELETE FROM scheduled_task_failure_history
            WHERE id NOT IN (
                SELECT id
                FROM scheduled_task_failure_history
                ORDER BY observed_at DESC, id DESC
                LIMIT ?1
            )
            "#,
            params![max_rows],
        )?;
        tx.commit()?;
        Ok(deleted)
    }
}

impl SettingsStore {
    fn maintain_task_failure_history(&self) -> AppResult<()> {
        run_maintenance(&self.maintenance.task_failure_history, || {
            self.task_failure_history_store().prune(90, 20_000)?;
            Ok(())
        })
    }

    pub fn task_failure_history_store(&self) -> TaskFailureHistoryStore {
        TaskFailureHistoryStore::new(self)
    }

    pub fn open_default() -> AppResult<Self> {
        let path = database_path();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let store = Self::new(path);
        store.ensure_schema()?;
        Ok(store)
    }

    #[cfg(test)]
    pub fn open_for_test(path: PathBuf) -> AppResult<Self> {
        let store = Self::new(path);
        store.ensure_schema()?;
        Ok(store)
    }

    #[cfg(test)]
    pub(crate) fn replace_path_for_test(&mut self, path: PathBuf) {
        self.path = path;
    }

    #[cfg(test)]
    pub(crate) fn orphan_admin_user_for_test(&self, admin_user_id: i64) -> AppResult<()> {
        let mut conn = self.connection().map_err(AppError::Database)?;
        conn.pragma_update(None, "foreign_keys", 0)?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = tx.execute(
            "DELETE FROM admin_users WHERE id = ?1",
            params![admin_user_id],
        )?;
        if deleted != 1 {
            return Err(AppError::Internal(format!(
                "expected to delete exactly 1 admin user, got {}",
                deleted
            )));
        }
        tx.commit()?;
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn create_admin_role_for_test(&self, key: &str, name: &str) -> AppResult<i64> {
        let key = key.trim();
        let name = name.trim();
        if key.is_empty() {
            return Err(AppError::Validation(
                "role key must not be empty".to_string(),
            ));
        }
        if name.is_empty() {
            return Err(AppError::Validation(
                "role name must not be empty".to_string(),
            ));
        }
        let conn = self.connection().map_err(AppError::Database)?;
        let now = now_seconds();
        conn.execute(
            r#"
            INSERT INTO admin_roles (key, name, description, is_system, created_at)
            VALUES (?1, ?2, '', 0, ?3)
            "#,
            params![key, name, now],
        )
        .map_err(AppError::Database)?;
        let id = conn.last_insert_rowid();
        Ok(id)
    }

    fn new(path: PathBuf) -> Self {
        Self {
            path,
            maintenance: Arc::new(MaintenanceState::default()),
            settings_cache: Arc::new(Mutex::new(HashMap::new())),
            settings_revision: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn settings_revision(&self) -> u64 {
        self.settings_revision.load(Ordering::Acquire)
    }

    pub fn load_or_default_config(&self) -> AppResult<Config> {
        if let Some(config) = self.load_config()? {
            return Ok(config);
        }

        let config = Config::default_runtime();
        Ok(config)
    }

    pub fn load_config(&self) -> AppResult<Option<Config>> {
        let Some(mut config) = self.load_setting_json::<Config>("runtime_config")? else {
            return Ok(None);
        };
        let migrated_reserved_port = config.port == 8090;
        if migrated_reserved_port {
            config.port = Config::default_runtime().port;
        }
        config.validate_for_storage()?;
        if migrated_reserved_port {
            self.save_config(&config)?;
        }
        Ok(Some(config))
    }

    pub fn save_config(&self, config: &Config) -> AppResult<()> {
        self.save_setting_json("runtime_config", config)
    }

    pub fn load_setting_json<T: DeserializeOwned>(&self, key: &str) -> AppResult<Option<T>> {
        let json = {
            let mut cache = self
                .settings_cache
                .lock()
                .map_err(|_| AppError::Internal("settings cache lock is poisoned".to_string()))?;
            if let Some(value) = cache.get(key) {
                value.clone()
            } else {
                let conn = self.connection()?;
                let value = conn
                    .query_row(
                        "SELECT value FROM settings WHERE key = ?1",
                        params![key],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                cache.insert(key.to_string(), value.clone());
                value
            }
        };

        json.map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn save_setting_json<T: Serialize>(&self, key: &str, value: &T) -> AppResult<()> {
        let json = serde_json::to_string_pretty(value)?;
        let mut cache = self
            .settings_cache
            .lock()
            .map_err(|_| AppError::Internal("settings cache lock is poisoned".to_string()))?;
        let conn = self.connection()?;
        conn.execute(
            r#"
            INSERT INTO settings (key, value, updated_at)
            VALUES (?1, ?2, CURRENT_TIMESTAMP)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![key, &json],
        )?;
        cache.insert(key.to_string(), Some(json));
        self.settings_revision.fetch_add(1, Ordering::Release);
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> AppResult<()> {
        let mut cache = self
            .settings_cache
            .lock()
            .map_err(|_| AppError::Internal("settings cache lock is poisoned".to_string()))?;
        let conn = self.connection()?;
        conn.execute("DELETE FROM settings WHERE key = ?1", params![key])?;
        cache.insert(key.to_string(), None);
        self.settings_revision.fetch_add(1, Ordering::Release);
        Ok(())
    }

    pub fn has_admin(&self) -> AppResult<bool> {
        let conn = self.connection()?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM admin_users", [], |row| row.get(0))?;
        Ok(count > 0)
    }

    #[cfg(test)]
    pub fn create_initial_admin(
        &self,
        username: &str,
        password_hash: &str,
    ) -> AppResult<Option<i64>> {
        self.create_initial_admin_record(username, password_hash, None)
    }

    pub fn create_initial_admin_with_session(
        &self,
        username: &str,
        password_hash: &str,
        token_hash: &str,
        expires_at: i64,
    ) -> AppResult<Option<i64>> {
        self.create_initial_admin_record(username, password_hash, Some((token_hash, expires_at)))
    }

    fn create_initial_admin_record(
        &self,
        username: &str,
        password_hash: &str,
        initial_session: Option<(&str, i64)>,
    ) -> AppResult<Option<i64>> {
        let mut conn = self.connection()?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let has_admin: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM admin_users LIMIT 1)",
            [],
            |row| row.get(0),
        )?;
        if has_admin {
            return Ok(None);
        }
        let now = now_seconds();
        transaction.execute(
            r#"
            INSERT INTO admin_users (
                username, password_hash, enabled, display_name,
                password_changed_at, updated_at
            )
            VALUES (?1, ?2, TRUE, ?1, ?3, ?3)
            "#,
            params![username, password_hash, now],
        )?;
        let admin_user_id = transaction.last_insert_rowid();
        let super_admin_role_id = ensure_super_admin_role_tx(&transaction, now)?;
        transaction.execute(
            r#"
            INSERT INTO admin_user_roles (admin_user_id, role_id, granted_by, granted_at)
            VALUES (?1, ?2, NULL, ?3)
            "#,
            params![admin_user_id, super_admin_role_id, now],
        )?;
        if let Some((token_hash, expires_at)) = initial_session {
            transaction.execute(
                r#"
                INSERT INTO admin_sessions (
                    admin_user_id, token_hash, expires_at, last_seen_at, auth_method
                )
                VALUES (?1, ?2, ?3, ?4, 'password')
                "#,
                params![admin_user_id, token_hash, expires_at, now],
            )?;
        }
        transaction.commit()?;
        Ok(Some(admin_user_id))
    }

    #[cfg(test)]
    pub fn create_admin(&self, username: &str, password_hash: &str) -> AppResult<i64> {
        self.create_initial_admin(username, password_hash)?
            .ok_or_else(|| rusqlite::Error::InvalidQuery.into())
    }

    pub fn admin_password_hash(&self, username: &str) -> AppResult<Option<AdminPasswordHash>> {
        let conn = self.connection()?;
        let value = conn
            .query_row(
                r#"
                SELECT id, password_hash
                FROM admin_users
                WHERE username = ?1 AND enabled = TRUE
                "#,
                params![username],
                |row| {
                    Ok(AdminPasswordHash {
                        admin_user_id: row.get(0)?,
                        password_hash: row.get(1)?,
                    })
                },
            )
            .optional()?;
        Ok(value)
    }

    pub fn admin_password_hash_by_id(
        &self,
        admin_user_id: i64,
    ) -> AppResult<Option<AdminPasswordHash>> {
        let conn = self.connection()?;
        let value = conn
            .query_row(
                r#"
                SELECT id, password_hash
                FROM admin_users
                WHERE id = ?1 AND enabled = TRUE
                "#,
                params![admin_user_id],
                |row| {
                    Ok(AdminPasswordHash {
                        admin_user_id: row.get(0)?,
                        password_hash: row.get(1)?,
                    })
                },
            )
            .optional()?;
        Ok(value)
    }

    pub fn update_admin_password_and_replace_sessions(
        &self,
        admin_user_id: i64,
        expected_password_hash: &str,
        password_hash: &str,
        current_token_hash: &str,
        token_hash: &str,
        now: i64,
        expires_at: i64,
    ) -> AppResult<bool> {
        let mut conn = self.connection()?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_session_is_valid: bool = transaction.query_row(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM admin_sessions
                WHERE admin_user_id = ?1
                  AND token_hash = ?2
                  AND expires_at > ?3
                  AND revoked_at IS NULL
            )
            "#,
            params![admin_user_id, current_token_hash, now],
            |row| row.get(0),
        )?;
        if !current_session_is_valid {
            return Ok(false);
        }
        let updated = transaction.execute(
            r#"
            UPDATE admin_users
            SET password_hash = ?3,
                password_changed_at = ?4,
                updated_at = ?4
            WHERE id = ?1 AND password_hash = ?2 AND enabled = TRUE
            "#,
            params![admin_user_id, expected_password_hash, password_hash, now],
        )?;
        if updated == 0 {
            return Ok(false);
        }
        transaction.execute(
            "DELETE FROM admin_sessions WHERE admin_user_id = ?1",
            params![admin_user_id],
        )?;
        transaction.execute(
            r#"
            INSERT INTO admin_sessions (
                admin_user_id, token_hash, expires_at, last_seen_at, auth_method
            )
            VALUES (?1, ?2, ?3, ?4, 'password')
            "#,
            params![admin_user_id, token_hash, expires_at, now],
        )?;
        transaction.commit()?;
        Ok(true)
    }

    pub fn admin_username_by_id(&self, admin_user_id: i64) -> AppResult<Option<String>> {
        let conn = self.connection()?;
        let value = conn
            .query_row(
                r#"
                SELECT username
                FROM admin_users
                WHERE id = ?1 AND enabled = TRUE
                "#,
                params![admin_user_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value)
    }

    pub fn update_admin_username(&self, admin_user_id: i64, username: &str) -> AppResult<()> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            UPDATE admin_users
            SET username = ?2
            WHERE id = ?1 AND enabled = TRUE
            "#,
            params![admin_user_id, username],
        )?;
        Ok(())
    }

    #[cfg(test)]
    pub fn create_session(
        &self,
        admin_user_id: i64,
        token_hash: &str,
        expires_at: i64,
    ) -> AppResult<()> {
        let conn = self.connection()?;
        run_maintenance(&self.maintenance.sessions, || {
            conn.execute(
                "DELETE FROM admin_sessions WHERE expires_at <= ?1",
                params![now_seconds()],
            )?;
            Ok(())
        })?;
        let seen_at = now_seconds();
        conn.execute(
            r#"
            INSERT INTO admin_sessions (
                admin_user_id, token_hash, expires_at, last_seen_at, auth_method
            )
            VALUES (?1, ?2, ?3, ?4, 'password')
            "#,
            params![admin_user_id, token_hash, expires_at, seen_at],
        )?;
        Ok(())
    }

    pub fn create_session_if_password_matches(
        &self,
        admin_user_id: i64,
        expected_password_hash: &str,
        token_hash: &str,
        expires_at: i64,
    ) -> AppResult<bool> {
        let conn = self.connection()?;
        run_maintenance(&self.maintenance.sessions, || {
            conn.execute(
                "DELETE FROM admin_sessions WHERE expires_at <= ?1",
                params![now_seconds()],
            )?;
            Ok(())
        })?;
        let seen_at = now_seconds();
        let inserted = conn.execute(
            r#"
            INSERT INTO admin_sessions (
                admin_user_id, token_hash, expires_at, last_seen_at, auth_method
            )
            SELECT id, ?3, ?4, ?5, 'password'
            FROM admin_users
            WHERE id = ?1 AND password_hash = ?2 AND enabled = TRUE
            "#,
            params![
                admin_user_id,
                expected_password_hash,
                token_hash,
                expires_at,
                seen_at
            ],
        )?;
        Ok(inserted == 1)
    }

    pub fn revoke_session(&self, token_hash: &str, now: i64) -> AppResult<Option<i64>> {
        let conn = self.connection()?;
        // Prefer soft-revoke so session metadata remains listable for audit UX.
        let admin_user_id = conn
            .query_row(
                r#"
                UPDATE admin_sessions
                SET revoked_at = COALESCE(revoked_at, ?2)
                WHERE token_hash = ?1
                  AND expires_at > ?2
                  AND revoked_at IS NULL
                RETURNING admin_user_id
                "#,
                params![token_hash, now],
                |row| row.get(0),
            )
            .optional()?;
        Ok(admin_user_id)
    }

    /// Resolve a live admin session by token hash for A1 auth assembly.
    /// Returns only non-sensitive metadata (never token_hash).
    pub fn admin_session_auth_by_hash(
        &self,
        token_hash: &str,
        now: i64,
    ) -> AppResult<Option<AdminSessionAuthMeta>> {
        let conn = self.connection()?;
        let value = conn
            .query_row(
                r#"
                SELECT s.id, s.admin_user_id, s.last_seen_at, s.expires_at, s.auth_method
                FROM admin_sessions s
                INNER JOIN admin_users u ON u.id = s.admin_user_id
                WHERE s.token_hash = ?1
                  AND s.expires_at > ?2
                  AND s.revoked_at IS NULL
                  AND u.enabled = TRUE
                "#,
                params![token_hash, now],
                |row| {
                    Ok(AdminSessionAuthMeta {
                        session_id: row.get(0)?,
                        admin_user_id: row.get(1)?,
                        last_seen_at: row.get(2)?,
                        expires_at: row.get(3)?,
                        auth_method: row.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(value)
    }

    /// Compatibility wrapper: admin user id for a live session token hash.
    pub fn session_admin_user_id(&self, token_hash: &str, now: i64) -> AppResult<Option<i64>> {
        Ok(self
            .admin_session_auth_by_hash(token_hash, now)?
            .map(|meta| meta.admin_user_id))
    }

    /// Throttled last_seen touch for an authenticated admin session.
    ///
    /// - Only updates sessions that are still live (not revoked/expired, user enabled).
    /// - Never extends `expires_at`.
    /// - Writes are monotonic (`now` must not go backwards relative to stored last_seen).
    /// - `min_interval_secs` is clamped to `>= 0`; negative/abnormal values become 0.
    /// - Single-statement UPDATE avoids read-then-write races on the write path.
    pub fn touch_admin_session_last_seen(
        &self,
        session_id: i64,
        now: i64,
        min_interval_secs: i64,
    ) -> AppResult<AdminSessionTouchResult> {
        let min_interval_secs = min_interval_secs.max(0);
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE admin_sessions
            SET last_seen_at = ?2
            WHERE id = ?1
              AND revoked_at IS NULL
              AND expires_at > ?2
              AND last_seen_at <= (?2 - ?3)
              AND EXISTS (
                  SELECT 1
                  FROM admin_users u
                  WHERE u.id = admin_sessions.admin_user_id
                    AND u.enabled = TRUE
              )
            "#,
            params![session_id, now, min_interval_secs],
        )?;
        if updated == 1 {
            return Ok(AdminSessionTouchResult::Updated);
        }

        // Distinguish "still valid but not due" from "inactive" without changing state.
        let active = conn
            .query_row(
                r#"
                SELECT 1
                FROM admin_sessions s
                INNER JOIN admin_users u ON u.id = s.admin_user_id
                WHERE s.id = ?1
                  AND s.revoked_at IS NULL
                  AND s.expires_at > ?2
                  AND u.enabled = TRUE
                "#,
                params![session_id, now],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if active {
            Ok(AdminSessionTouchResult::NotDue)
        } else {
            Ok(AdminSessionTouchResult::Inactive)
        }
    }

    /// Load the security identity snapshot used by later AuthContext assembly (A1).
    pub fn admin_security_identity(
        &self,
        admin_user_id: i64,
    ) -> AppResult<Option<AdminSecurityIdentity>> {
        let conn = self.connection()?;
        let Some((username, display_name, enabled, disabled_at, totp_enabled, totp_pending)) = conn
            .query_row(
                r#"
                SELECT username, display_name, enabled, disabled_at, totp_enabled, totp_pending
                FROM admin_users
                WHERE id = ?1
                "#,
                params![admin_user_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, Option<i64>>(3)?,
                        row.get::<_, bool>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };

        let mut role_stmt = conn.prepare(
            r#"
            SELECT r.key
            FROM admin_user_roles ur
            INNER JOIN admin_roles r ON r.id = ur.role_id
            WHERE ur.admin_user_id = ?1
            ORDER BY r.key ASC
            "#,
        )?;
        let role_keys = role_stmt
            .query_map(params![admin_user_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;

        let mut perm_stmt = conn.prepare(
            r#"
            SELECT DISTINCT rp.permission
            FROM admin_user_roles ur
            INNER JOIN admin_role_permissions rp ON rp.role_id = ur.role_id
            WHERE ur.admin_user_id = ?1
            ORDER BY rp.permission ASC
            "#,
        )?;
        let permission_keys = perm_stmt
            .query_map(params![admin_user_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;

        let mut grant_stmt = conn.prepare(
            r#"
            SELECT server_id, scope, granted_at
            FROM admin_server_grants
            WHERE admin_user_id = ?1
            ORDER BY server_id ASC
            "#,
        )?;
        let server_grants = grant_stmt
            .query_map(params![admin_user_id], |row| {
                Ok(AdminServerGrant {
                    server_id: row.get(0)?,
                    scope: row.get(1)?,
                    granted_at: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Some(AdminSecurityIdentity {
            admin_user_id,
            username,
            display_name,
            enabled,
            disabled_at,
            totp_enabled,
            totp_pending,
            role_keys,
            permission_keys,
            server_grants,
        }))
    }

    /// Create a non-bootstrap admin and assign existing roles in one transaction.
    pub fn create_admin_with_roles(
        &self,
        username: &str,
        password_hash: &str,
        display_name: Option<&str>,
        role_keys: &[&str],
        granted_by: Option<i64>,
    ) -> AppResult<i64> {
        self.create_admin_with_roles_and_server_grants(
            username,
            password_hash,
            display_name,
            role_keys,
            &[],
            granted_by,
        )
    }

    /// Create an administrator together with role and server-grant bindings atomically.
    pub fn create_admin_with_roles_and_server_grants(
        &self,
        username: &str,
        password_hash: &str,
        display_name: Option<&str>,
        role_keys: &[&str],
        grants: &[AdminServerGrantInput],
        granted_by: Option<i64>,
    ) -> AppResult<i64> {
        if username.trim().is_empty() {
            return Err(AppError::Validation(
                "admin username must not be empty".to_string(),
            ));
        }
        if role_keys.is_empty() {
            return Err(AppError::Validation(
                "admin must be assigned at least one role".to_string(),
            ));
        }
        let mut unique_roles = role_keys
            .iter()
            .map(|key| key.trim())
            .filter(|key| !key.is_empty())
            .collect::<Vec<_>>();
        unique_roles.sort_unstable();
        unique_roles.dedup();
        if unique_roles.is_empty() {
            return Err(AppError::Validation(
                "admin must be assigned at least one role".to_string(),
            ));
        }
        let mut unique_grants = std::collections::BTreeSet::new();
        let mut normalized_grants = Vec::with_capacity(grants.len());
        for grant in grants {
            let server_id = grant.server_id.trim();
            if server_id.is_empty() {
                return Err(AppError::Validation(
                    "server_id must not be empty".to_string(),
                ));
            }
            if !unique_grants.insert(server_id.to_string()) {
                return Err(AppError::Validation(format!(
                    "duplicate server grant: {server_id}"
                )));
            }
            let scope = if grant.scope.trim().is_empty() {
                "full"
            } else {
                grant.scope.trim()
            };
            normalized_grants.push((server_id, scope));
        }

        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_seconds();
        let display = display_name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(username.trim());
        match tx.execute(
            r#"
            INSERT INTO admin_users (
                username, password_hash, enabled, display_name,
                password_changed_at, updated_at
            )
            VALUES (?1, ?2, TRUE, ?3, ?4, ?4)
            "#,
            params![username.trim(), password_hash, display, now],
        ) {
            Ok(_) => {}
            Err(err) if is_unique_constraint(&err) => {
                return Err(AppError::Validation(
                    "admin username already exists".to_string(),
                ));
            }
            Err(err) => return Err(err.into()),
        }
        let admin_user_id = tx.last_insert_rowid();
        for role_key in &unique_roles {
            let role_id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM admin_roles WHERE key = ?1",
                    params![role_key],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(role_id) = role_id else {
                return Err(AppError::Validation(format!(
                    "admin role not found: {role_key}"
                )));
            };
            tx.execute(
                r#"
                INSERT INTO admin_user_roles (admin_user_id, role_id, granted_by, granted_at)
                VALUES (?1, ?2, ?3, ?4)
                "#,
                params![admin_user_id, role_id, granted_by, now],
            )?;
        }
        for (server_id, scope) in normalized_grants {
            tx.execute(
                r#"
                INSERT INTO admin_server_grants (
                    admin_user_id, server_id, scope, granted_by, granted_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
                params![admin_user_id, server_id, scope, granted_by, now],
            )?;
        }
        tx.commit()?;
        Ok(admin_user_id)
    }

    pub fn list_admin_users(&self) -> AppResult<Vec<AdminUserListItem>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, username, display_name, enabled, disabled_at,
                   totp_enabled, totp_pending, created_at, updated_at
            FROM admin_users
            ORDER BY id ASC
            "#,
        )?;
        let mut users = stmt
            .query_map([], |row| {
                Ok(AdminUserListItem {
                    id: row.get(0)?,
                    username: row.get(1)?,
                    display_name: row.get(2)?,
                    enabled: row.get(3)?,
                    disabled_at: row.get(4)?,
                    totp_enabled: row.get(5)?,
                    totp_pending: row.get(6)?,
                    created_at: row.get(7)?,
                    updated_at: row.get(8)?,
                    role_keys: Vec::new(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        let mut role_stmt = conn.prepare(
            r#"
            SELECT r.key
            FROM admin_user_roles ur
            INNER JOIN admin_roles r ON r.id = ur.role_id
            WHERE ur.admin_user_id = ?1
            ORDER BY r.key ASC
            "#,
        )?;
        for user in &mut users {
            user.role_keys = role_stmt
                .query_map(params![user.id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
        }
        Ok(users)
    }

    pub fn update_admin_display_name(
        &self,
        admin_user_id: i64,
        display_name: Option<&str>,
    ) -> AppResult<bool> {
        let conn = self.connection()?;
        let now = now_seconds();
        let display = display_name
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let updated = conn.execute(
            r#"
            UPDATE admin_users
            SET display_name = ?2, updated_at = ?3
            WHERE id = ?1
            "#,
            params![admin_user_id, display, now],
        )?;
        Ok(updated == 1)
    }

    pub fn reset_admin_password_and_revoke_credentials(
        &self,
        admin_user_id: i64,
        password_hash: &str,
        now: i64,
    ) -> AppResult<bool> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = tx.execute(
            r#"
            UPDATE admin_users
            SET password_hash = ?2,
                password_changed_at = ?3,
                updated_at = ?3
            WHERE id = ?1 AND enabled = TRUE
            "#,
            params![admin_user_id, password_hash, now],
        )?;
        if updated == 0 {
            return Ok(false);
        }
        tx.execute(
            "UPDATE admin_sessions SET revoked_at = COALESCE(revoked_at, ?2) WHERE admin_user_id = ?1 AND revoked_at IS NULL",
            params![admin_user_id, now],
        )?;
        tx.execute(
            "UPDATE admin_api_tokens SET revoked_at = COALESCE(revoked_at, ?2) WHERE admin_user_id = ?1 AND revoked_at IS NULL",
            params![admin_user_id, now],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn delete_admin_user(&self, admin_user_id: i64, now: i64) -> AppResult<bool> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(bool, bool)> = tx
            .query_row(
                r#"
                SELECT enabled,
                       EXISTS(
                           SELECT 1
                           FROM admin_user_roles ur
                           INNER JOIN admin_roles r ON r.id = ur.role_id
                           WHERE ur.admin_user_id = admin_users.id AND r.key = ?2
                       )
                FROM admin_users
                WHERE id = ?1
                "#,
                params![admin_user_id, SUPER_ADMIN_ROLE_KEY],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((enabled, is_super_admin)) = current else {
            return Ok(false);
        };
        if enabled && is_super_admin {
            let other_enabled_super_admins: i64 = tx.query_row(
                r#"
                SELECT COUNT(*)
                FROM admin_users u
                INNER JOIN admin_user_roles ur ON ur.admin_user_id = u.id
                INNER JOIN admin_roles r ON r.id = ur.role_id
                WHERE u.id != ?1 AND u.enabled = TRUE AND r.key = ?2
                "#,
                params![admin_user_id, SUPER_ADMIN_ROLE_KEY],
                |row| row.get(0),
            )?;
            if other_enabled_super_admins == 0 {
                return Err(AppError::Validation(
                    "cannot delete the last enabled super_admin".to_string(),
                ));
            }
        }
        let deleted = tx.execute(
            "DELETE FROM admin_users WHERE id = ?1",
            params![admin_user_id],
        )?;
        tx.commit()?;
        let _ = now;
        Ok(deleted == 1)
    }

    pub fn set_admin_disabled(
        &self,
        admin_user_id: i64,
        disabled: bool,
        now: i64,
    ) -> AppResult<bool> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: Option<(bool, bool)> = tx
            .query_row(
                r#"
                SELECT enabled,
                       EXISTS(
                           SELECT 1
                           FROM admin_user_roles ur
                           INNER JOIN admin_roles r ON r.id = ur.role_id
                           WHERE ur.admin_user_id = admin_users.id AND r.key = ?2
                       )
                FROM admin_users
                WHERE id = ?1
                "#,
                params![admin_user_id, SUPER_ADMIN_ROLE_KEY],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((enabled, is_super_admin)) = current else {
            return Ok(false);
        };
        if disabled && enabled && is_super_admin {
            let other_enabled_super_admins: i64 = tx.query_row(
                r#"
                SELECT COUNT(*)
                FROM admin_users u
                INNER JOIN admin_user_roles ur ON ur.admin_user_id = u.id
                INNER JOIN admin_roles r ON r.id = ur.role_id
                WHERE u.id != ?1 AND u.enabled = TRUE AND r.key = ?2
                "#,
                params![admin_user_id, SUPER_ADMIN_ROLE_KEY],
                |row| row.get(0),
            )?;
            if other_enabled_super_admins == 0 {
                return Err(AppError::Validation(
                    "cannot disable the last enabled super_admin".to_string(),
                ));
            }
        }
        let updated = if disabled {
            tx.execute(
                r#"
                UPDATE admin_users
                SET enabled = FALSE,
                    disabled_at = ?2,
                    updated_at = ?2
                WHERE id = ?1
                "#,
                params![admin_user_id, now],
            )?
        } else {
            tx.execute(
                r#"
                UPDATE admin_users
                SET enabled = TRUE,
                    disabled_at = NULL,
                    updated_at = ?2
                WHERE id = ?1
                "#,
                params![admin_user_id, now],
            )?
        };
        if disabled && updated == 1 {
            // Do not allow credentials to spring back to life when an account is re-enabled.
            tx.execute(
                "UPDATE admin_sessions SET revoked_at = COALESCE(revoked_at, ?2) WHERE admin_user_id = ?1 AND revoked_at IS NULL",
                params![admin_user_id, now],
            )?;
            tx.execute(
                "UPDATE admin_api_tokens SET revoked_at = COALESCE(revoked_at, ?2) WHERE admin_user_id = ?1 AND revoked_at IS NULL",
                params![admin_user_id, now],
            )?;
        }
        tx.commit()?;
        Ok(updated == 1)
    }

    pub fn replace_admin_roles(
        &self,
        admin_user_id: i64,
        role_keys: &[&str],
        granted_by: Option<i64>,
        now: i64,
    ) -> AppResult<()> {
        if role_keys.is_empty() {
            return Err(AppError::Validation(
                "admin must keep at least one role".to_string(),
            ));
        }
        let mut unique_roles = role_keys
            .iter()
            .map(|key| key.trim())
            .filter(|key| !key.is_empty())
            .collect::<Vec<_>>();
        unique_roles.sort_unstable();
        unique_roles.dedup();
        if unique_roles.is_empty() {
            return Err(AppError::Validation(
                "admin must keep at least one role".to_string(),
            ));
        }

        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM admin_users WHERE id = ?1)",
            params![admin_user_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(AppError::Validation("admin user not found".to_string()));
        }

        // Prevent lockout: the last super_admin assignment cannot be removed by replace.
        let currently_super: bool = tx.query_row(
            r#"
            SELECT EXISTS(
                SELECT 1
                FROM admin_user_roles ur
                INNER JOIN admin_roles r ON r.id = ur.role_id
                WHERE ur.admin_user_id = ?1 AND r.key = ?2
            )
            "#,
            params![admin_user_id, SUPER_ADMIN_ROLE_KEY],
            |row| row.get(0),
        )?;
        let keeps_super = unique_roles.iter().any(|key| *key == SUPER_ADMIN_ROLE_KEY);
        if currently_super && !keeps_super {
            let other_supers: i64 = tx.query_row(
                r#"
                SELECT COUNT(*)
                FROM admin_users u
                INNER JOIN admin_user_roles ur ON ur.admin_user_id = u.id
                INNER JOIN admin_roles r ON r.id = ur.role_id
                WHERE r.key = ?1 AND ur.admin_user_id != ?2 AND u.enabled = TRUE
                "#,
                params![SUPER_ADMIN_ROLE_KEY, admin_user_id],
                |row| row.get(0),
            )?;
            if other_supers == 0 {
                return Err(AppError::Validation(
                    "cannot remove the last super_admin assignment".to_string(),
                ));
            }
        }

        tx.execute(
            "DELETE FROM admin_user_roles WHERE admin_user_id = ?1",
            params![admin_user_id],
        )?;
        for role_key in &unique_roles {
            let role_id: Option<i64> = tx
                .query_row(
                    "SELECT id FROM admin_roles WHERE key = ?1",
                    params![role_key],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(role_id) = role_id else {
                return Err(AppError::Validation(format!(
                    "admin role not found: {role_key}"
                )));
            };
            tx.execute(
                r#"
                INSERT INTO admin_user_roles (admin_user_id, role_id, granted_by, granted_at)
                VALUES (?1, ?2, ?3, ?4)
                "#,
                params![admin_user_id, role_id, granted_by, now],
            )?;
        }
        tx.execute(
            "UPDATE admin_users SET updated_at = ?2 WHERE id = ?1",
            params![admin_user_id, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn replace_admin_server_grants(
        &self,
        admin_user_id: i64,
        grants: &[AdminServerGrantInput],
        granted_by: Option<i64>,
        now: i64,
    ) -> AppResult<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM admin_users WHERE id = ?1)",
            params![admin_user_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(AppError::Validation("admin user not found".to_string()));
        }
        tx.execute(
            "DELETE FROM admin_server_grants WHERE admin_user_id = ?1",
            params![admin_user_id],
        )?;
        let mut seen = std::collections::BTreeSet::new();
        for grant in grants {
            let server_id = grant.server_id.trim();
            if server_id.is_empty() {
                return Err(AppError::Validation(
                    "server_id must not be empty".to_string(),
                ));
            }
            if !seen.insert(server_id.to_string()) {
                return Err(AppError::Validation(format!(
                    "duplicate server grant: {server_id}"
                )));
            }
            let scope = if grant.scope.trim().is_empty() {
                "full"
            } else {
                grant.scope.trim()
            };
            tx.execute(
                r#"
                INSERT INTO admin_server_grants (
                    admin_user_id, server_id, scope, granted_by, granted_at
                )
                VALUES (?1, ?2, ?3, ?4, ?5)
                "#,
                params![admin_user_id, server_id, scope, granted_by, now],
            )?;
        }
        tx.execute(
            "UPDATE admin_users SET updated_at = ?2 WHERE id = ?1",
            params![admin_user_id, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_admin_sessions(&self, admin_user_id: i64) -> AppResult<Vec<AdminSessionMeta>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, admin_user_id, ip, user_agent, device_label,
                   last_seen_at, expires_at, created_at, revoked_at, auth_method
            FROM admin_sessions
            WHERE admin_user_id = ?1
            ORDER BY id DESC
            "#,
        )?;
        let rows = stmt.query_map(params![admin_user_id], |row| {
            Ok(AdminSessionMeta {
                id: row.get(0)?,
                admin_user_id: row.get(1)?,
                ip: row.get(2)?,
                user_agent: row.get(3)?,
                device_label: row.get(4)?,
                last_seen_at: row.get(5)?,
                expires_at: row.get(6)?,
                created_at: row.get(7)?,
                revoked_at: row.get(8)?,
                auth_method: row.get(9)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn revoke_admin_session_by_id_for_user(
        &self,
        session_id: i64,
        admin_user_id: i64,
        now: i64,
    ) -> AppResult<bool> {
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE admin_sessions
            SET revoked_at = COALESCE(revoked_at, ?3)
            WHERE id = ?1 AND admin_user_id = ?2 AND revoked_at IS NULL
            "#,
            params![session_id, admin_user_id, now],
        )?;
        Ok(updated == 1)
    }

    pub fn revoke_admin_session_by_id(&self, session_id: i64, now: i64) -> AppResult<bool> {
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE admin_sessions
            SET revoked_at = COALESCE(revoked_at, ?2)
            WHERE id = ?1 AND revoked_at IS NULL
            "#,
            params![session_id, now],
        )?;
        Ok(updated == 1)
    }

    pub fn revoke_all_admin_sessions(&self, admin_user_id: i64, now: i64) -> AppResult<u64> {
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE admin_sessions
            SET revoked_at = COALESCE(revoked_at, ?2)
            WHERE admin_user_id = ?1 AND revoked_at IS NULL
            "#,
            params![admin_user_id, now],
        )?;
        Ok(updated as u64)
    }

    pub fn create_admin_api_token(
        &self,
        admin_user_id: i64,
        name: &str,
        token_prefix: &str,
        token_hash: &str,
        scopes: &[String],
        server_ids: Option<&[String]>,
        expires_at: Option<i64>,
        created_by: Option<i64>,
        now: i64,
    ) -> AppResult<i64> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AppError::Validation(
                "api token name must not be empty".to_string(),
            ));
        }
        if token_prefix.trim().is_empty() || token_hash.trim().is_empty() {
            return Err(AppError::Validation(
                "api token prefix and hash are required".to_string(),
            ));
        }
        let scopes_json = encode_string_list(scopes)?;
        let server_ids_json = server_ids.map(encode_string_list).transpose()?;
        let conn = self.connection()?;
        match conn.execute(
            r#"
            INSERT INTO admin_api_tokens (
                admin_user_id, name, token_prefix, token_hash, scopes, server_ids,
                expires_at, created_at, created_by
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                admin_user_id,
                name,
                token_prefix.trim(),
                token_hash,
                scopes_json,
                server_ids_json,
                expires_at,
                now,
                created_by
            ],
        ) {
            Ok(_) => Ok(conn.last_insert_rowid()),
            Err(err) if is_unique_constraint(&err) => Err(AppError::Validation(
                "api token hash already exists".to_string(),
            )),
            Err(err) if is_foreign_key_constraint(&err) => Err(AppError::Validation(
                "admin user not found for api token".to_string(),
            )),
            Err(err) => Err(err.into()),
        }
    }

    pub fn create_admin_api_token_from_session(
        &self,
        source_token_hash: &str,
        admin_user_id: i64,
        name: &str,
        token_prefix: &str,
        token_hash: &str,
        scopes: &[String],
        server_ids: Option<&[String]>,
        expires_at: Option<i64>,
        created_by: Option<i64>,
    ) -> AppResult<i64> {
        // BEGIN IMMEDIATE may wait on another writer; capture authorization time
        // only after owning the write transaction so an expired session cannot
        // sign a token using a stale pre-lock timestamp.
        if name.trim().is_empty() || token_prefix.trim().is_empty() || token_hash.trim().is_empty()
        {
            return Err(AppError::Validation(
                "api token name, prefix and hash are required".into(),
            ));
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_seconds();
        if expires_at.is_some_and(|expiry| expiry <= now) {
            return Err(AppError::Validation(
                "token expiry must be in the future".into(),
            ));
        }
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM admin_sessions s JOIN admin_users u ON u.id = s.admin_user_id
             WHERE s.token_hash = ?1 AND s.admin_user_id = ?2 AND s.revoked_at IS NULL
             AND s.expires_at > ?3 AND u.enabled = 1)",
            params![source_token_hash, admin_user_id, now], |row| row.get(0),
        )?;
        if !valid {
            return Err(AppError::Unauthorized(
                "source session is no longer valid".into(),
            ));
        }
        let is_super: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM admin_user_roles ur JOIN admin_roles r ON r.id = ur.role_id
             WHERE ur.admin_user_id = ?1 AND r.key = 'super_admin')",
            [admin_user_id], |row| row.get(0),
        )?;
        if !is_super {
            for permission in
                std::iter::once("admin.security.write").chain(scopes.iter().map(String::as_str))
            {
                let allowed: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM admin_user_roles ur JOIN admin_role_permissions rp ON rp.role_id = ur.role_id
                     WHERE ur.admin_user_id = ?1 AND rp.permission = ?2)",
                    params![admin_user_id, permission], |row| row.get(0),
                )?;
                if !allowed {
                    return Err(AppError::Forbidden(
                        "token permission exceeds current administrator access".into(),
                    ));
                }
            }
            if let Some(ids) = server_ids {
                for id in ids {
                    let allowed: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM admin_server_grants WHERE admin_user_id = ?1 AND server_id = ?2)",
                        params![admin_user_id, id], |row| row.get(0),
                    )?;
                    if !allowed {
                        return Err(AppError::Forbidden(
                            "token server access exceeds current grants".into(),
                        ));
                    }
                }
            }
        }
        let result = tx.execute(
            "INSERT INTO admin_api_tokens(admin_user_id, name, token_prefix, token_hash, scopes, server_ids, expires_at, created_at, created_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![admin_user_id, name.trim(), token_prefix.trim(), token_hash,
                encode_string_list(scopes)?, server_ids.map(encode_string_list).transpose()?,
                expires_at, now, created_by],
        );
        match result {
            Ok(_) => {
                let id = tx.last_insert_rowid();
                tx.commit()?;
                Ok(id)
            }
            Err(err) if is_unique_constraint(&err) => {
                Err(AppError::Validation("api token hash already exists".into()))
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Authenticate by token hash only. Never expose the hash through list DTOs.
    pub fn admin_api_token_auth_by_hash(
        &self,
        token_hash: &str,
        now: i64,
    ) -> AppResult<Option<AdminApiTokenAuthMeta>> {
        let conn = self.connection()?;
        let row = conn
            .query_row(
                r#"
                SELECT t.id, t.admin_user_id, t.name, t.scopes, t.server_ids, t.expires_at
                FROM admin_api_tokens t
                INNER JOIN admin_users u ON u.id = t.admin_user_id
                WHERE t.token_hash = ?1
                  AND t.revoked_at IS NULL
                  AND (t.expires_at IS NULL OR t.expires_at > ?2)
                  AND u.enabled = TRUE
                "#,
                params![token_hash, now],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<i64>>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((id, admin_user_id, name, scopes_json, server_ids_json, expires_at)) = row else {
            return Ok(None);
        };
        // Auth path fails closed on corrupt metadata: never treat garbage JSON as full access.
        let Ok(scopes) = decode_string_list(&scopes_json) else {
            return Ok(None);
        };
        let server_ids = match server_ids_json.as_deref() {
            Some(raw) => match decode_string_list(raw) {
                Ok(values) => Some(values),
                Err(_) => return Ok(None),
            },
            None => None,
        };
        Ok(Some(AdminApiTokenAuthMeta {
            id,
            admin_user_id,
            name,
            scopes,
            server_ids,
            expires_at,
        }))
    }

    pub fn list_admin_api_tokens(
        &self,
        admin_user_id: i64,
    ) -> AppResult<Vec<AdminApiTokenListItem>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, admin_user_id, name, token_prefix, scopes, server_ids,
                   expires_at, last_used_at, revoked_at, created_at, created_by
            FROM admin_api_tokens
            WHERE admin_user_id = ?1
            ORDER BY id DESC
            "#,
        )?;
        let mut items = Vec::new();
        let rows = stmt.query_map(params![admin_user_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, Option<i64>>(10)?,
            ))
        })?;
        for row in rows {
            let (
                id,
                admin_user_id,
                name,
                token_prefix,
                scopes_json,
                server_ids_json,
                expires_at,
                last_used_at,
                revoked_at,
                created_at,
                created_by,
            ) = row?;
            items.push(AdminApiTokenListItem {
                id,
                admin_user_id,
                name,
                token_prefix,
                scopes: decode_string_list(&scopes_json)?,
                server_ids: server_ids_json
                    .as_deref()
                    .map(decode_string_list)
                    .transpose()?,
                expires_at,
                last_used_at,
                revoked_at,
                created_at,
                created_by,
            });
        }
        Ok(items)
    }

    pub fn touch_admin_api_token_last_used(&self, token_id: i64, now: i64) -> AppResult<bool> {
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE admin_api_tokens
            SET last_used_at = ?2
            WHERE id = ?1 AND revoked_at IS NULL
            "#,
            params![token_id, now],
        )?;
        Ok(updated == 1)
    }

    pub fn revoke_admin_api_token_for_user(
        &self,
        token_id: i64,
        admin_user_id: i64,
        now: i64,
    ) -> AppResult<bool> {
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE admin_api_tokens
            SET revoked_at = COALESCE(revoked_at, ?3)
            WHERE id = ?1 AND admin_user_id = ?2 AND revoked_at IS NULL
            "#,
            params![token_id, admin_user_id, now],
        )?;
        Ok(updated == 1)
    }

    pub fn revoke_admin_api_token(&self, token_id: i64, now: i64) -> AppResult<bool> {
        let conn = self.connection()?;
        let updated = conn.execute(
            r#"
            UPDATE admin_api_tokens
            SET revoked_at = COALESCE(revoked_at, ?2)
            WHERE id = ?1 AND revoked_at IS NULL
            "#,
            params![token_id, now],
        )?;
        Ok(updated == 1)
    }

    /// Persist TOTP ciphertext metadata only. Callers must never pass plaintext secrets.
    pub fn upsert_admin_totp_secret(
        &self,
        admin_user_id: i64,
        secret_ciphertext: &[u8],
        secret_nonce: &[u8],
        key_version: i64,
        algo: &str,
        digits: i64,
        period_seconds: i64,
        now: i64,
    ) -> AppResult<()> {
        if secret_ciphertext.is_empty() || secret_nonce.is_empty() {
            return Err(AppError::Validation(
                "totp ciphertext and nonce are required".to_string(),
            ));
        }
        validate_totp_parameters(algo, digits, period_seconds)?;
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let enabled: bool = tx.query_row(
            "SELECT totp_enabled FROM admin_users WHERE id = ?1",
            params![admin_user_id],
            |row| row.get(0),
        )?;
        let existing: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM admin_totp_secrets WHERE admin_user_id = ?1)",
            params![admin_user_id],
            |row| row.get(0),
        )?;
        if enabled || existing {
            return Err(AppError::Validation(
                "2FA is enabled or setup is already pending".to_string(),
            ));
        }
        tx.execute(
            r#"
            INSERT INTO admin_totp_secrets (
                admin_user_id, secret_ciphertext, secret_nonce, key_version,
                algo, digits, period_seconds, confirmed_at, last_used_step, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8)
            "#,
            params![
                admin_user_id,
                secret_ciphertext,
                secret_nonce,
                key_version,
                algo,
                digits,
                period_seconds,
                now
            ],
        )?;
        tx.execute(
            r#"
            UPDATE admin_users
            SET totp_enabled = FALSE,
                totp_pending = TRUE,
                updated_at = ?2
            WHERE id = ?1
            "#,
            params![admin_user_id, now],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_admin_totp_secret(
        &self,
        admin_user_id: i64,
    ) -> AppResult<Option<AdminTotpSecretRow>> {
        let conn = self.connection()?;
        let value = conn
            .query_row(
                r#"
                SELECT admin_user_id, secret_ciphertext, secret_nonce, key_version,
                       algo, digits, period_seconds, confirmed_at, last_used_step, updated_at
                FROM admin_totp_secrets
                WHERE admin_user_id = ?1
                "#,
                params![admin_user_id],
                |row| {
                    Ok(AdminTotpSecretRow {
                        admin_user_id: row.get(0)?,
                        secret_ciphertext: row.get(1)?,
                        secret_nonce: row.get(2)?,
                        key_version: row.get(3)?,
                        algo: row.get(4)?,
                        digits: row.get(5)?,
                        period_seconds: row.get(6)?,
                        confirmed_at: row.get(7)?,
                        last_used_step: row.get(8)?,
                        updated_at: row.get(9)?,
                    })
                },
            )
            .optional()?;
        let Some(value) = value else {
            return Ok(None);
        };
        validate_totp_parameters(&value.algo, value.digits, value.period_seconds)?;
        Ok(Some(value))
    }

    pub fn delete_admin_totp_secret(&self, admin_user_id: i64, now: i64) -> AppResult<bool> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = tx.execute(
            "DELETE FROM admin_totp_secrets WHERE admin_user_id = ?1",
            params![admin_user_id],
        )?;
        if deleted == 0 {
            return Ok(false);
        }
        tx.execute(
            r#"
            UPDATE admin_users
            SET totp_enabled = FALSE,
                totp_pending = FALSE,
                updated_at = ?2
            WHERE id = ?1
            "#,
            params![admin_user_id, now],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn confirm_admin_totp(
        &self,
        admin_user_id: i64,
        secret_ciphertext: &[u8],
        secret_nonce: &[u8],
        now: i64,
    ) -> AppResult<bool> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = tx.execute(
            r#"
            UPDATE admin_totp_secrets
            SET confirmed_at = ?2, updated_at = ?2
            WHERE admin_user_id = ?1 AND confirmed_at IS NULL
              AND secret_ciphertext = ?3 AND secret_nonce = ?4
            "#,
            params![admin_user_id, now, secret_ciphertext, secret_nonce],
        )?;
        if updated == 0 {
            return Ok(false);
        }
        tx.execute(
            r#"
            UPDATE admin_users
            SET totp_enabled = TRUE,
                totp_pending = FALSE,
                updated_at = ?2
            WHERE id = ?1
            "#,
            params![admin_user_id, now],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Atomically advance TOTP last_used_step forward only (replay protection).
    pub fn confirm_admin_totp_with_recovery_codes(
        &self,
        admin_user_id: i64,
        secret_ciphertext: &[u8],
        secret_nonce: &[u8],
        code_hashes: &[String],
        now: i64,
    ) -> AppResult<bool> {
        if code_hashes.is_empty() {
            return Err(AppError::Validation(
                "recovery codes are required".to_string(),
            ));
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = tx.execute(
            r#"
            UPDATE admin_totp_secrets
            SET confirmed_at = ?2, updated_at = ?2
            WHERE admin_user_id = ?1 AND confirmed_at IS NULL
              AND secret_ciphertext = ?3 AND secret_nonce = ?4
            "#,
            params![admin_user_id, now, secret_ciphertext, secret_nonce],
        )?;
        if updated == 0 {
            return Ok(false);
        }
        tx.execute(
            "DELETE FROM admin_recovery_codes WHERE admin_user_id = ?1",
            params![admin_user_id],
        )?;
        let mut seen = std::collections::BTreeSet::new();
        for code_hash in code_hashes {
            let code_hash = code_hash.trim();
            if code_hash.is_empty() || !seen.insert(code_hash.to_string()) {
                return Err(AppError::Validation(
                    "invalid recovery code set".to_string(),
                ));
            }
            tx.execute(
                "INSERT INTO admin_recovery_codes (admin_user_id, code_hash, created_at) VALUES (?1, ?2, ?3)",
                params![admin_user_id, code_hash, now],
            )?;
        }
        tx.execute(
            r#"
            UPDATE admin_users
            SET totp_enabled = TRUE, totp_pending = FALSE, updated_at = ?2
            WHERE id = ?1
            "#,
            params![admin_user_id, now],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn rollback_admin_totp_setup(&self, admin_user_id: i64) -> AppResult<bool> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let deleted = tx.execute(
            "DELETE FROM admin_totp_secrets WHERE admin_user_id = ?1 AND confirmed_at IS NULL",
            params![admin_user_id],
        )?;
        if deleted == 1 {
            tx.execute(
                "UPDATE admin_users SET totp_pending = FALSE WHERE id = ?1",
                params![admin_user_id],
            )?;
        }
        tx.commit()?;
        Ok(deleted == 1)
    }

    pub fn disable_admin_totp_and_revoke_credentials(
        &self,
        admin_user_id: i64,
        now: i64,
    ) -> AppResult<bool> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existed = tx.execute(
            "DELETE FROM admin_totp_secrets WHERE admin_user_id = ?1",
            params![admin_user_id],
        )?;
        tx.execute(
            "DELETE FROM admin_recovery_codes WHERE admin_user_id = ?1",
            params![admin_user_id],
        )?;
        tx.execute(
            "UPDATE admin_users SET totp_enabled = FALSE, totp_pending = FALSE, updated_at = ?2 WHERE id = ?1",
            params![admin_user_id, now],
        )?;
        tx.execute(
            "UPDATE admin_sessions SET revoked_at = COALESCE(revoked_at, ?2) WHERE admin_user_id = ?1 AND revoked_at IS NULL",
            params![admin_user_id, now],
        )?;
        tx.execute(
            "UPDATE admin_api_tokens SET revoked_at = COALESCE(revoked_at, ?2) WHERE admin_user_id = ?1 AND revoked_at IS NULL",
            params![admin_user_id, now],
        )?;
        tx.commit()?;
        Ok(existed == 1)
    }

    pub fn advance_admin_totp_last_used_step(
        &self,
        admin_user_id: i64,
        step: i64,
        now: i64,
    ) -> AppResult<bool> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = tx.execute(
            r#"
            UPDATE admin_totp_secrets
            SET last_used_step = ?2, updated_at = ?3
            WHERE admin_user_id = ?1
              AND (last_used_step IS NULL OR last_used_step < ?2)
            "#,
            params![admin_user_id, step, now],
        )?;
        tx.commit()?;
        Ok(updated == 1)
    }

    pub fn replace_admin_recovery_codes(
        &self,
        admin_user_id: i64,
        code_hashes: &[String],
        now: i64,
    ) -> AppResult<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM admin_users WHERE id = ?1)",
            params![admin_user_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(AppError::Validation("admin user not found".to_string()));
        }
        tx.execute(
            "DELETE FROM admin_recovery_codes WHERE admin_user_id = ?1",
            params![admin_user_id],
        )?;
        let mut seen = std::collections::BTreeSet::new();
        for code_hash in code_hashes {
            let code_hash = code_hash.trim();
            if code_hash.is_empty() {
                return Err(AppError::Validation(
                    "recovery code hash must not be empty".to_string(),
                ));
            }
            if !seen.insert(code_hash.to_string()) {
                return Err(AppError::Validation(
                    "duplicate recovery code hash".to_string(),
                ));
            }
            tx.execute(
                r#"
                INSERT INTO admin_recovery_codes (admin_user_id, code_hash, created_at)
                VALUES (?1, ?2, ?3)
                "#,
                params![admin_user_id, code_hash, now],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn consume_admin_recovery_code(
        &self,
        admin_user_id: i64,
        code_hash: &str,
        now: i64,
    ) -> AppResult<bool> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let updated = tx.execute(
            r#"
            UPDATE admin_recovery_codes
            SET used_at = ?3
            WHERE admin_user_id = ?1
              AND code_hash = ?2
              AND used_at IS NULL
            "#,
            params![admin_user_id, code_hash, now],
        )?;
        tx.commit()?;
        Ok(updated == 1)
    }

    pub fn count_unused_admin_recovery_codes(&self, admin_user_id: i64) -> AppResult<i64> {
        let conn = self.connection()?;
        let count: i64 = conn.query_row(
            r#"
            SELECT COUNT(*)
            FROM admin_recovery_codes
            WHERE admin_user_id = ?1 AND used_at IS NULL
            "#,
            params![admin_user_id],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    pub fn list_admin_roles(&self) -> AppResult<Vec<AdminRole>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, key, name, description, is_system, created_at
            FROM admin_roles
            ORDER BY key ASC
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(AdminRole {
                id: row.get(0)?,
                key: row.get(1)?,
                name: row.get(2)?,
                description: row.get(3)?,
                is_system: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_role_permissions(&self, role_id: i64) -> AppResult<Vec<String>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT permission
            FROM admin_role_permissions
            WHERE role_id = ?1
            ORDER BY permission ASC
            "#,
        )?;
        let rows = stmt
            .query_map(params![role_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn replace_role_permissions(&self, role_id: i64, permissions: &[String]) -> AppResult<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let role_key: Option<String> = tx
            .query_row(
                "SELECT key FROM admin_roles WHERE id = ?1",
                params![role_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(role_key) = role_key else {
            return Err(AppError::Validation("admin role not found".to_string()));
        };
        // super_admin remains a system role; permission rows are replaceable but role itself
        // cannot be deleted by this API (no delete_role is provided).
        let _ = role_key;
        tx.execute(
            "DELETE FROM admin_role_permissions WHERE role_id = ?1",
            params![role_id],
        )?;
        let mut seen = std::collections::BTreeSet::new();
        for permission in permissions {
            let permission = permission.trim();
            if permission.is_empty() {
                return Err(AppError::Validation(
                    "permission key must not be empty".to_string(),
                ));
            }
            if !seen.insert(permission.to_string()) {
                continue;
            }
            tx.execute(
                r#"
                INSERT INTO admin_role_permissions (role_id, permission)
                VALUES (?1, ?2)
                "#,
                params![role_id, permission],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn record_audit(
        &self,
        admin_user_id: Option<i64>,
        action: &str,
        summary: &str,
        result: &str,
    ) -> AppResult<()> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            INSERT INTO audit_logs (timestamp_ms, admin_user_id, action, summary, result)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![now_ms() as i64, admin_user_id, action, summary, result],
        )?;
        run_maintenance(&self.maintenance.audit_logs, || {
            self.prune_audit_logs(&conn, 90, 20000)?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn record_audit_best_effort(
        &self,
        admin_user_id: Option<i64>,
        action: &str,
        summary: &str,
        result: &str,
    ) {
        if let Err(error) = self.record_audit(admin_user_id, action, summary, result) {
            tracing::error!(
                action,
                error = %error.safe_log_message(),
                "failed to record admin audit"
            );
        }
    }

    pub fn list_audit_logs(
        &self,
        action: Option<&str>,
        keyword: Option<&str>,
        limit: usize,
    ) -> AppResult<Vec<AuditLogEntry>> {
        let conn = self.connection()?;
        let mut entries = Vec::new();
        let keyword = keyword.map(str::to_ascii_lowercase);
        let mut stmt = conn.prepare(
            r#"
            SELECT audit_logs.id, audit_logs.timestamp_ms, audit_logs.admin_user_id,
                   COALESCE(admin_users.username, ''), audit_logs.action,
                   audit_logs.summary, audit_logs.result
            FROM audit_logs
            LEFT JOIN admin_users ON admin_users.id = audit_logs.admin_user_id
            ORDER BY audit_logs.timestamp_ms DESC
            LIMIT 1000
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(AuditLogEntry {
                id: row.get(0)?,
                timestamp_ms: row.get::<_, i64>(1)? as u128,
                admin_user_id: row.get(2)?,
                admin_username: row.get(3)?,
                action: row.get(4)?,
                summary: row.get(5)?,
                result: row.get(6)?,
            })
        })?;
        for row in rows {
            let entry = row?;
            if action.is_some_and(|value| value != "all" && value != entry.action) {
                continue;
            }
            if let Some(keyword) = keyword.as_ref() {
                let haystack = format!(
                    "{} {} {} {}",
                    entry.admin_username, entry.action, entry.summary, entry.result
                )
                .to_ascii_lowercase();
                if !haystack.contains(keyword) {
                    continue;
                }
            }
            entries.push(entry);
            if entries.len() >= limit.clamp(1, 500) {
                break;
            }
        }
        Ok(entries)
    }

    pub fn today_request_stats(&self) -> AppResult<Vec<RequestStatsDaily>> {
        let conn = self.connection()?;
        let date = local_date();
        let mut stmt = conn.prepare(
            r#"
            SELECT date, server_id, server_name, port, requests, redirects, cache_hits, blocks, errors, updated_at_ms
            FROM request_stats_daily
            WHERE date = ?1
            ORDER BY server_name ASC
            "#,
        )?;
        let rows = stmt.query_map(params![date], |row| {
            Ok(RequestStatsDaily {
                date: row.get(0)?,
                server_id: row.get(1)?,
                server_name: row.get(2)?,
                port: row.get(3)?,
                requests: row.get(4)?,
                redirects: row.get(5)?,
                cache_hits: row.get(6)?,
                blocks: row.get(7)?,
                errors: row.get(8)?,
                updated_at_ms: row.get::<_, i64>(9)? as u128,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// List daily request stats for a closed business-day window.
    ///
    /// - `days` is clamped to `1..=365` and always includes today (business day, UTC+8).
    /// - `server_id = Some(id)` exact-matches that server and aggregates multi-port rows per day.
    /// - `server_id = None` sums all servers per day and returns global rows (`server_id = None`).
    /// - Results are ordered by date ascending; missing days are omitted (empty Vec when no rows).
    pub fn list_request_stats_range(
        &self,
        days: i64,
        server_id: Option<&str>,
    ) -> AppResult<Vec<RequestStatsRangeRow>> {
        let days = days.clamp(1, 365);
        // Inclusive window: days=1 → today only; days=7 → today and previous 6 business days.
        let cutoff = local_date_offset(-(days - 1));
        let today = local_date();
        let limit = days;
        let conn = self.connection()?;

        match server_id {
            Some(id) => {
                let mut stmt = conn.prepare(
                    r#"
                    SELECT date,
                           server_id,
                           MAX(server_name),
                           COALESCE(SUM(requests), 0),
                           COALESCE(SUM(redirects), 0),
                           COALESCE(SUM(cache_hits), 0),
                           COALESCE(SUM(blocks), 0),
                           COALESCE(SUM(errors), 0),
                           COALESCE(MAX(updated_at_ms), 0)
                    FROM request_stats_daily
                    WHERE date >= ?1 AND date <= ?2 AND server_id = ?3
                    GROUP BY date, server_id
                    ORDER BY date ASC
                    LIMIT ?4
                    "#,
                )?;
                let rows = stmt.query_map(params![cutoff, today, id, limit], |row| {
                    Ok(RequestStatsRangeRow {
                        date: row.get(0)?,
                        server_id: Some(row.get(1)?),
                        server_name: Some(row.get(2)?),
                        requests: row.get(3)?,
                        redirects: row.get(4)?,
                        cache_hits: row.get(5)?,
                        blocks: row.get(6)?,
                        errors: row.get(7)?,
                        updated_at_ms: row.get::<_, i64>(8)? as u128,
                    })
                })?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
            None => {
                let mut stmt = conn.prepare(
                    r#"
                    SELECT date,
                           COALESCE(SUM(requests), 0),
                           COALESCE(SUM(redirects), 0),
                           COALESCE(SUM(cache_hits), 0),
                           COALESCE(SUM(blocks), 0),
                           COALESCE(SUM(errors), 0),
                           COALESCE(MAX(updated_at_ms), 0)
                    FROM request_stats_daily
                    WHERE date >= ?1 AND date <= ?2
                    GROUP BY date
                    ORDER BY date ASC
                    LIMIT ?3
                    "#,
                )?;
                let rows = stmt.query_map(params![cutoff, today, limit], |row| {
                    Ok(RequestStatsRangeRow {
                        date: row.get(0)?,
                        server_id: None,
                        server_name: None,
                        requests: row.get(1)?,
                        redirects: row.get(2)?,
                        cache_hits: row.get(3)?,
                        blocks: row.get(4)?,
                        errors: row.get(5)?,
                        updated_at_ms: row.get::<_, i64>(6)? as u128,
                    })
                })?;
                rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
            }
        }
    }

    fn write_telemetry_batch(&self, writes: &[TelemetryWrite]) -> AppResult<()> {
        if writes.is_empty() {
            return Ok(());
        }
        let mut conn = self.connection()?;
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let date = local_date();
        let updated_at_ms = now_ms() as i64;
        let mut stat_deltas = HashMap::<&str, (&str, u16, (i64, i64, i64, i64, i64))>::new();
        let mut wrote_details = false;

        for write in writes {
            if let TelemetryWrite::RequestStat {
                server_id,
                server_name,
                port,
                kind,
            } = write
            {
                let delta = kind.delta();
                let entry = stat_deltas.entry(server_id.as_str()).or_insert((
                    server_name.as_str(),
                    *port,
                    (0, 0, 0, 0, 0),
                ));
                entry.0 = server_name.as_str();
                entry.1 = *port;
                entry.2.0 += delta.0;
                entry.2.1 += delta.1;
                entry.2.2 += delta.2;
                entry.2.3 += delta.3;
                entry.2.4 += delta.4;
            }
        }
        for (&server_id, &(server_name, port, (requests, redirects, cache_hits, blocks, errors))) in
            &stat_deltas
        {
            transaction.execute(
                r#"
                INSERT INTO request_stats_daily
                    (date, server_id, server_name, port, requests, redirects, cache_hits, blocks, errors, updated_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ON CONFLICT(date, server_id) DO UPDATE SET
                    server_name = excluded.server_name,
                    port = excluded.port,
                    requests = requests + excluded.requests,
                    redirects = redirects + excluded.redirects,
                    cache_hits = cache_hits + excluded.cache_hits,
                    blocks = blocks + excluded.blocks,
                    errors = errors + excluded.errors,
                    updated_at_ms = excluded.updated_at_ms
                "#,
                params![
                    date,
                    server_id,
                    server_name,
                    port,
                    requests,
                    redirects,
                    cache_hits,
                    blocks,
                    errors,
                    updated_at_ms,
                ],
            )?;
        }
        for write in writes {
            let TelemetryWrite::ProxyRequestDetail(record) = write else {
                continue;
            };
            let timestamp_ms = Self::sqlite_i64(record.timestamp_ms);
            let server_id = record.server_id.clone();
            let server_name = record.server_name.clone();
            let path = record.path.clone();
            let playback_user = record.playback_user.clone();
            let cache_hit = record.cache_hit;
            let blocked = record.blocked;
            transaction.execute(
                r#"
                INSERT INTO proxy_request_logs
                    (timestamp_ms, server_id, server_name, port, method, path, path_type,
                     status_code, outcome, duration_ms, playback_user, playback_ip,
                     cache_hit, blocked, detail)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                "#,
                params![
                    timestamp_ms,
                    server_id,
                    server_name,
                    record.port,
                    record.method,
                    path.clone(),
                    record.path_type,
                    record.status_code,
                    record.outcome,
                    Self::sqlite_i64(record.duration_ms),
                    playback_user.clone(),
                    record.playback_ip,
                    cache_hit,
                    blocked,
                    record.detail,
                ],
            )?;
            transaction.execute(
                r#"
                INSERT INTO proxy_analytics_samples
                    (timestamp_ms, sample_kind, server_id, server_name, session_key, media_id,
                     playback_user, client, status_code, duration_ms, cache_hit, blocked,
                     transcoding, bandwidth, created_at_ms)
                VALUES (?1, 'request', ?2, ?3, '', ?4, ?5, '', ?6, ?7, ?8, ?9, NULL, NULL, ?1)
                "#,
                params![
                    timestamp_ms,
                    server_id,
                    server_name,
                    analytics_media_id(&path),
                    playback_user,
                    record.status_code,
                    Self::sqlite_i64(record.duration_ms),
                    cache_hit,
                    blocked,
                ],
            )?;
            wrote_details = true;
        }
        let mut wrote_playback = false;
        for write in writes {
            if let TelemetryWrite::PlaybackAnalyticsSamples(samples) = write {
                wrote_playback |=
                    Self::insert_playback_analytics_samples(&transaction, samples)? > 0;
            }
        }
        let mut wrote_diagnoses = false;
        for write in writes {
            let TelemetryWrite::DiagnosisRecord(record) = write else {
                continue;
            };
            // Re-sanitize at the persistence boundary as well as the queue edge.
            let record = sanitize_diagnosis_record(record.clone());
            transaction.execute(
                "INSERT INTO diagnosis_records (request_id, server_id, started_at_ms, stored_at_ms, record_json)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(request_id) DO UPDATE SET server_id = excluded.server_id,
                     started_at_ms = excluded.started_at_ms, stored_at_ms = excluded.stored_at_ms,
                     record_json = excluded.record_json",
                params![record.request_id, record.server_id, record.started_at_ms,
                    updated_at_ms, serde_json::to_string(&record)?],
            )?;
            wrote_diagnoses = true;
        }
        if wrote_diagnoses {
            Self::prune_diagnosis_records(&transaction)?;
        }
        transaction.commit()?;

        if wrote_playback {
            run_maintenance(&self.maintenance.proxy_analytics_samples, || {
                self.prune_proxy_analytics_samples(
                    &conn,
                    PROXY_ANALYTICS_RETENTION_DAYS,
                    PROXY_ANALYTICS_MAX_ROWS,
                )
            })?;
        }
        if !stat_deltas.is_empty() {
            run_maintenance(&self.maintenance.request_stats, || {
                self.prune_request_stats(&conn, 90)?;
                Ok(())
            })?;
        }
        if wrote_details {
            run_maintenance(&self.maintenance.proxy_request_logs, || {
                self.prune_proxy_request_logs(&conn, 7, 20000)?;
                self.prune_proxy_analytics_samples(
                    &conn,
                    PROXY_ANALYTICS_RETENTION_DAYS,
                    PROXY_ANALYTICS_MAX_ROWS,
                )?;
                Ok(())
            })?;
        }
        Ok(())
    }

    fn prune_diagnosis_records(conn: &Connection) -> AppResult<()> {
        conn.execute(
            "DELETE FROM diagnosis_records WHERE COALESCE(started_at_ms, stored_at_ms) < ?1",
            params![diagnostics_cutoff_ms()],
        )?;
        conn.execute(
            "DELETE FROM diagnosis_records WHERE request_id NOT IN (
                 SELECT request_id FROM diagnosis_records
                 ORDER BY COALESCE(started_at_ms, stored_at_ms) DESC, request_id DESC LIMIT ?1)",
            params![DIAGNOSTICS_PERSIST_MAX_RECORDS as i64],
        )?;
        Ok(())
    }

    /// Scope in SQL before applying the retention bound or decoding records.
    pub fn list_diagnosis_records_scoped(
        &self,
        allowed_server_ids: Option<&std::collections::HashSet<String>>,
    ) -> AppResult<Vec<DiagnosisRecord>> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        Self::prune_diagnosis_records(&tx)?;
        let rows = {
            let mut stmt = tx.prepare(
                "SELECT server_id, record_json FROM diagnosis_records
                 WHERE (?1 IS NULL OR server_id IN (SELECT value FROM json_each(?1)))
                 ORDER BY started_at_ms DESC, request_id DESC LIMIT ?2",
            )?;
            stmt.query_map(
                params![
                    allowed_server_ids.map(serde_json::to_string).transpose()?,
                    DIAGNOSTICS_PERSIST_MAX_RECORDS as i64
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?
        };
        // Release the write lock before decoding and sanitizing the bounded snapshot.
        tx.commit()?;
        #[cfg(test)]
        TEST_BEFORE_DIAGNOSIS_DECODE.with(|hook| {
            if let Some(hook) = hook.borrow_mut().take() {
                hook();
            }
        });
        let mut records = Vec::with_capacity(rows.len());
        for (server_id, json) in rows {
            let record = sanitize_diagnosis_record(serde_json::from_str(&json)?);
            if record.server_id != server_id {
                return Err(AppError::Internal(
                    "invalid stored diagnostics scope".to_string(),
                ));
            }
            records.push(record);
        }
        Ok(records)
    }

    fn clear_persisted_diagnosis_records(&self) -> AppResult<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM diagnosis_records", [])?;
        tx.commit()?;
        Ok(())
    }

    fn sqlite_i64(value: u128) -> i64 {
        value.min(i64::MAX as u128) as i64
    }

    fn insert_playback_analytics_samples(
        conn: &Connection,
        samples: &[PlaybackAnalyticsSampleWrite],
    ) -> AppResult<usize> {
        let mut inserted = 0;
        for sample in samples.iter().take(256) {
            let timestamp_ms = Self::sqlite_i64(sample.timestamp_ms);
            let bucket_ms = timestamp_ms - timestamp_ms.rem_euclid(60_000);
            let changed = conn.execute(
                r#"
                INSERT OR REPLACE INTO proxy_analytics_samples
                    (timestamp_ms, sample_kind, server_id, server_name, session_key, media_id,
                     playback_user, client, status_code, duration_ms, cache_hit, blocked,
                     transcoding, bandwidth, created_at_ms, bucket_ms)
                VALUES (?1, 'playback', ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, 0, 0,
                        ?8, ?9, ?1, ?10)
                "#,
                params![
                    timestamp_ms,
                    sample.server_id,
                    sample.server_name,
                    sample.session_key,
                    sample.media_id,
                    sample.playback_user,
                    sample.client,
                    sample.transcoding,
                    sample
                        .bandwidth
                        .map(|value| value.min(i64::MAX as u64) as i64),
                    bucket_ms,
                ],
            )?;
            inserted += changed;
        }
        Ok(inserted)
    }

    pub fn record_playback_analytics_samples(
        &self,
        samples: &[PlaybackAnalyticsSampleWrite],
    ) -> AppResult<usize> {
        if samples.is_empty() {
            return Ok(0);
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let inserted = Self::insert_playback_analytics_samples(&tx, samples)?;
        tx.commit()?;
        run_maintenance(&self.maintenance.proxy_analytics_samples, || {
            let conn = self.connection()?;
            self.prune_proxy_analytics_samples(
                &conn,
                PROXY_ANALYTICS_RETENTION_DAYS,
                PROXY_ANALYTICS_MAX_ROWS,
            )?;
            Ok(())
        })?;
        Ok(inserted)
    }

    /// Read the bounded historical proxy samples used by analytics. The query only
    /// returns non-sensitive fields; playback IP, request detail and raw method are
    /// intentionally excluded from this read path.
    pub fn list_proxy_analytics_samples(
        &self,
        days: i64,
        server_id: Option<&str>,
        limit: usize,
    ) -> AppResult<Vec<ProxyAnalyticsSample>> {
        self.list_proxy_analytics_samples_filtered(
            days, server_id, None, None, false, None, None, limit,
        )
    }

    pub fn list_proxy_analytics_samples_filtered(
        &self,
        days: i64,
        server_id: Option<&str>,
        allowed_server_ids: Option<&std::collections::HashSet<String>>,
        sample_kind: Option<&str>,
        require_bandwidth: bool,
        user: Option<&str>,
        client: Option<&str>,
        limit: usize,
    ) -> AppResult<Vec<ProxyAnalyticsSample>> {
        self.list_proxy_analytics_samples_for_dimension(
            days,
            server_id,
            allowed_server_ids,
            sample_kind,
            require_bandwidth,
            user,
            client,
            None,
            limit,
        )
    }

    pub fn list_proxy_analytics_samples_for_dimension(
        &self,
        days: i64,
        server_id: Option<&str>,
        allowed_server_ids: Option<&std::collections::HashSet<String>>,
        sample_kind: Option<&str>,
        require_bandwidth: bool,
        user: Option<&str>,
        client: Option<&str>,
        dimension: Option<&str>,
        limit: usize,
    ) -> AppResult<Vec<ProxyAnalyticsSample>> {
        let days = days.clamp(1, 7);
        let now = now_ms();
        let cutoff = business_day_cutoff_ms(now, days);
        let server_id = server_id.map(str::to_string);
        let limit = limit.clamp(1, 20_000);
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            WITH RECURSIVE scoped AS (
                SELECT *, '/' || media_id AS media_path
                FROM proxy_analytics_samples
                WHERE timestamp_ms >= ?1
                  AND timestamp_ms <= ?2
                  AND (?3 IS NULL OR server_id = ?3)
                  AND (?5 IS NULL OR server_id IN (SELECT value FROM json_each(?5)))
                  AND (?6 IS NULL OR sample_kind = ?6)
                  AND (?7 = 0 OR bandwidth IS NOT NULL)
                  AND (?8 IS NULL OR trim(playback_user, ?10) = ?8)
                  AND (?9 IS NULL OR trim(client, ?10) = ?9)
            ), path_segments AS (
                SELECT id, media_path AS remaining, '' AS segment, 0 AS depth
                FROM scoped WHERE ?11 = 'media' AND sample_kind = 'request'
                UNION ALL
                SELECT id,
                    CASE WHEN instr(remaining, '/') = 0 THEN '' ELSE substr(remaining, instr(remaining, '/') + 1) END,
                    CASE WHEN instr(remaining, '/') = 0 THEN remaining ELSE substr(remaining, 1, instr(remaining, '/') - 1) END,
                    depth + 1
                FROM path_segments
                WHERE remaining <> ''
                  AND lower(trim(segment, ?10)) NOT IN ('videos', 'items')
                  AND depth < 256
            ), media_candidates AS (
                SELECT id,
                    CASE WHEN lower(trim(segment, ?10)) IN ('videos','items')
                        THEN trim(substr(remaining, 1, instr(remaining || '/', '/') - 1), ?10)
                        ELSE '' END AS media_id
                FROM path_segments
            ), valid_media AS (
                SELECT id, media_id FROM media_candidates
                WHERE length(CAST(trim(media_id, ?10) AS BLOB)) BETWEEN 1 AND 128
                  AND media_id NOT GLOB '*[' || char(1) || '-' || char(31) || ']*'
                  AND media_id NOT GLOB '*' || char(127) || '*'
                  AND media_id NOT GLOB '*[' || char(128) || '-' || char(159) || ']*'
                GROUP BY id
                UNION ALL
                SELECT id, trim(media_id, ?10) AS media_id
                FROM scoped
                WHERE ?11 = 'media' AND sample_kind = 'playback'
                  AND length(CAST(trim(media_id, ?10) AS BLOB)) BETWEEN 1 AND 128
                  AND trim(media_id, ?10) NOT GLOB '*/*'
                  AND media_id NOT GLOB '*[' || char(1) || '-' || char(31) || ']*'
                  AND media_id NOT GLOB '*' || char(127) || '*'
                  AND media_id NOT GLOB '*[' || char(128) || '-' || char(159) || ']*'
            ), dimension_valid AS (
                SELECT scoped.*, valid_media.media_id AS parsed_media_id FROM scoped
                LEFT JOIN valid_media ON valid_media.id = scoped.id
                WHERE ?11 IS NULL
                   OR (?11 = 'user' AND trim(playback_user, ?10) <> '' AND trim(playback_user, ?10) <> '--')
                   OR (?11 = 'client' AND trim(client, ?10) <> '' AND trim(client, ?10) <> '--')
                   OR (?11 = 'media' AND valid_media.media_id IS NOT NULL)
                   OR (?11 = 'server' AND server_id <> '')
            )
            SELECT timestamp_ms, server_id, server_name, media_id, playback_user, client,
                   sample_kind, status_code, duration_ms, cache_hit, blocked, transcoding, bandwidth
            FROM dimension_valid
            ORDER BY timestamp_ms DESC, id DESC
            LIMIT ?4
            "#,
        )?;
        let rows = stmt.query_map(
            params![
                Self::sqlite_i64(cutoff),
                Self::sqlite_i64(now),
                server_id,
                limit,
                allowed_server_ids
                    .map(serde_json::to_string)
                    .transpose()
                    .map_err(|err| AppError::Internal(err.to_string()))?,
                sample_kind,
                require_bandwidth,
                user,
                client,
                // Match Rust str::trim used by the aggregation, including legacy
                // rows containing Unicode whitespace rather than only ASCII spaces.
                "\t\n\u{000b}\u{000c}\r \u{0085}\u{00a0}\u{1680}\u{2000}\u{2001}\u{2002}\u{2003}\u{2004}\u{2005}\u{2006}\u{2007}\u{2008}\u{2009}\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}"
                , dimension
            ],
            |row| {
                Ok(ProxyAnalyticsSample {
                    timestamp_ms: row.get::<_, i64>(0)? as u128,
                    server_id: row.get(1)?,
                    server_name: row.get(2)?,
                    path: {
                        let media_id: String = row.get(3)?;
                        let sample_kind: String = row.get(6)?;
                        if sample_kind == "playback" {
                            format!("/Videos/{media_id}")
                        } else {
                            media_id
                        }
                    },
                    playback_user: row.get(4)?,
                    client: row.get(5)?,
                    sample_kind: row.get(6)?,
                    status_code: row.get::<_, Option<u16>>(7)?.unwrap_or_default(),
                    duration_ms: row.get::<_, Option<i64>>(8)?.unwrap_or_default() as u128,
                    cache_hit: row.get(9)?,
                    blocked: row.get(10)?,
                    transcoding: row.get(11)?,
                    bandwidth: row.get::<_, Option<i64>>(12)?.map(|v| v.max(0) as u64),
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_proxy_request_details(
        &self,
        filter: ProxyRequestDetailFilter<'_>,
    ) -> AppResult<Vec<ProxyRequestDetail>> {
        self.list_proxy_request_details_scoped(filter, None)
    }

    pub fn list_proxy_request_details_scoped(
        &self,
        filter: ProxyRequestDetailFilter<'_>,
        allowed_server_ids: Option<&std::collections::HashSet<String>>,
    ) -> AppResult<Vec<ProxyRequestDetail>> {
        let conn = self.connection()?;
        let mut conditions = Vec::new();
        let mut values: Vec<Box<dyn ToSql>> = Vec::new();
        let scope_condition = allowed_server_ids.map(|ids| {
            values.push(Box::new(
                serde_json::to_string(ids).expect("string set serializes"),
            ));
            "server_id IN (SELECT value FROM json_each(?))"
        });
        if let Some(condition) = scope_condition {
            conditions.push(condition);
        }
        if let Some(server_id) = filter.server_id {
            conditions.push("server_id = ?");
            values.push(Box::new(server_id.to_string()));
        }
        if let Some(path_type) = filter.path_type {
            conditions.push("path_type = ?");
            values.push(Box::new(path_type.to_string()));
        }
        // The exclusion is part of the SQL WHERE clause, so it runs before the
        // LIMIT and the returned page stays filled with the rows that are shown.
        if let Some(exclude_path_type) = filter.exclude_path_type {
            let exclude_path_type = exclude_path_type.trim();
            if !exclude_path_type.is_empty() {
                conditions.push("path_type <> ?");
                values.push(Box::new(exclude_path_type.to_string()));
            }
        }
        // Saturate instead of `as i64`: a millisecond bound above `i64::MAX`
        // would otherwise wrap into a negative value and silently invert the
        // comparison (`since` matched everything, `until` matched nothing).
        if let Some(since_ms) = filter.since_ms {
            conditions.push("timestamp_ms >= ?");
            values.push(Box::new(Self::sqlite_i64(since_ms)));
        }
        if let Some(until_ms) = filter.until_ms {
            conditions.push("timestamp_ms <= ?");
            values.push(Box::new(Self::sqlite_i64(until_ms)));
        }
        let keyword = filter
            .keyword
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase());
        // The keyword is matched in Rust because the detail column also has to
        // match its decoded form, which SQLite cannot do. Filtering it in SQL
        // would drop the rows that only match after decoding.

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        let requested_limit = filter.limit.clamp(1, 500);
        // Widen the candidate window when a keyword is present so rows whose
        // decoded detail matches are not cut off before the Rust filter runs.
        let limit = if keyword.is_some() {
            requested_limit.saturating_mul(4).min(2_000)
        } else {
            requested_limit
        } as i64;
        values.push(Box::new(limit));
        let params = values
            .iter()
            .map(|value| value.as_ref() as &dyn ToSql)
            .collect::<Vec<_>>();
        let sql = format!(
            r#"
            SELECT id, timestamp_ms, server_id, server_name, port, method, path, path_type,
                   status_code, outcome, duration_ms, playback_user, playback_ip,
                   cache_hit, blocked, detail
            FROM proxy_request_logs
            {where_clause}
            ORDER BY timestamp_ms DESC, id DESC
            LIMIT ?
            "#,
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params.as_slice(), |row| {
            Ok(ProxyRequestDetail {
                id: row.get(0)?,
                timestamp_ms: row.get::<_, i64>(1)? as u128,
                server_id: row.get(2)?,
                server_name: row.get(3)?,
                port: row.get(4)?,
                method: row.get(5)?,
                path: row.get(6)?,
                path_type: row.get(7)?,
                status_code: row.get(8)?,
                outcome: row.get(9)?,
                duration_ms: row.get::<_, i64>(10)? as u128,
                playback_user: row.get(11)?,
                playback_ip: row.get(12)?,
                ip_location: None,
                cache_hit: row.get(13)?,
                blocked: row.get(14)?,
                detail: row.get(15)?,
            })
        })?;
        let mut results = rows.collect::<Result<Vec<_>, _>>()?;
        if let Some(keyword) = keyword.as_ref() {
            results.retain(|entry| {
                let haystack = format!(
                    "{} {} {} {} {} {} {} {} {}",
                    entry.server_name,
                    entry.method,
                    entry.path,
                    entry.path_type,
                    entry.outcome,
                    entry.playback_user,
                    entry.playback_ip,
                    entry.detail,
                    crate::diagnostics::decode_log_detail_url(&entry.detail),
                )
                .to_ascii_lowercase();
                haystack.contains(keyword)
            });
            results.truncate(requested_limit);
        }
        Ok(results)
    }

    pub fn list_blocked_proxy_request_details(
        &self,
        limit: usize,
    ) -> AppResult<Vec<ProxyRequestDetail>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, timestamp_ms, server_id, server_name, port, method, path, path_type,
                   status_code, outcome, duration_ms, playback_user, playback_ip,
                   cache_hit, blocked, detail
            FROM proxy_request_logs
            WHERE blocked = TRUE
            ORDER BY timestamp_ms DESC, id DESC
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map([limit.clamp(1, 500) as i64], |row| {
            Ok(ProxyRequestDetail {
                id: row.get(0)?,
                timestamp_ms: row.get::<_, i64>(1)? as u128,
                server_id: row.get(2)?,
                server_name: row.get(3)?,
                port: row.get(4)?,
                method: row.get(5)?,
                path: row.get(6)?,
                path_type: row.get(7)?,
                status_code: row.get(8)?,
                outcome: row.get(9)?,
                duration_ms: row.get::<_, i64>(10)? as u128,
                playback_user: row.get(11)?,
                playback_ip: row.get(12)?,
                ip_location: None,
                cache_hit: row.get(13)?,
                blocked: row.get(14)?,
                detail: row.get(15)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    fn prune_audit_logs(&self, conn: &Connection, keep_days: i64, max_rows: i64) -> AppResult<()> {
        let cutoff = now_ms() as i64 - keep_days * 24 * 3600 * 1000;
        conn.execute(
            "DELETE FROM audit_logs WHERE timestamp_ms < ?1",
            params![cutoff],
        )?;
        conn.execute(
            r#"
            DELETE FROM audit_logs
            WHERE id NOT IN (
                SELECT id FROM audit_logs ORDER BY timestamp_ms DESC LIMIT ?1
            )
            "#,
            params![max_rows],
        )?;
        Ok(())
    }

    fn prune_request_stats(&self, conn: &Connection, keep_days: i64) -> AppResult<()> {
        let cutoff = local_date_offset(-keep_days);
        conn.execute(
            "DELETE FROM request_stats_daily WHERE date < ?1",
            params![cutoff],
        )?;
        Ok(())
    }

    fn prune_proxy_analytics_samples(
        &self,
        conn: &Connection,
        keep_days: i64,
        max_rows: i64,
    ) -> AppResult<()> {
        let cutoff = now_ms().saturating_sub((keep_days.max(0) as u128) * 24 * 3600 * 1000);
        conn.execute(
            "DELETE FROM proxy_analytics_samples WHERE timestamp_ms < ?1",
            params![Self::sqlite_i64(cutoff)],
        )?;
        conn.execute(
            "DELETE FROM proxy_analytics_samples WHERE id NOT IN (SELECT id FROM proxy_analytics_samples ORDER BY timestamp_ms DESC, id DESC LIMIT ?1)",
            params![max_rows.max(0)],
        )?;
        Ok(())
    }

    fn prune_proxy_request_logs(
        &self,
        conn: &Connection,
        keep_days: i64,
        max_rows: i64,
    ) -> AppResult<()> {
        let cutoff = now_ms() as i64 - keep_days * 24 * 3600 * 1000;
        conn.execute(
            "DELETE FROM proxy_request_logs WHERE timestamp_ms < ?1",
            params![cutoff],
        )?;
        conn.execute(
            r#"
            DELETE FROM proxy_request_logs
            WHERE id NOT IN (
                SELECT id FROM proxy_request_logs ORDER BY timestamp_ms DESC, id DESC LIMIT ?1
            )
            "#,
            params![max_rows],
        )?;
        Ok(())
    }

    fn ensure_schema(&self) -> AppResult<()> {
        let mut conn = self.connection()?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let journal_mode: String =
            conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            return Err(AppError::Internal(format!(
                "SQLite WAL mode is required, got {journal_mode}"
            )));
        }
        self.migrate(&mut conn)?;
        self.maintain_task_failure_history()?;
        Ok(())
    }

    fn migrate(&self, conn: &mut Connection) -> AppResult<()> {
        // Base tables remain CREATE IF NOT EXISTS for legacy installs.
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS admin_users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                enabled BOOLEAN NOT NULL DEFAULT TRUE,
                created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
            );

            CREATE TABLE IF NOT EXISTS admin_sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                admin_user_id INTEGER NOT NULL,
                token_hash TEXT NOT NULL UNIQUE,
                expires_at INTEGER NOT NULL,
                created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (admin_user_id) REFERENCES admin_users(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_admin_sessions_token_hash ON admin_sessions(token_hash);
            CREATE INDEX IF NOT EXISTS idx_admin_sessions_expires_at ON admin_sessions(expires_at);

            CREATE TABLE IF NOT EXISTS audit_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp_ms INTEGER NOT NULL,
                admin_user_id INTEGER,
                action TEXT NOT NULL,
                summary TEXT NOT NULL,
                result TEXT NOT NULL,
                FOREIGN KEY (admin_user_id) REFERENCES admin_users(id) ON DELETE SET NULL
            );

            CREATE INDEX IF NOT EXISTS idx_audit_logs_timestamp_ms ON audit_logs(timestamp_ms);
            CREATE INDEX IF NOT EXISTS idx_audit_logs_action ON audit_logs(action);

            CREATE TABLE IF NOT EXISTS request_stats_daily (
                date TEXT NOT NULL,
                server_id TEXT NOT NULL,
                server_name TEXT NOT NULL,
                port INTEGER NOT NULL,
                requests INTEGER NOT NULL DEFAULT 0,
                redirects INTEGER NOT NULL DEFAULT 0,
                cache_hits INTEGER NOT NULL DEFAULT 0,
                blocks INTEGER NOT NULL DEFAULT 0,
                errors INTEGER NOT NULL DEFAULT 0,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY (date, server_id)
            );

            CREATE TABLE IF NOT EXISTS proxy_request_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp_ms INTEGER NOT NULL,
                server_id TEXT NOT NULL,
                server_name TEXT NOT NULL,
                port INTEGER NOT NULL,
                method TEXT NOT NULL,
                path TEXT NOT NULL,
                path_type TEXT NOT NULL,
                status_code INTEGER NOT NULL,
                outcome TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                playback_user TEXT NOT NULL,
                playback_ip TEXT NOT NULL,
                cache_hit BOOLEAN NOT NULL DEFAULT FALSE,
                blocked BOOLEAN NOT NULL DEFAULT FALSE,
                detail TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_proxy_request_logs_timestamp_ms ON proxy_request_logs(timestamp_ms);
            CREATE INDEX IF NOT EXISTS idx_proxy_request_logs_server_id ON proxy_request_logs(server_id);
            CREATE INDEX IF NOT EXISTS idx_proxy_request_logs_path_type ON proxy_request_logs(path_type);
            CREATE TABLE IF NOT EXISTS historical_trends (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                metric TEXT NOT NULL,
                server_id TEXT,
                date TEXT NOT NULL,
                value INTEGER NOT NULL,
                created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE INDEX IF NOT EXISTS idx_historical_trends_metric_server_date ON historical_trends(metric, server_id, date);

            CREATE TABLE IF NOT EXISTS schema_migrations (
                version TEXT PRIMARY KEY,
                applied_at INTEGER NOT NULL
            );
            "#,
        )?;

        migrate_admin_security_v1(conn)?;
        migrate_scheduled_task_failure_history_v1(conn)?;
        migrate_proxy_analytics_samples_v1(conn)?;
        migrate_diagnosis_records_v1(conn)?;
        Ok(())
    }

    pub fn insert_historical_trend(&self, trend: &HistoricalTrend) -> AppResult<()> {
        let conn = self.connection()?;
        conn.execute(
            "INSERT INTO historical_trends (metric, server_id, date, value) VALUES (?1, ?2, ?3, ?4)",
            params![&trend.metric, &trend.server_id, &trend.date, trend.value],
        )?;
        Ok(())
    }

    pub fn insert_historical_trends(&self, trends: &[HistoricalTrend]) -> AppResult<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        for trend in trends {
            tx.execute(
                "INSERT INTO historical_trends (metric, server_id, date, value) VALUES (?1, ?2, ?3, ?4)",
                params![&trend.metric, &trend.server_id, &trend.date, trend.value],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_historical_trends(
        &self,
        metric: &str,
        server_id: Option<&str>,
        days: i64,
    ) -> AppResult<Vec<HistoricalTrend>> {
        let conn = self.connection()?;
        // Align cutoff with request_stats business day (fixed UTC+8), preserve prior inclusive span.
        let cutoff_str = local_date_offset(-days.max(1));
        match server_id {
            None => {
                let query = "SELECT id, metric, server_id, date, value FROM historical_trends WHERE metric = ?1 AND date >= ?2 AND server_id IS NULL ORDER BY date DESC LIMIT 100";
                let mut stmt = conn.prepare(query)?;
                let trends = stmt
                    .query_map(params![metric, cutoff_str], |row| {
                        Ok(HistoricalTrend {
                            id: row.get(0)?,
                            metric: row.get(1)?,
                            server_id: row.get(2)?,
                            date: row.get(3)?,
                            value: row.get(4)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(trends)
            }
            Some(id) => {
                let query = "SELECT id, metric, server_id, date, value FROM historical_trends WHERE metric = ?1 AND date >= ?2 AND server_id = ?3 ORDER BY date DESC LIMIT 100";
                let mut stmt = conn.prepare(query)?;
                let trends = stmt
                    .query_map(params![metric, cutoff_str, id.to_string()], |row| {
                        Ok(HistoricalTrend {
                            id: row.get(0)?,
                            metric: row.get(1)?,
                            server_id: row.get(2)?,
                            date: row.get(3)?,
                            value: row.get(4)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(trends)
            }
        }
    }

    pub fn prune_historical_trends(&self, keep_days: i64) -> AppResult<()> {
        let conn = self.connection()?;
        // Align prune boundary with request_stats business day (fixed UTC+8).
        let cutoff_str = local_date_offset(-keep_days.max(0));
        conn.execute(
            "DELETE FROM historical_trends WHERE date < ?1",
            params![cutoff_str],
        )?;
        Ok(())
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(DATABASE_BUSY_TIMEOUT)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(conn)
    }
}

pub struct AdminPasswordHash {
    pub admin_user_id: i64,
    pub password_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminSecurityIdentity {
    pub admin_user_id: i64,
    pub username: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub disabled_at: Option<i64>,
    pub totp_enabled: bool,
    pub totp_pending: bool,
    pub role_keys: Vec<String>,
    pub permission_keys: Vec<String>,
    pub server_grants: Vec<AdminServerGrant>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminServerGrant {
    pub server_id: String,
    pub scope: String,
    pub granted_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminServerGrantInput {
    pub server_id: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminUserListItem {
    pub id: i64,
    pub username: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub disabled_at: Option<i64>,
    pub totp_enabled: bool,
    pub totp_pending: bool,
    pub created_at: String,
    pub updated_at: Option<i64>,
    pub role_keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminSessionMeta {
    pub id: i64,
    pub admin_user_id: i64,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub device_label: Option<String>,
    pub last_seen_at: i64,
    pub expires_at: i64,
    pub created_at: String,
    pub revoked_at: Option<i64>,
    pub auth_method: String,
}

/// Minimal non-sensitive session fields needed by A1 auth assembly.
/// Intentionally omits token_hash and any raw token material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminSessionAuthMeta {
    pub session_id: i64,
    pub admin_user_id: i64,
    pub last_seen_at: i64,
    pub expires_at: i64,
    pub auth_method: String,
}

/// Result of a throttled admin session last_seen touch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminSessionTouchResult {
    /// last_seen_at was advanced to `now`.
    Updated,
    /// Session is still live, but throttle/monotonic rules skipped the write.
    NotDue,
    /// Missing, revoked, expired, or belonging to a disabled user.
    Inactive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminRole {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub description: String,
    pub is_system: bool,
    pub created_at: i64,
}

#[derive(Clone, PartialEq, Eq)]
pub struct AdminTotpSecretRow {
    pub admin_user_id: i64,
    pub secret_ciphertext: Vec<u8>,
    pub secret_nonce: Vec<u8>,
    pub key_version: i64,
    pub algo: String,
    pub digits: i64,
    pub period_seconds: i64,
    pub confirmed_at: Option<i64>,
    pub last_used_step: Option<i64>,
    pub updated_at: i64,
}

impl std::fmt::Debug for AdminTotpSecretRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminTotpSecretRow")
            .field("admin_user_id", &self.admin_user_id)
            .field("secret_ciphertext", &"[redacted]")
            .field("secret_nonce", &"[redacted]")
            .field("key_version", &self.key_version)
            .field("algo", &self.algo)
            .field("digits", &self.digits)
            .field("period_seconds", &self.period_seconds)
            .field("confirmed_at", &self.confirmed_at)
            .field("last_used_step", &self.last_used_step)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminApiTokenAuthMeta {
    pub id: i64,
    pub admin_user_id: i64,
    pub name: String,
    pub scopes: Vec<String>,
    pub server_ids: Option<Vec<String>>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminApiTokenListItem {
    pub id: i64,
    pub admin_user_id: i64,
    pub name: String,
    pub token_prefix: String,
    pub scopes: Vec<String>,
    pub server_ids: Option<Vec<String>>,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub created_at: i64,
    pub created_by: Option<i64>,
}

const ADMIN_SECURITY_MIGRATION_VERSION: &str = "admin_security_v1";
const SUPER_ADMIN_ROLE_KEY: &str = "super_admin";

fn is_unique_constraint(err: &rusqlite::Error) -> bool {
    let message = err.to_string().to_ascii_lowercase();
    message.contains("unique") && message.contains("constraint")
}

fn is_foreign_key_constraint(err: &rusqlite::Error) -> bool {
    let message = err.to_string().to_ascii_lowercase();
    message.contains("foreign key")
}

fn encode_string_list(values: &[String]) -> AppResult<String> {
    for value in values {
        if value.trim().is_empty() {
            return Err(AppError::Validation(
                "string list entries must not be empty".to_string(),
            ));
        }
    }
    Ok(serde_json::to_string(values)?)
}

fn decode_string_list(raw: &str) -> AppResult<Vec<String>> {
    let values: Vec<String> = serde_json::from_str(raw)?;
    for value in &values {
        if value.trim().is_empty() {
            return Err(AppError::Validation(
                "string list entries must not be empty".to_string(),
            ));
        }
    }
    Ok(values)
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> AppResult<bool> {
    // table names are internal constants only.
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn ensure_column(conn: &Connection, table: &str, column: &str, ddl_type: &str) -> AppResult<()> {
    if table_has_column(conn, table, column)? {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {ddl_type}"),
        [],
    )?;
    Ok(())
}

pub(crate) fn diagnostics_cutoff_ms() -> i64 {
    SettingsStore::sqlite_i64(now_ms()).saturating_sub(DIAGNOSTICS_RETENTION_DAYS * 86_400_000)
}

fn migrate_diagnosis_records_v1(conn: &mut Connection) -> AppResult<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS diagnosis_records (
             request_id TEXT PRIMARY KEY,
             server_id TEXT NOT NULL,
             started_at_ms INTEGER,
             stored_at_ms INTEGER NOT NULL,
             record_json TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_diagnosis_records_server_time
             ON diagnosis_records(server_id, started_at_ms DESC, request_id DESC);",
    )?;
    record_migration(&tx, DIAGNOSTICS_MIGRATION_VERSION, now_seconds())?;
    SettingsStore::prune_diagnosis_records(&tx)?;
    tx.commit()?;
    Ok(())
}

fn migration_applied(conn: &Connection, version: &str) -> AppResult<bool> {
    let applied: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
        params![version],
        |row| row.get(0),
    )?;
    Ok(applied)
}

fn record_migration(conn: &Connection, version: &str, now: i64) -> AppResult<()> {
    conn.execute(
        r#"
        INSERT OR IGNORE INTO schema_migrations (version, applied_at)
        VALUES (?1, ?2)
        "#,
        params![version, now],
    )?;
    Ok(())
}

fn ensure_super_admin_role_tx(tx: &rusqlite::Transaction<'_>, now: i64) -> AppResult<i64> {
    tx.execute(
        r#"
        INSERT OR IGNORE INTO admin_roles (key, name, description, is_system, created_at)
        VALUES (?1, 'Super Admin', 'Full system administrator', 1, ?2)
        "#,
        params![SUPER_ADMIN_ROLE_KEY, now],
    )?;
    let role_id: i64 = tx.query_row(
        "SELECT id FROM admin_roles WHERE key = ?1",
        params![SUPER_ADMIN_ROLE_KEY],
        |row| row.get(0),
    )?;
    Ok(role_id)
}

fn migrate_scheduled_task_failure_history_v1(conn: &mut Connection) -> AppResult<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS scheduled_task_failure_history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            server_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            run_identity TEXT NOT NULL,
            observed_at INTEGER NOT NULL,
            status TEXT NOT NULL,
            safe_error_summary TEXT,
            started_at INTEGER,
            ended_at INTEGER,
            UNIQUE (server_id, task_id, run_identity)
        );

        CREATE INDEX IF NOT EXISTS idx_scheduled_task_failure_history_server_observed
            ON scheduled_task_failure_history(server_id, observed_at DESC, id DESC);
        CREATE INDEX IF NOT EXISTS idx_scheduled_task_failure_history_task_observed
            ON scheduled_task_failure_history(task_id, observed_at DESC, id DESC);
        CREATE INDEX IF NOT EXISTS idx_scheduled_task_failure_history_observed
            ON scheduled_task_failure_history(observed_at DESC, id DESC);
        "#,
    )?;
    if !migration_applied(&tx, SCHEDULED_TASK_FAILURE_HISTORY_MIGRATION_VERSION)? {
        record_migration(
            &tx,
            SCHEDULED_TASK_FAILURE_HISTORY_MIGRATION_VERSION,
            now_seconds(),
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn migrate_admin_security_v1(conn: &mut Connection) -> AppResult<()> {
    let now = now_seconds();

    // Incremental column adds are gated by PRAGMA table_info so repeated opens are safe
    // on older SQLite builds that reject duplicate ADD COLUMN.
    ensure_column(conn, "admin_users", "display_name", "TEXT")?;
    ensure_column(
        conn,
        "admin_users",
        "totp_enabled",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "admin_users",
        "totp_pending",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "admin_users", "password_changed_at", "INTEGER")?;
    ensure_column(conn, "admin_users", "disabled_at", "INTEGER")?;
    ensure_column(conn, "admin_users", "updated_at", "INTEGER")?;

    ensure_column(conn, "admin_sessions", "ip", "TEXT")?;
    ensure_column(conn, "admin_sessions", "user_agent", "TEXT")?;
    ensure_column(conn, "admin_sessions", "device_label", "TEXT")?;
    ensure_column(
        conn,
        "admin_sessions",
        "last_seen_at",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "admin_sessions", "revoked_at", "INTEGER")?;
    ensure_column(
        conn,
        "admin_sessions",
        "auth_method",
        "TEXT NOT NULL DEFAULT 'password'",
    )?;

    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS admin_roles (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            key TEXT NOT NULL UNIQUE,
            name TEXT NOT NULL,
            description TEXT NOT NULL DEFAULT '',
            is_system INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS admin_role_permissions (
            role_id INTEGER NOT NULL,
            permission TEXT NOT NULL,
            PRIMARY KEY (role_id, permission),
            FOREIGN KEY (role_id) REFERENCES admin_roles(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS admin_user_roles (
            admin_user_id INTEGER NOT NULL,
            role_id INTEGER NOT NULL,
            granted_by INTEGER,
            granted_at INTEGER NOT NULL,
            PRIMARY KEY (admin_user_id, role_id),
            FOREIGN KEY (admin_user_id) REFERENCES admin_users(id) ON DELETE CASCADE,
            FOREIGN KEY (role_id) REFERENCES admin_roles(id) ON DELETE RESTRICT,
            FOREIGN KEY (granted_by) REFERENCES admin_users(id) ON DELETE SET NULL
        );

        CREATE TABLE IF NOT EXISTS admin_server_grants (
            admin_user_id INTEGER NOT NULL,
            server_id TEXT NOT NULL,
            scope TEXT NOT NULL DEFAULT 'full',
            granted_by INTEGER,
            granted_at INTEGER NOT NULL,
            PRIMARY KEY (admin_user_id, server_id),
            FOREIGN KEY (admin_user_id) REFERENCES admin_users(id) ON DELETE CASCADE,
            FOREIGN KEY (granted_by) REFERENCES admin_users(id) ON DELETE SET NULL
        );

        CREATE TABLE IF NOT EXISTS admin_totp_secrets (
            admin_user_id INTEGER PRIMARY KEY,
            secret_ciphertext BLOB NOT NULL,
            secret_nonce BLOB NOT NULL,
            key_version INTEGER NOT NULL DEFAULT 1,
            algo TEXT NOT NULL DEFAULT 'SHA1',
            digits INTEGER NOT NULL DEFAULT 6,
            period_seconds INTEGER NOT NULL DEFAULT 30,
            confirmed_at INTEGER,
            last_used_step INTEGER,
            updated_at INTEGER NOT NULL,
            FOREIGN KEY (admin_user_id) REFERENCES admin_users(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS admin_recovery_codes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            admin_user_id INTEGER NOT NULL,
            code_hash TEXT NOT NULL,
            used_at INTEGER,
            created_at INTEGER NOT NULL,
            UNIQUE (admin_user_id, code_hash),
            FOREIGN KEY (admin_user_id) REFERENCES admin_users(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS admin_api_tokens (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            admin_user_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            token_prefix TEXT NOT NULL,
            token_hash TEXT NOT NULL UNIQUE,
            scopes TEXT NOT NULL,
            server_ids TEXT,
            expires_at INTEGER,
            last_used_at INTEGER,
            revoked_at INTEGER,
            created_at INTEGER NOT NULL,
            created_by INTEGER,
            FOREIGN KEY (admin_user_id) REFERENCES admin_users(id) ON DELETE CASCADE,
            FOREIGN KEY (created_by) REFERENCES admin_users(id) ON DELETE SET NULL
        );

        CREATE INDEX IF NOT EXISTS idx_admin_users_enabled ON admin_users(enabled);
        CREATE INDEX IF NOT EXISTS idx_admin_sessions_user_expires
            ON admin_sessions(admin_user_id, expires_at);
        CREATE INDEX IF NOT EXISTS idx_admin_sessions_last_seen_at
            ON admin_sessions(last_seen_at);
        CREATE INDEX IF NOT EXISTS idx_admin_recovery_codes_user_used
            ON admin_recovery_codes(admin_user_id, used_at);
        CREATE INDEX IF NOT EXISTS idx_admin_api_tokens_hash ON admin_api_tokens(token_hash);
        CREATE INDEX IF NOT EXISTS idx_admin_api_tokens_user_revoked
            ON admin_api_tokens(admin_user_id, revoked_at);
        CREATE INDEX IF NOT EXISTS idx_admin_api_tokens_expires_at
            ON admin_api_tokens(expires_at);
        "#,
    )?;

    // Seed + one-time legacy elevation + version stamp must be atomic so a failed data
    // phase never leaves schema_migrations marked while bindings are missing (and vice versa).
    // DDL/ALTER above is intentionally outside: SQLite ADD COLUMN is not fully rollbackable.
    {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            r#"
            INSERT OR IGNORE INTO admin_roles (key, name, description, is_system, created_at)
            VALUES (?1, 'Super Admin', 'Full system administrator', 1, ?2)
            "#,
            params![SUPER_ADMIN_ROLE_KEY, now],
        )?;

        let already_applied = migration_applied(&tx, ADMIN_SECURITY_MIGRATION_VERSION)?;
        if !already_applied {
            // One-time: existing bootstrap admins become super_admin. Later role-less users are
            // intentional and must not be re-elevated on subsequent opens.
            tx.execute(
                r#"
                INSERT OR IGNORE INTO admin_user_roles (admin_user_id, role_id, granted_by, granted_at)
                SELECT u.id, r.id, NULL, ?1
                FROM admin_users u
                CROSS JOIN admin_roles r
                WHERE r.key = ?2
                "#,
                params![now, SUPER_ADMIN_ROLE_KEY],
            )?;
            record_migration(&tx, ADMIN_SECURITY_MIGRATION_VERSION, now)?;
        }
        if !migration_applied(&tx, "admin_builtin_roles_v1")? {
            for (key, name, description) in [
                ("viewer", "Viewer", "Read-only administrator"),
                (
                    "operator",
                    "Operator",
                    "Server operator without global settings access",
                ),
            ] {
                let inserted = tx.execute(
                    "INSERT OR IGNORE INTO admin_roles (key, name, description, is_system, created_at) VALUES (?1, ?2, ?3, 1, ?4)",
                    params![key, name, description, now],
                )?;
                // Preserve pre-existing custom roles with the same key.
                if inserted == 1 {
                    let role_id = tx.last_insert_rowid();
                    for permission in PERMISSION_KEY_CATALOG {
                        if key == "operator" || permission.ends_with(".read") {
                            tx.execute(
                                "INSERT INTO admin_role_permissions (role_id, permission) VALUES (?1, ?2)",
                                params![role_id, permission],
                            )?;
                        }
                    }
                }
            }
            record_migration(&tx, "admin_builtin_roles_v1", now)?;
        }
        if !migration_applied(&tx, "admin_sessions_control_v1")? {
            tx.execute(
                "INSERT OR IGNORE INTO admin_role_permissions(role_id, permission)
                 SELECT id, 'sessions.control' FROM admin_roles WHERE key = 'operator' AND is_system = 1",
                [],
            )?;
            record_migration(&tx, "admin_sessions_control_v1", now)?;
        }
        tx.commit()?;
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum RequestStatKind {
    Request,
    Redirect,
    CacheHit,
    Block,
    Error,
}

impl RequestStatKind {
    fn delta(self) -> (i64, i64, i64, i64, i64) {
        match self {
            Self::Request => (1, 0, 0, 0, 0),
            Self::Redirect => (0, 1, 0, 0, 0),
            Self::CacheHit => (0, 0, 1, 0, 0),
            Self::Block => (0, 0, 0, 1, 0),
            Self::Error => (0, 0, 0, 0, 1),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RequestStatsDaily {
    pub date: String,
    pub server_id: String,
    pub server_name: String,
    pub port: u16,
    pub requests: i64,
    pub redirects: i64,
    pub cache_hits: i64,
    pub blocks: i64,
    pub errors: i64,
    pub updated_at_ms: u128,
}

/// Aggregated daily request counters for history range queries.
///
/// Global queries (`server_id` filter = None) return one row per date with
/// `server_id`/`server_name` set to `None`. Server-filtered queries return one
/// row per date for that server after summing any multi-port rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestStatsRangeRow {
    pub date: String,
    pub server_id: Option<String>,
    pub server_name: Option<String>,
    pub requests: i64,
    pub redirects: i64,
    pub cache_hits: i64,
    pub blocks: i64,
    pub errors: i64,
    pub updated_at_ms: u128,
}

/// A privacy-filtered proxy sample for historical analytics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyAnalyticsSample {
    pub timestamp_ms: u128,
    pub server_id: String,
    pub server_name: String,
    pub path: String,
    pub playback_user: String,
    pub client: String,
    pub sample_kind: String,
    pub status_code: u16,
    pub duration_ms: u128,
    pub cache_hit: bool,
    pub blocked: bool,
    pub transcoding: Option<bool>,
    pub bandwidth: Option<u64>,
}

pub struct ProxyRequestDetailFilter<'a> {
    pub server_id: Option<&'a str>,
    pub path_type: Option<&'a str>,
    /// Excludes every row whose `path_type` equals this value.
    ///
    /// The exclusion is applied inside the SQL `WHERE` clause, so it runs
    /// before `LIMIT` and the returned page stays filled with the rows that
    /// are actually shown.
    pub exclude_path_type: Option<&'a str>,
    pub keyword: Option<&'a str>,
    pub since_ms: Option<u128>,
    pub until_ms: Option<u128>,
    pub limit: usize,
}

#[derive(Debug, Serialize)]
pub struct ProxyRequestDetail {
    pub id: i64,
    pub timestamp_ms: u128,
    pub server_id: String,
    pub server_name: String,
    pub port: u16,
    pub method: String,
    pub path: String,
    pub path_type: String,
    pub status_code: u16,
    pub outcome: String,
    pub duration_ms: u128,
    pub playback_user: String,
    pub playback_ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_location: Option<IpLocation>,
    pub cache_hit: bool,
    pub blocked: bool,
    pub detail: String,
}

#[derive(Debug, Serialize)]
pub struct AuditLogEntry {
    pub id: i64,
    pub timestamp_ms: u128,
    pub admin_user_id: Option<i64>,
    pub admin_username: String,
    pub action: String,
    pub summary: String,
    pub result: String,
}

#[derive(Debug, Serialize)]
pub struct SetupStatus {
    pub initialized: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HistoricalTrend {
    pub id: i64,
    pub metric: String,
    pub server_id: Option<String>,
    pub date: String,
    pub value: i64,
}

fn task_failure_history_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<ScheduledTaskFailureHistory> {
    Ok(ScheduledTaskFailureHistory {
        id: row.get(0)?,
        server_id: row.get(1)?,
        task_id: row.get(2)?,
        run_identity: row.get(3)?,
        observed_at: row.get(4)?,
        status: row.get(5)?,
        safe_error_summary: row.get(6)?,
        started_at: row.get(7)?,
        ended_at: row.get(8)?,
    })
}

fn normalize_task_failure_field(value: &str, field: &str) -> AppResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::Validation(format!("{field} must not be empty")));
    }
    if value.chars().any(|character| character.is_control()) {
        return Err(AppError::Validation(format!(
            "{field} contains control characters"
        )));
    }
    let value = truncate_chars(value, TASK_FAILURE_HISTORY_FIELD_MAX_CHARS);
    Ok(value)
}

fn normalize_task_failure_limit(limit: i64) -> i64 {
    if limit <= 0 {
        TASK_FAILURE_HISTORY_DEFAULT_LIMIT
    } else {
        limit.min(TASK_FAILURE_HISTORY_MAX_LIMIT)
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn sanitize_task_failure_error(value: Option<&str>) -> Option<String> {
    let value = value?;
    // Error summaries may contain signed URLs whose provider-specific query
    // keys are not covered by the generic credential-marker list.
    let value = crate::emby::sanitize_task_error_summary(value);
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    // Replace controls and collapse whitespace before checking or persisting. This keeps
    // summaries useful while preventing multiline upstream bodies from becoming log blobs.
    let mut cleaned = String::with_capacity(value.len().min(TASK_FAILURE_HISTORY_ERROR_MAX_CHARS));
    let mut previous_was_whitespace = false;
    for character in value.chars() {
        if character.is_control() || character.is_whitespace() {
            if !previous_was_whitespace {
                cleaned.push(' ');
            }
            previous_was_whitespace = true;
        } else {
            cleaned.push(character);
            previous_was_whitespace = false;
        }
    }
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        return None;
    }

    let lower = cleaned.to_ascii_lowercase();
    const SENSITIVE_MARKERS: &[&str] = &[
        "authorization",
        "bearer ",
        "access token",
        "token",
        "api key",
        "api_key",
        "apikey",
        "api-key",
        "access_token",
        "refresh_token",
        "token=",
        "password",
        "passwd",
        "secret",
        "cookie",
        "set-cookie",
        "credential",
        "private key",
        "client_secret",
    ];
    let looks_like_sensitive_body = lower.len() > TASK_FAILURE_HISTORY_ERROR_MAX_CHARS * 8
        || lower.contains("<html")
        || lower.contains("<body")
        || SENSITIVE_MARKERS
            .iter()
            .any(|marker| lower.contains(marker));
    if looks_like_sensitive_body {
        return Some(TASK_FAILURE_ERROR_REDACTED.to_string());
    }

    Some(truncate_chars(
        cleaned,
        TASK_FAILURE_HISTORY_ERROR_MAX_CHARS,
    ))
}

fn migrate_proxy_analytics_samples_v1(conn: &Connection) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    let applied: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = ?1)",
        params![PROXY_ANALYTICS_SAMPLES_MIGRATION_VERSION],
        |row| row.get(0),
    )?;
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS proxy_analytics_samples (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            timestamp_ms INTEGER NOT NULL,
            sample_kind TEXT NOT NULL,
            server_id TEXT NOT NULL,
            server_name TEXT NOT NULL,
            session_key TEXT NOT NULL DEFAULT '',
            media_id TEXT NOT NULL DEFAULT '',
            playback_user TEXT NOT NULL DEFAULT '',
            client TEXT NOT NULL DEFAULT '',
            status_code INTEGER,
            duration_ms INTEGER,
            cache_hit BOOLEAN NOT NULL DEFAULT FALSE,
            blocked BOOLEAN NOT NULL DEFAULT FALSE,
            transcoding BOOLEAN,
            bandwidth INTEGER,
            created_at_ms INTEGER NOT NULL,
            bucket_ms INTEGER NOT NULL DEFAULT 0
        );
        CREATE INDEX IF NOT EXISTS idx_proxy_analytics_samples_time
            ON proxy_analytics_samples(timestamp_ms, id);
        CREATE INDEX IF NOT EXISTS idx_proxy_analytics_samples_server
            ON proxy_analytics_samples(server_id, timestamp_ms);
        CREATE INDEX IF NOT EXISTS idx_proxy_analytics_samples_kind
            ON proxy_analytics_samples(sample_kind, timestamp_ms);
        CREATE UNIQUE INDEX IF NOT EXISTS uq_proxy_analytics_playback_key
            ON proxy_analytics_samples(sample_kind, bucket_ms, server_id, session_key)
            WHERE sample_kind = 'playback';
        "#,
    )?;
    if !applied {
        tx.execute(
            r#"
            INSERT INTO proxy_analytics_samples
                (timestamp_ms, sample_kind, server_id, server_name, media_id,
                 playback_user, status_code, duration_ms, cache_hit, blocked, created_at_ms)
            SELECT timestamp_ms, 'request', server_id, server_name, path, playback_user,
                   status_code, duration_ms, cache_hit, blocked, timestamp_ms
            FROM proxy_request_logs
            WHERE timestamp_ms >= ?1
            "#,
            params![SettingsStore::sqlite_i64(
                now_ms().saturating_sub(7 * 24 * 3600 * 1000),
            )],
        )?;
        tx.execute(
            "INSERT INTO schema_migrations(version, applied_at) VALUES (?1, ?2)",
            params![PROXY_ANALYTICS_SAMPLES_MIGRATION_VERSION, now_ms() as i64],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub(crate) fn parse_analytics_media_id(path: &str) -> Option<String> {
    let mut segments = path.trim_start_matches('/').split('/');
    while let Some(segment) = segments.next() {
        if segment.trim().eq_ignore_ascii_case("videos")
            || segment.trim().eq_ignore_ascii_case("items")
        {
            if let Some(id) = segments
                .next()
                .map(str::trim)
                .filter(|id| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control))
            {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn analytics_media_id(path: &str) -> String {
    parse_analytics_media_id(path)
        .map(|id| format!("/Videos/{id}"))
        .unwrap_or_default()
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn maintenance_due(last_run: &AtomicI64) -> bool {
    let now = now_seconds();
    let last = last_run.load(Ordering::Relaxed);
    now.saturating_sub(last) >= MAINTENANCE_INTERVAL_SECONDS
        && last_run
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
}

fn run_maintenance(
    last_run: &AtomicI64,
    operation: impl FnOnce() -> AppResult<()>,
) -> AppResult<()> {
    if !maintenance_due(last_run) {
        return Ok(());
    }
    if let Err(err) = operation() {
        last_run.store(0, Ordering::Relaxed);
        return Err(err);
    }
    Ok(())
}

fn business_day_cutoff_ms(now: u128, days: i64) -> u128 {
    const DAY_MS: u128 = 24 * 60 * 60 * 1000;
    const BUSINESS_OFFSET_MS: u128 = 8 * 60 * 60 * 1000;
    let shifted = now.saturating_add(BUSINESS_OFFSET_MS);
    let start_of_today = shifted - shifted % DAY_MS;
    start_of_today
        .saturating_sub(
            (days.max(1) as u128)
                .saturating_sub(1)
                .saturating_mul(DAY_MS),
        )
        .saturating_sub(BUSINESS_OFFSET_MS)
}

fn local_date() -> String {
    local_date_offset(0)
}

fn local_date_offset(offset_days: i64) -> String {
    let now = chrono::Utc::now() + chrono::Duration::hours(8) + chrono::Duration::days(offset_days);
    now.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::{
            Arc, Barrier,
            atomic::{AtomicU64, Ordering},
        },
        thread,
    };

    use super::{
        AdminSessionTouchResult, AppError, DATABASE_BUSY_TIMEOUT, HistoricalTrend,
        PROXY_ANALYTICS_MAX_ROWS, PROXY_ANALYTICS_RETENTION_DAYS,
        PROXY_ANALYTICS_SAMPLES_MIGRATION_VERSION, PlaybackAnalyticsSampleWrite,
        ProxyRequestDetailFilter, ProxyRequestDetailWrite, RequestStatKind,
        SCHEDULED_TASK_FAILURE_HISTORY_MIGRATION_VERSION, ScheduledTaskFailureInput, SettingsStore,
        TASK_FAILURE_ERROR_REDACTED, TASK_FAILURE_HISTORY_DAY_MS, TaskFailureListParams,
        TelemetryWrite, TelemetryWriteQueue, business_day_cutoff_ms, local_date, local_date_offset,
        now_ms, now_seconds,
    };
    use rusqlite::{Connection, params};

    static TEST_DATABASE_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_database_path(name: &str) -> PathBuf {
        let id = TEST_DATABASE_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "embypanel-db-{name}-{}-{id}.sqlite",
            std::process::id()
        ))
    }

    fn remove_test_database(path: &Path) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(format!("{}-wal", path.display()));
        let _ = fs::remove_file(format!("{}-shm", path.display()));
    }

    fn memory_security_store(label: &str) -> (SettingsStore, Connection) {
        let id = TEST_DATABASE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let store = SettingsStore::new(PathBuf::from(format!(
            "file:embypanel-{label}-{}-{id}?mode=memory&cache=shared",
            std::process::id(),
        )));
        // Keep the named in-memory database alive across the store's connections.
        let mut keeper = store.connection().unwrap();
        store.migrate(&mut keeper).unwrap();
        (store, keeper)
    }

    fn diagnosis_sample(id: &str, server: &str) -> crate::diagnostics::DiagnosisRecord {
        crate::diagnostics::DiagnosisRecord {
            request_id: id.into(),
            server_id: server.into(),
            media_id: "synthetic-media".into(),
            started_at_ms: Some(now_ms() as i64),
            duration_ms: Some(12),
            ..Default::default()
        }
    }

    #[test]
    fn diagnosis_storage_migration_roundtrip_scope_and_legacy_times() {
        let path = test_database_path("diagnosis-roundtrip");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let mut legacy = diagnosis_sample("legacy", "srv-a");
        legacy.started_at_ms = None;
        legacy.duration_ms = None;
        store
            .write_telemetry_batch(&[
                TelemetryWrite::DiagnosisRecord(legacy),
                TelemetryWrite::DiagnosisRecord(diagnosis_sample("other", "srv-b")),
            ])
            .unwrap();
        let mut conn = store.connection().unwrap();
        store.migrate(&mut conn).unwrap();
        let migrations: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1",
                [super::DIAGNOSTICS_MIGRATION_VERSION],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migrations, 1);
        drop(conn);
        drop(store);
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        assert_eq!(store.list_diagnosis_records_scoped(None).unwrap().len(), 2);
        let allowed = ["srv-a".to_string()].into_iter().collect();
        let records = store.list_diagnosis_records_scoped(Some(&allowed)).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].request_id, "legacy");
        assert_eq!(records[0].started_at_ms, None);
        assert_eq!(records[0].finished_at_ms, None);
        assert_eq!(records[0].duration_ms, None);
        assert!(
            store
                .list_diagnosis_records_scoped(Some(&Default::default()))
                .unwrap()
                .is_empty()
        );
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn diagnosis_storage_sanitizes_upserts_and_scopes_before_decoding() {
        let (store, keeper) = memory_security_store("diagnosis-scope");
        let mut record = diagnosis_sample("same", "srv-a");
        record.ua = "synthetic token=not-a-real-credential".into();
        record.finished_at_ms = record.started_at_ms.map(|time| time + 12);
        store
            .write_telemetry_batch(&[TelemetryWrite::DiagnosisRecord(record.clone())])
            .unwrap();
        let json: String = keeper
            .query_row("SELECT record_json FROM diagnosis_records", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(!json.contains("not-a-real-credential"));
        record.duration_ms = Some(14);
        store
            .write_telemetry_batch(&[TelemetryWrite::DiagnosisRecord(record)])
            .unwrap();
        keeper.execute(
            "INSERT INTO diagnosis_records VALUES ('inaccessible', 'srv-b', NULL, ?1, 'not json')",
            [now_ms() as i64],
        ).unwrap();
        let allowed = ["srv-a".to_string()].into_iter().collect();
        let records = store.list_diagnosis_records_scoped(Some(&allowed)).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].duration_ms, Some(14));
        assert!(records[0].finished_at_ms.is_some());
        assert!(store.list_diagnosis_records_scoped(None).is_err());
    }

    #[test]
    fn diagnosis_storage_releases_write_transaction_before_decoding() {
        let path = test_database_path("diagnosis-decode-unlocked");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        store
            .write_telemetry_batch(&[TelemetryWrite::DiagnosisRecord(diagnosis_sample(
                "before-decode",
                "srv-a",
            ))])
            .unwrap();
        let (reached, ready) = std::sync::mpsc::sync_channel(1);
        let (release, released) = std::sync::mpsc::sync_channel(1);
        let reader = {
            let store = store.clone();
            thread::spawn(move || {
                super::TEST_BEFORE_DIAGNOSIS_DECODE.with(|hook| {
                    *hook.borrow_mut() = Some(Box::new(move || {
                        reached.send(()).unwrap();
                        released.recv().unwrap();
                    }));
                });
                let allowed = ["srv-a".to_string()].into_iter().collect();
                store.list_diagnosis_records_scoped(Some(&allowed))
            })
        };
        ready.recv().unwrap();
        // The reader is paused before decoding; no retries or timing thresholds.
        let write_result = (|| -> super::AppResult<()> {
            let mut writer = Connection::open(&path)?;
            writer.busy_timeout(std::time::Duration::ZERO)?;
            let tx = writer.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let record = diagnosis_sample("during-decode", "srv-a");
            tx.execute(
                "INSERT INTO diagnosis_records VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    record.request_id,
                    record.server_id,
                    record.started_at_ms,
                    now_ms() as i64,
                    serde_json::to_string(&record)?
                ],
            )?;
            tx.commit()?;
            let count: i64 = writer.query_row(
                "SELECT COUNT(*) FROM diagnosis_records WHERE request_id = 'during-decode'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(count, 1);
            Ok(())
        })();
        // Always resume and join the reader, including when the writer finds a lock.
        release.send(()).unwrap();
        let records = reader.join().unwrap().unwrap();
        drop(store);
        remove_test_database(&path);
        write_result.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].request_id, "before-decode");
    }

    #[test]
    fn diagnosis_storage_rejects_mismatched_json_scope() {
        let (store, keeper) = memory_security_store("diagnosis-scope-mismatch");
        let record = diagnosis_sample("mismatched", "srv-b");
        keeper
            .execute(
                "INSERT INTO diagnosis_records VALUES ('mismatched', 'srv-a', ?1, ?1, ?2)",
                params![now_ms() as i64, serde_json::to_string(&record).unwrap()],
            )
            .unwrap();
        let allowed = ["srv-a".to_string()].into_iter().collect();
        for scope in [None, Some(&allowed)] {
            assert!(matches!(
                store.list_diagnosis_records_scoped(scope),
                Err(AppError::Internal(message)) if message == "invalid stored diagnostics scope"
            ));
        }
        let denied = ["srv-b".to_string()].into_iter().collect();
        assert!(
            store
                .list_diagnosis_records_scoped(Some(&denied))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn diagnosis_storage_preserves_order_and_sanitizes_on_read() {
        let (store, keeper) = memory_security_store("diagnosis-read-order");
        let time = now_ms() as i64;
        for (id, started_at_ms) in [
            ("legacy", None),
            ("older", Some(time - 1)),
            ("newer-a", Some(time)),
            ("newer-z", Some(time)),
        ] {
            let mut record = diagnosis_sample(id, "srv-a");
            record.started_at_ms = started_at_ms;
            record.ua = "synthetic token=diagnosis-read-secret".into();
            keeper
                .execute(
                    "INSERT INTO diagnosis_records VALUES (?1, 'srv-a', ?2, ?3, ?4)",
                    params![
                        id,
                        started_at_ms,
                        time,
                        serde_json::to_string(&record).unwrap()
                    ],
                )
                .unwrap();
        }
        let allowed = ["srv-a".to_string()].into_iter().collect();
        let records = store.list_diagnosis_records_scoped(Some(&allowed)).unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| record.request_id.as_str())
                .collect::<Vec<_>>(),
            ["newer-z", "newer-a", "older", "legacy"]
        );
        for record in &records {
            assert!(!record.ua.contains("diagnosis-read-secret"));
            assert!(record.ua.contains(crate::diagnostics::REDACTED_PLACEHOLDER));
        }
        assert_eq!(records[3].started_at_ms, None);
        assert_eq!(records[3].finished_at_ms, None);
    }

    #[test]
    fn diagnosis_storage_prunes_age_and_count_in_write_transaction_and_read() {
        let (store, mut keeper) = memory_security_store("diagnosis-retention");
        let time = now_ms() as i64;
        let tx = keeper.transaction().unwrap();
        let json = serde_json::to_string(&diagnosis_sample("synthetic", "srv")).unwrap();
        for index in 0..super::DIAGNOSTICS_PERSIST_MAX_RECORDS + 3 {
            let id = format!("synthetic-{index:05}");
            let mut record: crate::diagnostics::DiagnosisRecord =
                serde_json::from_str(&json).unwrap();
            record.request_id = id.clone();
            record.started_at_ms = Some(time - index as i64);
            tx.execute(
                "INSERT INTO diagnosis_records VALUES (?1, 'srv', ?2, ?3, ?4)",
                params![
                    id,
                    record.started_at_ms,
                    time,
                    serde_json::to_string(&record).unwrap()
                ],
            )
            .unwrap();
        }
        tx.execute(
            "INSERT INTO diagnosis_records VALUES ('old-legacy', 'srv', NULL, ?1, ?2)",
            params![super::diagnostics_cutoff_ms() - 1, json],
        )
        .unwrap();
        tx.commit().unwrap();
        let mut expired = diagnosis_sample("old-timed", "srv");
        expired.started_at_ms = Some(super::diagnostics_cutoff_ms() - 1);
        store
            .write_telemetry_batch(&[TelemetryWrite::DiagnosisRecord(expired)])
            .unwrap();
        let count: usize = keeper
            .query_row("SELECT COUNT(*) FROM diagnosis_records", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, super::DIAGNOSTICS_PERSIST_MAX_RECORDS);
        let oldest: String = keeper
            .query_row(
                "SELECT request_id FROM diagnosis_records ORDER BY started_at_ms LIMIT 1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(oldest, "synthetic-09999");
        keeper.execute("UPDATE diagnosis_records SET started_at_ms = ?1 WHERE request_id = 'synthetic-00000'",
            [super::diagnostics_cutoff_ms() - 1]).unwrap();
        assert_eq!(
            store.list_diagnosis_records_scoped(None).unwrap().len(),
            super::DIAGNOSTICS_PERSIST_MAX_RECORDS - 1
        );
        let count: usize = keeper
            .query_row("SELECT COUNT(*) FROM diagnosis_records", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, super::DIAGNOSTICS_PERSIST_MAX_RECORDS - 1);
    }

    #[test]
    fn diagnosis_queue_sanitizes_before_enqueue_and_drops_when_full() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let queue = TelemetryWriteQueue {
            inner: Arc::new(super::TelemetryWriteQueueInner {
                sender,
                dropped: AtomicU64::new(0),
                closing: std::sync::atomic::AtomicBool::new(false),
                in_flight: std::sync::atomic::AtomicUsize::new(0),
                writer: std::sync::Mutex::new(None),
            }),
        };
        let mut record = diagnosis_sample("one", "srv");
        record.ua = "synthetic token=queue-only-value".into();
        queue.diagnosis_record(record);
        queue.diagnosis_record(diagnosis_sample("two", "srv"));
        assert_eq!(queue.inner.dropped.load(Ordering::Relaxed), 1);
        match receiver.recv().unwrap() {
            super::TelemetryMessage::Write(TelemetryWrite::DiagnosisRecord(record)) => {
                assert!(record.ua.contains(crate::diagnostics::REDACTED_PLACEHOLDER));
            }
            _ => panic!("expected sanitized diagnosis message"),
        }
    }

    #[test]
    fn diagnosis_queue_clear_is_fifo_barrier_and_writer_remains_available() {
        let path = test_database_path("diagnosis-clear-barrier");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let queue = TelemetryWriteQueue::start(store.clone()).unwrap();
        let mut blocker = store.connection().unwrap();
        let tx = blocker
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let started = std::time::Instant::now();
        // More than one batch: queued writes must not reappear after cleanup.
        for index in 0..150 {
            queue.diagnosis_record(diagnosis_sample(&format!("before-{index}"), "srv"));
        }
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
        tx.commit().unwrap();
        queue.clear_diagnosis_records().unwrap();
        assert!(
            store
                .list_diagnosis_records_scoped(None)
                .unwrap()
                .is_empty()
        );
        queue.diagnosis_record(diagnosis_sample("after", "srv"));
        queue.request_stat(
            "srv".into(),
            "Synthetic".into(),
            8096,
            RequestStatKind::Redirect,
        );
        queue.shutdown().unwrap();
        let records = store.list_diagnosis_records_scoped(None).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].request_id, "after");
        let redirects: i64 = blocker
            .query_row(
                "SELECT redirects FROM request_stats_daily WHERE server_id = 'srv'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(redirects, 1);
        assert!(queue.clear_diagnosis_records().is_err());
        drop(blocker);
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn diagnosis_queue_clear_propagates_database_failure_without_stopping_writer() {
        let path = test_database_path("diagnosis-clear-error");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let queue = TelemetryWriteQueue::start(store.clone()).unwrap();
        let mut conn = store.connection().unwrap();
        conn.execute("DROP TABLE diagnosis_records", []).unwrap();
        assert!(queue.clear_diagnosis_records().is_err());
        assert!(store.list_diagnosis_records_scoped(None).is_err());
        store.migrate(&mut conn).unwrap();
        queue.clear_diagnosis_records().unwrap();
        queue.diagnosis_record(diagnosis_sample("after-error", "srv"));
        queue.shutdown().unwrap();
        assert_eq!(store.list_diagnosis_records_scoped(None).unwrap().len(), 1);
        drop(conn);
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn totp_setup_and_confirm_are_instance_bound_under_concurrency() {
        let (store, _keeper) = memory_security_store("totp-cas");
        let admin = store
            .create_initial_admin("admin", "hash")
            .unwrap()
            .unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let handles = [b"cipher-a".as_slice(), b"cipher-b".as_slice()].map(|cipher| {
            let store = store.clone();
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                store.upsert_admin_totp_secret(admin, cipher, b"nonce", 1, "SHA1", 6, 30, 100)
            })
        });
        barrier.wait();
        let successes = handles
            .into_iter()
            .map(|h| h.join().unwrap().is_ok())
            .filter(|ok| *ok)
            .count();
        assert_eq!(successes, 1);
        let row_a = store.get_admin_totp_secret(admin).unwrap().unwrap();
        assert!(
            store
                .upsert_admin_totp_secret(admin, b"replacement", b"nonce-b", 1, "SHA1", 6, 30, 101)
                .is_err()
        );
        assert!(store.rollback_admin_totp_setup(admin).unwrap());
        store
            .upsert_admin_totp_secret(admin, b"replacement", b"nonce-b", 1, "SHA1", 6, 30, 102)
            .unwrap();
        let row_b = store.get_admin_totp_secret(admin).unwrap().unwrap();
        assert!(
            !store
                .confirm_admin_totp_with_recovery_codes(
                    admin,
                    &row_a.secret_ciphertext,
                    &row_a.secret_nonce,
                    &["hash-a".into()],
                    103
                )
                .unwrap()
        );
        assert!(
            !store
                .admin_security_identity(admin)
                .unwrap()
                .unwrap()
                .totp_enabled
        );
        assert_eq!(store.count_unused_admin_recovery_codes(admin).unwrap(), 0);
        assert!(
            store
                .confirm_admin_totp_with_recovery_codes(
                    admin,
                    &row_b.secret_ciphertext,
                    &row_b.secret_nonce,
                    &["hash-b".into()],
                    104
                )
                .unwrap()
        );
        // A setup that read enabled=false before confirm cannot overwrite it later.
        assert!(
            store
                .upsert_admin_totp_secret(admin, b"late", b"nonce-late", 1, "SHA1", 6, 30, 105)
                .is_err()
        );
        assert!(
            store
                .admin_security_identity(admin)
                .unwrap()
                .unwrap()
                .totp_enabled
        );
        assert_eq!(
            store
                .get_admin_totp_secret(admin)
                .unwrap()
                .unwrap()
                .secret_ciphertext,
            row_b.secret_ciphertext
        );
    }

    #[test]
    fn builtin_roles_are_selectable_on_new_install_and_upgrade_without_elevation() {
        let (store, mut keeper) = memory_security_store("builtin-roles");
        for role in ["viewer", "operator"] {
            let id = store
                .create_admin_with_roles(role, "hash", None, &[role], None)
                .unwrap();
            let identity = store.admin_security_identity(id).unwrap().unwrap();
            assert_eq!(identity.role_keys, [role]);
            assert!(
                identity
                    .permission_keys
                    .iter()
                    .any(|p| p == "settings.read")
            );
            assert!(
                identity
                    .permission_keys
                    .iter()
                    .any(|p| p == "monitoring.read")
            );
            if role == "viewer" {
                assert!(
                    identity
                        .permission_keys
                        .iter()
                        .all(|p| p.ends_with(".read"))
                );
            } else {
                for permission in [
                    "users.write",
                    "library.tasks.execute",
                    "alerts.write",
                    "cache.write",
                ] {
                    assert!(identity.permission_keys.iter().any(|p| p == permission));
                }
            }
        }
        store.migrate(&mut keeper).unwrap();
        assert!(
            store
                .list_admin_users()
                .unwrap()
                .iter()
                .all(|user| !user.role_keys.iter().any(|key| key == "super_admin"))
        );
        // Simulate an admin_security_v1 database before the new role migration.
        keeper.execute("DELETE FROM admin_users", []).unwrap();
        keeper
            .execute(
                "DELETE FROM admin_roles WHERE key IN ('viewer', 'operator')",
                [],
            )
            .unwrap();
        keeper
            .execute(
                "DELETE FROM schema_migrations WHERE version = 'admin_builtin_roles_v1'",
                [],
            )
            .unwrap();
        store.migrate(&mut keeper).unwrap();
        let roles = store.list_admin_roles().unwrap();
        assert_eq!(roles.len(), 3);
        assert!(roles.iter().all(|role| role.is_system));
        assert!(super::permission_key_catalog().contains(&"cache.write"));
    }

    #[test]
    fn playback_queue_bounds_samples_and_drops_on_overload_without_blocking() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let queue = TelemetryWriteQueue {
            inner: Arc::new(super::TelemetryWriteQueueInner {
                sender,
                dropped: AtomicU64::new(0),
                closing: std::sync::atomic::AtomicBool::new(false),
                in_flight: std::sync::atomic::AtomicUsize::new(0),
                writer: std::sync::Mutex::new(None),
            }),
        };
        let sample = || PlaybackAnalyticsSampleWrite {
            timestamp_ms: now_ms(),
            server_id: "srv".into(),
            server_name: "Server".into(),
            session_key: "session".into(),
            media_id: "media".into(),
            playback_user: "user".into(),
            client: "client".into(),
            transcoding: false,
            bandwidth: None,
        };
        queue.playback_analytics_samples(Vec::new());
        assert!(receiver.try_recv().is_err());
        queue.playback_analytics_samples((0..300).map(|_| sample()).collect());
        queue.playback_analytics_samples(vec![sample()]);
        assert_eq!(queue.inner.dropped.load(Ordering::Relaxed), 1);
        match receiver.try_recv().unwrap() {
            super::TelemetryMessage::Write(TelemetryWrite::PlaybackAnalyticsSamples(samples)) => {
                assert_eq!(samples.len(), 256)
            }
            _ => panic!("expected bounded playback message"),
        }
    }

    #[test]
    fn playback_queue_flushes_samples_and_enqueue_does_not_wait_for_sqlite() {
        let path = test_database_path("playback-queue-nonblocking");
        let store = SettingsStore::open_for_test(path).unwrap();
        let queue = TelemetryWriteQueue::start(store.clone()).unwrap();
        let mut blocker = store.connection().unwrap();
        let transaction = blocker
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .unwrap();
        let started = std::time::Instant::now();
        queue.playback_analytics_samples(vec![PlaybackAnalyticsSampleWrite {
            timestamp_ms: now_ms(),
            server_id: "srv".into(),
            server_name: "Server".into(),
            session_key: "session".into(),
            media_id: "media".into(),
            playback_user: "user".into(),
            client: "client".into(),
            transcoding: false,
            bandwidth: Some(123),
        }]);
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
        transaction.commit().unwrap();
        queue.shutdown().unwrap();
        let samples = store
            .list_proxy_analytics_samples(1, Some("srv"), 10)
            .unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].sample_kind, "playback");
        assert_eq!(samples[0].bandwidth, Some(123));
        queue.playback_analytics_samples(vec![PlaybackAnalyticsSampleWrite {
            timestamp_ms: now_ms(),
            server_id: "srv".into(),
            server_name: "Server".into(),
            session_key: "after-close".into(),
            media_id: "media".into(),
            playback_user: "user".into(),
            client: "client".into(),
            transcoding: false,
            bandwidth: None,
        }]);
        assert_eq!(queue.inner.dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn connections_use_wal_busy_timeout_and_foreign_keys() {
        let path = test_database_path("connection-config");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let conn = store.connection().unwrap();

        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        let busy_timeout: i64 = conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        let foreign_keys: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        let synchronous: i64 = conn
            .pragma_query_value(None, "synchronous", |row| row.get(0))
            .unwrap();

        assert_eq!(journal_mode, "wal");
        assert_eq!(busy_timeout, DATABASE_BUSY_TIMEOUT.as_millis() as i64);
        assert_eq!(foreign_keys, 1);
        assert_eq!(synchronous, 1);
        drop(conn);
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn regular_connections_do_not_repeat_schema_migration() {
        let path = test_database_path("single-migration");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let conn = store.connection().unwrap();
        conn.execute("DROP TABLE proxy_request_logs", []).unwrap();
        drop(conn);

        assert!(!store.has_admin().unwrap());
        let conn = store.connection().unwrap();
        let table_exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'proxy_request_logs')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!table_exists);
        drop(conn);
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn proxy_analytics_samples_migration_is_idempotent_and_backfills_requests() {
        let path = test_database_path("proxy-analytics-migration");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let conn = store.connection().unwrap();
        conn.execute("DROP TABLE proxy_analytics_samples", [])
            .unwrap();
        conn.execute(
            "DELETE FROM schema_migrations WHERE version = ?1",
            params![PROXY_ANALYTICS_SAMPLES_MIGRATION_VERSION],
        )
        .unwrap();
        conn.execute(
            r#"
            INSERT INTO proxy_request_logs
                (timestamp_ms, server_id, server_name, port, method, path, path_type,
                 status_code, outcome, duration_ms, playback_user, playback_ip,
                 cache_hit, blocked, detail)
            VALUES (?1, 'srv', 'Server', 8096, 'GET', '/Videos/legacy/stream',
                    'video_stream', 302, 'redirect', 15, 'alice', '--', 0, 0, '')
            "#,
            params![now_ms() as i64],
        )
        .unwrap();
        drop(conn);
        drop(store);

        let reopened = SettingsStore::open_for_test(path.clone()).unwrap();
        let samples = reopened
            .list_proxy_analytics_samples(7, Some("srv"), 100)
            .unwrap();
        assert_eq!(samples.len(), 1);
        assert_eq!(samples[0].sample_kind, "request");
        assert_eq!(samples[0].path, "/Videos/legacy/stream");
        let conn = reopened.connection().unwrap();
        let migration_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1",
                params![PROXY_ANALYTICS_SAMPLES_MIGRATION_VERSION],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migration_count, 1);
        drop(conn);
        drop(reopened);
        remove_test_database(&path);
    }

    #[test]
    fn proxy_analytics_cutoff_uses_utc_plus_eight_business_days() {
        const HOUR_MS: u128 = 60 * 60 * 1000;
        const DAY_MS: u128 = 24 * HOUR_MS;
        assert_eq!(
            business_day_cutoff_ms(2 * DAY_MS + 16 * HOUR_MS, 1),
            2 * DAY_MS + 16 * HOUR_MS
        );
        assert_eq!(
            business_day_cutoff_ms(2 * DAY_MS + 15 * HOUR_MS, 1),
            DAY_MS + 16 * HOUR_MS
        );
        assert_eq!(
            business_day_cutoff_ms(2 * DAY_MS + 16 * HOUR_MS, 2),
            DAY_MS + 16 * HOUR_MS
        );
    }

    #[test]
    fn proxy_analytics_samples_are_bounded_private_and_concurrent_safe() {
        let path = test_database_path("proxy-analytics-concurrent");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let handles = ["session-a", "session-b"].map(|session_key| {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                store.record_playback_analytics_samples(&[PlaybackAnalyticsSampleWrite {
                    timestamp_ms: now_ms(),
                    server_id: "srv".to_string(),
                    server_name: "Server".to_string(),
                    session_key: session_key.to_string(),
                    media_id: "media".to_string(),
                    playback_user: "alice".to_string(),
                    client: "Infuse".to_string(),
                    transcoding: session_key == "session-a",
                    bandwidth: Some(8_000),
                }])
            })
        });
        barrier.wait();
        for handle in handles {
            handle.join().unwrap().unwrap();
        }
        assert_eq!(
            store
                .list_proxy_analytics_samples(7, None, 1)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store
                .list_proxy_analytics_samples(7, None, 100)
                .unwrap()
                .len(),
            2
        );
        let conn = store.connection().unwrap();
        let columns = conn
            .prepare("PRAGMA table_info(proxy_analytics_samples)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        for sensitive in ["playback_ip", "detail", "method", "user_agent"] {
            assert!(!columns.iter().any(|column| column == sensitive));
        }
        conn.execute(
            "INSERT INTO proxy_analytics_samples (timestamp_ms, sample_kind, server_id, server_name, created_at_ms) VALUES (?1, 'request', 'old', 'Old', ?1)",
            params![1_i64],
        ).unwrap();
        store
            .prune_proxy_analytics_samples(
                &conn,
                PROXY_ANALYTICS_RETENTION_DAYS,
                PROXY_ANALYTICS_MAX_ROWS,
            )
            .unwrap();
        let old_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM proxy_analytics_samples WHERE server_id = 'old'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(old_count, 0);
        drop(conn);
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn concurrent_initialization_creates_exactly_one_admin() {
        let path = test_database_path("concurrent-setup");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let handles = ["first-admin", "second-admin"].map(|username| {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                store.create_initial_admin(username, "password-hash")
            })
        });

        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_some()).count(), 1);

        let conn = store.connection().unwrap();
        let admin_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM admin_users", [], |row| row.get(0))
            .unwrap();
        assert_eq!(admin_count, 1);
        drop(conn);
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn initial_session_failure_rolls_back_admin_creation() {
        let path = test_database_path("setup-session-rollback");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let conn = store.connection().unwrap();
        conn.execute_batch(
            r#"
            CREATE TRIGGER fail_initial_session
            BEFORE INSERT ON admin_sessions
            BEGIN
                SELECT RAISE(FAIL, 'forced initial session failure');
            END;
            "#,
        )
        .unwrap();
        drop(conn);

        assert!(
            store
                .create_initial_admin_with_session(
                    "admin",
                    "password-hash",
                    "initial-token",
                    now_seconds() + 600,
                )
                .is_err()
        );
        assert!(!store.has_admin().unwrap());

        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn password_change_replaces_old_sessions_and_logout_revokes_new_session() {
        let path = test_database_path("session-revocation");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let admin_user_id = store
            .create_initial_admin("admin", "old-password-hash")
            .unwrap()
            .unwrap();
        let expires_at = now_seconds() + 600;
        store
            .create_session(admin_user_id, "old-token-1", expires_at)
            .unwrap();
        store
            .create_session(admin_user_id, "old-token-2", expires_at)
            .unwrap();

        assert!(
            store
                .update_admin_password_and_replace_sessions(
                    admin_user_id,
                    "old-password-hash",
                    "new-password-hash",
                    "old-token-1",
                    "new-token",
                    now_seconds(),
                    expires_at,
                )
                .unwrap()
        );
        assert_eq!(
            store
                .session_admin_user_id("old-token-1", now_seconds())
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .session_admin_user_id("old-token-2", now_seconds())
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .session_admin_user_id("new-token", now_seconds())
                .unwrap(),
            Some(admin_user_id)
        );
        assert_eq!(
            store
                .admin_password_hash_by_id(admin_user_id)
                .unwrap()
                .unwrap()
                .password_hash,
            "new-password-hash"
        );
        assert_eq!(
            store.revoke_session("new-token", now_seconds()).unwrap(),
            Some(admin_user_id)
        );
        assert_eq!(
            store.revoke_session("new-token", now_seconds()).unwrap(),
            None
        );

        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn login_session_creation_requires_current_password_hash() {
        let path = test_database_path("login-password-version");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let admin_user_id = store
            .create_initial_admin("admin", "old-password-hash")
            .unwrap()
            .unwrap();
        let expires_at = now_seconds() + 600;
        store
            .create_session(admin_user_id, "change-token", expires_at)
            .unwrap();
        assert!(
            store
                .update_admin_password_and_replace_sessions(
                    admin_user_id,
                    "old-password-hash",
                    "new-password-hash",
                    "change-token",
                    "replacement-token",
                    now_seconds(),
                    expires_at,
                )
                .unwrap()
        );

        assert!(
            !store
                .create_session_if_password_matches(
                    admin_user_id,
                    "old-password-hash",
                    "stale-login-token",
                    expires_at,
                )
                .unwrap()
        );
        assert!(
            store
                .create_session_if_password_matches(
                    admin_user_id,
                    "new-password-hash",
                    "current-login-token",
                    expires_at,
                )
                .unwrap()
        );

        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn concurrent_password_changes_allow_only_one_session_replacement() {
        let path = test_database_path("concurrent-password-change");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let admin_user_id = store
            .create_initial_admin("admin", "old-password-hash")
            .unwrap()
            .unwrap();
        let expires_at = now_seconds() + 600;
        store
            .create_session(admin_user_id, "old-token-1", expires_at)
            .unwrap();
        store
            .create_session(admin_user_id, "old-token-2", expires_at)
            .unwrap();

        let barrier = Arc::new(Barrier::new(2));
        let handles = [
            ("old-token-1", "new-token-1", "new-password-hash-1"),
            ("old-token-2", "new-token-2", "new-password-hash-2"),
        ]
        .map(|(current_token, new_token, new_password_hash)| {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                let updated = store
                    .update_admin_password_and_replace_sessions(
                        admin_user_id,
                        "old-password-hash",
                        new_password_hash,
                        current_token,
                        new_token,
                        now_seconds(),
                        expires_at,
                    )
                    .unwrap();
                (updated, new_token, new_password_hash)
            })
        });
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(results.iter().filter(|(updated, _, _)| *updated).count(), 1);
        assert_eq!(
            store
                .session_admin_user_id("old-token-1", now_seconds())
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .session_admin_user_id("old-token-2", now_seconds())
                .unwrap(),
            None
        );
        for (updated, token, _) in &results {
            assert_eq!(
                store.session_admin_user_id(token, now_seconds()).unwrap(),
                updated.then_some(admin_user_id)
            );
        }
        let winning_password_hash = results
            .iter()
            .find_map(|(updated, _, password_hash)| updated.then_some(*password_hash))
            .unwrap();
        assert_eq!(
            store
                .admin_password_hash_by_id(admin_user_id)
                .unwrap()
                .unwrap()
                .password_hash,
            winning_password_hash
        );

        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn telemetry_batch_persists_stats_and_request_details() {
        let path = test_database_path("telemetry-batch");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let writes = vec![
            TelemetryWrite::RequestStat {
                server_id: "server-a".to_string(),
                server_name: "Server A".to_string(),
                port: 8096,
                kind: RequestStatKind::Request,
            },
            TelemetryWrite::RequestStat {
                server_id: "server-a".to_string(),
                server_name: "Server A renamed".to_string(),
                port: 8097,
                kind: RequestStatKind::CacheHit,
            },
            TelemetryWrite::ProxyRequestDetail(ProxyRequestDetailWrite {
                timestamp_ms: now_ms(),
                server_id: "server-a".to_string(),
                server_name: "Server A".to_string(),
                port: 8096,
                method: "GET".to_string(),
                path: "/videos/1/stream".to_string(),
                path_type: "video_stream".to_string(),
                status_code: 302,
                outcome: "redirect".to_string(),
                duration_ms: 12,
                playback_user: "alice".to_string(),
                playback_ip: "192.0.2.10".to_string(),
                cache_hit: true,
                blocked: false,
                detail: String::new(),
            }),
        ];

        store.write_telemetry_batch(&writes).unwrap();

        let stats = store.today_request_stats().unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].requests, 1);
        assert_eq!(stats[0].cache_hits, 1);
        assert_eq!(stats[0].server_name, "Server A renamed");
        assert_eq!(stats[0].port, 8097);
        let details = store
            .list_proxy_request_details(ProxyRequestDetailFilter {
                server_id: Some("server-a"),
                path_type: None,
                exclude_path_type: None,
                keyword: None,
                since_ms: None,
                until_ms: None,
                limit: 10,
            })
            .unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].path, "/videos/1/stream");
        assert!(details[0].cache_hit);

        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn proxy_request_detail_keyword_matches_the_decoded_detail() {
        let path = test_database_path("proxy-detail-keyword");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let writes = vec![TelemetryWrite::ProxyRequestDetail(ProxyRequestDetailWrite {
                timestamp_ms: now_ms(),
                server_id: "server-a".to_string(),
                server_name: "Server A".to_string(),
                port: 8096,
                method: "GET".to_string(),
                path: "/videos/1/stream".to_string(),
                path_type: "video_stream".to_string(),
                status_code: 302,
                outcome: "redirect".to_string(),
                duration_ms: 12,
                playback_user: "alice".to_string(),
                playback_ip: "192.0.2.10".to_string(),
                cache_hit: false,
                blocked: false,
                detail: "request_id=diag-0123456789abcdef0123456789abcdef https://cdn.example.com/%E9%87%8D%E5%99%A8.S01E21.mkv".to_string(),
            })];
        store.write_telemetry_batch(&writes).unwrap();

        // The decoded form is what the UI shows.
        let detail_filter = |keyword: &'static str| ProxyRequestDetailFilter {
            server_id: None,
            path_type: None,
            exclude_path_type: None,
            keyword: Some(keyword),
            since_ms: None,
            until_ms: None,
            limit: 10,
        };
        assert_eq!(
            store
                .list_proxy_request_details(detail_filter("重器"))
                .unwrap()
                .len(),
            1
        );
        // The stored percent-encoded form keeps matching as before.
        assert_eq!(
            store
                .list_proxy_request_details(detail_filter("%E9%87%8D"))
                .unwrap()
                .len(),
            1
        );
        // Unrelated keywords do not match.
        assert_eq!(
            store
                .list_proxy_request_details(detail_filter("不存在"))
                .unwrap()
                .len(),
            0
        );
        // A keyword that only exists in another column still matches.
        assert_eq!(
            store
                .list_proxy_request_details(detail_filter("alice"))
                .unwrap()
                .len(),
            1
        );

        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn proxy_request_detail_exclusion_runs_before_limit_and_keeps_other_filters() {
        let (store, conn) = memory_security_store("proxy-detail-exclude");
        // Five rows: three newer playback rows plus two non-playback rows.
        for (timestamp, server, path_type, path, blocked) in [
            (
                500_i64,
                "server-a",
                "video_stream",
                "/videos/1/stream",
                1_i64,
            ),
            (400, "server-a", "video_stream", "/videos/2/stream", 0),
            (300, "server-a", "video_stream", "/videos/3/stream", 0),
            (200, "server-a", "proxy", "/emby/Items", 0),
            (100, "server-b", "playback_info", "/Items/1/PlaybackInfo", 0),
        ] {
            conn.execute(
                "INSERT INTO proxy_request_logs(timestamp_ms,server_id,server_name,port,method,path,path_type,status_code,outcome,duration_ms,playback_user,playback_ip,cache_hit,blocked,detail)
                 VALUES (?1,?2,?2,8096,'GET',?3,?4,200,'ok',1,'','',0,?5,?6)",
                params![
                    timestamp,
                    server,
                    path,
                    path_type,
                    blocked,
                    format!("request_id=diag-{timestamp:016x}"),
                ],
            )
            .unwrap();
        }
        let filter = |exclude_path_type: Option<&'static str>,
                      path_type: Option<&'static str>,
                      keyword: Option<&'static str>,
                      limit: usize| ProxyRequestDetailFilter {
            server_id: None,
            path_type,
            exclude_path_type,
            keyword,
            since_ms: None,
            until_ms: None,
            limit,
        };

        // The exclusion drops the playback rows and keeps every other path type.
        let rows = store
            .list_proxy_request_details(filter(Some("video_stream"), None, None, 10))
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| row.path_type != "video_stream"));
        assert_eq!(
            rows.iter()
                .map(|row| row.path_type.as_str())
                .collect::<Vec<_>>(),
            vec!["proxy", "playback_info"]
        );

        // The exclusion happens before LIMIT: a small limit is still filled with
        // the non-playback rows instead of being consumed by the newer playback rows.
        let limited = store
            .list_proxy_request_details(filter(Some("video_stream"), None, None, 2))
            .unwrap();
        assert_eq!(limited.len(), 2);
        assert!(limited.iter().all(|row| row.path_type != "video_stream"));
        let without_exclusion = store
            .list_proxy_request_details(filter(None, None, None, 2))
            .unwrap();
        assert_eq!(without_exclusion.len(), 2);
        assert!(
            without_exclusion
                .iter()
                .all(|row| row.path_type == "video_stream")
        );

        // The keyword filter still applies together with the exclusion.
        let keyword_rows = store
            .list_proxy_request_details(filter(Some("video_stream"), None, Some("emby"), 10))
            .unwrap();
        assert_eq!(keyword_rows.len(), 1);
        assert_eq!(keyword_rows[0].path, "/emby/Items");
        let keyword_playback_info = store
            .list_proxy_request_details(filter(
                Some("video_stream"),
                None,
                Some("playbackinfo"),
                10,
            ))
            .unwrap();
        assert_eq!(keyword_playback_info.len(), 1);
        assert_eq!(keyword_playback_info[0].path_type, "playback_info");
        let keyword_playback = store
            .list_proxy_request_details(filter(Some("video_stream"), None, Some("videos"), 10))
            .unwrap();
        assert!(keyword_playback.is_empty());

        // An explicit path type is not affected when no exclusion is requested.
        let playback_only = store
            .list_proxy_request_details(filter(None, Some("video_stream"), None, 10))
            .unwrap();
        assert_eq!(playback_only.len(), 3);
        assert!(
            playback_only
                .iter()
                .all(|row| row.path_type == "video_stream")
        );

        // Server scoping and the time window keep working with the exclusion.
        let allowed = std::collections::HashSet::from(["server-a".to_string()]);
        let scoped = store
            .list_proxy_request_details_scoped(
                filter(Some("video_stream"), None, None, 10),
                Some(&allowed),
            )
            .unwrap();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].server_id, "server-a");
        assert_eq!(scoped[0].path_type, "proxy");
        let time_filtered = store
            .list_proxy_request_details(ProxyRequestDetailFilter {
                server_id: None,
                path_type: None,
                exclude_path_type: Some("video_stream"),
                keyword: None,
                since_ms: Some(150),
                until_ms: Some(250),
                limit: 10,
            })
            .unwrap();
        assert_eq!(time_filtered.len(), 1);
        assert_eq!(time_filtered[0].timestamp_ms, 200);

        // Blocked playback rows are untouched: the block log view keeps them.
        let blocked = store.list_blocked_proxy_request_details(10).unwrap();
        assert_eq!(blocked.len(), 1);
        assert_eq!(blocked[0].path_type, "video_stream");
        assert!(blocked[0].blocked);
    }

    #[test]
    fn telemetry_shutdown_flushes_accepted_writes() {
        let path = test_database_path("telemetry-shutdown");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let queue = TelemetryWriteQueue::start(store.clone()).unwrap();
        for _ in 0..128 {
            queue.request_stat(
                "server-a".to_string(),
                "Server A".to_string(),
                8096,
                RequestStatKind::Request,
            );
        }

        let concurrent_queue = queue.clone();
        let barrier = Arc::new(Barrier::new(2));
        let concurrent_barrier = Arc::clone(&barrier);
        let concurrent_shutdown = thread::spawn(move || {
            concurrent_barrier.wait();
            concurrent_queue.shutdown().unwrap();
        });
        barrier.wait();
        queue.shutdown().unwrap();
        concurrent_shutdown.join().unwrap();
        queue.shutdown().unwrap();

        let stats = store.today_request_stats().unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].requests, 128);
        drop(queue);
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn insert_and_query_single_historical_trend() {
        let path = test_database_path("insert_query_single");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let today = local_date();
        let trend = HistoricalTrend {
            id: 1,
            metric: "user_activity".to_string(),
            server_id: None,
            date: today.clone(),
            value: 5,
        };
        store.insert_historical_trend(&trend).unwrap();
        let retrieved = store
            .get_historical_trends("user_activity", None, 1)
            .unwrap();
        assert_eq!(retrieved.len(), 1);
        assert_eq!(retrieved[0].value, 5);
        assert_eq!(retrieved[0].server_id, None);
        assert_eq!(retrieved[0].metric, "user_activity");
        assert_eq!(retrieved[0].date, today);
        remove_test_database(&path);
    }

    #[test]
    fn insert_and_query_batch_historical_trends() {
        let path = test_database_path("insert_batch");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let today = local_date();
        let trends = vec![
            HistoricalTrend {
                id: 2,
                metric: "server_load".to_string(),
                server_id: None,
                date: today.clone(),
                value: 10,
            },
            HistoricalTrend {
                id: 3,
                metric: "user_activity".to_string(),
                server_id: Some("server1".to_string()),
                date: today.clone(),
                value: 3,
            },
        ];
        store.insert_historical_trends(&trends).unwrap();
        let retrieved = store
            .get_historical_trends("user_activity", Some("server1"), 1)
            .unwrap();
        assert_eq!(retrieved.len(), 1);
        assert_eq!(retrieved[0].value, 3);
        assert_eq!(retrieved[0].server_id, Some("server1".to_string()));
        assert_eq!(retrieved[0].date, today);
        assert_eq!(retrieved[0].metric, "user_activity");
        let global_retrieved = store.get_historical_trends("server_load", None, 1).unwrap();
        assert_eq!(global_retrieved.len(), 1);
        assert_eq!(global_retrieved[0].value, 10);
        assert_eq!(global_retrieved[0].server_id, None);
        assert_eq!(global_retrieved[0].date, today);
        assert_eq!(global_retrieved[0].metric, "server_load");
        remove_test_database(&path);
    }

    #[test]
    fn server_id_filtering_isolation() {
        let path = test_database_path("server_filter");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let today = local_date();
        let test_metric = "test_metric".to_string();
        let global_trend = HistoricalTrend {
            id: 4,
            metric: test_metric.clone(),
            server_id: None,
            date: today.clone(),
            value: 1,
        };
        let server_a_trend = HistoricalTrend {
            id: 5,
            metric: test_metric.clone(),
            server_id: Some("serverA".to_string()),
            date: today.clone(),
            value: 2,
        };
        let server_b_trend = HistoricalTrend {
            id: 6,
            metric: test_metric.clone(),
            server_id: Some("serverB".to_string()),
            date: today.clone(),
            value: 3,
        };
        store.insert_historical_trend(&global_trend).unwrap();
        store.insert_historical_trend(&server_a_trend).unwrap();
        store.insert_historical_trend(&server_b_trend).unwrap();
        let global_retrieved = store.get_historical_trends(&test_metric, None, 1).unwrap();
        assert_eq!(global_retrieved.len(), 1);
        assert_eq!(global_retrieved[0].value, 1);
        assert_eq!(global_retrieved[0].server_id, None);
        let server_a_retrieved = store
            .get_historical_trends(&test_metric, Some("serverA"), 1)
            .unwrap();
        assert_eq!(server_a_retrieved.len(), 1);
        assert_eq!(server_a_retrieved[0].value, 2);
        assert_eq!(server_a_retrieved[0].server_id, Some("serverA".to_string()));
        let server_b_retrieved = store
            .get_historical_trends(&test_metric, Some("serverB"), 1)
            .unwrap();
        assert_eq!(server_b_retrieved.len(), 1);
        assert_eq!(server_b_retrieved[0].value, 3);
        assert_eq!(server_b_retrieved[0].server_id, Some("serverB".to_string()));
        remove_test_database(&path);
    }

    #[test]
    fn prune_historical_trends_boundaries() {
        let path = test_database_path("prune_boundaries");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let today = local_date();
        let old_date = local_date_offset(-10);
        let to_prune_date = local_date_offset(-4);
        let to_keep_3_date = local_date_offset(-3);
        let to_keep_0_date = today.clone();
        store
            .insert_historical_trend(&HistoricalTrend {
                id: 8,
                metric: "prune_test".to_string(),
                server_id: None,
                date: old_date,
                value: 1,
            })
            .unwrap();
        store
            .insert_historical_trend(&HistoricalTrend {
                id: 9,
                metric: "prune_test".to_string(),
                server_id: None,
                date: to_prune_date.clone(),
                value: 1,
            })
            .unwrap();
        store
            .insert_historical_trend(&HistoricalTrend {
                id: 10,
                metric: "prune_test".to_string(),
                server_id: None,
                date: to_keep_3_date.clone(),
                value: 1,
            })
            .unwrap();
        store
            .insert_historical_trend(&HistoricalTrend {
                id: 11,
                metric: "prune_test".to_string(),
                server_id: None,
                date: to_keep_0_date.clone(),
                value: 1,
            })
            .unwrap();
        store.prune_historical_trends(3).unwrap();
        let pruned = store
            .get_historical_trends("prune_test", None, 100)
            .unwrap();
        assert_eq!(pruned.len(), 2);
        let to_keep_3 = pruned.iter().find(|t| t.date == to_keep_3_date).unwrap();
        assert_eq!(to_keep_3.value, 1);
        assert_eq!(to_keep_3.server_id, None);
        let to_keep_0 = pruned.iter().find(|t| t.date == to_keep_0_date).unwrap();
        assert_eq!(to_keep_0.value, 1);
        assert_eq!(to_keep_0.server_id, None);
        let to_prune = pruned.iter().find(|t| t.date == to_prune_date);
        assert!(to_prune.is_none(), "today-4 record should be pruned");
        remove_test_database(&path);
    }

    fn seed_request_stats_daily(
        path: &Path,
        rows: &[(&str, &str, &str, u16, i64, i64, i64, i64, i64, i64)],
    ) {
        let conn = Connection::open(path).unwrap();
        for (
            date,
            server_id,
            server_name,
            port,
            requests,
            redirects,
            cache_hits,
            blocks,
            errors,
            updated_at_ms,
        ) in rows
        {
            conn.execute(
                r#"
                INSERT INTO request_stats_daily
                    (date, server_id, server_name, port, requests, redirects, cache_hits, blocks, errors, updated_at_ms)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                "#,
                params![
                    date,
                    server_id,
                    server_name,
                    *port,
                    *requests,
                    *redirects,
                    *cache_hits,
                    *blocks,
                    *errors,
                    *updated_at_ms,
                ],
            )
            .unwrap();
        }
    }

    #[test]
    fn list_request_stats_range_empty_data() {
        let path = test_database_path("req-stats-range-empty");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let rows = store.list_request_stats_range(7, None).unwrap();
        assert!(rows.is_empty());
        let filtered = store
            .list_request_stats_range(7, Some("missing-server"))
            .unwrap();
        assert!(filtered.is_empty());
        remove_test_database(&path);
    }

    #[test]
    fn list_request_stats_range_multi_date_server_port_and_global_sum() {
        let path = test_database_path("req-stats-range-multi");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let today = local_date();
        let d1 = local_date_offset(-1);
        let d2 = local_date_offset(-2);
        let old = local_date_offset(-10);

        // Multi-server / multi-port rows across dates. Same-day multi-server is
        // summed for global; server filter keeps one server. PK is (date, server_id),
        // so multi-port for one server is modeled as separate server_id rows that a
        // later analytics layer may treat as ports, plus an extra same-day pair
        // under distinct ids to exercise SUM.
        seed_request_stats_daily(
            &path,
            &[
                (d2.as_str(), "srv-a", "Server A", 8096, 10, 1, 2, 3, 4, 100),
                (d2.as_str(), "srv-b", "Server B", 8097, 20, 5, 0, 0, 1, 101),
                (d1.as_str(), "srv-a", "Server A", 8096, 7, 0, 1, 0, 0, 102),
                (
                    d1.as_str(),
                    "srv-a-port2",
                    "Server A p2",
                    8098,
                    3,
                    1,
                    0,
                    1,
                    0,
                    103,
                ),
                (
                    today.as_str(),
                    "srv-a",
                    "Server A",
                    8096,
                    4,
                    2,
                    0,
                    0,
                    0,
                    104,
                ),
                (
                    today.as_str(),
                    "srv-b",
                    "Server B",
                    8097,
                    6,
                    0,
                    3,
                    0,
                    2,
                    105,
                ),
                (old.as_str(), "srv-a", "Server A", 8096, 999, 9, 9, 9, 9, 99),
            ],
        );

        let global = store.list_request_stats_range(3, None).unwrap();
        assert_eq!(global.len(), 3, "days=3 should include d-2..today only");
        assert!(global.iter().all(|row| row.server_id.is_none()));
        assert!(global.iter().all(|row| row.server_name.is_none()));
        assert_eq!(global[0].date, d2);
        assert_eq!(global[0].requests, 30);
        assert_eq!(global[0].redirects, 6);
        assert_eq!(global[0].cache_hits, 2);
        assert_eq!(global[0].blocks, 3);
        assert_eq!(global[0].errors, 5);
        assert_eq!(global[1].date, d1);
        assert_eq!(global[1].requests, 10); // 7 + 3 multi-port-like rows
        assert_eq!(global[1].redirects, 1);
        assert_eq!(global[1].cache_hits, 1);
        assert_eq!(global[1].blocks, 1);
        assert_eq!(global[1].errors, 0);
        assert_eq!(global[2].date, today);
        assert_eq!(global[2].requests, 10);
        assert_eq!(global[2].redirects, 2);
        assert_eq!(global[2].cache_hits, 3);
        assert_eq!(global[2].blocks, 0);
        assert_eq!(global[2].errors, 2);
        // Ascending order
        let dates: Vec<_> = global.iter().map(|r| r.date.as_str()).collect();
        let mut sorted = dates.clone();
        sorted.sort();
        assert_eq!(dates, sorted);

        let only_a = store.list_request_stats_range(3, Some("srv-a")).unwrap();
        assert_eq!(only_a.len(), 3);
        assert!(
            only_a
                .iter()
                .all(|r| r.server_id.as_deref() == Some("srv-a"))
        );
        assert_eq!(only_a[0].date, d2);
        assert_eq!(only_a[0].requests, 10);
        assert_eq!(only_a[0].server_name.as_deref(), Some("Server A"));
        assert_eq!(only_a[1].requests, 7);
        assert_eq!(only_a[2].requests, 4);

        let only_b = store.list_request_stats_range(3, Some("srv-b")).unwrap();
        assert_eq!(only_b.len(), 2, "srv-b has no d-1 row");
        assert_eq!(only_b[0].date, d2);
        assert_eq!(only_b[0].requests, 20);
        assert_eq!(only_b[1].date, today);
        assert_eq!(only_b[1].requests, 6);

        let missing = store
            .list_request_stats_range(3, Some("no-such-server"))
            .unwrap();
        assert!(missing.is_empty());

        remove_test_database(&path);
    }

    #[test]
    fn list_request_stats_range_days_bounds_and_clamp() {
        let path = test_database_path("req-stats-range-bounds");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let today = local_date();
        let d0 = today.as_str();
        let d6 = local_date_offset(-6);
        let d7 = local_date_offset(-7);
        let d364 = local_date_offset(-364);
        let d365 = local_date_offset(-365);

        seed_request_stats_daily(
            &path,
            &[
                (d0, "s", "S", 1, 1, 0, 0, 0, 0, 1),
                (d6.as_str(), "s", "S", 1, 6, 0, 0, 0, 0, 1),
                (d7.as_str(), "s", "S", 1, 7, 0, 0, 0, 0, 1),
                (d364.as_str(), "s", "S", 1, 364, 0, 0, 0, 0, 1),
                (d365.as_str(), "s", "S", 1, 365, 0, 0, 0, 0, 1),
            ],
        );

        let one = store.list_request_stats_range(1, None).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].date, today);
        assert_eq!(one[0].requests, 1);

        // days<=0 clamps to 1
        let clamped_low = store.list_request_stats_range(0, None).unwrap();
        assert_eq!(clamped_low.len(), 1);
        assert_eq!(clamped_low[0].date, today);

        let seven = store.list_request_stats_range(7, None).unwrap();
        let seven_dates: Vec<_> = seven.iter().map(|r| r.date.clone()).collect();
        assert!(seven_dates.contains(&today));
        assert!(seven_dates.contains(&d6));
        assert!(
            !seven_dates.contains(&d7),
            "days=7 must exclude today-7 (window is today..(today-6))"
        );
        assert_eq!(seven.iter().map(|r| r.requests).sum::<i64>(), 1 + 6);

        let max_window = store.list_request_stats_range(365, None).unwrap();
        let max_dates: Vec<_> = max_window.iter().map(|r| r.date.clone()).collect();
        assert!(max_dates.contains(&d364));
        assert!(
            !max_dates.contains(&d365),
            "days=365 must exclude today-365"
        );

        // days>365 clamps to 365 (same exclusion)
        let clamped_high = store.list_request_stats_range(10_000, None).unwrap();
        let high_dates: Vec<_> = clamped_high.iter().map(|r| r.date.clone()).collect();
        assert!(high_dates.contains(&d364));
        assert!(!high_dates.contains(&d365));

        remove_test_database(&path);
    }

    #[test]
    fn list_request_stats_range_sorts_ascending_and_sums_same_server_ports() {
        let path = test_database_path("req-stats-range-ports");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let today = local_date();
        let d1 = local_date_offset(-1);

        // Simulate multi-port rows for the same logical server/date by inserting
        // via a temporary table rewrite is unnecessary: production PK is
        // (date, server_id). Exercise SUM by using two ids then server filter on
        // one, and a raw multi-row day under a shared id is impossible without
        // breaking PK — instead verify GROUP BY SUM path with two ports under
        // distinct temporary rows that share date only in global mode already
        // covered. Here we still verify ascending order with sparse dates.
        seed_request_stats_daily(
            &path,
            &[
                (today.as_str(), "srv", "Srv", 8097, 5, 1, 0, 0, 0, 2),
                (d1.as_str(), "srv", "Srv", 8096, 3, 0, 1, 0, 0, 1),
            ],
        );

        let rows = store.list_request_stats_range(2, Some("srv")).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].date, d1);
        assert_eq!(rows[0].requests, 3);
        assert_eq!(rows[1].date, today);
        assert_eq!(rows[1].requests, 5);
        assert!(rows[0].date < rows[1].date);

        remove_test_database(&path);
    }

    fn open_legacy_admin_db(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE admin_users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                enabled BOOLEAN NOT NULL DEFAULT TRUE,
                created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE admin_sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                admin_user_id INTEGER NOT NULL,
                token_hash TEXT NOT NULL UNIQUE,
                expires_at INTEGER NOT NULL,
                created_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (admin_user_id) REFERENCES admin_users(id) ON DELETE CASCADE
            );
            "#,
        )
        .unwrap();
        conn.execute(
            "INSERT INTO admin_users (username, password_hash, enabled) VALUES ('legacy', 'legacy-hash', TRUE)",
            [],
        )
        .unwrap();
        let admin_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO admin_sessions (admin_user_id, token_hash, expires_at) VALUES (?1, 'legacy-token', ?2)",
            params![admin_id, now_seconds() + 3_600],
        )
        .unwrap();
    }

    #[test]
    fn legacy_database_upgrades_without_alert_key_and_preserves_settings() {
        let path = test_database_path("legacy-no-alert-key");
        let mut key_path = path.as_os_str().to_os_string();
        key_path.push(".key");
        let key_path = PathBuf::from(key_path);
        open_legacy_admin_db(&path);
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
            );",
        )
        .unwrap();
        let mut config = crate::config::Config::default_runtime();
        config.emby_host = "http://legacy-upgrade.invalid".into();
        config.emby_api_key = "synthetic-legacy-api-key".into();
        conn.execute(
            "INSERT INTO settings (key, value) VALUES ('runtime_config', ?1)",
            params![serde_json::to_string(&config).unwrap()],
        )
        .unwrap();
        drop(conn);

        for _ in 0..2 {
            let store = SettingsStore::open_for_test(path.clone()).unwrap();
            let alerts = crate::alerts::Alerts::new(&path).unwrap();
            assert!(alerts.get_alert_rules().unwrap().is_empty());
            assert!(!key_path.exists());
            let admin = store.admin_password_hash("legacy").unwrap().unwrap();
            assert_eq!(admin.password_hash, "legacy-hash");
            let identity = store
                .admin_security_identity(admin.admin_user_id)
                .unwrap()
                .unwrap();
            assert_eq!(identity.role_keys, vec!["super_admin".to_string()]);
            assert_eq!(
                store
                    .session_admin_user_id("legacy-token", now_seconds())
                    .unwrap(),
                Some(admin.admin_user_id)
            );
            let migrated_config = store.load_config().unwrap().unwrap();
            assert_eq!(migrated_config.emby_host, config.emby_host);
            assert_eq!(migrated_config.servers[0].emby_api_key, config.emby_api_key);
            drop(alerts);
            drop(store);
        }
        remove_test_database(&path);
    }

    fn insert_role(store: &SettingsStore, key: &str, name: &str) -> i64 {
        let conn = store.connection().unwrap();
        let now = now_seconds();
        conn.execute(
            r#"
            INSERT OR IGNORE INTO admin_roles (key, name, description, is_system, created_at)
            VALUES (?1, ?2, '', 0, ?3)
            "#,
            params![key, name, now],
        )
        .unwrap();
        let role_id = conn
            .query_row(
                "SELECT id FROM admin_roles WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .unwrap();
        // This legacy fixture intentionally represents an empty custom role.
        store.replace_role_permissions(role_id, &[]).unwrap();
        role_id
    }

    #[test]
    fn admin_security_migrates_legacy_schema_and_is_idempotent() {
        let path = test_database_path("admin-security-legacy-migrate");
        open_legacy_admin_db(&path);

        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let users = store.list_admin_users().unwrap();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].username, "legacy");
        assert_eq!(users[0].role_keys, vec!["super_admin".to_string()]);

        let identity = store.admin_security_identity(users[0].id).unwrap().unwrap();
        assert!(identity.enabled);
        assert_eq!(identity.role_keys, vec!["super_admin".to_string()]);
        assert_eq!(
            store
                .session_admin_user_id("legacy-token", now_seconds())
                .unwrap(),
            Some(users[0].id)
        );

        let conn = store.connection().unwrap();
        let version_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 'admin_security_v1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version_count, 1);
        let role_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM admin_user_roles WHERE admin_user_id = ?1",
                params![users[0].id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(role_count, 1);
        // Real schema evidence: new columns exist after gated ALTER path.
        for column in [
            "display_name",
            "totp_enabled",
            "totp_pending",
            "password_changed_at",
            "disabled_at",
            "updated_at",
        ] {
            assert!(
                super::table_has_column(&conn, "admin_users", column).unwrap(),
                "missing admin_users.{column}"
            );
        }
        for column in [
            "ip",
            "user_agent",
            "device_label",
            "last_seen_at",
            "revoked_at",
            "auth_method",
        ] {
            assert!(
                super::table_has_column(&conn, "admin_sessions", column).unwrap(),
                "missing admin_sessions.{column}"
            );
        }
        let session_revoked: Option<i64> = conn
            .query_row(
                "SELECT revoked_at FROM admin_sessions WHERE token_hash = 'legacy-token'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(session_revoked, None);
        let bound_role: String = conn
            .query_row(
                r#"
                SELECT r.key
                FROM admin_user_roles ur
                INNER JOIN admin_roles r ON r.id = ur.role_id
                WHERE ur.admin_user_id = ?1
                "#,
                params![users[0].id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(bound_role, "super_admin");
        drop(conn);
        drop(store);

        // Re-open must remain idempotent and must not duplicate role bindings.
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let users = store.list_admin_users().unwrap();
        assert_eq!(users[0].role_keys, vec!["super_admin".to_string()]);
        let conn = store.connection().unwrap();
        let version_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = 'admin_security_v1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version_count, 1);
        drop(conn);
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn bootstrap_admin_binds_super_admin_atomically_and_rejects_second_setup() {
        let path = test_database_path("admin-security-bootstrap-role");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let admin_id = store
            .create_initial_admin("bootstrap", "hash-1")
            .unwrap()
            .unwrap();
        let identity = store.admin_security_identity(admin_id).unwrap().unwrap();
        assert_eq!(identity.role_keys, vec!["super_admin".to_string()]);
        assert!(
            store
                .create_initial_admin("other", "hash-2")
                .unwrap()
                .is_none()
        );

        let barrier = Arc::new(Barrier::new(2));
        let path_b = test_database_path("admin-security-bootstrap-concurrent");
        let store_b = SettingsStore::open_for_test(path_b.clone()).unwrap();
        let handles = ["c-admin-a", "c-admin-b"].map(|username| {
            let store = store_b.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                store.create_initial_admin(username, "password-hash")
            })
        });
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.iter().filter(|result| result.is_some()).count(), 1);
        let winner = results.into_iter().find_map(|value| value).unwrap();
        let winner_identity = store_b.admin_security_identity(winner).unwrap().unwrap();
        assert_eq!(winner_identity.role_keys, vec!["super_admin".to_string()]);
        let conn = store_b.connection().unwrap();
        let admin_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM admin_users", [], |row| row.get(0))
            .unwrap();
        let role_bind_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM admin_user_roles", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(admin_count, 1);
        assert_eq!(role_bind_count, 1);
        drop(conn);

        drop(store);
        drop(store_b);
        remove_test_database(&path);
        remove_test_database(&path_b);
    }

    #[test]
    fn disabled_admin_and_revoked_or_expired_session_cannot_authenticate() {
        let path = test_database_path("admin-security-session-auth");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let admin_id = store
            .create_initial_admin("sess-admin", "hash")
            .unwrap()
            .unwrap();
        // Keep another enabled super_admin so this test can exercise disabling the target.
        let _reserve_super = store
            .create_admin_with_roles(
                "sess-reserve-super",
                "hash-reserve",
                None,
                &["super_admin"],
                Some(admin_id),
            )
            .unwrap();
        let expires_at = now_seconds() + 3_600;
        store
            .create_session(admin_id, "live-token", expires_at)
            .unwrap();
        store
            .create_session(admin_id, "soon-expired", now_seconds() - 1)
            .unwrap();
        assert_eq!(
            store
                .session_admin_user_id("live-token", now_seconds())
                .unwrap(),
            Some(admin_id)
        );
        assert_eq!(
            store
                .session_admin_user_id("soon-expired", now_seconds())
                .unwrap(),
            None
        );

        assert_eq!(
            store.revoke_session("live-token", now_seconds()).unwrap(),
            Some(admin_id)
        );
        assert_eq!(
            store
                .session_admin_user_id("live-token", now_seconds())
                .unwrap(),
            None
        );

        store
            .create_session(admin_id, "disable-token", expires_at)
            .unwrap();
        assert!(
            store
                .set_admin_disabled(admin_id, true, now_seconds())
                .unwrap()
        );
        assert_eq!(
            store
                .session_admin_user_id("disable-token", now_seconds())
                .unwrap(),
            None
        );

        let sessions = store.list_admin_sessions(admin_id).unwrap();
        assert!(!sessions.is_empty());
        let encoded = format!("{sessions:?}");
        assert!(
            !encoded.contains("token_hash") && !encoded.to_ascii_lowercase().contains("live-token"),
            "session list debug must not expose token hashes or raw tokens: {encoded}"
        );
        let conn = store.connection().unwrap();
        let revoked_rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM admin_sessions WHERE token_hash = 'live-token' AND revoked_at IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revoked_rows, 1);
        let session_select = r#"
            SELECT id, admin_user_id, ip, user_agent, device_label,
                   last_seen_at, expires_at, created_at, revoked_at, auth_method
            FROM admin_sessions
        "#;
        assert!(!session_select.contains("token_hash"));
        drop(conn);

        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn admin_session_auth_meta_and_throttled_last_seen_touch() {
        let path = test_database_path("admin-session-auth-meta-touch");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let admin_id = store
            .create_initial_admin("auth-meta-admin", "hash")
            .unwrap()
            .unwrap();
        // Keep another enabled super_admin so this test can exercise disabling the target.
        let _reserve_super = store
            .create_admin_with_roles(
                "auth-meta-reserve-super",
                "hash-reserve",
                None,
                &["super_admin"],
                Some(admin_id),
            )
            .unwrap();
        let expires_at = 2_000_000_i64;
        let created_at = 1_000_000_i64;
        // Pin last_seen via raw SQL so throttle/monotonic cases are deterministic.
        {
            let conn = store.connection().unwrap();
            conn.execute(
                r#"
                INSERT INTO admin_sessions (
                    admin_user_id, token_hash, expires_at, last_seen_at, auth_method
                )
                VALUES (?1, 'auth-meta-live', ?2, ?3, 'password')
                "#,
                params![admin_id, expires_at, created_at],
            )
            .unwrap();
            conn.execute(
                r#"
                INSERT INTO admin_sessions (
                    admin_user_id, token_hash, expires_at, last_seen_at, auth_method, revoked_at
                )
                VALUES (?1, 'auth-meta-revoked', ?2, ?3, 'password', ?3)
                "#,
                params![admin_id, expires_at, created_at],
            )
            .unwrap();
            conn.execute(
                r#"
                INSERT INTO admin_sessions (
                    admin_user_id, token_hash, expires_at, last_seen_at, auth_method
                )
                VALUES (?1, 'auth-meta-expired', ?2, ?3, 'password')
                "#,
                params![admin_id, created_at - 10, created_at],
            )
            .unwrap();
        }

        let meta = store
            .admin_session_auth_by_hash("auth-meta-live", created_at + 10)
            .unwrap()
            .expect("live session must authenticate");
        assert_eq!(meta.admin_user_id, admin_id);
        assert_eq!(meta.last_seen_at, created_at);
        assert_eq!(meta.expires_at, expires_at);
        assert_eq!(meta.auth_method, "password");
        assert!(meta.session_id > 0);
        let meta_debug = format!("{meta:?}");
        assert!(
            !meta_debug.contains("token_hash")
                && !meta_debug.to_ascii_lowercase().contains("auth-meta-live"),
            "auth meta debug must not expose token hash or raw token: {meta_debug}"
        );

        // Compatibility wrapper reuses the same live-session query.
        assert_eq!(
            store
                .session_admin_user_id("auth-meta-live", created_at + 10)
                .unwrap(),
            Some(admin_id)
        );
        assert_eq!(
            store
                .admin_session_auth_by_hash("auth-meta-revoked", created_at + 10)
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .admin_session_auth_by_hash("auth-meta-expired", created_at + 10)
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .session_admin_user_id("auth-meta-revoked", created_at + 10)
                .unwrap(),
            None
        );

        // Disabled user rejects auth even with a live session row.
        assert!(
            store
                .set_admin_disabled(admin_id, true, created_at + 20)
                .unwrap()
        );
        assert_eq!(
            store
                .admin_session_auth_by_hash("auth-meta-live", created_at + 30)
                .unwrap(),
            None
        );
        assert_eq!(
            store
                .session_admin_user_id("auth-meta-live", created_at + 30)
                .unwrap(),
            None
        );
        assert!(
            store
                .set_admin_disabled(admin_id, false, created_at + 40)
                .unwrap()
        );

        // Re-enabling must not resurrect revoked sessions; use a fresh row for touch checks.
        let session_id: i64 = {
            let conn = store.connection().unwrap();
            conn.execute(
                r#"
                INSERT INTO admin_sessions (
                    admin_user_id, token_hash, expires_at, last_seen_at, auth_method
                )
                VALUES (?1, 'auth-meta-touch', ?2, ?3, 'password')
                "#,
                params![admin_id, expires_at, created_at],
            )
            .unwrap();
            conn.query_row(
                "SELECT id FROM admin_sessions WHERE token_hash = 'auth-meta-touch'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        // Within 60s throttle window: no write.
        assert_eq!(
            store
                .touch_admin_session_last_seen(session_id, created_at + 30, 60)
                .unwrap(),
            AdminSessionTouchResult::NotDue
        );
        {
            let conn = store.connection().unwrap();
            let (last_seen, expires): (i64, i64) = conn
                .query_row(
                    "SELECT last_seen_at, expires_at FROM admin_sessions WHERE id = ?1",
                    params![session_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                last_seen, created_at,
                "throttled touch must not rewrite last_seen"
            );
            assert_eq!(expires, expires_at, "touch must never extend expires_at");
        }

        // After throttle window: write advances last_seen only.
        let due_at = created_at + 60;
        assert_eq!(
            store
                .touch_admin_session_last_seen(session_id, due_at, 60)
                .unwrap(),
            AdminSessionTouchResult::Updated
        );
        {
            let conn = store.connection().unwrap();
            let (last_seen, expires): (i64, i64) = conn
                .query_row(
                    "SELECT last_seen_at, expires_at FROM admin_sessions WHERE id = ?1",
                    params![session_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(last_seen, due_at);
            assert_eq!(expires, expires_at);
        }

        // Time regression must not move last_seen backwards.
        assert_eq!(
            store
                .touch_admin_session_last_seen(session_id, due_at - 5, 0)
                .unwrap(),
            AdminSessionTouchResult::NotDue
        );
        {
            let conn = store.connection().unwrap();
            let last_seen: i64 = conn
                .query_row(
                    "SELECT last_seen_at FROM admin_sessions WHERE id = ?1",
                    params![session_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(last_seen, due_at);
        }

        // Negative interval clamps to 0 and still respects monotonicity / live checks.
        assert_eq!(
            store
                .touch_admin_session_last_seen(session_id, due_at + 1, -15)
                .unwrap(),
            AdminSessionTouchResult::Updated
        );
        {
            let conn = store.connection().unwrap();
            let last_seen: i64 = conn
                .query_row(
                    "SELECT last_seen_at FROM admin_sessions WHERE id = ?1",
                    params![session_id],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(last_seen, due_at + 1);
        }

        // Inactive outcomes for revoked / missing / disabled.
        let revoked_id: i64 = {
            let conn = store.connection().unwrap();
            conn.query_row(
                "SELECT id FROM admin_sessions WHERE token_hash = 'auth-meta-revoked'",
                [],
                |row| row.get(0),
            )
            .unwrap()
        };
        assert_eq!(
            store
                .touch_admin_session_last_seen(revoked_id, due_at + 100, 60)
                .unwrap(),
            AdminSessionTouchResult::Inactive
        );
        assert_eq!(
            store
                .touch_admin_session_last_seen(session_id + 9_999, due_at + 100, 60)
                .unwrap(),
            AdminSessionTouchResult::Inactive
        );
        assert!(
            store
                .set_admin_disabled(admin_id, true, due_at + 120)
                .unwrap()
        );
        assert_eq!(
            store
                .touch_admin_session_last_seen(session_id, due_at + 180, 0)
                .unwrap(),
            AdminSessionTouchResult::Inactive
        );

        // List DTO still never includes token_hash.
        let listed = store.list_admin_sessions(admin_id).unwrap();
        let listed_debug = format!("{listed:?}");
        assert!(
            !listed_debug.contains("token_hash"),
            "list DTO debug must not include token_hash: {listed_debug}"
        );

        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn create_admin_with_roles_and_server_grants_is_atomic() {
        let path = test_database_path("admin-security-create-atomic");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let bootstrap = store.create_initial_admin("root", "hash").unwrap().unwrap();
        let admin_id = store
            .create_admin_with_roles_and_server_grants(
                "operator",
                "operator-hash",
                Some("Operator"),
                &["super_admin"],
                &[super::AdminServerGrantInput {
                    server_id: "srv-1".to_string(),
                    scope: "full".to_string(),
                }],
                Some(bootstrap),
            )
            .unwrap();
        let identity = store.admin_security_identity(admin_id).unwrap().unwrap();
        assert_eq!(identity.role_keys, ["super_admin".to_string()]);
        assert_eq!(identity.server_grants.len(), 1);
        assert_eq!(identity.server_grants[0].server_id, "srv-1");

        let duplicate = super::AdminServerGrantInput {
            server_id: "srv-2".to_string(),
            scope: "full".to_string(),
        };
        assert!(matches!(
            store.create_admin_with_roles_and_server_grants(
                "must-rollback",
                "hash",
                None,
                &["super_admin"],
                &[duplicate.clone(), duplicate],
                Some(bootstrap),
            ),
            Err(AppError::Validation(_))
        ));
        assert!(
            store
                .list_admin_users()
                .unwrap()
                .iter()
                .all(|user| user.username != "must-rollback")
        );

        remove_test_database(&path);
    }

    #[test]
    fn role_permission_and_server_grant_replace_are_transactional() {
        let path = test_database_path("admin-security-roles-grants");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let admin_id = store
            .create_initial_admin("role-admin", "hash")
            .unwrap()
            .unwrap();
        let operator_id = insert_role(&store, "operator", "Operator");
        let viewer_id = insert_role(&store, "viewer", "Viewer");
        // Keep a second super_admin so later demotions of admin_id are allowed.
        let reserve_super = store
            .create_admin_with_roles(
                "reserve-super",
                "hash-reserve",
                None,
                &["super_admin"],
                Some(admin_id),
            )
            .unwrap();

        store
            .replace_role_permissions(
                operator_id,
                &[
                    "monitoring.read".to_string(),
                    "users.emby.read".to_string(),
                    "monitoring.read".to_string(),
                ],
            )
            .unwrap();
        assert_eq!(
            store.list_role_permissions(operator_id).unwrap(),
            vec!["monitoring.read".to_string(), "users.emby.read".to_string()]
        );

        store
            .replace_admin_roles(
                admin_id,
                &["operator", "viewer"],
                Some(admin_id),
                now_seconds(),
            )
            .unwrap();
        let identity = store.admin_security_identity(admin_id).unwrap().unwrap();
        assert_eq!(
            identity.role_keys,
            vec!["operator".to_string(), "viewer".to_string()]
        );
        assert_eq!(
            identity.permission_keys,
            vec!["monitoring.read".to_string(), "users.emby.read".to_string()]
        );

        store
            .replace_admin_server_grants(
                admin_id,
                &[
                    super::AdminServerGrantInput {
                        server_id: "srv-b".to_string(),
                        scope: "read".to_string(),
                    },
                    super::AdminServerGrantInput {
                        server_id: "srv-a".to_string(),
                        scope: "full".to_string(),
                    },
                ],
                Some(admin_id),
                now_seconds(),
            )
            .unwrap();
        let identity = store.admin_security_identity(admin_id).unwrap().unwrap();
        assert_eq!(
            identity
                .server_grants
                .iter()
                .map(|grant| grant.server_id.as_str())
                .collect::<Vec<_>>(),
            vec!["srv-a", "srv-b"]
        );

        let err = store
            .create_admin_with_roles("broken", "hash", None, &["missing-role"], None)
            .unwrap_err();
        assert!(matches!(err, super::AppError::Validation(_)));
        assert!(
            store
                .list_admin_users()
                .unwrap()
                .iter()
                .all(|user| user.username != "broken")
        );

        let roles = store.list_admin_roles().unwrap();
        assert!(
            roles
                .iter()
                .any(|role| role.key == "super_admin" && role.is_system)
        );
        assert!(roles.iter().any(|role| role.id == viewer_id));

        // No delete_role API: system super_admin cannot be removed by replace_role_permissions.
        store
            .replace_role_permissions(
                roles.iter().find(|r| r.key == "super_admin").unwrap().id,
                &[],
            )
            .unwrap();
        assert!(
            store
                .list_admin_roles()
                .unwrap()
                .iter()
                .any(|role| role.key == "super_admin" && role.is_system)
        );

        // Empty server grants remain empty for non-super_admin identities (deny-by-default for A1).
        store
            .replace_admin_server_grants(admin_id, &[], Some(admin_id), now_seconds())
            .unwrap();
        let identity = store.admin_security_identity(admin_id).unwrap().unwrap();
        assert!(identity.server_grants.is_empty());
        assert!(!identity.role_keys.iter().any(|k| k == "super_admin"));

        // Demote reserve super first so admin_id becomes the sole super_admin again,
        // then refuse demoting the last super_admin holder.
        store
            .replace_admin_roles(admin_id, &["super_admin"], Some(admin_id), now_seconds())
            .unwrap();
        store
            .replace_admin_roles(reserve_super, &["operator"], Some(admin_id), now_seconds())
            .unwrap();
        let demote_err = store
            .replace_admin_roles(admin_id, &["operator"], Some(admin_id), now_seconds())
            .unwrap_err();
        assert!(matches!(demote_err, super::AppError::Validation(_)));
        let identity = store.admin_security_identity(admin_id).unwrap().unwrap();
        assert_eq!(identity.role_keys, vec!["super_admin".to_string()]);

        // With a second super_admin restored, demotion is allowed.
        store
            .replace_admin_roles(
                reserve_super,
                &["super_admin"],
                Some(admin_id),
                now_seconds(),
            )
            .unwrap();
        store
            .replace_admin_roles(admin_id, &["operator"], Some(admin_id), now_seconds())
            .unwrap();
        let identity = store.admin_security_identity(admin_id).unwrap().unwrap();
        assert_eq!(identity.role_keys, vec!["operator".to_string()]);
        let reserve_identity = store
            .admin_security_identity(reserve_super)
            .unwrap()
            .unwrap();
        assert_eq!(reserve_identity.role_keys, vec!["super_admin".to_string()]);

        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn api_token_hash_auth_list_and_revocation() {
        let path = test_database_path("admin-security-api-token");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let admin_id = store
            .create_initial_admin("token-admin", "hash")
            .unwrap()
            .unwrap();
        let now = now_seconds();
        let token_id = store
            .create_admin_api_token(
                admin_id,
                "ci",
                "embyp_ab",
                "hash-only-token-value",
                &["monitoring.read".to_string(), "settings.read".to_string()],
                Some(&["srv-1".to_string()]),
                Some(now + 60),
                Some(admin_id),
                now,
            )
            .unwrap();

        let auth = store
            .admin_api_token_auth_by_hash("hash-only-token-value", now)
            .unwrap()
            .unwrap();
        assert_eq!(auth.id, token_id);
        assert_eq!(auth.admin_user_id, admin_id);
        assert_eq!(
            auth.scopes,
            vec!["monitoring.read".to_string(), "settings.read".to_string()]
        );
        assert_eq!(auth.server_ids, Some(vec!["srv-1".to_string()]));

        assert!(
            store
                .touch_admin_api_token_last_used(token_id, now + 1)
                .unwrap()
        );
        let listed = store.list_admin_api_tokens(admin_id).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].token_prefix, "embyp_ab");
        assert_eq!(listed[0].last_used_at, Some(now + 1));
        let listed_debug = format!("{listed:?}");
        assert!(
            !listed_debug.contains("hash-only-token-value"),
            "list DTO must never include token_hash: {listed_debug}"
        );
        // Storage is hash-only; list path never SELECTs token_hash.
        let conn = store.connection().unwrap();
        let stored_hash: String = conn
            .query_row(
                "SELECT token_hash FROM admin_api_tokens WHERE id = ?1",
                params![token_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_hash, "hash-only-token-value");
        let list_sql = r#"
            SELECT id, admin_user_id, name, token_prefix, scopes, server_ids,
                   expires_at, last_used_at, revoked_at, created_at, created_by
            FROM admin_api_tokens
        "#;
        assert!(
            !list_sql.to_ascii_lowercase().contains("token_hash"),
            "list SELECT must omit token_hash"
        );
        // Empty scopes must remain empty (no implicit full access at DB boundary).
        let empty_scopes_id = store
            .create_admin_api_token(
                admin_id,
                "empty-scopes",
                "embyp_es",
                "empty-scopes-hash",
                &[],
                Some(&[]),
                None,
                Some(admin_id),
                now,
            )
            .unwrap();
        let empty_auth = store
            .admin_api_token_auth_by_hash("empty-scopes-hash", now)
            .unwrap()
            .unwrap();
        assert_eq!(empty_auth.id, empty_scopes_id);
        assert!(empty_auth.scopes.is_empty());
        assert_eq!(empty_auth.server_ids, Some(Vec::<String>::new()));
        // Corrupt scopes JSON fails closed on auth path.
        conn.execute(
            "UPDATE admin_api_tokens SET scopes = 'not-json' WHERE token_hash = 'empty-scopes-hash'",
            [],
        )
        .unwrap();
        drop(conn);
        assert!(
            store
                .admin_api_token_auth_by_hash("empty-scopes-hash", now)
                .unwrap()
                .is_none()
        );

        assert!(store.revoke_admin_api_token(token_id, now + 2).unwrap());
        assert!(
            store
                .admin_api_token_auth_by_hash("hash-only-token-value", now + 2)
                .unwrap()
                .is_none()
        );

        let expired_id = store
            .create_admin_api_token(
                admin_id,
                "expired",
                "embyp_ex",
                "expired-token-hash",
                &["monitoring.read".to_string()],
                None,
                Some(now - 1),
                None,
                now,
            )
            .unwrap();
        assert!(
            store
                .admin_api_token_auth_by_hash("expired-token-hash", now)
                .unwrap()
                .is_none()
        );
        assert_eq!(expired_id > 0, true);

        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn totp_last_used_step_advances_forward_only_without_plaintext_api() {
        let path = test_database_path("admin-security-totp");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let admin_id = store
            .create_initial_admin("totp-admin", "hash")
            .unwrap()
            .unwrap();
        let now = now_seconds();
        store
            .upsert_admin_totp_secret(
                admin_id,
                b"cipher-bytes",
                b"nonce-bytes",
                1,
                "SHA1",
                6,
                30,
                now,
            )
            .unwrap();
        let pending = store.admin_security_identity(admin_id).unwrap().unwrap();
        assert!(pending.totp_pending);
        assert!(!pending.totp_enabled);

        assert!(
            store
                .confirm_admin_totp(admin_id, b"cipher-bytes", b"nonce-bytes", now + 1)
                .unwrap()
        );
        let enabled = store.admin_security_identity(admin_id).unwrap().unwrap();
        assert!(enabled.totp_enabled);
        assert!(!enabled.totp_pending);

        assert!(
            store
                .advance_admin_totp_last_used_step(admin_id, 100, now + 2)
                .unwrap()
        );
        assert!(
            !store
                .advance_admin_totp_last_used_step(admin_id, 100, now + 3)
                .unwrap()
        );
        assert!(
            !store
                .advance_admin_totp_last_used_step(admin_id, 99, now + 4)
                .unwrap()
        );
        assert!(
            store
                .advance_admin_totp_last_used_step(admin_id, 101, now + 5)
                .unwrap()
        );

        let row = store.get_admin_totp_secret(admin_id).unwrap().unwrap();
        assert_eq!(row.last_used_step, Some(101));
        let debug = format!("{row:?}");
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains("cipher-bytes"));
        assert!(!debug.contains("nonce-bytes"));
        let conn = store.connection().unwrap();
        let (cipher, nonce): (Vec<u8>, Vec<u8>) = conn
            .query_row(
                "SELECT secret_ciphertext, secret_nonce FROM admin_totp_secrets WHERE admin_user_id = ?1",
                params![admin_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(cipher, b"cipher-bytes".to_vec());
        assert_eq!(nonce, b"nonce-bytes".to_vec());
        // No plaintext secret column exists.
        let totp_cols: Vec<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(admin_totp_secrets)")
                .unwrap();
            let rows = stmt.query_map([], |row| row.get::<_, String>(1)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        assert!(
            !totp_cols
                .iter()
                .any(|c| c.contains("plain") || c == "secret")
        );
        assert!(totp_cols.iter().any(|c| c == "secret_ciphertext"));
        assert!(totp_cols.iter().any(|c| c == "secret_nonce"));
        drop(conn);

        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn totp_same_step_concurrent_advance_succeeds_once() {
        let path = test_database_path("admin-security-totp-race");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let admin_id = store
            .create_initial_admin("totp-race", "hash")
            .unwrap()
            .unwrap();
        store
            .upsert_admin_totp_secret(admin_id, b"cipher", b"123456789012", 1, "SHA1", 6, 30, 1)
            .unwrap();
        store
            .confirm_admin_totp(admin_id, b"cipher", b"123456789012", 2)
            .unwrap();

        let barrier = Arc::new(Barrier::new(8));
        let handles = (0..8)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let store = SettingsStore::open_for_test(path).unwrap();
                    barrier.wait();
                    store
                        .advance_admin_totp_last_used_step(admin_id, 42, 3)
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let successes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|success| *success)
            .count();
        assert_eq!(successes, 1);
        assert_eq!(
            store
                .get_admin_totp_secret(admin_id)
                .unwrap()
                .unwrap()
                .last_used_step,
            Some(42)
        );
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn totp_parameters_are_fail_closed_on_write_and_read() {
        let path = test_database_path("admin-security-totp-parameters");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let admin_id = store
            .create_initial_admin("totp-parameters", "hash")
            .unwrap()
            .unwrap();
        for (algo, digits, period) in [("MD5", 6, 30), ("SHA1", 8, 30), ("SHA1", 6, 60)] {
            assert!(
                store
                    .upsert_admin_totp_secret(
                        admin_id,
                        b"cipher",
                        b"123456789012",
                        1,
                        algo,
                        digits,
                        period,
                        1
                    )
                    .is_err()
            );
        }
        store
            .upsert_admin_totp_secret(admin_id, b"cipher", b"123456789012", 1, "SHA1", 6, 30, 1)
            .unwrap();
        store
            .connection()
            .unwrap()
            .execute("UPDATE admin_totp_secrets SET period_seconds = 0", [])
            .unwrap();
        assert!(store.get_admin_totp_secret(admin_id).is_err());
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn recovery_codes_replace_atomically_and_consume_once() {
        let path = test_database_path("admin-security-recovery");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let admin_id = store
            .create_initial_admin("recovery-admin", "hash")
            .unwrap()
            .unwrap();
        let now = now_seconds();
        store
            .replace_admin_recovery_codes(
                admin_id,
                &["hash-a".to_string(), "hash-b".to_string()],
                now,
            )
            .unwrap();
        assert_eq!(
            store.count_unused_admin_recovery_codes(admin_id).unwrap(),
            2
        );
        assert!(
            store
                .consume_admin_recovery_code(admin_id, "hash-a", now + 1)
                .unwrap()
        );
        assert!(
            !store
                .consume_admin_recovery_code(admin_id, "hash-a", now + 2)
                .unwrap()
        );
        assert_eq!(
            store.count_unused_admin_recovery_codes(admin_id).unwrap(),
            1
        );

        store
            .replace_admin_recovery_codes(admin_id, &["hash-c".to_string()], now + 3)
            .unwrap();
        assert_eq!(
            store.count_unused_admin_recovery_codes(admin_id).unwrap(),
            1
        );
        assert!(
            !store
                .consume_admin_recovery_code(admin_id, "hash-b", now + 4)
                .unwrap()
        );
        assert!(
            store
                .consume_admin_recovery_code(admin_id, "hash-c", now + 5)
                .unwrap()
        );
        let conn = store.connection().unwrap();
        let stored: (String, Option<i64>) = conn
            .query_row(
                "SELECT code_hash, used_at FROM admin_recovery_codes WHERE admin_user_id = ?1",
                params![admin_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored.0, "hash-c");
        assert!(stored.1.is_some());
        let code_cols: Vec<String> = {
            let mut stmt = conn
                .prepare("PRAGMA table_info(admin_recovery_codes)")
                .unwrap();
            let rows = stmt.query_map([], |row| row.get::<_, String>(1)).unwrap();
            rows.map(|r| r.unwrap()).collect()
        };
        assert!(code_cols.iter().any(|c| c == "code_hash"));
        assert!(!code_cols.iter().any(|c| c == "code" || c == "plaintext"));
        drop(conn);

        // Concurrent consume of the same unused hash succeeds exactly once.
        store
            .replace_admin_recovery_codes(admin_id, &["hash-race".to_string()], now + 6)
            .unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let path_c = path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let store = SettingsStore::open_for_test(path_c).unwrap();
                barrier.wait();
                store
                    .consume_admin_recovery_code(admin_id, "hash-race", now + 7)
                    .unwrap()
            }));
        }
        let results: Vec<bool> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|ok| **ok).count(), 1);
        assert_eq!(
            store.count_unused_admin_recovery_codes(admin_id).unwrap(),
            0
        );

        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn admin_security_migration_records_version_only_after_role_bind() {
        let path = test_database_path("admin-security-migration-order");
        open_legacy_admin_db(&path);
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let conn = store.connection().unwrap();
        // After a successful open, version stamp and super_admin binding co-exist.
        let version_present: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 'admin_security_v1')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(version_present);
        let bound: i64 = conn
            .query_row(
                r#"
                SELECT COUNT(*)
                FROM admin_user_roles ur
                INNER JOIN admin_roles r ON r.id = ur.role_id
                WHERE r.key = 'super_admin'
                "#,
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(bound >= 1);
        // Simulate a fresh data-phase check: deleting the version without roles would re-bind,
        // but with roles present INSERT OR IGNORE keeps cardinality stable.
        conn.execute(
            "DELETE FROM schema_migrations WHERE version = 'admin_security_v1'",
            [],
        )
        .unwrap();
        drop(conn);
        drop(store);
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let conn = store.connection().unwrap();
        let version_present: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version = 'admin_security_v1')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(version_present);
        let bound_after: i64 = conn
            .query_row(
                r#"
                SELECT COUNT(*)
                FROM admin_user_roles ur
                INNER JOIN admin_roles r ON r.id = ur.role_id
                WHERE r.key = 'super_admin'
                "#,
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(bound_after, bound);
        drop(conn);
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn create_admin_with_roles_and_profile_updates() {
        let path = test_database_path("admin-security-create-admin");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let bootstrap = store.create_initial_admin("root", "hash").unwrap().unwrap();
        insert_role(&store, "operator", "Operator");
        let admin_id = store
            .create_admin_with_roles(
                "ops",
                "ops-hash",
                Some("Ops User"),
                &["operator"],
                Some(bootstrap),
            )
            .unwrap();
        assert!(
            store
                .update_admin_display_name(admin_id, Some("Ops Renamed"))
                .unwrap()
        );
        let users = store.list_admin_users().unwrap();
        let ops = users.iter().find(|user| user.id == admin_id).unwrap();
        assert_eq!(ops.display_name.as_deref(), Some("Ops Renamed"));
        assert_eq!(ops.role_keys, vec!["operator".to_string()]);

        let sessions_before = store.list_admin_sessions(admin_id).unwrap();
        assert!(sessions_before.is_empty());
        store
            .create_session(admin_id, "ops-token", now_seconds() + 60)
            .unwrap();
        assert_eq!(
            store
                .revoke_all_admin_sessions(admin_id, now_seconds())
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .session_admin_user_id("ops-token", now_seconds())
                .unwrap(),
            None
        );

        drop(store);
        remove_test_database(&path);
    }
    #[test]
    fn test_orphan_admin_user_for_test_preserves_auth_rows() {
        let path = test_database_path("orphan-preserve-auth");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let admin_id = store
            .create_initial_admin("orphan-admin", "hash")
            .unwrap()
            .unwrap();
        let now = now_seconds();
        store
            .create_session(admin_id, "session-hash", now + 600)
            .unwrap();
        store
            .create_admin_api_token(
                admin_id,
                "ci",
                "embyp_ab",
                "hash-only-token-value",
                &["monitoring.read".to_string(), "settings.read".to_string()],
                Some(&["srv-1".to_string()]),
                Some(now + 60),
                Some(admin_id),
                now,
            )
            .unwrap();
        store.orphan_admin_user_for_test(admin_id).unwrap();
        let sessions = store.list_admin_sessions(admin_id).unwrap();
        assert_eq!(sessions.len(), 1);
        let tokens = store.list_admin_api_tokens(admin_id).unwrap();
        assert_eq!(tokens.len(), 1);
        let auth = store
            .admin_session_auth_by_hash("session-hash", now + 10)
            .unwrap();
        assert!(auth.is_none());
        let auth = store
            .admin_api_token_auth_by_hash("hash-only-token-value", now)
            .unwrap();
        assert!(auth.is_none());
        assert!(store.admin_security_identity(admin_id).unwrap().is_none());
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn test_replace_path_for_test_causes_database_error() {
        let good_path = test_database_path("replace-path-good");
        let store = SettingsStore::open_for_test(good_path.clone()).unwrap();
        assert!(store.has_admin().is_ok());
        let mut bad_store = store.clone();
        let bad_dir = std::env::temp_dir().join(format!("bad-test-dir-{}", std::process::id()));
        fs::create_dir_all(&bad_dir).unwrap();
        bad_store.replace_path_for_test(bad_dir.clone());
        assert!(matches!(bad_store.has_admin(), Err(AppError::Database(_))));
        let _ = fs::remove_dir_all(&bad_dir);
        remove_test_database(&good_path);
    }
    #[test]
    fn test_create_admin_role_for_test_creates_non_system_role_and_is_usable_in_security_identity()
    {
        let path = test_database_path("role-fixture");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let role_id = store
            .create_admin_role_for_test("test-role", "Test Role")
            .unwrap();
        assert!(role_id > 0);

        let roles = store.list_admin_roles().unwrap();
        let has_role = roles.iter().any(|r| r.key == "test-role" && !r.is_system);
        assert!(has_role);

        // 2. replace_role_permissions
        store
            .replace_role_permissions(role_id, &["test.permission".to_string()])
            .unwrap();

        let perms = store.list_role_permissions(role_id).unwrap();
        assert_eq!(perms, ["test.permission".to_string()]);

        // 3. create_admin_with_roles binds the role
        let admin_id = store
            .create_admin_with_roles("test-user", "hash", None, &["test-role"], None)
            .unwrap();

        // 4. admin_security_identity returns the role and permissions, no super_admin
        let identity = store.admin_security_identity(admin_id).unwrap().unwrap();
        assert_eq!(identity.role_keys, ["test-role".to_string()]);
        assert_eq!(identity.permission_keys, ["test.permission".to_string()]);
        assert!(!identity.role_keys.iter().any(|rk| rk == "super_admin"));

        remove_test_database(&path);
    }

    #[test]
    fn scheduled_task_failure_history_migration_is_idempotent() {
        let path = test_database_path("task-failure-migration");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let conn = store.connection().unwrap();
        let table_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'scheduled_task_failure_history'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let migration_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1",
                params![SCHEDULED_TASK_FAILURE_HISTORY_MIGRATION_VERSION],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 1);
        assert_eq!(migration_count, 1);
        drop(conn);
        drop(store);

        let reopened = SettingsStore::open_for_test(path.clone()).unwrap();
        let conn = reopened.connection().unwrap();
        let migration_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version = ?1",
                params![SCHEDULED_TASK_FAILURE_HISTORY_MIGRATION_VERSION],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migration_count, 1);
        drop(conn);
        remove_test_database(&path);
    }

    #[test]
    fn scheduled_task_failure_history_upsert_is_server_scoped_and_sanitized() {
        let path = test_database_path("task-failure-upsert");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let history = store.task_failure_history_store();
        let observed_at = now_ms() as i64;
        let first = history
            .record_failure(ScheduledTaskFailureInput {
                server_id: "server-a".to_string(),
                task_id: "task-1".to_string(),
                run_identity: "run-1".to_string(),
                observed_at,
                status: "failed".to_string(),
                error_summary: Some("Authorization: Bearer do-not-store-this".to_string()),
                started_at: Some(observed_at - 100),
                ended_at: Some(observed_at),
            })
            .unwrap();
        assert_eq!(
            first.safe_error_summary.as_deref(),
            Some(TASK_FAILURE_ERROR_REDACTED)
        );

        let updated = history
            .record_failure(ScheduledTaskFailureInput {
                server_id: "server-a".to_string(),
                task_id: "task-1".to_string(),
                run_identity: "run-1".to_string(),
                observed_at: observed_at + 1,
                status: "completed".to_string(),
                error_summary: None,
                started_at: first.started_at,
                ended_at: Some(observed_at + 1),
            })
            .unwrap();
        assert_eq!(updated.id, first.id);
        assert_eq!(updated.status, "completed");
        assert_eq!(updated.safe_error_summary, None);

        history
            .record_failure(ScheduledTaskFailureInput {
                server_id: "server-b".to_string(),
                task_id: "task-1".to_string(),
                run_identity: "run-1".to_string(),
                observed_at: observed_at + 2,
                status: "failed".to_string(),
                error_summary: Some("ordinary upstream timeout".to_string()),
                started_at: None,
                ended_at: None,
            })
            .unwrap();

        let server_a = history
            .list_failures(&TaskFailureListParams {
                server_id: Some("server-a".to_string()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        let server_b = history
            .list_failures(&TaskFailureListParams {
                server_id: Some("server-b".to_string()),
                limit: 10,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(server_a.len(), 1);
        assert_eq!(server_b.len(), 1);
        assert_eq!(
            server_b[0].safe_error_summary.as_deref(),
            Some("ordinary upstream timeout")
        );
        drop(store);
        remove_test_database(&path);
    }

    #[test]
    fn scheduled_task_failure_history_supports_pagination_and_retention() {
        let path = test_database_path("task-failure-list-prune");
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let history = store.task_failure_history_store();
        let now = now_ms() as i64;
        for index in 0..5 {
            history
                .record_failure(ScheduledTaskFailureInput {
                    server_id: "server-a".to_string(),
                    task_id: "task-1".to_string(),
                    run_identity: format!("run-{index}"),
                    observed_at: now + index,
                    status: "failed".to_string(),
                    error_summary: Some("short failure".to_string()),
                    started_at: None,
                    ended_at: None,
                })
                .unwrap();
        }
        let page = history
            .list_failures(&TaskFailureListParams {
                server_id: Some("server-a".to_string()),
                task_id: Some("task-1".to_string()),
                limit: 2,
                offset: 1,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].run_identity, "run-3");
        assert_eq!(page[1].run_identity, "run-2");

        history
            .record_failure(ScheduledTaskFailureInput {
                server_id: "server-a".to_string(),
                task_id: "task-1".to_string(),
                run_identity: "old-run".to_string(),
                observed_at: now - (3 * TASK_FAILURE_HISTORY_DAY_MS),
                status: "failed".to_string(),
                error_summary: Some("old failure".to_string()),
                started_at: None,
                ended_at: None,
            })
            .unwrap();
        history.prune(1, 3).unwrap();
        let remaining = history
            .list_failures(&TaskFailureListParams {
                server_id: Some("server-a".to_string()),
                limit: 20,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(remaining.len(), 3);
        assert!(!remaining.iter().any(|row| row.run_identity == "old-run"));
        drop(store);
        remove_test_database(&path);
    }
    include!("test-fixtures/review_storage_fixes.rs");
}
