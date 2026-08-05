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
use serde::{Serialize, de::DeserializeOwned};

use crate::{
    config::Config,
    error::{AppError, AppResult},
    ip_location::IpLocation,
};

const DEFAULT_DATABASE_PATH: &str = "data/embypanel.db";
const CONTAINER_DATABASE_PATH: &str = "/data/embypanel.db";
const DATABASE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const MAINTENANCE_INTERVAL_SECONDS: i64 = 60 * 60;
const TELEMETRY_WRITE_QUEUE_CAPACITY: usize = 2048;
const TELEMETRY_WRITE_BATCH_SIZE: usize = 64;

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
    while let Ok(message) = receiver.recv() {
        let TelemetryMessage::Write(first) = message else {
            break;
        };
        let mut batch = Vec::with_capacity(TELEMETRY_WRITE_BATCH_SIZE);
        batch.push(first);
        let mut shutdown = false;
        while batch.len() < TELEMETRY_WRITE_BATCH_SIZE {
            match receiver.try_recv() {
                Ok(TelemetryMessage::Write(write)) => batch.push(write),
                Ok(TelemetryMessage::Shutdown) => {
                    shutdown = true;
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
        if shutdown {
            break;
        }
    }
}

#[derive(Default)]
struct MaintenanceState {
    audit_logs: AtomicI64,
    request_stats: AtomicI64,
    proxy_request_logs: AtomicI64,
    sessions: AtomicI64,
}

impl SettingsStore {
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
        transaction.execute(
            r#"
            INSERT INTO admin_users (username, password_hash, enabled)
            VALUES (?1, ?2, TRUE)
            "#,
            params![username, password_hash],
        )?;
        let admin_user_id = transaction.last_insert_rowid();
        if let Some((token_hash, expires_at)) = initial_session {
            transaction.execute(
                r#"
                INSERT INTO admin_sessions (admin_user_id, token_hash, expires_at)
                VALUES (?1, ?2, ?3)
                "#,
                params![admin_user_id, token_hash, expires_at],
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
                WHERE admin_user_id = ?1 AND token_hash = ?2 AND expires_at > ?3
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
            SET password_hash = ?3
            WHERE id = ?1 AND password_hash = ?2 AND enabled = TRUE
            "#,
            params![admin_user_id, expected_password_hash, password_hash],
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
            INSERT INTO admin_sessions (admin_user_id, token_hash, expires_at)
            VALUES (?1, ?2, ?3)
            "#,
            params![admin_user_id, token_hash, expires_at],
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
        conn.execute(
            r#"
            INSERT INTO admin_sessions (admin_user_id, token_hash, expires_at)
            VALUES (?1, ?2, ?3)
            "#,
            params![admin_user_id, token_hash, expires_at],
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
        let inserted = conn.execute(
            r#"
            INSERT INTO admin_sessions (admin_user_id, token_hash, expires_at)
            SELECT id, ?3, ?4
            FROM admin_users
            WHERE id = ?1 AND password_hash = ?2 AND enabled = TRUE
            "#,
            params![
                admin_user_id,
                expected_password_hash,
                token_hash,
                expires_at
            ],
        )?;
        Ok(inserted == 1)
    }

    pub fn revoke_session(&self, token_hash: &str, now: i64) -> AppResult<Option<i64>> {
        let conn = self.connection()?;
        let admin_user_id = conn
            .query_row(
                r#"
                DELETE FROM admin_sessions
                WHERE token_hash = ?1 AND expires_at > ?2
                RETURNING admin_user_id
                "#,
                params![token_hash, now],
                |row| row.get(0),
            )
            .optional()?;
        Ok(admin_user_id)
    }

    pub fn session_admin_user_id(&self, token_hash: &str, now: i64) -> AppResult<Option<i64>> {
        let conn = self.connection()?;
        let value = conn
            .query_row(
                r#"
                SELECT admin_user_id
                FROM admin_sessions
                WHERE token_hash = ?1 AND expires_at > ?2
                "#,
                params![token_hash, now],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value)
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
            transaction.execute(
                r#"
                INSERT INTO proxy_request_logs
                    (timestamp_ms, server_id, server_name, port, method, path, path_type,
                     status_code, outcome, duration_ms, playback_user, playback_ip,
                     cache_hit, blocked, detail)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
                "#,
                params![
                    Self::sqlite_i64(record.timestamp_ms),
                    record.server_id,
                    record.server_name,
                    record.port,
                    record.method,
                    record.path,
                    record.path_type,
                    record.status_code,
                    record.outcome,
                    Self::sqlite_i64(record.duration_ms),
                    record.playback_user,
                    record.playback_ip,
                    record.cache_hit,
                    record.blocked,
                    record.detail,
                ],
            )?;
            wrote_details = true;
        }
        transaction.commit()?;

        if !stat_deltas.is_empty() {
            run_maintenance(&self.maintenance.request_stats, || {
                self.prune_request_stats(&conn, 90)?;
                Ok(())
            })?;
        }
        if wrote_details {
            run_maintenance(&self.maintenance.proxy_request_logs, || {
                self.prune_proxy_request_logs(&conn, 7, 20000)?;
                Ok(())
            })?;
        }
        Ok(())
    }

    fn sqlite_i64(value: u128) -> i64 {
        value.min(i64::MAX as u128) as i64
    }

    pub fn list_proxy_request_details(
        &self,
        filter: ProxyRequestDetailFilter<'_>,
    ) -> AppResult<Vec<ProxyRequestDetail>> {
        let conn = self.connection()?;
        let mut conditions = Vec::new();
        let mut values: Vec<Box<dyn ToSql>> = Vec::new();
        if let Some(server_id) = filter.server_id {
            conditions.push("server_id = ?");
            values.push(Box::new(server_id.to_string()));
        }
        if let Some(path_type) = filter.path_type {
            conditions.push("path_type = ?");
            values.push(Box::new(path_type.to_string()));
        }
        if let Some(since_ms) = filter.since_ms {
            conditions.push("timestamp_ms >= ?");
            values.push(Box::new(since_ms as i64));
        }
        if let Some(until_ms) = filter.until_ms {
            conditions.push("timestamp_ms <= ?");
            values.push(Box::new(until_ms as i64));
        }
        if let Some(keyword) = filter
            .keyword
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let keyword = format!("%{}%", keyword.to_ascii_lowercase());
            conditions.push(
                r#"(lower(server_name) LIKE ?
                    OR lower(method) LIKE ?
                    OR lower(path) LIKE ?
                    OR lower(path_type) LIKE ?
                    OR lower(outcome) LIKE ?
                    OR lower(playback_user) LIKE ?
                    OR lower(playback_ip) LIKE ?
                    OR lower(detail) LIKE ?)"#,
            );
            for _ in 0..8 {
                values.push(Box::new(keyword.clone()));
            }
        }

        let where_clause = if conditions.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", conditions.join(" AND "))
        };
        let limit = filter.limit.clamp(1, 500) as i64;
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
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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
        let conn = self.connection()?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let journal_mode: String =
            conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            return Err(AppError::Internal(format!(
                "SQLite WAL mode is required, got {journal_mode}"
            )));
        }
        self.migrate(&conn)?;
        Ok(())
    }

    fn migrate(&self, conn: &Connection) -> AppResult<()> {
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
            "#,
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

pub struct ProxyRequestDetailFilter<'a> {
    pub server_id: Option<&'a str>,
    pub path_type: Option<&'a str>,
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
        DATABASE_BUSY_TIMEOUT, ProxyRequestDetailFilter, ProxyRequestDetailWrite, RequestStatKind,
        SettingsStore, TelemetryWrite, TelemetryWriteQueue, now_ms, now_seconds,
    };

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
}
