use rusqlite::{
    Connection, OptionalExtension, Result as SqlResult, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;

use crate::error::{AppError, AppResult};

pub const MAX_RULE_ID_LEN: usize = 64;
pub const MAX_RULE_NAME_LEN: usize = 128;
pub const MAX_RULE_CONFIG_LEN: usize = 4096;
pub const MAX_THRESHOLD_LEN: usize = 64;
pub const MAX_DELIVERY_MESSAGE_LEN: usize = 256;
pub const MAX_DELIVERY_LIMIT: usize = 100;
pub const MAX_SILENCE_PERIOD_SECS: u64 = 366 * 24 * 60 * 60;
pub const MAX_RETRY_COUNT: u32 = 3;
pub const MAX_SERVER_ID_LEN: usize = 128;
pub(crate) const NOTIFICATION_LEASE_MS: u128 = 60_000;
const DATABASE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const DELIVERY_HISTORY_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// Maximum rolling runtime window retained by the bounded in-memory evaluator.
pub const MAX_EVALUATION_WINDOW_SECS: u64 = 7 * 24 * 60 * 60;

pub const ALERT_STATUS_NORMAL: &str = "normal";
pub const ALERT_STATUS_FIRING: &str = "firing";

pub const ALERT_METRICS: &[&str] = &[
    "proxy.requests",
    "proxy.redirects",
    "proxy.cache_hits",
    "proxy.blocks",
    "proxy.errors",
];

pub const ALERT_OPERATORS: &[&str] = &["gt", "gte", "lt", "lte", "eq"];

pub const DEFAULT_OPERATOR: &str = "gte";

pub fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

pub fn sanitize_delivery_message(message: &str) -> String {
    let mut cleaned = String::with_capacity(message.len().min(MAX_DELIVERY_MESSAGE_LEN));
    for ch in message.chars() {
        if cleaned.len() >= MAX_DELIVERY_MESSAGE_LEN {
            break;
        }
        if ch.is_control() {
            continue;
        }
        cleaned.push(ch);
    }
    cleaned
}

fn reject_control_chars(label: &str, value: &str) -> AppResult<()> {
    if value.chars().any(|ch| ch.is_control()) {
        return Err(AppError::Validation(format!(
            "{label} contains control characters"
        )));
    }
    Ok(())
}

/// Whitelist event returned by the state machine for a future async dispatcher.
/// Never contains webhook URL, credentials, or raw network errors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlertNotifyEvent {
    /// `firing` or `recovered`
    pub kind: String,
    pub rule_id: String,
    pub rule_name: String,
    pub metric: String,
    pub operator: String,
    pub threshold: String,
    pub value: String,
    pub server_id: Option<String>,
    pub at_ms: u128,
    /// Internal delivery reservation token; never included in webhook payloads.
    #[serde(default, skip_serializing)]
    pub notification_id: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AlertRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// Webhook URL plain or `{"url":"..."}` only. Never metric config.
    pub rule_config: String,
    /// Free-form when metric is empty; finite numeric string when metric is set.
    pub threshold: String,
    /// Cooldown seconds after a firing *notification* (not manual silence).
    pub silence_period: u64,
    pub created_at: u128,
    /// Empty = manual/test only (no auto evaluation). Else whitelist metric id.
    #[serde(default)]
    pub metric: String,
    /// gt|gte|lt|lte|eq ; default gte
    #[serde(default = "default_operator_serde")]
    pub operator: String,
    /// Empty = all servers aggregated by runtime.
    #[serde(default)]
    pub server_id: String,
    /// 0 = previous runtime evaluation interval; >0 = rolling runtime window.
    #[serde(default)]
    pub evaluation_window_secs: u64,
}

fn default_operator_serde() -> String {
    DEFAULT_OPERATOR.to_string()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AlertRuleView {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub rule_configured: bool,
    pub threshold: String,
    pub silence_period: u64,
    pub created_at: u128,
    pub metric: String,
    pub operator: String,
    pub server_id: String,
    pub evaluation_window_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleConfigUpdate {
    Preserve,
    Replace(String),
    Clear,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertState {
    pub rule_id: String,
    /// `normal` or `firing`
    #[serde(default = "default_status_serde")]
    pub status: String,
    pub last_triggered_ms: Option<u128>,
    pub last_recovered_ms: Option<u128>,
    /// Manual silence flag (separate from cooldown).
    pub is_silent: bool,
    pub silent_until_ms: Option<u128>,
    /// After firing notification; suppresses re-notify until elapsed.
    pub cooldown_until_ms: Option<u128>,
    /// True only after a firing Webhook was delivered successfully and until recovery succeeds.
    pub notification_active: bool,
    pub recovery_count: u32,
    #[serde(default, skip_serializing)]
    pub pending_notification_kind: Option<String>,
    #[serde(default, skip_serializing)]
    pub pending_notification_id: Option<u64>,
    #[serde(default, skip_serializing)]
    pub pending_since_ms: Option<u128>,
    #[serde(default, skip_serializing)]
    pub retry_notification_kind: Option<String>,
    #[serde(default, skip_serializing)]
    pub notification_sequence: u64,
}

fn default_status_serde() -> String {
    ALERT_STATUS_NORMAL.to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlertDelivery {
    pub id: i64,
    pub rule_id: String,
    pub timestamp_ms: u128,
    pub status: String,
    pub retry_count: u32,
    pub message: String,
}

#[derive(Clone)]
pub struct Alerts {
    db: Arc<Mutex<Connection>>,
    rule_config_key: Arc<Mutex<Option<[u8; 32]>>>,
    rule_config_key_path: Arc<PathBuf>,
    last_delivery_history_prune: Arc<Mutex<Option<Instant>>>,
}

impl Alerts {
    pub fn new(db_path: impl AsRef<Path>) -> AppResult<Self> {
        let db_path = db_path.as_ref();
        let mut conn = Connection::open(db_path)?;
        conn.busy_timeout(DATABASE_BUSY_TIMEOUT)?;
        conn.pragma_update(None, "foreign_keys", true)?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        let journal_mode: String =
            conn.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        if !journal_mode.eq_ignore_ascii_case("wal") {
            return Err(AppError::Internal(format!(
                "SQLite WAL mode is required, got {journal_mode}"
            )));
        }
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS alert_rules (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                enabled INTEGER DEFAULT 1,
                rule_config TEXT,
                threshold TEXT,
                silence_period INTEGER,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS alert_states (
                rule_id TEXT PRIMARY KEY,
                last_triggered_ms INTEGER,
                is_silent INTEGER DEFAULT 0,
                silent_until_ms INTEGER,
                recovery_count INTEGER DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS alert_deliveries (
                id INTEGER PRIMARY KEY,
                rule_id TEXT NOT NULL,
                timestamp_ms INTEGER NOT NULL,
                status TEXT,
                retry_count INTEGER DEFAULT 0,
                message TEXT
            );
            "#,
        )?;
        Self::migrate_schema(&conn).map_err(AppError::Database)?;
        conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_alert_delivery_time ON alert_deliveries(timestamp_ms DESC, id DESC);")?;
        let has_nonempty_rule_config = Self::has_nonempty_rule_config(&conn)?;
        let key_path = rule_config_crypto::key_path(db_path);
        let rule_config_key = if has_nonempty_rule_config {
            let has_encrypted_rule_config = Self::has_encrypted_rule_config(&conn)?;
            let key = if has_encrypted_rule_config {
                rule_config_crypto::load_existing_key(&key_path)
            } else {
                rule_config_crypto::load_or_create_key(&key_path)
            }
            .map_err(|_| Self::rule_config_storage_unavailable())?;
            Self::migrate_rule_configs(&mut conn, &key)?;
            Some(key)
        } else {
            None
        };
        let alerts = Self {
            db: Arc::new(Mutex::new(conn)),
            rule_config_key: Arc::new(Mutex::new(rule_config_key)),
            rule_config_key_path: Arc::new(key_path),
            last_delivery_history_prune: Arc::new(Mutex::new(None)),
        };
        alerts.prune_delivery_history()?;
        Ok(alerts)
    }

    fn migrate_rule_configs(conn: &mut Connection, key: &[u8; 32]) -> AppResult<()> {
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| Self::rule_config_storage_unavailable())?;
        let stored_configs = {
            let mut statement = transaction
                .prepare("SELECT id, rule_config FROM alert_rules")
                .map_err(|_| Self::rule_config_storage_unavailable())?;
            let rows = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
                })
                .map_err(|_| Self::rule_config_storage_unavailable())?;
            rows.collect::<SqlResult<Vec<_>>>()
                .map_err(|_| Self::rule_config_storage_unavailable())?
        };

        let mut migrations = Vec::new();
        for (id, stored) in stored_configs {
            let Some(stored) = stored else {
                continue;
            };
            if stored.trim().is_empty() {
                continue;
            }
            if stored.starts_with(rule_config_crypto::RULE_CONFIG_PREFIX) {
                rule_config_crypto::decrypt_rule_config(&stored, key)
                    .map_err(|_| Self::rule_config_storage_unavailable())?;
                continue;
            }
            let encrypted = rule_config_crypto::encrypt_rule_config(&stored, key)
                .map_err(|_| Self::rule_config_storage_unavailable())?;
            migrations.push((id, encrypted));
        }

        for (id, encrypted) in migrations {
            let updated = transaction
                .execute(
                    "UPDATE alert_rules SET rule_config = ?1 WHERE id = ?2",
                    params![encrypted, id],
                )
                .map_err(|_| Self::rule_config_storage_unavailable())?;
            if updated != 1 {
                return Err(Self::rule_config_storage_unavailable());
            }
        }

        transaction
            .commit()
            .map_err(|_| Self::rule_config_storage_unavailable())
    }

    fn has_encrypted_rule_config(conn: &Connection) -> AppResult<bool> {
        conn.query_row(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM alert_rules
                WHERE rule_config IS NOT NULL
                  AND rule_config <> ''
                  AND substr(rule_config, 1, length(?1)) = ?1
            )
            "#,
            params![rule_config_crypto::RULE_CONFIG_PREFIX],
            |row| row.get(0),
        )
        .map_err(|_| Self::rule_config_storage_unavailable())
    }

    fn has_nonempty_rule_config(conn: &Connection) -> AppResult<bool> {
        let mut statement = conn
            .prepare("SELECT rule_config FROM alert_rules")
            .map_err(|_| Self::rule_config_storage_unavailable())?;
        let rows = statement
            .query_map([], |row| row.get::<_, Option<String>>(0))
            .map_err(|_| Self::rule_config_storage_unavailable())?;
        for row in rows {
            let value = row.map_err(|_| Self::rule_config_storage_unavailable())?;
            if value.is_some_and(|value| !value.trim().is_empty()) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn rule_config_storage_unavailable() -> AppError {
        AppError::Internal("alert rule configuration storage is unavailable".to_string())
    }

    fn current_rule_config_key(&self) -> AppResult<[u8; 32]> {
        let key = self.rule_config_key.lock().map_err(|_| {
            AppError::Internal("alert rule configuration key lock is poisoned".to_string())
        })?;
        key.ok_or_else(Self::rule_config_storage_unavailable)
    }

    fn ensure_rule_config_key(&self) -> AppResult<[u8; 32]> {
        let mut key = self.rule_config_key.lock().map_err(|_| {
            AppError::Internal("alert rule configuration key lock is poisoned".to_string())
        })?;
        if let Some(key) = *key {
            return Ok(key);
        }
        let loaded = rule_config_crypto::load_or_create_key(self.rule_config_key_path.as_path())
            .map_err(|_| Self::rule_config_storage_unavailable())?;
        *key = Some(loaded);
        Ok(loaded)
    }

    fn encrypt_rule_config_for_storage(&self, plaintext: &str) -> AppResult<String> {
        if plaintext.trim().is_empty() {
            return Ok(String::new());
        }
        let key = self.ensure_rule_config_key()?;
        rule_config_crypto::encrypt_rule_config(plaintext, &key)
            .map_err(|_| Self::rule_config_storage_unavailable())
    }

    /// Idempotent column migration for older alert DBs.
    fn migrate_schema(conn: &Connection) -> SqlResult<()> {
        let rule_cols = Self::table_columns(conn, "alert_rules")?;
        Self::add_column_if_missing(
            conn,
            &rule_cols,
            "alert_rules",
            "metric",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        Self::add_column_if_missing(
            conn,
            &rule_cols,
            "alert_rules",
            "operator",
            "TEXT NOT NULL DEFAULT 'gte'",
        )?;
        Self::add_column_if_missing(
            conn,
            &rule_cols,
            "alert_rules",
            "server_id",
            "TEXT NOT NULL DEFAULT ''",
        )?;
        Self::add_column_if_missing(
            conn,
            &rule_cols,
            "alert_rules",
            "evaluation_window_secs",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

        let state_cols = Self::table_columns(conn, "alert_states")?;
        Self::add_column_if_missing(
            conn,
            &state_cols,
            "alert_states",
            "status",
            "TEXT NOT NULL DEFAULT 'normal'",
        )?;
        Self::add_column_if_missing(
            conn,
            &state_cols,
            "alert_states",
            "last_recovered_ms",
            "INTEGER",
        )?;
        Self::add_column_if_missing(
            conn,
            &state_cols,
            "alert_states",
            "cooldown_until_ms",
            "INTEGER",
        )?;
        Self::add_column_if_missing(
            conn,
            &state_cols,
            "alert_states",
            "notification_active",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        Self::add_column_if_missing(
            conn,
            &state_cols,
            "alert_states",
            "pending_notification_kind",
            "TEXT",
        )?;
        Self::add_column_if_missing(
            conn,
            &state_cols,
            "alert_states",
            "pending_notification_id",
            "INTEGER",
        )?;
        Self::add_column_if_missing(
            conn,
            &state_cols,
            "alert_states",
            "pending_since_ms",
            "INTEGER",
        )?;
        Self::add_column_if_missing(
            conn,
            &state_cols,
            "alert_states",
            "retry_notification_kind",
            "TEXT",
        )?;
        Self::add_column_if_missing(
            conn,
            &state_cols,
            "alert_states",
            "notification_sequence",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        Ok(())
    }

    fn table_columns(conn: &Connection, table: &str) -> SqlResult<Vec<String>> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let cols = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(cols)
    }

    fn add_column_if_missing(
        conn: &Connection,
        existing: &[String],
        table: &str,
        column: &str,
        decl: &str,
    ) -> SqlResult<()> {
        if existing.iter().any(|c| c == column) {
            return Ok(());
        }
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"),
            [],
        )?;
        Ok(())
    }

    fn with_conn<F, R>(&self, f: F) -> AppResult<R>
    where
        F: FnOnce(&Connection) -> SqlResult<R>,
    {
        let conn = self
            .db
            .lock()
            .map_err(|_| AppError::Internal("alerts db mutex is poisoned".to_string()))?;
        f(&conn).map_err(AppError::Database)
    }

    fn with_conn_mut<F, R>(&self, f: F) -> AppResult<R>
    where
        F: FnOnce(&mut Connection) -> AppResult<R>,
    {
        let mut conn = self
            .db
            .lock()
            .map_err(|_| AppError::Internal("alerts db mutex is poisoned".to_string()))?;
        f(&mut conn)
    }

    /// Manual silence only (not cooldown).
    pub fn is_rule_silenced(&self, rule_id: &str) -> AppResult<bool> {
        let state = self.get_alert_state(rule_id)?;
        Ok(Self::state_is_manually_silenced(&state, now_ms()))
    }

    fn state_is_manually_silenced(state: &AlertState, now: u128) -> bool {
        match state.silent_until_ms {
            Some(until) if until > now => true,
            Some(_) => false,
            None => state.is_silent,
        }
    }

    fn state_in_cooldown(state: &AlertState, now: u128) -> bool {
        matches!(state.cooldown_until_ms, Some(until) if until > now)
    }

    fn clear_pending_notification(state: &mut AlertState) {
        state.pending_notification_kind = None;
        state.pending_notification_id = None;
        state.pending_since_ms = None;
    }

    fn reserve_notification(state: &mut AlertState, kind: &str, now: u128) -> u64 {
        state.notification_sequence = state.notification_sequence.saturating_add(1).max(1);
        state.pending_notification_kind = Some(kind.to_string());
        state.pending_notification_id = Some(state.notification_sequence);
        state.pending_since_ms = Some(now);
        state.notification_sequence
    }

    pub fn normalize_metric(raw: &str) -> AppResult<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(String::new());
        }
        reject_control_chars("metric", trimmed)?;
        if trimmed.len() > 64 {
            return Err(AppError::Validation("metric too long".to_string()));
        }
        if ALERT_METRICS.iter().any(|m| *m == trimmed) {
            return Ok(trimmed.to_string());
        }
        Err(AppError::Validation("unsupported alert metric".to_string()))
    }

    pub fn normalize_operator(raw: &str) -> AppResult<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(DEFAULT_OPERATOR.to_string());
        }
        reject_control_chars("operator", trimmed)?;
        if ALERT_OPERATORS.iter().any(|op| *op == trimmed) {
            return Ok(trimmed.to_string());
        }
        Err(AppError::Validation(
            "unsupported alert operator".to_string(),
        ))
    }

    pub fn normalize_server_id(raw: &str) -> AppResult<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("all") {
            return Ok(String::new());
        }
        reject_control_chars("server_id", trimmed)?;
        if trimmed.len() > MAX_SERVER_ID_LEN {
            return Err(AppError::Validation("server_id too long".to_string()));
        }
        Ok(trimmed.to_string())
    }

    pub fn normalize_evaluation_window_secs(value: u64) -> AppResult<u64> {
        if value > MAX_EVALUATION_WINDOW_SECS {
            return Err(AppError::Validation(
                "evaluation_window_secs exceeds maximum".to_string(),
            ));
        }
        Ok(value)
    }

    /// Parse threshold when metric is set. Rejects NaN/inf/empty.
    pub fn parse_numeric_threshold(raw: &str) -> AppResult<f64> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(AppError::Validation(
                "threshold is required for metric rules".to_string(),
            ));
        }
        if trimmed.len() > MAX_THRESHOLD_LEN {
            return Err(AppError::Validation("threshold too long".to_string()));
        }
        reject_control_chars("threshold", trimmed)?;
        let value: f64 = trimmed
            .parse()
            .map_err(|_| AppError::Validation("threshold must be a finite number".to_string()))?;
        if !value.is_finite() {
            return Err(AppError::Validation(
                "threshold must be a finite number".to_string(),
            ));
        }
        Ok(value)
    }

    pub fn format_threshold_value(value: f64) -> String {
        // Compact stable form for storage/API.
        let s = format!("{value}");
        if s.len() <= MAX_THRESHOLD_LEN {
            s
        } else {
            format!("{value:.10}")
        }
    }

    pub fn compare_metric(operator: &str, value: f64, threshold: f64) -> AppResult<bool> {
        let op = Self::normalize_operator(operator)?;
        let result = match op.as_str() {
            "gt" => value > threshold,
            "gte" => value >= threshold,
            "lt" => value < threshold,
            "lte" => value <= threshold,
            "eq" => (value - threshold).abs() <= f64::EPSILON,
            _ => {
                return Err(AppError::Validation(
                    "unsupported alert operator".to_string(),
                ));
            }
        };
        Ok(result)
    }

    /// Atomic evaluation edge detector for runtime.
    ///
    /// The returned event is a delivery reservation, not a successful notification.
    /// Call `finish_notification_delivery` exactly once after the Webhook attempt. A
    /// failed attempt clears the reservation and remains retryable without starting
    /// cooldown or activating recovery notifications.
    pub fn evaluate_condition(
        &self,
        rule_id: &str,
        is_breaching: bool,
        observed_value: Option<f64>,
    ) -> AppResult<Option<AlertNotifyEvent>> {
        self.evaluate_condition_at(rule_id, is_breaching, observed_value, now_ms())
    }

    pub fn evaluate_condition_at(
        &self,
        rule_id: &str,
        is_breaching: bool,
        observed_value: Option<f64>,
        now: u128,
    ) -> AppResult<Option<AlertNotifyEvent>> {
        self.with_conn_mut(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(AppError::Database)?;
            let rule = match Self::load_rule_tx(&tx, rule_id).map_err(AppError::Database)? {
                Some(rule) => rule,
                None => return Ok(None),
            };

            let mut state = Self::load_state_tx(&tx, rule_id).map_err(AppError::Database)?;
            let expired_retry_kind = if state
                .pending_since_ms
                .is_some_and(|since| since.saturating_add(NOTIFICATION_LEASE_MS) <= now)
            {
                let kind = state.pending_notification_kind.clone();
                if let Some(kind) = kind.as_ref() {
                    state.retry_notification_kind = Some(kind.clone());
                }
                Self::clear_pending_notification(&mut state);
                kind
            } else {
                None
            };

            let was_firing = state.status == ALERT_STATUS_FIRING;
            if !rule.enabled {
                if was_firing
                    || state.notification_active
                    || state.cooldown_until_ms.is_some()
                    || state.pending_notification_kind.is_some()
                    || state.retry_notification_kind.is_some()
                {
                    state.status = ALERT_STATUS_NORMAL.to_string();
                    state.notification_active = false;
                    state.cooldown_until_ms = None;
                    state.retry_notification_kind = None;
                    Self::clear_pending_notification(&mut state);
                    Self::upsert_state_tx(&tx, &state).map_err(AppError::Database)?;
                }
                tx.commit().map_err(AppError::Database)?;
                return Ok(None);
            }

            if is_breaching && !was_firing {
                state.status = ALERT_STATUS_FIRING.to_string();
                state.last_triggered_ms = Some(now);
                if state.retry_notification_kind.as_deref() == Some("recovered") {
                    state.retry_notification_kind = None;
                }
            } else if !is_breaching && was_firing {
                state.status = ALERT_STATUS_NORMAL.to_string();
                state.last_recovered_ms = Some(now);
                if state.retry_notification_kind.as_deref() == Some("firing") {
                    state.retry_notification_kind = None;
                }
            }

            // An opposite edge may discard a previously failed delivery kind, but a
            // reservation recovered at the start of this evaluation still belongs to
            // its original ownerless delivery and must survive this same edge.
            if let Some(kind) = expired_retry_kind {
                state.retry_notification_kind = Some(kind);
            }

            let manual_silent = Self::state_is_manually_silenced(&state, now);
            let in_cooldown = Self::state_in_cooldown(&state, now);
            let cooldown_expired = state.cooldown_until_ms.is_some_and(|until| until <= now);
            let mut notify_kind = None;

            if manual_silent {
                state.retry_notification_kind = None;
                if !is_breaching && state.notification_active {
                    state.notification_active = false;
                    Self::clear_pending_notification(&mut state);
                }
            } else if state.pending_notification_kind.is_none() {
                if is_breaching && !state.notification_active {
                    let fresh_edge = !was_firing;
                    let retry = state.retry_notification_kind.as_deref() == Some("firing");
                    if (fresh_edge && !in_cooldown) || cooldown_expired || retry {
                        notify_kind = Some("firing");
                    }
                } else if !is_breaching && state.notification_active {
                    let fresh_edge = was_firing;
                    let retry = state.retry_notification_kind.as_deref() == Some("recovered");
                    if fresh_edge || retry {
                        notify_kind = Some("recovered");
                    }
                }
            }

            let notify = notify_kind.map(|kind| {
                let notification_id = Self::reserve_notification(&mut state, kind, now);
                AlertNotifyEvent {
                    kind: kind.to_string(),
                    rule_id: rule.id.clone(),
                    rule_name: rule.name.clone(),
                    metric: rule.metric.clone(),
                    operator: rule.operator.clone(),
                    threshold: rule.threshold.clone(),
                    value: observed_value
                        .map(Self::format_threshold_value)
                        .unwrap_or_default(),
                    server_id: if rule.server_id.is_empty() {
                        None
                    } else {
                        Some(rule.server_id.clone())
                    },
                    at_ms: now,
                    notification_id,
                }
            });

            Self::upsert_state_tx(&tx, &state).map_err(AppError::Database)?;
            tx.commit().map_err(AppError::Database)?;
            Ok(notify)
        })
    }

    /// Persist the Webhook outcome and update notification state atomically.
    /// Returns false when the reservation is stale or the rule was deleted.
    pub fn finish_notification_delivery(
        &self,
        event: &AlertNotifyEvent,
        delivered: bool,
        message: &str,
        retry_count: u32,
        completed_at: u128,
    ) -> AppResult<bool> {
        self.with_conn_mut(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(AppError::Database)?;
            let Some(rule) = Self::load_rule_tx(&tx, &event.rule_id).map_err(AppError::Database)?
            else {
                tx.commit().map_err(AppError::Database)?;
                return Ok(false);
            };
            let mut state = Self::load_state_tx(&tx, &event.rule_id).map_err(AppError::Database)?;
            let reservation_matches = rule.enabled
                && state.pending_notification_kind.as_deref() == Some(event.kind.as_str())
                && state.pending_notification_id == Some(event.notification_id);

            if reservation_matches {
                Self::insert_delivery_tx(
                    &tx,
                    &event.rule_id,
                    completed_at,
                    if delivered { "success" } else { "failed" },
                    message,
                    retry_count,
                )
                .map_err(AppError::Database)?;
                Self::clear_pending_notification(&mut state);
                if delivered {
                    match event.kind.as_str() {
                        "firing" => {
                            state.notification_active = true;
                            state.cooldown_until_ms = if rule.silence_period == 0 {
                                None
                            } else {
                                Some(completed_at.saturating_add(
                                    (rule.silence_period as u128).saturating_mul(1000),
                                ))
                            };
                            state.retry_notification_kind = (state.status != ALERT_STATUS_FIRING)
                                .then(|| "recovered".to_string());
                        }
                        "recovered" => {
                            state.notification_active = false;
                            state.recovery_count = state.recovery_count.saturating_add(1);
                            state.retry_notification_kind =
                                (state.status != ALERT_STATUS_NORMAL).then(|| "firing".to_string());
                        }
                        _ => {
                            state.retry_notification_kind = None;
                        }
                    }
                } else {
                    // The reservation owner may finish after the condition has changed
                    // (for example, a firing Webhook can fail after recovery). Preserve
                    // that exact notification kind for a future matching edge; the
                    // failed attempt never activates the notification or starts cooldown.
                    state.retry_notification_kind = Some(event.kind.clone());
                }
                Self::upsert_state_tx(&tx, &state).map_err(AppError::Database)?;
            }

            self.commit_delivery_tx(tx, completed_at, false)?;
            Ok(reservation_matches)
        })
    }

    /// Renew reservations owned by the in-process delivery runtime.
    ///
    /// A runtime job can remain queued longer than the stale-reservation lease. Only
    /// an exact reservation token may be renewed, so a restarted process or a stale
    /// completion cannot extend another job's ownership.
    pub(crate) fn renew_notification_leases(
        &self,
        events: &[AlertNotifyEvent],
        renewed_at: u128,
    ) -> AppResult<usize> {
        if events.is_empty() {
            return Ok(0);
        }
        self.with_conn_mut(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(AppError::Database)?;
            let mut renewed = 0;
            for event in events {
                let changed = tx.execute(
                    "UPDATE alert_states
                         SET pending_since_ms = ?1
                         WHERE rule_id = ?2
                           AND pending_notification_kind = ?3
                           AND pending_notification_id = ?4",
                    params![
                        renewed_at as i64,
                        event.rule_id,
                        event.kind,
                        event.notification_id as i64,
                    ],
                )?;
                renewed += changed;
            }
            tx.commit().map_err(AppError::Database)?;
            Ok(renewed)
        })
    }

    /// Finalize runtime-owned reservations as failed in one transaction.
    ///
    /// This is used after the bounded shutdown deadline. It clears only matching
    /// reservation tokens and preserves retry semantics without activating a
    /// notification or starting cooldown.
    pub(crate) fn fail_notification_deliveries(
        &self,
        events: &[AlertNotifyEvent],
        message: &str,
        retry_count: u32,
        completed_at: u128,
    ) -> AppResult<usize> {
        if events.is_empty() {
            return Ok(0);
        }
        self.with_conn_mut(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(AppError::Database)?;
            let mut finalized = 0;
            for event in events {
                if Self::load_rule_tx(&tx, &event.rule_id)
                    .map_err(AppError::Database)?
                    .is_none()
                {
                    continue;
                }
                let mut state =
                    Self::load_state_tx(&tx, &event.rule_id).map_err(AppError::Database)?;
                let matches = state.pending_notification_kind.as_deref()
                    == Some(event.kind.as_str())
                    && state.pending_notification_id == Some(event.notification_id);
                if !matches {
                    continue;
                }
                Self::insert_delivery_tx(
                    &tx,
                    &event.rule_id,
                    completed_at,
                    "failed",
                    message,
                    retry_count,
                )
                .map_err(AppError::Database)?;
                Self::clear_pending_notification(&mut state);
                state.retry_notification_kind = Some(event.kind.clone());
                Self::upsert_state_tx(&tx, &state).map_err(AppError::Database)?;
                finalized += 1;
            }
            self.commit_delivery_tx(tx, completed_at, false)?;
            Ok(finalized)
        })
    }

    /// Compatibility wrapper used by tests and manual domain callers. It treats
    /// the produced event as synchronously delivered successfully.
    pub fn trigger_alert(&self, rule_id: &str) -> AppResult<bool> {
        match self.evaluate_condition(rule_id, true, None)? {
            Some(event) if event.kind == "firing" => {
                self.finish_notification_delivery(&event, true, "alert triggered", 0, now_ms())
            }
            _ => Ok(false),
        }
    }

    /// Compatibility wrapper used by tests and manual domain callers. It treats
    /// the produced recovery event as synchronously delivered successfully.
    pub fn mark_recovered(&self, rule_id: &str) -> AppResult<bool> {
        match self.evaluate_condition(rule_id, false, None)? {
            Some(event) if event.kind == "recovered" => {
                self.finish_notification_delivery(&event, true, "alert recovered", 0, now_ms())
            }
            _ => Ok(false),
        }
    }

    pub fn get_alert_state(&self, rule_id: &str) -> AppResult<AlertState> {
        self.with_conn(|conn| Self::load_state_tx(conn, rule_id))
    }

    fn load_state_tx(conn: &Connection, rule_id: &str) -> SqlResult<AlertState> {
        let state: Option<AlertState> = conn
            .query_row(
                "SELECT rule_id, last_triggered_ms, is_silent, silent_until_ms, recovery_count,
                        status, last_recovered_ms, cooldown_until_ms, notification_active,
                        pending_notification_kind, pending_notification_id, pending_since_ms,
                        retry_notification_kind, notification_sequence
                 FROM alert_states WHERE rule_id = ?1",
                params![rule_id],
                |row| {
                    Ok(AlertState {
                        rule_id: row.get(0)?,
                        last_triggered_ms: row.get::<_, Option<i64>>(1)?.map(|v| v.max(0) as u128),
                        is_silent: row.get::<_, i64>(2)? != 0,
                        silent_until_ms: row.get::<_, Option<i64>>(3)?.map(|v| v.max(0) as u128),
                        recovery_count: row.get::<_, i64>(4)?.max(0) as u32,
                        status: row
                            .get::<_, Option<String>>(5)?
                            .unwrap_or_else(|| ALERT_STATUS_NORMAL.to_string()),
                        last_recovered_ms: row.get::<_, Option<i64>>(6)?.map(|v| v.max(0) as u128),
                        cooldown_until_ms: row.get::<_, Option<i64>>(7)?.map(|v| v.max(0) as u128),
                        notification_active: row.get::<_, Option<i64>>(8)?.unwrap_or(0) != 0,
                        pending_notification_kind: row.get(9)?,
                        pending_notification_id: row
                            .get::<_, Option<i64>>(10)?
                            .map(|value| value.max(0) as u64),
                        pending_since_ms: row
                            .get::<_, Option<i64>>(11)?
                            .map(|value| value.max(0) as u128),
                        retry_notification_kind: row.get(12)?,
                        notification_sequence: row.get::<_, Option<i64>>(13)?.unwrap_or(0).max(0)
                            as u64,
                    })
                },
            )
            .optional()?;
        Ok(state.unwrap_or_else(|| AlertState {
            rule_id: rule_id.to_string(),
            status: ALERT_STATUS_NORMAL.to_string(),
            last_triggered_ms: None,
            last_recovered_ms: None,
            is_silent: false,
            silent_until_ms: None,
            cooldown_until_ms: None,
            notification_active: false,
            recovery_count: 0,
            pending_notification_kind: None,
            pending_notification_id: None,
            pending_since_ms: None,
            retry_notification_kind: None,
            notification_sequence: 0,
        }))
    }

    fn upsert_state_tx(conn: &Connection, state: &AlertState) -> SqlResult<()> {
        conn.execute(
            "INSERT INTO alert_states (
                rule_id, last_triggered_ms, is_silent, silent_until_ms, recovery_count,
                status, last_recovered_ms, cooldown_until_ms, notification_active,
                pending_notification_kind, pending_notification_id, pending_since_ms,
                retry_notification_kind, notification_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(rule_id) DO UPDATE SET
               last_triggered_ms = excluded.last_triggered_ms,
               is_silent = excluded.is_silent,
               silent_until_ms = excluded.silent_until_ms,
               recovery_count = excluded.recovery_count,
               status = excluded.status,
               last_recovered_ms = excluded.last_recovered_ms,
               cooldown_until_ms = excluded.cooldown_until_ms,
               notification_active = excluded.notification_active,
               pending_notification_kind = excluded.pending_notification_kind,
               pending_notification_id = excluded.pending_notification_id,
               pending_since_ms = excluded.pending_since_ms,
               retry_notification_kind = excluded.retry_notification_kind,
               notification_sequence = excluded.notification_sequence",
            params![
                state.rule_id,
                state.last_triggered_ms.map(|v| v as i64),
                state.is_silent as u8,
                state.silent_until_ms.map(|v| v as i64),
                state.recovery_count as i64,
                state.status,
                state.last_recovered_ms.map(|v| v as i64),
                state.cooldown_until_ms.map(|v| v as i64),
                state.notification_active as u8,
                state.pending_notification_kind,
                state.pending_notification_id.map(|value| value as i64),
                state.pending_since_ms.map(|value| value as i64),
                state.retry_notification_kind,
                state.notification_sequence as i64,
            ],
        )?;
        Ok(())
    }

    /// Manual silence only. Does not touch cooldown or firing status.
    pub fn update_silence(
        &self,
        rule_id: &str,
        silent: bool,
        until_ms: Option<u128>,
    ) -> AppResult<()> {
        if !self.rule_exists(rule_id)? {
            return Err(AppError::Validation("alert rule not found".to_string()));
        }
        self.with_conn_mut(|conn| {
            let mut state = Self::load_state_tx(conn, rule_id).map_err(AppError::Database)?;
            state.is_silent = silent;
            state.silent_until_ms = until_ms;
            Self::upsert_state_tx(conn, &state).map_err(AppError::Database)?;
            Ok(())
        })
    }

    fn map_rule_row(row: &rusqlite::Row<'_>) -> SqlResult<AlertRule> {
        Ok(AlertRule {
            id: row.get(0)?,
            name: row.get(1)?,
            enabled: row.get::<_, i64>(2)? != 0,
            rule_config: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            threshold: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            silence_period: row.get::<_, Option<i64>>(5)?.unwrap_or(0).max(0) as u64,
            created_at: row.get::<_, Option<i64>>(6)?.unwrap_or(0).max(0) as u128,
            metric: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
            operator: {
                let op = row.get::<_, Option<String>>(8)?.unwrap_or_default();
                if op.trim().is_empty() {
                    DEFAULT_OPERATOR.to_string()
                } else {
                    op
                }
            },
            server_id: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
            evaluation_window_secs: row.get::<_, Option<i64>>(10)?.unwrap_or(0).max(0) as u64,
        })
    }

    fn map_rule_view_row(row: &rusqlite::Row<'_>) -> SqlResult<AlertRuleView> {
        let stored_rule_config = row.get::<_, Option<String>>(3)?.unwrap_or_default();
        Ok(AlertRuleView {
            id: row.get(0)?,
            name: row.get(1)?,
            enabled: row.get::<_, i64>(2)? != 0,
            rule_configured: !stored_rule_config.trim().is_empty(),
            threshold: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
            silence_period: row.get::<_, Option<i64>>(5)?.unwrap_or(0).max(0) as u64,
            created_at: row.get::<_, Option<i64>>(6)?.unwrap_or(0).max(0) as u128,
            metric: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
            operator: {
                let op = row.get::<_, Option<String>>(8)?.unwrap_or_default();
                if op.trim().is_empty() {
                    DEFAULT_OPERATOR.to_string()
                } else {
                    op
                }
            },
            server_id: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
            evaluation_window_secs: row.get::<_, Option<i64>>(10)?.unwrap_or(0).max(0) as u64,
        })
    }

    const RULE_SELECT: &'static str =
        "SELECT id, name, enabled, rule_config, threshold, silence_period, created_at,
            metric, operator, server_id, evaluation_window_secs
     FROM alert_rules";

    const RULE_VIEW_SELECT: &'static str =
        "SELECT id, name, enabled, rule_config, threshold, silence_period, created_at,
            metric, operator, server_id, evaluation_window_secs
     FROM alert_rules";

    fn load_rule_tx(conn: &Connection, rule_id: &str) -> SqlResult<Option<AlertRule>> {
        conn.query_row(
            &format!("{} WHERE id = ?1", Self::RULE_SELECT),
            params![rule_id],
            Self::map_rule_row,
        )
        .optional()
    }

    pub fn get_alert_rules(&self) -> AppResult<Vec<AlertRuleView>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "{} ORDER BY created_at DESC, id ASC",
                Self::RULE_VIEW_SELECT
            ))?;
            let rules = stmt
                .query_map([], Self::map_rule_view_row)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rules)
        })
    }

    pub fn get_alert_rule(&self, rule_id: &str) -> AppResult<Option<AlertRuleView>> {
        self.with_conn(|conn| {
            conn.query_row(
                &format!("{} WHERE id = ?1", Self::RULE_VIEW_SELECT),
                params![rule_id],
                Self::map_rule_view_row,
            )
            .optional()
        })
    }

    pub fn create_alert_rule(&self, rule: AlertRule) -> AppResult<()> {
        let rule = Self::normalize_rule(rule)?;
        Self::validate_rule_fields(&rule)?;
        let stored_rule_config = self.encrypt_rule_config_for_storage(&rule.rule_config)?;
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO alert_rules (
                    id, name, enabled, rule_config, threshold, silence_period, created_at,
                    metric, operator, server_id, evaluation_window_secs
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    rule.id,
                    rule.name,
                    rule.enabled as u8,
                    stored_rule_config,
                    rule.threshold,
                    rule.silence_period as i64,
                    rule.created_at as i64,
                    rule.metric,
                    rule.operator,
                    rule.server_id,
                    rule.evaluation_window_secs as i64,
                ],
            )?;
            Ok(())
        })
    }

    pub fn update_alert_rule(
        &self,
        mut rule: AlertRule,
        rule_config_update: RuleConfigUpdate,
    ) -> AppResult<()> {
        let replace_rule_config = match rule_config_update {
            RuleConfigUpdate::Preserve => {
                rule.rule_config.clear();
                false
            }
            RuleConfigUpdate::Replace(value) => {
                rule.rule_config = value;
                true
            }
            RuleConfigUpdate::Clear => {
                rule.rule_config.clear();
                true
            }
        };
        let rule = Self::normalize_rule(rule)?;
        Self::validate_rule_fields(&rule)?;
        let stored_rule_config = if replace_rule_config {
            Some(self.encrypt_rule_config_for_storage(&rule.rule_config)?)
        } else {
            None
        };

        self.with_conn_mut(|conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(AppError::Database)?;
            let existing = Self::load_rule_tx(&tx, &rule.id)
                .map_err(AppError::Database)?
                .ok_or_else(|| AppError::Validation("alert rule not found".to_string()))?;

            let eval_changed = existing.metric != rule.metric
                || existing.operator != rule.operator
                || existing.threshold != rule.threshold
                || existing.server_id != rule.server_id
                || existing.evaluation_window_secs != rule.evaluation_window_secs;

            match stored_rule_config {
                Some(stored_rule_config) => {
                    tx.execute(
                        "UPDATE alert_rules
                         SET name = ?1, enabled = ?2, rule_config = ?3, threshold = ?4,
                             silence_period = ?5, metric = ?6, operator = ?7, server_id = ?8,
                             evaluation_window_secs = ?9
                         WHERE id = ?10",
                        params![
                            rule.name,
                            rule.enabled as u8,
                            stored_rule_config,
                            rule.threshold,
                            rule.silence_period as i64,
                            rule.metric,
                            rule.operator,
                            rule.server_id,
                            rule.evaluation_window_secs as i64,
                            rule.id,
                        ],
                    )
                    .map_err(AppError::Database)?;
                }
                None => {
                    tx.execute(
                        "UPDATE alert_rules
                         SET name = ?1, enabled = ?2, threshold = ?3, silence_period = ?4,
                             metric = ?5, operator = ?6, server_id = ?7,
                             evaluation_window_secs = ?8
                         WHERE id = ?9",
                        params![
                            rule.name,
                            rule.enabled as u8,
                            rule.threshold,
                            rule.silence_period as i64,
                            rule.metric,
                            rule.operator,
                            rule.server_id,
                            rule.evaluation_window_secs as i64,
                            rule.id,
                        ],
                    )
                    .map_err(AppError::Database)?;
                }
            }

            if eval_changed {
                Self::reset_state_tx(&tx, &rule.id).map_err(AppError::Database)?;
            } else if !rule.enabled {
                // Disabling via full update: drop episode without recovered.
                let mut state = Self::load_state_tx(&tx, &rule.id).map_err(AppError::Database)?;
                if state.status == ALERT_STATUS_FIRING
                    || state.notification_active
                    || state.cooldown_until_ms.is_some()
                    || state.pending_notification_kind.is_some()
                    || state.retry_notification_kind.is_some()
                {
                    state.status = ALERT_STATUS_NORMAL.to_string();
                    state.notification_active = false;
                    state.cooldown_until_ms = None;
                    state.retry_notification_kind = None;
                    Self::clear_pending_notification(&mut state);
                    Self::upsert_state_tx(&tx, &state).map_err(AppError::Database)?;
                }
            }
            if rule.enabled
                && !eval_changed
                && (replace_rule_config
                    || existing.name != rule.name
                    || existing.silence_period != rule.silence_period)
            {
                // Invalidate queued payloads without discarding an already delivered episode.
                let mut state = Self::load_state_tx(&tx, &rule.id).map_err(AppError::Database)?;
                if let Some(kind) = state.pending_notification_kind.clone() {
                    state.retry_notification_kind = Some(kind);
                    Self::clear_pending_notification(&mut state);
                    Self::upsert_state_tx(&tx, &state).map_err(AppError::Database)?;
                }
            }
            tx.commit().map_err(AppError::Database)?;
            Ok(())
        })
    }

    fn reset_state_tx(conn: &Connection, rule_id: &str) -> SqlResult<()> {
        let mut state = Self::load_state_tx(conn, rule_id)?;
        state.status = ALERT_STATUS_NORMAL.to_string();
        state.notification_active = false;
        state.cooldown_until_ms = None;
        state.retry_notification_kind = None;
        Self::clear_pending_notification(&mut state);
        // Keep recovery_count / last_* as audit trail; drop active episode only.
        Self::upsert_state_tx(conn, &state)
    }

    pub fn delete_rule(&self, rule_id: &str) -> AppResult<()> {
        if !self.rule_exists(rule_id)? {
            return Err(AppError::Validation("alert rule not found".to_string()));
        }
        self.with_conn(|conn| {
            conn.execute("DELETE FROM alert_rules WHERE id = ?1", params![rule_id])?;
            conn.execute(
                "DELETE FROM alert_states WHERE rule_id = ?1",
                params![rule_id],
            )?;
            conn.execute(
                "DELETE FROM alert_deliveries WHERE rule_id = ?1",
                params![rule_id],
            )?;
            Ok(())
        })
    }

    pub fn toggle_rule(&self, rule_id: &str, enabled: bool) -> AppResult<()> {
        self.with_conn_mut(|conn| {
            let updated = conn
                .execute(
                    "UPDATE alert_rules SET enabled = ?1 WHERE id = ?2",
                    params![enabled as u8, rule_id],
                )
                .map_err(AppError::Database)?;
            if updated == 0 {
                return Err(AppError::Validation("alert rule not found".to_string()));
            }
            if !enabled {
                let mut state = Self::load_state_tx(conn, rule_id).map_err(AppError::Database)?;
                if state.status == ALERT_STATUS_FIRING
                    || state.notification_active
                    || state.cooldown_until_ms.is_some()
                    || state.pending_notification_kind.is_some()
                    || state.retry_notification_kind.is_some()
                {
                    state.status = ALERT_STATUS_NORMAL.to_string();
                    state.notification_active = false;
                    state.cooldown_until_ms = None;
                    state.retry_notification_kind = None;
                    Self::clear_pending_notification(&mut state);
                    Self::upsert_state_tx(conn, &state).map_err(AppError::Database)?;
                }
            }
            Ok(())
        })
    }

    /// Filter authorized rules before applying the history limit; an empty list returns nothing.
    pub fn list_authorized_deliveries(
        &self,
        allowed_rule_ids: &[String],
        limit: usize,
    ) -> AppResult<Vec<AlertDelivery>> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, rule_id, timestamp_ms, status, retry_count, message
                 FROM alert_deliveries WHERE rule_id IN (SELECT value FROM json_each(?1))
                 ORDER BY timestamp_ms DESC, id DESC LIMIT ?2",
            )?;
            let ids = serde_json::to_string(allowed_rule_ids).expect("string list");
            let rows = stmt.query_map(
                params![ids, limit.clamp(1, MAX_DELIVERY_LIMIT) as i64],
                |row| {
                    Ok(AlertDelivery {
                        id: row.get(0)?,
                        rule_id: row.get(1)?,
                        timestamp_ms: row.get::<_, i64>(2)?.max(0) as u128,
                        status: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                        retry_count: row.get::<_, Option<i64>>(4)?.unwrap_or(0).max(0) as u32,
                        message: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    })
                },
            )?;
            rows.collect()
        })
    }

    pub(crate) fn notification_is_current(&self, event: &AlertNotifyEvent) -> AppResult<bool> {
        self.with_conn(|conn| {
            let Some(rule) = Self::load_rule_tx(conn, &event.rule_id)? else {
                return Ok(false);
            };
            let state = Self::load_state_tx(conn, &event.rule_id)?;
            Ok(rule.enabled
                && state.pending_notification_id == Some(event.notification_id)
                && state.pending_notification_kind.as_deref() == Some(event.kind.as_str()))
        })
    }

    pub(crate) fn prune_delivery_history(&self) -> AppResult<()> {
        self.with_conn_mut(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            self.commit_delivery_tx(tx, now_ms(), true)
        })
    }

    fn commit_delivery_tx(
        &self,
        tx: Transaction<'_>,
        now: u128,
        force_prune: bool,
    ) -> AppResult<()> {
        // The DB mutex is already held. Clones share this timestamp, and batches
        // prune only once after all inserts, avoiding history scans on normal writes.
        let mut last_pruned = self
            .last_delivery_history_prune
            .lock()
            .map_err(|_| AppError::Internal("alerts maintenance mutex is poisoned".to_string()))?;
        let prune = force_prune
            || last_pruned.is_none_or(|last| last.elapsed() >= DELIVERY_HISTORY_PRUNE_INTERVAL);
        if prune {
            Self::prune_deliveries_tx(&tx, now)?;
        }
        tx.commit()?;
        // A failed prune or commit must leave maintenance due for the next write.
        if prune {
            *last_pruned = Some(Instant::now());
        }
        Ok(())
    }

    fn prune_deliveries_tx(conn: &Connection, now: u128) -> SqlResult<()> {
        conn.execute(
            "DELETE FROM alert_deliveries WHERE timestamp_ms < ?1",
            params![now.saturating_sub(90 * 24 * 60 * 60 * 1000) as i64],
        )?;
        conn.execute("DELETE FROM alert_deliveries WHERE id IN (
            SELECT id FROM alert_deliveries ORDER BY timestamp_ms DESC, id DESC LIMIT -1 OFFSET 20000)", [])?;
        Ok(())
    }

    pub fn list_alert_deliveries(
        &self,
        rule_id: Option<&str>,
        limit: usize,
    ) -> AppResult<Vec<AlertDelivery>> {
        let limit = limit.clamp(1, MAX_DELIVERY_LIMIT) as i64;
        self.with_conn(|conn| {
            let map_row = |row: &rusqlite::Row<'_>| {
                Ok(AlertDelivery {
                    id: row.get(0)?,
                    rule_id: row.get(1)?,
                    timestamp_ms: row.get::<_, i64>(2)?.max(0) as u128,
                    status: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    retry_count: row.get::<_, Option<i64>>(4)?.unwrap_or(0).max(0) as u32,
                    message: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                })
            };
            match rule_id {
                Some(id) => {
                    let mut stmt = conn.prepare(
                        "SELECT id, rule_id, timestamp_ms, status, retry_count, message
                         FROM alert_deliveries
                         WHERE rule_id = ?1
                         ORDER BY timestamp_ms DESC, id DESC
                         LIMIT ?2",
                    )?;
                    let deliveries = stmt
                        .query_map(params![id, limit], map_row)?
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(deliveries)
                }
                None => {
                    let mut stmt = conn.prepare(
                        "SELECT id, rule_id, timestamp_ms, status, retry_count, message
                         FROM alert_deliveries
                         ORDER BY timestamp_ms DESC, id DESC
                         LIMIT ?1",
                    )?;
                    let deliveries = stmt
                        .query_map(params![limit], map_row)?
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(deliveries)
                }
            }
        })
    }

    /// Parse webhook URL from plain URL or JSON `{"url":"..."}`.
    ///
    /// Security boundary (admin-configured webhook threat model):
    /// - only `http` / `https`
    /// - reject userinfo / credentials
    /// - reject empty / malformed host
    /// - private / loopback / public hosts are allowed for authenticated admins
    ///   (intentional; not a claim of "no SSRF")
    pub fn parse_webhook_url(rule_config: &str) -> AppResult<Option<String>> {
        let trimmed = rule_config.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if trimmed.len() > MAX_RULE_CONFIG_LEN {
            return Err(AppError::Validation("rule config too long".to_string()));
        }
        reject_control_chars("rule config", trimmed)?;

        let json_config: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(_) => return Self::validate_url(trimmed),
        };
        let url_str = json_config
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AppError::Validation("rule_config must contain 'url' field".to_string())
            })?;
        Self::validate_url(url_str)
    }

    pub fn validate_url(url_str: &str) -> AppResult<Option<String>> {
        let trimmed = url_str.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if trimmed.len() > MAX_RULE_CONFIG_LEN {
            return Err(AppError::Validation("webhook URL too long".to_string()));
        }
        reject_control_chars("webhook URL", trimmed)?;

        let url = Url::parse(trimmed)
            .map_err(|_| AppError::Validation("invalid webhook URL".to_string()))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(AppError::Validation(
                "webhook URL scheme must be http or https".to_string(),
            ));
        }
        let host = url
            .host_str()
            .ok_or_else(|| AppError::Validation("webhook URL host is required".to_string()))?;
        if host.is_empty() {
            return Err(AppError::Validation(
                "webhook URL host is required".to_string(),
            ));
        }
        if host.contains(' ') {
            return Err(AppError::Validation(
                "webhook URL host is invalid".to_string(),
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(AppError::Validation(
                "userinfo not allowed in webhook URL".to_string(),
            ));
        }
        Ok(Some(url.to_string()))
    }

    /// Read and decrypt rule configuration for the outbound delivery boundary only.
    ///
    /// Public rule queries never call this method and never decrypt at-rest values. A
    /// non-empty value that is not valid ciphertext is rejected rather than treated as
    /// plaintext, so the stored envelope cannot reach webhook URL parsing.
    pub(crate) fn get_rule_webhook_url(&self, rule_id: &str) -> AppResult<Option<String>> {
        let stored_rule_config = self.with_conn(|conn| {
            conn.query_row(
                "SELECT rule_config FROM alert_rules WHERE id = ?1",
                params![rule_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
        })?;
        let stored_rule_config = stored_rule_config
            .ok_or_else(|| AppError::Validation("alert rule not found".to_string()))?
            .unwrap_or_default();
        if stored_rule_config.trim().is_empty() {
            return Ok(None);
        }
        let key = self.current_rule_config_key()?;
        let plaintext = rule_config_crypto::decrypt_rule_config(&stored_rule_config, &key)
            .map_err(|_| Self::rule_config_storage_unavailable())?;
        if plaintext.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(plaintext))
    }

    fn insert_delivery_tx(
        conn: &Connection,
        rule_id: &str,
        timestamp_ms: u128,
        status: &str,
        message: &str,
        retry_count: u32,
    ) -> SqlResult<()> {
        let safe_message = sanitize_delivery_message(message);
        conn.execute(
            "INSERT INTO alert_deliveries (rule_id, timestamp_ms, status, retry_count, message)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                rule_id,
                timestamp_ms as i64,
                status,
                retry_count.min(MAX_RETRY_COUNT) as i64,
                safe_message,
            ],
        )?;
        Ok(())
    }

    pub fn record_delivery(
        &self,
        rule_id: &str,
        status: &str,
        message: &str,
        retry_count: u32,
    ) -> AppResult<()> {
        if !self.rule_exists(rule_id)? {
            return Ok(());
        }
        self.with_conn_mut(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let now = now_ms();
            Self::insert_delivery_tx(&tx, rule_id, now, status, message, retry_count)?;
            self.commit_delivery_tx(tx, now, false)
        })
    }

    pub fn record_test_delivery(
        &self,
        rule_id: &str,
        status: &str,
        message: &str,
    ) -> AppResult<()> {
        self.record_delivery(rule_id, status, message, 0)
    }

    pub fn rule_exists(&self, rule_id: &str) -> AppResult<bool> {
        self.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT 1 FROM alert_rules WHERE id = ?1 LIMIT 1")?;
            let found = stmt
                .query_row(params![rule_id], |_| Ok(1_i64))
                .optional()?
                .is_some();
            Ok(found)
        })
    }

    pub fn normalize_rule(mut rule: AlertRule) -> AppResult<AlertRule> {
        rule.rule_config = rule.rule_config.trim().to_string();
        rule.metric = Self::normalize_metric(&rule.metric)?;
        rule.operator = Self::normalize_operator(&rule.operator)?;
        rule.server_id = Self::normalize_server_id(&rule.server_id)?;
        rule.evaluation_window_secs =
            Self::normalize_evaluation_window_secs(rule.evaluation_window_secs)?;
        if !rule.metric.is_empty() {
            let value = Self::parse_numeric_threshold(&rule.threshold)?;
            rule.threshold = Self::format_threshold_value(value);
        } else {
            rule.threshold = rule.threshold.trim().to_string();
            if rule.threshold.len() > MAX_THRESHOLD_LEN {
                return Err(AppError::Validation("threshold too long".to_string()));
            }
            reject_control_chars("threshold", &rule.threshold)?;
        }
        Ok(rule)
    }

    pub fn validate_rule_fields(rule: &AlertRule) -> AppResult<()> {
        if rule.id.is_empty() || rule.id.len() > MAX_RULE_ID_LEN {
            return Err(AppError::Validation("invalid rule id".to_string()));
        }
        reject_control_chars("rule id", &rule.id)?;
        if rule.name.is_empty() || rule.name.len() > MAX_RULE_NAME_LEN {
            return Err(AppError::Validation(
                "rule name is required and must be <= 128 chars".to_string(),
            ));
        }
        reject_control_chars("rule name", &rule.name)?;
        if rule.rule_config.len() > MAX_RULE_CONFIG_LEN {
            return Err(AppError::Validation("rule config too long".to_string()));
        }
        reject_control_chars("rule config", &rule.rule_config)?;
        if rule.threshold.len() > MAX_THRESHOLD_LEN {
            return Err(AppError::Validation("threshold too long".to_string()));
        }
        reject_control_chars("threshold", &rule.threshold)?;
        if rule.silence_period > MAX_SILENCE_PERIOD_SECS {
            return Err(AppError::Validation(
                "silence_period exceeds maximum".to_string(),
            ));
        }
        // Re-validate normalized metric fields (callers should normalize first).
        let _ = Self::normalize_metric(&rule.metric)?;
        let _ = Self::normalize_operator(&rule.operator)?;
        let _ = Self::normalize_server_id(&rule.server_id)?;
        let _ = Self::normalize_evaluation_window_secs(rule.evaluation_window_secs)?;
        if !rule.metric.is_empty() {
            let _ = Self::parse_numeric_threshold(&rule.threshold)?;
        }
        if !rule.rule_config.trim().is_empty() {
            let _ = Self::parse_webhook_url(&rule.rule_config)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_db() -> (String, Alerts) {
        let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/alerts_domain_test_{}_{}.db", std::process::id(), n);
        let _ = std::fs::remove_file(&path);
        let alerts = Alerts::new(&path).expect("open alerts db");
        (path, alerts)
    }

    fn temp_rule_config_db(label: &str) -> std::path::PathBuf {
        let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "alerts_rule_config_{label}_{}_{}.db",
            std::process::id(),
            n
        ));
        let _ = std::fs::remove_file(rule_config_crypto::key_path(&path));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn cleanup_rule_config_db(path: &Path) {
        let _ = std::fs::remove_file(rule_config_crypto::key_path(path));
        let _ = std::fs::remove_file(path);
    }

    fn insert_rule_config_fixture(path: &Path, id: &str, rule_config: &str) {
        insert_nullable_rule_config_fixture(path, id, Some(rule_config));
    }

    fn insert_nullable_rule_config_fixture(path: &Path, id: &str, rule_config: Option<&str>) {
        let conn = Connection::open(path).expect("open alert rule fixture db");
        conn.execute(
            r#"
            INSERT INTO alert_rules (
                id, name, enabled, rule_config, threshold, silence_period, created_at
            ) VALUES (?1, 'fixture', 1, ?2, '1', 0, 1)
            "#,
            params![id, rule_config],
        )
        .expect("insert alert rule fixture");
    }

    fn insert_encrypted_rule_config_fixture(path: &Path) -> [u8; 32] {
        let initialized = Alerts::new(path).unwrap();
        let existing_key = initialized.ensure_rule_config_key().unwrap();
        let ciphertext = rule_config_crypto::encrypt_rule_config(
            "https://example.com/encrypted-hook",
            &existing_key,
        )
        .unwrap();
        drop(initialized);
        insert_rule_config_fixture(path, "encrypted", &ciphertext);
        existing_key
    }

    fn stored_rule_config_value(path: &Path, id: &str) -> Option<String> {
        let conn = Connection::open(path).expect("open alert rule fixture db");
        conn.query_row(
            "SELECT rule_config FROM alert_rules WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .expect("load alert rule fixture")
    }

    fn stored_rule_config(path: &Path, id: &str) -> String {
        stored_rule_config_value(path, id).expect("alert rule fixture config is null")
    }

    fn assert_rule_config_storage_unavailable<T>(result: AppResult<T>) {
        match result {
            Err(AppError::Internal(message)) => {
                assert_eq!(message, "alert rule configuration storage is unavailable")
            }
            Err(other) => panic!("unexpected alert storage error: {other}"),
            Ok(_) => panic!("alert storage unexpectedly initialized"),
        }
    }

    #[test]
    fn connection_config_is_applied_to_existing_database_and_reopen() {
        let db_path = temp_rule_config_db("connection_config");
        let existing_conn = Connection::open(&db_path).unwrap();
        existing_conn
            .execute_batch(
                r#"
                CREATE TABLE existing_alert_fixture (
                    id INTEGER PRIMARY KEY,
                    value TEXT NOT NULL
                );
                INSERT INTO existing_alert_fixture (value) VALUES ('preserved');
                "#,
            )
            .unwrap();
        drop(existing_conn);

        for _ in 0..2 {
            let alerts = Alerts::new(&db_path).unwrap();
            let conn = alerts.db.lock().unwrap();
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
            let preserved: String = conn
                .query_row(
                    "SELECT value FROM existing_alert_fixture WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .unwrap();

            assert!(journal_mode.eq_ignore_ascii_case("wal"));
            assert_eq!(busy_timeout, DATABASE_BUSY_TIMEOUT.as_millis() as i64);
            assert_eq!(foreign_keys, 1);
            assert_eq!(synchronous, 1);
            assert_eq!(preserved, "preserved");
        }

        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn rule_config_key_empty_db_does_not_create_key() {
        let db_path = temp_rule_config_db("empty");
        let key_path = rule_config_crypto::key_path(&db_path);

        let first = Alerts::new(&db_path).unwrap();
        assert!(!key_path.exists());
        drop(first);

        let reopened = Alerts::new(&db_path).unwrap();
        assert!(!key_path.exists());
        drop(reopened);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn whitespace_only_rule_config_does_not_create_key() {
        let db_path = temp_rule_config_db("whitespace_only");
        let key_path = rule_config_crypto::key_path(&db_path);

        drop(Alerts::new(&db_path).unwrap());
        insert_rule_config_fixture(&db_path, "whitespace", "  \t\n  ");
        drop(Alerts::new(&db_path).unwrap());
        assert!(!key_path.exists());
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn empty_rule_configs_ignore_invalid_existing_key() {
        let db_path = temp_rule_config_db("unused_invalid_key");
        let key_path = rule_config_crypto::key_path(&db_path);
        let invalid_key = b"unused-invalid-key";
        std::fs::write(&key_path, invalid_key).unwrap();

        drop(Alerts::new(&db_path).unwrap());
        insert_nullable_rule_config_fixture(&db_path, "null", None);
        insert_rule_config_fixture(&db_path, "empty", "");
        insert_rule_config_fixture(&db_path, "whitespace", " \t\n\u{2003} ");
        let alerts = Alerts::new(&db_path).unwrap();
        assert_eq!(alerts.get_alert_rules().unwrap().len(), 3);
        for id in ["null", "empty", "whitespace"] {
            assert!(alerts.get_rule_webhook_url(id).unwrap().is_none());
        }
        assert_eq!(std::fs::read(&key_path).unwrap(), invalid_key);
        drop(alerts);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn empty_rule_config_does_not_create_key_on_write() {
        let db_path = temp_rule_config_db("empty_write");
        let key_path = rule_config_crypto::key_path(&db_path);
        let alerts = Alerts::new(&db_path).unwrap();
        let mut rule = sample_rule("empty-write");
        rule.rule_config.clear();

        alerts.create_alert_rule(rule).unwrap();
        let view = alerts.get_alert_rule("empty-write").unwrap().unwrap();
        for update in [
            RuleConfigUpdate::Preserve,
            RuleConfigUpdate::Replace(" \t ".into()),
            RuleConfigUpdate::Clear,
        ] {
            alerts
                .update_alert_rule(editable_rule(&view), update)
                .unwrap();
            assert!(!key_path.exists());
        }
        drop(alerts);
        drop(Alerts::new(&db_path).unwrap());
        assert!(!key_path.exists());
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn nonempty_rule_config_creates_key_on_first_write() {
        let db_path = temp_rule_config_db("nonempty_write");
        let key_path = rule_config_crypto::key_path(&db_path);
        let alerts = Alerts::new(&db_path).unwrap();

        let rule = sample_rule("nonempty-write");
        let expected = rule.rule_config.clone();
        alerts.create_alert_rule(rule).unwrap();
        assert!(key_path.is_file());
        assert_eq!(
            alerts.get_rule_webhook_url("nonempty-write").unwrap(),
            Some(expected.clone())
        );
        let original_key = std::fs::read(&key_path).unwrap();
        drop(alerts);

        let reopened = Alerts::new(&db_path).unwrap();
        assert_eq!(
            reopened.get_rule_webhook_url("nonempty-write").unwrap(),
            Some(expected)
        );
        assert_eq!(std::fs::read(&key_path).unwrap(), original_key);
        drop(reopened);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn first_webhook_update_creates_key_shared_with_clones() {
        let db_path = temp_rule_config_db("first_webhook_update");
        let key_path = rule_config_crypto::key_path(&db_path);
        let alerts = Alerts::new(&db_path).unwrap();
        let cloned = alerts.clone();
        let mut rule = sample_rule("first-update");
        rule.rule_config.clear();
        alerts.create_alert_rule(rule).unwrap();
        assert!(!key_path.exists());

        let view = alerts.get_alert_rule("first-update").unwrap().unwrap();
        let webhook = "https://example.invalid/first-update";
        cloned
            .update_alert_rule(
                editable_rule(&view),
                RuleConfigUpdate::Replace(webhook.into()),
            )
            .unwrap();
        assert!(key_path.is_file());
        assert_eq!(
            alerts
                .get_rule_webhook_url("first-update")
                .unwrap()
                .as_deref(),
            Some(webhook)
        );
        drop(cloned);
        drop(alerts);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn concurrent_first_webhook_writes_share_one_key() {
        let db_path = temp_rule_config_db("concurrent_lazy_key");
        let alerts = Alerts::new(&db_path).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let workers: Vec<_> = (0..4)
            .map(|index| {
                let alerts = alerts.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    alerts
                        .create_alert_rule(sample_rule(&format!("concurrent-{index}")))
                        .unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        drop(alerts);
        let reopened = Alerts::new(&db_path).unwrap();
        for index in 0..4 {
            assert_eq!(
                reopened
                    .get_rule_webhook_url(&format!("concurrent-{index}"))
                    .unwrap()
                    .as_deref(),
                Some("http://127.0.0.1:1")
            );
        }
        drop(reopened);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn plaintext_rule_config_is_migrated_with_stable_key() {
        let db_path = temp_rule_config_db("plaintext");
        let key_path = rule_config_crypto::key_path(&db_path);
        let plaintext = "https://example.com/plaintext-hook";

        drop(Alerts::new(&db_path).unwrap());
        insert_rule_config_fixture(&db_path, "plaintext", plaintext);
        assert!(!key_path.exists());

        let initialized = Alerts::new(&db_path).unwrap();
        let created_key = initialized.current_rule_config_key().unwrap();
        let stored = stored_rule_config(&db_path, "plaintext");
        assert!(key_path.is_file());
        assert!(stored.starts_with(rule_config_crypto::RULE_CONFIG_PREFIX));
        assert!(
            rule_config_crypto::decrypt_rule_config(&stored, &created_key).unwrap() == plaintext
        );
        assert!(
            initialized
                .get_rule_webhook_url("plaintext")
                .unwrap()
                .as_deref()
                == Some(plaintext)
        );
        drop(initialized);

        let reopened = Alerts::new(&db_path).unwrap();
        assert_eq!(reopened.current_rule_config_key().unwrap(), created_key);
        assert!(stored_rule_config(&db_path, "plaintext") == stored);
        drop(reopened);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn mixed_rule_configs_migrate_without_changing_ciphertext_or_empty_values() {
        let db_path = temp_rule_config_db("mixed");
        let initialized = Alerts::new(&db_path).unwrap();
        let key = initialized.ensure_rule_config_key().unwrap();
        let plaintext = "https://example.com/plaintext-hook";
        let existing_ciphertext =
            rule_config_crypto::encrypt_rule_config("https://example.com/existing-hook", &key)
                .unwrap();
        drop(initialized);

        insert_rule_config_fixture(&db_path, "plaintext", plaintext);
        insert_rule_config_fixture(&db_path, "encrypted", &existing_ciphertext);
        insert_nullable_rule_config_fixture(&db_path, "null", None);
        insert_rule_config_fixture(&db_path, "empty", "");
        insert_rule_config_fixture(&db_path, "whitespace", "  \t  ");

        drop(Alerts::new(&db_path).unwrap());

        let migrated = stored_rule_config(&db_path, "plaintext");
        assert!(migrated.starts_with(rule_config_crypto::RULE_CONFIG_PREFIX));
        assert!(rule_config_crypto::decrypt_rule_config(&migrated, &key).unwrap() == plaintext);
        assert!(stored_rule_config(&db_path, "encrypted") == existing_ciphertext);
        assert!(stored_rule_config_value(&db_path, "null").is_none());
        assert!(stored_rule_config(&db_path, "empty").is_empty());
        assert!(stored_rule_config(&db_path, "whitespace") == "  \t  ");
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn rule_config_migration_is_idempotent_across_reopen() {
        let db_path = temp_rule_config_db("idempotent");
        drop(Alerts::new(&db_path).unwrap());
        insert_rule_config_fixture(&db_path, "plaintext", "https://example.com/idempotent-hook");

        drop(Alerts::new(&db_path).unwrap());
        let first_ciphertext = stored_rule_config(&db_path, "plaintext");
        drop(Alerts::new(&db_path).unwrap());
        let second_ciphertext = stored_rule_config(&db_path, "plaintext");

        assert!(first_ciphertext == second_ciphertext);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn damaged_ciphertext_rolls_back_all_plaintext_migrations() {
        let db_path = temp_rule_config_db("damaged_ciphertext");
        let initialized = Alerts::new(&db_path).unwrap();
        initialized.ensure_rule_config_key().unwrap();
        drop(initialized);
        let plaintext = "https://example.com/pending-hook";
        let damaged = format!("{}:invalid", rule_config_crypto::RULE_CONFIG_PREFIX);
        insert_rule_config_fixture(&db_path, "plaintext", plaintext);
        insert_rule_config_fixture(&db_path, "damaged", &damaged);

        assert_rule_config_storage_unavailable(Alerts::new(&db_path));
        assert!(stored_rule_config(&db_path, "plaintext") == plaintext);
        assert!(stored_rule_config(&db_path, "damaged") == damaged);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn rule_config_key_ciphertext_db_loads_existing_key() {
        let db_path = temp_rule_config_db("ciphertext");
        let existing_key = insert_encrypted_rule_config_fixture(&db_path);

        let reopened = Alerts::new(&db_path).unwrap();
        assert_eq!(reopened.current_rule_config_key().unwrap(), existing_key);
        drop(reopened);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn rule_config_key_ciphertext_db_missing_key_fails_without_recreating() {
        let db_path = temp_rule_config_db("missing_key");
        let key_path = rule_config_crypto::key_path(&db_path);
        let _ = insert_encrypted_rule_config_fixture(&db_path);
        let original = stored_rule_config(&db_path, "encrypted");
        std::fs::remove_file(&key_path).unwrap();

        assert_rule_config_storage_unavailable(Alerts::new(&db_path));
        assert!(!key_path.exists());
        assert!(stored_rule_config(&db_path, "encrypted") == original);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn rule_config_key_ciphertext_db_invalid_key_fails_without_overwrite() {
        let db_path = temp_rule_config_db("invalid_key");
        let key_path = rule_config_crypto::key_path(&db_path);
        let _ = insert_encrypted_rule_config_fixture(&db_path);
        let original = stored_rule_config(&db_path, "encrypted");
        let invalid_key = b"short-key-bytes";
        std::fs::write(&key_path, invalid_key).unwrap();

        assert_rule_config_storage_unavailable(Alerts::new(&db_path));
        assert!(std::fs::read(&key_path).unwrap() == invalid_key);
        assert!(stored_rule_config(&db_path, "encrypted") == original);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn rule_config_key_mismatch_fails_without_overwriting_key_or_ciphertext() {
        let db_path = temp_rule_config_db("key_mismatch");
        let key_path = rule_config_crypto::key_path(&db_path);
        let _ = insert_encrypted_rule_config_fixture(&db_path);
        let original = stored_rule_config(&db_path, "encrypted");
        let mismatched_key = [0xA5_u8; 32];
        std::fs::write(&key_path, mismatched_key).unwrap();

        assert_rule_config_storage_unavailable(Alerts::new(&db_path));
        assert!(std::fs::read(&key_path).unwrap() == mismatched_key);
        assert!(stored_rule_config(&db_path, "encrypted") == original);
        cleanup_rule_config_db(&db_path);
    }

    fn sample_rule(id: &str) -> AlertRule {
        AlertRule {
            id: id.into(),
            name: "sample".into(),
            enabled: true,
            rule_config: "http://127.0.0.1:1".into(),
            threshold: "1".into(),
            silence_period: 30,
            created_at: 1_700_000_000_000,
            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        }
    }

    fn metric_rule(id: &str) -> AlertRule {
        AlertRule {
            id: id.into(),
            name: "metric".into(),
            enabled: true,
            rule_config: "http://127.0.0.1:1/hook".into(),
            threshold: "10".into(),
            silence_period: 60,
            created_at: 1_700_000_000_000,
            metric: "proxy.errors".into(),
            operator: "gte".into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        }
    }

    fn editable_rule(view: &AlertRuleView) -> AlertRule {
        AlertRule {
            id: view.id.clone(),
            name: view.name.clone(),
            enabled: view.enabled,
            rule_config: String::new(),
            threshold: view.threshold.clone(),
            silence_period: view.silence_period,
            created_at: view.created_at,
            metric: view.metric.clone(),
            operator: view.operator.clone(),
            server_id: view.server_id.clone(),
            evaluation_window_secs: view.evaluation_window_secs,
        }
    }

    #[test]
    fn create_encrypts_rule_config_and_public_views_are_redacted() {
        let db_path = temp_rule_config_db("create_domain");
        let alerts = Alerts::new(&db_path).unwrap();
        let plaintext = "https://hooks.example.invalid/notify?token=create-fixture";
        let mut rule = sample_rule("secure-create");
        rule.rule_config = format!("  {plaintext}  ");
        alerts.create_alert_rule(rule).unwrap();

        let stored = stored_rule_config(&db_path, "secure-create");
        assert!(stored.starts_with(rule_config_crypto::RULE_CONFIG_PREFIX));
        assert!(!stored.contains(plaintext));
        assert!(
            alerts
                .get_rule_webhook_url("secure-create")
                .unwrap()
                .as_deref()
                == Some(plaintext)
        );

        let detail = alerts
            .get_alert_rule("secure-create")
            .unwrap()
            .expect("created rule view");
        assert!(detail.rule_configured);
        let detail_json = serde_json::to_string(&detail).unwrap();
        assert!(!detail_json.contains("rule_config\""));
        assert!(!detail_json.contains(rule_config_crypto::RULE_CONFIG_PREFIX));
        assert!(!detail_json.contains(plaintext));
        assert!(detail_json.contains("\"rule_configured\":true"));
        assert_eq!(
            serde_json::to_value(&detail)
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            11
        );

        let list = alerts.get_alert_rules().unwrap();
        let list_json = serde_json::to_string(&list).unwrap();
        assert!(!list_json.contains("rule_config\""));
        assert!(!list_json.contains(rule_config_crypto::RULE_CONFIG_PREFIX));
        assert!(!list_json.contains(plaintext));

        let mut empty = sample_rule("empty-create");
        empty.rule_config = " \t ".to_string();
        alerts.create_alert_rule(empty).unwrap();
        assert!(stored_rule_config(&db_path, "empty-create").is_empty());
        assert!(
            alerts
                .get_rule_webhook_url("empty-create")
                .unwrap()
                .is_none()
        );
        assert!(
            !alerts
                .get_alert_rule("empty-create")
                .unwrap()
                .expect("empty rule view")
                .rule_configured
        );

        drop(alerts);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn update_rule_config_preserve_replace_clear_and_empty_replace() {
        let db_path = temp_rule_config_db("update_domain");
        let alerts = Alerts::new(&db_path).unwrap();
        let original_plaintext = "https://hooks.example.invalid/original?token=old-fixture";
        let replacement_plaintext = "https://hooks.example.invalid/replacement?token=new-fixture";
        let mut original = sample_rule("secure-update");
        original.rule_config = original_plaintext.to_string();
        alerts.create_alert_rule(original).unwrap();
        let original_stored = stored_rule_config(&db_path, "secure-update");

        let view = alerts.get_alert_rule("secure-update").unwrap().unwrap();
        let mut preserve = editable_rule(&view);
        preserve.name = "preserved-config".to_string();
        preserve.rule_config = "ignored by preserve".to_string();
        preserve.created_at = 1;
        alerts
            .update_alert_rule(preserve, RuleConfigUpdate::Preserve)
            .unwrap();
        assert!(stored_rule_config(&db_path, "secure-update") == original_stored);
        let preserved_view = alerts.get_alert_rule("secure-update").unwrap().unwrap();
        assert_eq!(preserved_view.name, "preserved-config");
        assert_eq!(preserved_view.created_at, 1_700_000_000_000);

        let mut replace = editable_rule(&preserved_view);
        replace.name = "replaced-config".to_string();
        alerts
            .update_alert_rule(
                replace,
                RuleConfigUpdate::Replace(format!("  {replacement_plaintext}  ")),
            )
            .unwrap();
        let replaced_stored = stored_rule_config(&db_path, "secure-update");
        assert!(replaced_stored.starts_with(rule_config_crypto::RULE_CONFIG_PREFIX));
        assert!(replaced_stored != original_stored);
        assert!(!replaced_stored.contains(original_plaintext));
        assert!(!replaced_stored.contains(replacement_plaintext));
        assert!(
            alerts
                .get_rule_webhook_url("secure-update")
                .unwrap()
                .as_deref()
                == Some(replacement_plaintext)
        );
        assert!(
            alerts
                .get_alert_rule("secure-update")
                .unwrap()
                .unwrap()
                .rule_configured
        );

        let replace_empty =
            editable_rule(&alerts.get_alert_rule("secure-update").unwrap().unwrap());
        alerts
            .update_alert_rule(
                replace_empty,
                RuleConfigUpdate::Replace("  \t  ".to_string()),
            )
            .unwrap();
        assert!(stored_rule_config(&db_path, "secure-update").is_empty());
        assert!(
            alerts
                .get_rule_webhook_url("secure-update")
                .unwrap()
                .is_none()
        );
        assert!(
            !alerts
                .get_alert_rule("secure-update")
                .unwrap()
                .unwrap()
                .rule_configured
        );

        let replace_again =
            editable_rule(&alerts.get_alert_rule("secure-update").unwrap().unwrap());
        alerts
            .update_alert_rule(
                replace_again,
                RuleConfigUpdate::Replace(replacement_plaintext.to_string()),
            )
            .unwrap();
        let clear = editable_rule(&alerts.get_alert_rule("secure-update").unwrap().unwrap());
        alerts
            .update_alert_rule(clear, RuleConfigUpdate::Clear)
            .unwrap();
        assert!(stored_rule_config(&db_path, "secure-update").is_empty());
        assert!(
            alerts
                .get_rule_webhook_url("secure-update")
                .unwrap()
                .is_none()
        );
        assert!(
            !alerts
                .get_alert_rule("secure-update")
                .unwrap()
                .unwrap()
                .rule_configured
        );

        drop(alerts);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn invalid_replace_leaves_rule_state_and_ciphertext_unchanged() {
        let db_path = temp_rule_config_db("update_invalid");
        let alerts = Alerts::new(&db_path).unwrap();
        alerts
            .create_alert_rule(metric_rule("invalid-update"))
            .unwrap();
        assert!(
            alerts
                .evaluate_condition_at("invalid-update", true, Some(11.0), 10_000)
                .unwrap()
                .is_some()
        );

        let before_view = alerts.get_alert_rule("invalid-update").unwrap().unwrap();
        let before_state =
            serde_json::to_value(alerts.get_alert_state("invalid-update").unwrap()).unwrap();
        let before_stored = stored_rule_config(&db_path, "invalid-update");
        let mut invalid = editable_rule(&before_view);
        invalid.name = "must-not-commit".to_string();
        invalid.enabled = false;
        invalid.threshold = "20".to_string();
        let result = alerts.update_alert_rule(
            invalid,
            RuleConfigUpdate::Replace("ftp://hooks.example.invalid/rejected".to_string()),
        );
        assert!(matches!(result, Err(AppError::Validation(_))));

        assert!(alerts.get_alert_rule("invalid-update").unwrap().unwrap() == before_view);
        assert!(stored_rule_config(&db_path, "invalid-update") == before_stored);
        assert!(
            serde_json::to_value(alerts.get_alert_state("invalid-update").unwrap()).unwrap()
                == before_state
        );

        drop(alerts);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn public_queries_do_not_decrypt_corrupted_storage() {
        let db_path = temp_rule_config_db("query_no_decrypt");
        let alerts = Alerts::new(&db_path).unwrap();
        alerts
            .create_alert_rule(sample_rule("corrupt-after-open"))
            .unwrap();
        let corrupted = format!("{}:invalid", rule_config_crypto::RULE_CONFIG_PREFIX);
        Connection::open(&db_path)
            .unwrap()
            .execute(
                "UPDATE alert_rules SET rule_config = ?1 WHERE id = ?2",
                params![corrupted, "corrupt-after-open"],
            )
            .unwrap();

        let detail = alerts
            .get_alert_rule("corrupt-after-open")
            .unwrap()
            .expect("corrupted rule view");
        assert!(detail.rule_configured);
        let json = serde_json::to_string(&detail).unwrap();
        assert!(!json.contains("rule_config\""));
        assert!(!json.contains(rule_config_crypto::RULE_CONFIG_PREFIX));

        let list = alerts.get_alert_rules().unwrap();
        let listed = list
            .iter()
            .find(|rule| rule.id == "corrupt-after-open")
            .expect("corrupted rule list view");
        assert!(listed.rule_configured);
        let list_json = serde_json::to_string(&list).unwrap();
        assert!(!list_json.contains("rule_config\""));
        assert!(!list_json.contains(rule_config_crypto::RULE_CONFIG_PREFIX));

        let mut preserve = editable_rule(&detail);
        preserve.name = "preserved-without-decrypt".to_string();
        alerts
            .update_alert_rule(preserve, RuleConfigUpdate::Preserve)
            .unwrap();
        assert!(stored_rule_config(&db_path, "corrupt-after-open") == corrupted);
        assert_eq!(
            alerts
                .get_alert_rule("corrupt-after-open")
                .unwrap()
                .unwrap()
                .name,
            "preserved-without-decrypt"
        );
        assert_rule_config_storage_unavailable(alerts.get_rule_webhook_url("corrupt-after-open"));

        drop(alerts);
        cleanup_rule_config_db(&db_path);
    }

    #[test]
    fn parse_webhook_url_empty_and_plain() {
        assert_eq!(Alerts::parse_webhook_url("").unwrap(), None);
        assert_eq!(Alerts::parse_webhook_url("   ").unwrap(), None);
        let url = Alerts::parse_webhook_url("http://127.0.0.1:9/hook")
            .unwrap()
            .expect("loopback url");
        assert!(url.starts_with("http://127.0.0.1:9/hook"));
    }

    #[test]
    fn parse_webhook_url_json_userinfo_and_schemes() {
        let url = Alerts::parse_webhook_url(r#"{"url":"https://example.com/a"}"#)
            .unwrap()
            .expect("json url");
        assert!(url.contains("https://example.com/a"));

        let err = Alerts::parse_webhook_url("http://user:pass@example.com")
            .expect_err("userinfo rejected");
        match err {
            AppError::Validation(msg) => assert!(msg.contains("userinfo")),
            other => panic!("unexpected {other:?}"),
        }

        let err = Alerts::parse_webhook_url("ftp://example.com").expect_err("non-http rejected");
        match err {
            AppError::Validation(_) => {}
            other => panic!("unexpected {other:?}"),
        }

        let err = Alerts::parse_webhook_url("http://").expect_err("host required");
        match err {
            AppError::Validation(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn sanitize_delivery_message_strips_controls_and_bounds_len() {
        let msg = sanitize_delivery_message(&format!("ok\nsecret\x00{}", "x".repeat(500)));
        assert!(!msg.contains('\n'));
        assert!(!msg.contains('\0'));
        assert!(msg.len() <= MAX_DELIVERY_MESSAGE_LEN);
    }

    #[test]
    fn migrate_old_schema_idempotent_and_defaults() {
        let n = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let path = format!("/tmp/alerts_old_schema_{}_{}.db", std::process::id(), n);
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE alert_rules (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    enabled INTEGER DEFAULT 1,
                    rule_config TEXT,
                    threshold TEXT,
                    silence_period INTEGER,
                    created_at INTEGER NOT NULL
                );
                CREATE TABLE alert_states (
                    rule_id TEXT PRIMARY KEY,
                    last_triggered_ms INTEGER,
                    is_silent INTEGER DEFAULT 0,
                    silent_until_ms INTEGER,
                    recovery_count INTEGER DEFAULT 0
                );
                CREATE TABLE alert_deliveries (
                    id INTEGER PRIMARY KEY,
                    rule_id TEXT NOT NULL,
                    timestamp_ms INTEGER NOT NULL,
                    status TEXT,
                    retry_count INTEGER DEFAULT 0,
                    message TEXT
                );
                INSERT INTO alert_rules (id, name, enabled, rule_config, threshold, silence_period, created_at)
                VALUES ('old1', 'legacy', 1, 'http://127.0.0.1:9', 'free text ok', 0, 100);
                "#,
            )
            .unwrap();
        }

        let alerts = Alerts::new(&path).unwrap();
        let again = Alerts::new(&path).unwrap(); // idempotent re-open
        drop(again);

        let rules = alerts.get_alert_rules().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, "old1");
        assert_eq!(rules[0].metric, "");
        assert_eq!(rules[0].operator, "gte");
        assert_eq!(rules[0].server_id, "");
        assert_eq!(rules[0].evaluation_window_secs, 0);
        assert_eq!(rules[0].threshold, "free text ok");

        let state = alerts.get_alert_state("old1").unwrap();
        assert_eq!(state.status, ALERT_STATUS_NORMAL);
        assert!(!state.notification_active);

        // edit + test path still works
        let mut updated = editable_rule(&rules[0]);
        updated.name = "legacy2".into();
        alerts
            .update_alert_rule(updated, RuleConfigUpdate::Preserve)
            .unwrap();
        alerts
            .record_test_delivery("old1", "success", "test delivery success")
            .unwrap();

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn field_whitelist_and_threshold_rules() {
        assert!(Alerts::normalize_metric("proxy.requests").is_ok());
        assert!(Alerts::normalize_metric("latency").is_err());
        assert_eq!(Alerts::normalize_operator("").unwrap(), "gte");
        assert!(Alerts::normalize_operator("neq").is_err());
        assert_eq!(Alerts::normalize_server_id(" all ").unwrap(), "");
        assert!(Alerts::normalize_server_id(&"x".repeat(MAX_SERVER_ID_LEN + 1)).is_err());
        assert!(Alerts::parse_numeric_threshold("inf").is_err());
        assert!(Alerts::parse_numeric_threshold("NaN").is_err());
        assert!(Alerts::parse_numeric_threshold("").is_err());
        assert_eq!(Alerts::parse_numeric_threshold("12.5").unwrap(), 12.5);
        assert!(Alerts::compare_metric("gte", 10.0, 10.0).unwrap());
        assert!(!Alerts::compare_metric("gt", 10.0, 10.0).unwrap());

        let mut rule = metric_rule("m1");
        rule.metric = "nope".into();
        assert!(Alerts::normalize_rule(rule).is_err());

        let mut rule = metric_rule("m1");
        rule.threshold = "abc".into();
        assert!(Alerts::normalize_rule(rule).is_err());

        let mut rule = sample_rule("s1");
        rule.threshold = "free".into();
        assert!(Alerts::normalize_rule(rule).is_ok());
    }

    #[test]
    fn state_machine_single_firing_and_recovered_edges() {
        let (path, alerts) = temp_db();
        alerts.create_alert_rule(metric_rule("e1")).unwrap();

        let t0 = 1_000_000_u128;
        let n1 = alerts
            .evaluate_condition_at("e1", true, Some(11.0), t0)
            .unwrap()
            .expect("firing");
        assert_eq!(n1.kind, "firing");
        assert_eq!(n1.value, "11");
        let state = alerts.get_alert_state("e1").unwrap();
        assert_eq!(state.status, ALERT_STATUS_FIRING);
        assert!(!state.notification_active);
        assert_eq!(state.cooldown_until_ms, None);
        assert!(
            alerts
                .finish_notification_delivery(&n1, true, "firing delivery success", 0, t0 + 1)
                .unwrap()
        );
        let state = alerts.get_alert_state("e1").unwrap();
        assert!(state.notification_active);
        assert_eq!(state.cooldown_until_ms, Some(t0 + 60_001));

        // sustained breach: no repeat
        assert!(
            alerts
                .evaluate_condition_at("e1", true, Some(12.0), t0 + 1)
                .unwrap()
                .is_none()
        );

        let n2 = alerts
            .evaluate_condition_at("e1", false, Some(1.0), t0 + 2)
            .unwrap()
            .expect("recovered");
        assert_eq!(n2.kind, "recovered");
        let state = alerts.get_alert_state("e1").unwrap();
        assert_eq!(state.status, ALERT_STATUS_NORMAL);
        assert!(state.notification_active);
        assert_eq!(state.recovery_count, 0);
        assert!(
            alerts
                .finish_notification_delivery(&n2, true, "recovery delivery success", 0, t0 + 3)
                .unwrap()
        );
        let state = alerts.get_alert_state("e1").unwrap();
        assert!(!state.notification_active);
        assert_eq!(state.recovery_count, 1);

        // sustained clear: no repeat recovered
        assert!(
            alerts
                .evaluate_condition_at("e1", false, Some(0.0), t0 + 3)
                .unwrap()
                .is_none()
        );
        assert_eq!(alerts.get_alert_state("e1").unwrap().recovery_count, 1);

        // infinite recovered fixed via wrappers too
        assert!(!alerts.mark_recovered("e1").unwrap());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn firing_failure_after_recovery_preserves_retry_without_false_activation() {
        let (path, alerts) = temp_db();
        alerts
            .create_alert_rule(metric_rule("late-failure"))
            .unwrap();

        let t0 = 1_500_000_u128;
        let firing = alerts
            .evaluate_condition_at("late-failure", true, Some(11.0), t0)
            .unwrap()
            .expect("firing reservation");
        assert!(
            alerts
                .evaluate_condition_at("late-failure", false, Some(0.0), t0 + 1)
                .unwrap()
                .is_none()
        );
        let recovered_before_finish = alerts.get_alert_state("late-failure").unwrap();
        assert_eq!(recovered_before_finish.status, ALERT_STATUS_NORMAL);
        assert_eq!(
            recovered_before_finish.pending_notification_id,
            Some(firing.notification_id)
        );

        assert!(
            alerts
                .finish_notification_delivery(&firing, false, "network delivery failed", 1, t0 + 2,)
                .unwrap()
        );
        let failed = alerts.get_alert_state("late-failure").unwrap();
        assert_eq!(failed.status, ALERT_STATUS_NORMAL);
        assert!(failed.pending_notification_kind.is_none());
        assert_eq!(failed.retry_notification_kind.as_deref(), Some("firing"));
        assert!(!failed.notification_active);
        assert_eq!(failed.cooldown_until_ms, None);
        assert!(
            alerts
                .evaluate_condition_at("late-failure", false, Some(0.0), t0 + 3)
                .unwrap()
                .is_none()
        );
        let retry = alerts
            .evaluate_condition_at("late-failure", true, Some(12.0), t0 + 4)
            .unwrap()
            .expect("firing retry on next matching condition");
        assert_ne!(retry.notification_id, firing.notification_id);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn expired_firing_reservation_survives_first_recovery_edge_and_retries_once() {
        let (path, alerts) = temp_db();
        let mut rule = metric_rule("stale-firing");
        rule.silence_period = 0;
        alerts.create_alert_rule(rule).unwrap();

        let t0 = 2_100_000_u128;
        let stale_firing = alerts
            .evaluate_condition_at("stale-firing", true, Some(11.0), t0)
            .unwrap()
            .expect("initial firing reservation");
        drop(alerts);
        let alerts = Alerts::new(&path).unwrap();

        assert!(
            alerts
                .evaluate_condition_at(
                    "stale-firing",
                    false,
                    Some(0.0),
                    t0 + NOTIFICATION_LEASE_MS + 1,
                )
                .unwrap()
                .is_none()
        );
        let recovered = alerts.get_alert_state("stale-firing").unwrap();
        assert_eq!(recovered.status, ALERT_STATUS_NORMAL);
        assert!(recovered.pending_notification_kind.is_none());
        assert!(recovered.pending_notification_id.is_none());
        assert_eq!(recovered.retry_notification_kind.as_deref(), Some("firing"));
        assert!(!recovered.notification_active);
        assert_eq!(recovered.cooldown_until_ms, None);
        assert!(
            alerts
                .list_alert_deliveries(Some("stale-firing"), 10)
                .unwrap()
                .is_empty()
        );

        let replacement = alerts
            .evaluate_condition_at(
                "stale-firing",
                true,
                Some(12.0),
                t0 + NOTIFICATION_LEASE_MS + 2,
            )
            .unwrap()
            .expect("firing retry after matching edge");
        assert_eq!(replacement.kind, "firing");
        assert_eq!(
            replacement.notification_id,
            stale_firing.notification_id + 1
        );
        assert!(
            alerts
                .evaluate_condition_at(
                    "stale-firing",
                    true,
                    Some(13.0),
                    t0 + NOTIFICATION_LEASE_MS + 3,
                )
                .unwrap()
                .is_none()
        );
        let before_stale_completion = alerts.get_alert_state("stale-firing").unwrap();
        assert_eq!(
            before_stale_completion.pending_notification_id,
            Some(replacement.notification_id)
        );
        assert_eq!(
            before_stale_completion.retry_notification_kind.as_deref(),
            Some("firing")
        );
        assert!(
            !alerts
                .finish_notification_delivery(
                    &stale_firing,
                    true,
                    "stale firing completion",
                    0,
                    t0 + NOTIFICATION_LEASE_MS + 4,
                )
                .unwrap()
        );
        let after_stale_completion = alerts.get_alert_state("stale-firing").unwrap();
        assert_eq!(
            after_stale_completion.pending_notification_id,
            before_stale_completion.pending_notification_id
        );
        assert_eq!(
            after_stale_completion.pending_notification_kind,
            before_stale_completion.pending_notification_kind
        );
        assert_eq!(
            after_stale_completion.notification_active,
            before_stale_completion.notification_active
        );
        assert_eq!(
            after_stale_completion.cooldown_until_ms,
            before_stale_completion.cooldown_until_ms
        );
        assert_eq!(
            after_stale_completion.retry_notification_kind,
            before_stale_completion.retry_notification_kind
        );
        let deliveries = alerts
            .list_alert_deliveries(Some("stale-firing"), 10)
            .unwrap();
        assert!(deliveries.is_empty());

        assert!(
            alerts
                .finish_notification_delivery(
                    &replacement,
                    true,
                    "replacement firing delivery",
                    0,
                    t0 + NOTIFICATION_LEASE_MS + 5,
                )
                .unwrap()
        );
        let active = alerts.get_alert_state("stale-firing").unwrap();
        assert!(active.pending_notification_kind.is_none());
        assert!(active.notification_active);
        assert_eq!(active.cooldown_until_ms, None);
        assert!(active.retry_notification_kind.is_none());
        let deliveries = alerts
            .list_alert_deliveries(Some("stale-firing"), 10)
            .unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].message, "replacement firing delivery");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn expired_recovered_reservation_survives_first_breach_edge_and_retries_once() {
        let (path, alerts) = temp_db();
        let mut rule = metric_rule("stale-recovered");
        rule.silence_period = 0;
        alerts.create_alert_rule(rule).unwrap();

        let t0 = now_ms();
        let firing = alerts
            .evaluate_condition_at("stale-recovered", true, Some(11.0), t0)
            .unwrap()
            .expect("initial firing reservation");
        assert!(
            alerts
                .finish_notification_delivery(&firing, true, "initial firing delivery", 0, t0 + 1,)
                .unwrap()
        );
        let stale_recovered = alerts
            .evaluate_condition_at("stale-recovered", false, Some(0.0), t0 + 2)
            .unwrap()
            .expect("initial recovered reservation");
        assert_eq!(stale_recovered.kind, "recovered");
        assert!(
            alerts
                .get_alert_state("stale-recovered")
                .unwrap()
                .notification_active
        );
        drop(alerts);
        let alerts = Alerts::new(&path).unwrap();

        assert!(
            alerts
                .evaluate_condition_at(
                    "stale-recovered",
                    true,
                    Some(11.0),
                    t0 + NOTIFICATION_LEASE_MS + 3,
                )
                .unwrap()
                .is_none()
        );
        let breaching = alerts.get_alert_state("stale-recovered").unwrap();
        assert_eq!(breaching.status, ALERT_STATUS_FIRING);
        assert!(breaching.pending_notification_kind.is_none());
        assert!(breaching.pending_notification_id.is_none());
        assert_eq!(
            breaching.retry_notification_kind.as_deref(),
            Some("recovered")
        );
        assert!(breaching.notification_active);
        assert_eq!(breaching.cooldown_until_ms, None);
        let deliveries = alerts
            .list_alert_deliveries(Some("stale-recovered"), 10)
            .unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].message, "initial firing delivery");

        assert!(
            alerts
                .evaluate_condition_at(
                    "stale-recovered",
                    true,
                    Some(12.0),
                    t0 + NOTIFICATION_LEASE_MS + 4,
                )
                .unwrap()
                .is_none()
        );
        let replacement = alerts
            .evaluate_condition_at(
                "stale-recovered",
                false,
                Some(0.0),
                t0 + NOTIFICATION_LEASE_MS + 5,
            )
            .unwrap()
            .expect("recovered retry after matching edge");
        assert_eq!(replacement.kind, "recovered");
        assert_eq!(
            replacement.notification_id,
            stale_recovered.notification_id + 1
        );
        assert!(
            alerts
                .evaluate_condition_at(
                    "stale-recovered",
                    false,
                    Some(0.0),
                    t0 + NOTIFICATION_LEASE_MS + 6,
                )
                .unwrap()
                .is_none()
        );
        let before_stale_completion = alerts.get_alert_state("stale-recovered").unwrap();
        assert_eq!(
            before_stale_completion.pending_notification_id,
            Some(replacement.notification_id)
        );
        assert_eq!(
            before_stale_completion.retry_notification_kind.as_deref(),
            Some("recovered")
        );
        assert!(
            !alerts
                .finish_notification_delivery(
                    &stale_recovered,
                    true,
                    "stale recovered completion",
                    0,
                    t0 + NOTIFICATION_LEASE_MS + 7,
                )
                .unwrap()
        );
        let after_stale_completion = alerts.get_alert_state("stale-recovered").unwrap();
        assert_eq!(
            after_stale_completion.pending_notification_id,
            before_stale_completion.pending_notification_id
        );
        assert_eq!(
            after_stale_completion.pending_notification_kind,
            before_stale_completion.pending_notification_kind
        );
        assert_eq!(
            after_stale_completion.notification_active,
            before_stale_completion.notification_active
        );
        assert_eq!(
            after_stale_completion.cooldown_until_ms,
            before_stale_completion.cooldown_until_ms
        );
        assert_eq!(
            after_stale_completion.retry_notification_kind,
            before_stale_completion.retry_notification_kind
        );
        let deliveries = alerts
            .list_alert_deliveries(Some("stale-recovered"), 10)
            .unwrap();
        assert_eq!(deliveries.len(), 1);
        assert!(
            !deliveries
                .iter()
                .any(|delivery| delivery.message == "stale recovered completion")
        );

        assert!(
            alerts
                .finish_notification_delivery(
                    &replacement,
                    true,
                    "replacement recovered delivery",
                    0,
                    t0 + NOTIFICATION_LEASE_MS + 8,
                )
                .unwrap()
        );
        let recovered = alerts.get_alert_state("stale-recovered").unwrap();
        assert!(recovered.pending_notification_kind.is_none());
        assert!(!recovered.notification_active);
        assert_eq!(recovered.cooldown_until_ms, None);
        assert!(recovered.retry_notification_kind.is_none());
        assert_eq!(recovered.recovery_count, 1);
        let deliveries = alerts
            .list_alert_deliveries(Some("stale-recovered"), 10)
            .unwrap();
        assert_eq!(deliveries.len(), 2);
        assert!(
            deliveries
                .iter()
                .any(|delivery| delivery.message == "replacement recovered delivery")
        );
        assert!(
            !deliveries
                .iter()
                .any(|delivery| delivery.message == "stale recovered completion")
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn failed_deliveries_retry_without_false_active_or_cooldown() {
        let (path, alerts) = temp_db();
        alerts.create_alert_rule(metric_rule("retry")).unwrap();

        let t0 = 2_000_000_u128;
        let first_firing = alerts
            .evaluate_condition_at("retry", true, Some(11.0), t0)
            .unwrap()
            .expect("first firing");
        assert!(
            alerts
                .finish_notification_delivery(
                    &first_firing,
                    false,
                    "network delivery failed",
                    MAX_RETRY_COUNT,
                    t0 + 1,
                )
                .unwrap()
        );
        let failed_firing = alerts.get_alert_state("retry").unwrap();
        assert_eq!(failed_firing.status, ALERT_STATUS_FIRING);
        assert!(!failed_firing.notification_active);
        assert_eq!(failed_firing.cooldown_until_ms, None);
        assert_eq!(
            failed_firing.retry_notification_kind.as_deref(),
            Some("firing")
        );

        let retry_firing = alerts
            .evaluate_condition_at("retry", true, Some(12.0), t0 + 2)
            .unwrap()
            .expect("retry firing");
        assert_ne!(retry_firing.notification_id, first_firing.notification_id);
        assert!(
            !alerts
                .finish_notification_delivery(&first_firing, true, "stale duplicate", 0, t0 + 3,)
                .unwrap()
        );
        assert!(
            alerts
                .finish_notification_delivery(
                    &retry_firing,
                    true,
                    "firing delivery success",
                    0,
                    t0 + 4,
                )
                .unwrap()
        );
        let active = alerts.get_alert_state("retry").unwrap();
        assert!(active.notification_active);
        assert_eq!(active.cooldown_until_ms, Some(t0 + 60_004));

        let first_recovery = alerts
            .evaluate_condition_at("retry", false, Some(0.0), t0 + 5)
            .unwrap()
            .expect("first recovery");
        assert!(
            alerts
                .finish_notification_delivery(
                    &first_recovery,
                    false,
                    "network delivery failed",
                    1,
                    t0 + 6,
                )
                .unwrap()
        );
        let failed_recovery = alerts.get_alert_state("retry").unwrap();
        assert_eq!(failed_recovery.status, ALERT_STATUS_NORMAL);
        assert!(failed_recovery.notification_active);
        assert_eq!(failed_recovery.recovery_count, 0);
        assert_eq!(
            failed_recovery.retry_notification_kind.as_deref(),
            Some("recovered")
        );

        let retry_recovery = alerts
            .evaluate_condition_at("retry", false, Some(0.0), t0 + 7)
            .unwrap()
            .expect("retry recovery");
        assert!(
            alerts
                .finish_notification_delivery(
                    &retry_recovery,
                    true,
                    "recovery delivery success",
                    0,
                    t0 + 8,
                )
                .unwrap()
        );
        assert!(
            !alerts
                .finish_notification_delivery(
                    &retry_recovery,
                    true,
                    "duplicate recovery",
                    0,
                    t0 + 9,
                )
                .unwrap()
        );
        let recovered = alerts.get_alert_state("retry").unwrap();
        assert!(!recovered.notification_active);
        assert_eq!(recovered.recovery_count, 1);

        let deliveries = alerts.list_alert_deliveries(Some("retry"), 10).unwrap();
        assert_eq!(deliveries.len(), 4);
        assert_eq!(
            deliveries.iter().filter(|d| d.status == "failed").count(),
            2
        );
        assert_eq!(
            deliveries.iter().filter(|d| d.status == "success").count(),
            2
        );
        assert!(!deliveries.iter().any(|d| d.message == "stale duplicate"));
        assert!(!deliveries.iter().any(|d| d.message == "duplicate recovery"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn manual_silence_suppresses_events_but_advances_state() {
        let (path, alerts) = temp_db();
        let mut rule = metric_rule("s1");
        rule.silence_period = 0;
        alerts.create_alert_rule(rule).unwrap();

        let t0 = now_ms();
        alerts
            .update_silence("s1", true, Some(t0 + 3_600_000))
            .unwrap();
        assert!(alerts.is_rule_silenced("s1").unwrap());

        // breach while silenced: internal firing, no notify
        assert!(
            alerts
                .evaluate_condition_at("s1", true, Some(99.0), t0)
                .unwrap()
                .is_none()
        );
        let state = alerts.get_alert_state("s1").unwrap();
        assert_eq!(state.status, ALERT_STATUS_FIRING);
        assert!(!state.notification_active);
        assert!(state.is_silent);

        // clear while silenced: no recovered (never notified / silenced)
        assert!(
            alerts
                .evaluate_condition_at("s1", false, Some(0.0), t0 + 1)
                .unwrap()
                .is_none()
        );
        let state = alerts.get_alert_state("s1").unwrap();
        assert_eq!(state.status, ALERT_STATUS_NORMAL);
        assert_eq!(state.recovery_count, 0);
        // manual silence preserved
        assert!(state.is_silent);

        // after silence ends, no stale catch-up; need full new edge
        alerts.update_silence("s1", false, None).unwrap();
        assert!(
            alerts
                .evaluate_condition_at("s1", false, Some(0.0), t0 + 2)
                .unwrap()
                .is_none()
        );
        let n = alerts
            .evaluate_condition_at("s1", true, Some(50.0), t0 + 3)
            .unwrap()
            .expect("new firing after silence");
        assert_eq!(n.kind, "firing");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn cooldown_blocks_reentry_notify_until_expiry() {
        let (path, alerts) = temp_db();
        let mut rule = metric_rule("c1");
        rule.silence_period = 30; // 30s cooldown
        alerts.create_alert_rule(rule).unwrap();

        let t0 = 3_000_000_u128;
        let firing = alerts
            .evaluate_condition_at("c1", true, Some(11.0), t0)
            .unwrap()
            .expect("firing");
        assert!(
            alerts
                .finish_notification_delivery(&firing, true, "firing delivery success", 0, t0 + 1)
                .unwrap()
        );
        let recovery = alerts
            .evaluate_condition_at("c1", false, Some(0.0), t0 + 2)
            .unwrap()
            .expect("recovered");
        assert!(
            alerts
                .finish_notification_delivery(
                    &recovery,
                    true,
                    "recovery delivery success",
                    0,
                    t0 + 3
                )
                .unwrap()
        );

        // re-breach inside cooldown: internal firing, no notify
        let mid = t0 + 10_000;
        assert!(
            alerts
                .evaluate_condition_at("c1", true, Some(11.0), mid)
                .unwrap()
                .is_none()
        );
        let state = alerts.get_alert_state("c1").unwrap();
        assert_eq!(state.status, ALERT_STATUS_FIRING);
        assert!(!state.notification_active);

        // clear quiet episode
        assert!(
            alerts
                .evaluate_condition_at("c1", false, Some(0.0), mid + 1)
                .unwrap()
                .is_none()
        );

        // after cooldown, full edge notifies again
        let after = t0 + 30_000 + 1;
        let n = alerts
            .evaluate_condition_at("c1", true, Some(11.0), after)
            .unwrap()
            .expect("firing after cooldown");
        assert_eq!(n.kind, "firing");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn cooldown_reentry_that_stays_breaching_notifies_after_expiry() {
        let (path, alerts) = temp_db();
        let mut rule = metric_rule("c2");
        rule.silence_period = 30;
        alerts.create_alert_rule(rule).unwrap();

        let t0 = 3_100_000_u128;
        let first_firing = alerts
            .evaluate_condition_at("c2", true, Some(11.0), t0)
            .unwrap()
            .expect("first firing");
        assert!(
            alerts
                .finish_notification_delivery(
                    &first_firing,
                    true,
                    "firing delivery success",
                    0,
                    t0 + 1,
                )
                .unwrap()
        );

        let first_recovery = alerts
            .evaluate_condition_at("c2", false, Some(0.0), t0 + 2)
            .unwrap()
            .expect("first recovery");
        assert!(
            alerts
                .finish_notification_delivery(
                    &first_recovery,
                    true,
                    "recovery delivery success",
                    0,
                    t0 + 3,
                )
                .unwrap()
        );

        // Re-breach inside cooldown: enter firing without reserving a notification.
        let rebreach = t0 + 10_000;
        assert!(
            alerts
                .evaluate_condition_at("c2", true, Some(11.0), rebreach)
                .unwrap()
                .is_none()
        );
        let state = alerts.get_alert_state("c2").unwrap();
        assert_eq!(state.status, ALERT_STATUS_FIRING);
        assert!(!state.notification_active);
        assert_eq!(state.cooldown_until_ms, Some(t0 + 30_001));

        // A sustained breach has no fresh edge at expiry, but must still notify once.
        let second_firing = alerts
            .evaluate_condition_at("c2", true, Some(12.0), t0 + 30_001)
            .unwrap()
            .expect("firing after cooldown while still breaching");
        assert_eq!(second_firing.kind, "firing");
        assert!(
            alerts
                .evaluate_condition_at("c2", true, Some(13.0), t0 + 30_002)
                .unwrap()
                .is_none()
        );
        assert!(
            alerts
                .finish_notification_delivery(
                    &second_firing,
                    true,
                    "firing delivery success",
                    0,
                    t0 + 30_003,
                )
                .unwrap()
        );

        // Recovery remains a single normal edge after the deferred firing.
        let second_recovery = alerts
            .evaluate_condition_at("c2", false, Some(0.0), t0 + 30_004)
            .unwrap()
            .expect("second recovery");
        assert_eq!(second_recovery.kind, "recovered");
        assert!(
            alerts
                .finish_notification_delivery(
                    &second_recovery,
                    true,
                    "recovery delivery success",
                    0,
                    t0 + 30_005,
                )
                .unwrap()
        );
        assert!(
            alerts
                .evaluate_condition_at("c2", false, Some(0.0), t0 + 30_006)
                .unwrap()
                .is_none()
        );

        let state = alerts.get_alert_state("c2").unwrap();
        assert_eq!(state.recovery_count, 2);
        assert!(!state.notification_active);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn disable_delete_missing_and_edit_reset() {
        let (path, alerts) = temp_db();
        alerts.create_alert_rule(metric_rule("d1")).unwrap();
        let t0 = 4_000_000_u128;
        assert!(
            alerts
                .evaluate_condition_at("d1", true, Some(11.0), t0)
                .unwrap()
                .is_some()
        );

        alerts.toggle_rule("d1", false).unwrap();
        let state = alerts.get_alert_state("d1").unwrap();
        assert_eq!(state.status, ALERT_STATUS_NORMAL);
        assert!(!state.notification_active);
        // no recovered after disable
        assert!(
            alerts
                .evaluate_condition_at("d1", false, Some(0.0), t0 + 1)
                .unwrap()
                .is_none()
        );

        alerts.toggle_rule("d1", true).unwrap();
        // re-enable does not emit stale recovered
        assert!(
            alerts
                .evaluate_condition_at("d1", false, Some(0.0), t0 + 2)
                .unwrap()
                .is_none()
        );

        // fire then edit metric => reset
        assert!(
            alerts
                .evaluate_condition_at("d1", true, Some(11.0), t0 + 3)
                .unwrap()
                .is_some()
        );
        let view = alerts.get_alert_rule("d1").unwrap().unwrap();
        let mut updated = editable_rule(&view);
        updated.threshold = "20".into();
        alerts
            .update_alert_rule(updated, RuleConfigUpdate::Preserve)
            .unwrap();
        let state = alerts.get_alert_state("d1").unwrap();
        assert_eq!(state.status, ALERT_STATUS_NORMAL);
        assert!(!state.notification_active);

        // missing evaluate is safe no-op
        assert!(
            alerts
                .evaluate_condition("missing", true, Some(1.0))
                .unwrap()
                .is_none()
        );
        assert!(!alerts.trigger_alert("missing").unwrap());
        assert!(!alerts.mark_recovered("missing").unwrap());

        alerts.delete_rule("d1").unwrap();
        assert!(alerts.get_alert_rule("d1").unwrap().is_none());
        assert!(
            alerts
                .evaluate_condition("d1", true, Some(1.0))
                .unwrap()
                .is_none()
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn concurrent_evaluate_single_firing_event() {
        let (path, alerts) = temp_db();
        let mut rule = metric_rule("race");
        rule.silence_period = 0;
        alerts.create_alert_rule(rule).unwrap();
        let alerts = Arc::new(alerts);
        let mut handles = Vec::new();
        for _ in 0..8 {
            let a = Arc::clone(&alerts);
            handles.push(thread::spawn(move || {
                a.evaluate_condition("race", true, Some(42.0)).unwrap()
            }));
        }
        let mut events = Vec::new();
        for h in handles {
            if let Some(event) = h.join().unwrap() {
                assert_eq!(event.kind, "firing");
                events.push(event);
            }
        }
        assert_eq!(events.len(), 1);
        let state = alerts.get_alert_state("race").unwrap();
        assert_eq!(state.status, ALERT_STATUS_FIRING);
        assert!(!state.notification_active);
        assert_eq!(
            state.pending_notification_id,
            Some(events[0].notification_id)
        );
        assert!(
            alerts
                .finish_notification_delivery(
                    &events[0],
                    true,
                    "firing delivery success",
                    0,
                    now_ms()
                )
                .unwrap()
        );
        assert!(alerts.get_alert_state("race").unwrap().notification_active);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn sqlite_timestamp_roundtrip_and_silence_preserves_state() {
        let (path, alerts) = temp_db();
        alerts.create_alert_rule(sample_rule("r1")).unwrap();
        alerts
            .record_test_delivery("r1", "success", "test delivery success")
            .unwrap();
        let rules = alerts.get_alert_rules().unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].created_at, 1_700_000_000_000);
        assert_eq!(rules[0].silence_period, 30);

        let deliveries = alerts.list_alert_deliveries(Some("r1"), 100).unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].status, "success");
        assert!(deliveries[0].timestamp_ms > 0);

        assert!(alerts.trigger_alert("r1").unwrap());
        let before = alerts.get_alert_state("r1").unwrap();
        assert!(before.last_triggered_ms.is_some());
        assert_eq!(before.status, ALERT_STATUS_FIRING);
        // silence_period becomes cooldown, not manual silence
        assert!(before.cooldown_until_ms.is_some());
        assert!(!before.is_silent);

        let future = now_ms() + 3_600_000;
        alerts.update_silence("r1", true, Some(future)).unwrap();
        let state = alerts.get_alert_state("r1").unwrap();
        assert!(state.is_silent);
        assert_eq!(state.silent_until_ms, Some(future));
        assert_eq!(state.last_triggered_ms, before.last_triggered_ms);
        assert_eq!(state.recovery_count, before.recovery_count);
        assert_eq!(state.cooldown_until_ms, before.cooldown_until_ms);

        // While manually silenced and already firing, trigger is a no-op notify.
        assert!(!alerts.trigger_alert("r1").unwrap());

        // Recovered suppressed by manual silence; state still clears to normal.
        assert!(!alerts.mark_recovered("r1").unwrap());
        let recovered = alerts.get_alert_state("r1").unwrap();
        assert_eq!(recovered.status, ALERT_STATUS_NORMAL);
        assert!(!recovered.notification_active);
        // manual silence preserved (not cleared by recovery)
        assert!(recovered.is_silent);
        assert_eq!(recovered.silent_until_ms, Some(future));
        assert_eq!(recovered.recovery_count, before.recovery_count);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn delivery_list_limit_and_missing_rule_no_delivery() {
        let (path, alerts) = temp_db();
        alerts
            .record_test_delivery("missing", "failed", "no webhook URL configured")
            .unwrap();
        let deliveries = alerts.list_alert_deliveries(Some("missing"), 10).unwrap();
        assert!(deliveries.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn validate_rule_fields_rejects_overlong_and_controls() {
        let mut rule = sample_rule("r1");
        rule.name = "n\x00".into();
        assert!(Alerts::validate_rule_fields(&rule).is_err());
        rule.name = "n".into();
        rule.threshold = "x".repeat(MAX_THRESHOLD_LEN + 1);
        assert!(Alerts::validate_rule_fields(&rule).is_err());
        rule.threshold = "1".into();
        rule.silence_period = MAX_SILENCE_PERIOD_SECS + 1;
        assert!(Alerts::validate_rule_fields(&rule).is_err());
    }

    #[test]
    fn update_preserves_created_at() {
        let (path, alerts) = temp_db();
        alerts.create_alert_rule(sample_rule("r1")).unwrap();
        let mut updated = sample_rule("r1");
        updated.name = "renamed".into();
        updated.created_at = 1; // should be ignored by UPDATE path
        alerts
            .update_alert_rule(updated, RuleConfigUpdate::Preserve)
            .unwrap();
        let rule = alerts.get_alert_rule("r1").unwrap().expect("exists");
        assert_eq!(rule.name, "renamed");
        assert_eq!(rule.created_at, 1_700_000_000_000);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn wrappers_share_state_machine_semantics() {
        let (path, alerts) = temp_db();
        let mut rule = sample_rule("w1");
        rule.silence_period = 0;
        alerts.create_alert_rule(rule).unwrap();

        assert!(alerts.trigger_alert("w1").unwrap());
        assert!(!alerts.trigger_alert("w1").unwrap()); // sustained
        assert!(alerts.mark_recovered("w1").unwrap());
        assert!(!alerts.mark_recovered("w1").unwrap()); // no infinite recovered

        let deliveries = alerts.list_alert_deliveries(Some("w1"), 10).unwrap();
        assert!(
            deliveries
                .iter()
                .any(|d| d.status == "success" && d.message == "alert triggered")
        );
        assert!(
            deliveries
                .iter()
                .any(|d| d.status == "success" && d.message == "alert recovered")
        );
        assert_eq!(
            deliveries
                .iter()
                .filter(|d| d.message == "alert recovered")
                .count(),
            1
        );

        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn webhook_update_invalidates_pending_but_preserves_delivered_episode() {
        let (path, alerts) = temp_db();
        let rule = metric_rule("config-update");
        alerts.create_alert_rule(rule.clone()).unwrap();
        let now = now_ms();
        let firing = alerts
            .evaluate_condition_at(&rule.id, true, Some(11.0), now)
            .unwrap()
            .unwrap();
        assert!(
            alerts
                .finish_notification_delivery(&firing, true, "sent", 0, now + 1)
                .unwrap()
        );
        let recovery = alerts
            .evaluate_condition_at(&rule.id, false, Some(0.0), now + 2)
            .unwrap()
            .unwrap();
        alerts
            .update_alert_rule(
                rule.clone(),
                RuleConfigUpdate::Replace("http://127.0.0.1:9/new".into()),
            )
            .unwrap();
        assert!(!alerts.notification_is_current(&recovery).unwrap());
        let state = alerts.get_alert_state(&rule.id).unwrap();
        assert!(state.notification_active);
        assert_eq!(state.retry_notification_kind.as_deref(), Some("recovered"));
        assert!(state.pending_notification_id.is_none());
        assert!(
            !alerts
                .finish_notification_delivery(&recovery, false, "cancelled", 0, now + 3)
                .unwrap()
        );
        assert_eq!(alerts.list_alert_deliveries(None, 100).unwrap().len(), 1);
        drop(alerts);
        cleanup_rule_config_db(Path::new(&path));
    }

    #[test]
    fn retention_preserves_exact_ninety_day_boundary() {
        let (path, alerts) = temp_db();
        let now = now_ms();
        let cutoff = now - 90 * 24 * 60 * 60 * 1000;
        alerts.with_conn(|conn| {
            conn.execute("INSERT INTO alert_deliveries(rule_id,timestamp_ms,status) VALUES ('boundary',?1,'failed'),('boundary',?2,'success')",
                params![(cutoff - 1) as i64, cutoff as i64])?;
            Alerts::prune_deliveries_tx(conn, now)
        }).unwrap();
        let deliveries = alerts.list_alert_deliveries(None, 10).unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_eq!(deliveries[0].timestamp_ms, cutoff);
        assert_eq!(deliveries[0].status, "success");
        drop(alerts);
        cleanup_rule_config_db(Path::new(&path));
    }

    #[test]
    fn retention_write_throttle_is_shared_and_timer_forces_cleanup() {
        let (path, alerts) = temp_db();
        alerts.create_alert_rule(metric_rule("throttle")).unwrap();
        let clone = alerts.clone();
        let initialized_at = *alerts.last_delivery_history_prune.lock().unwrap();
        assert!(initialized_at.is_some());
        alerts.with_conn(|conn| {
            conn.execute("INSERT INTO alert_deliveries(rule_id,timestamp_ms,status) VALUES ('throttle',1,'failed')", [])?;
            Ok(())
        }).unwrap();
        clone
            .record_test_delivery("throttle", "success", "within the hour")
            .unwrap();
        assert_eq!(alerts.list_alert_deliveries(None, 100).unwrap().len(), 2);
        assert_eq!(
            *clone.last_delivery_history_prune.lock().unwrap(),
            initialized_at
        );

        *alerts.last_delivery_history_prune.lock().unwrap() =
            Some(Instant::now() - DELIVERY_HISTORY_PRUNE_INTERVAL);
        clone
            .record_test_delivery("throttle", "success", "next hour")
            .unwrap();
        assert_eq!(alerts.list_alert_deliveries(None, 100).unwrap().len(), 2);
        assert!(
            alerts
                .list_alert_deliveries(None, 100)
                .unwrap()
                .iter()
                .all(|row| row.timestamp_ms > 1)
        );
        let pruned_at = *alerts.last_delivery_history_prune.lock().unwrap();
        assert_ne!(pruned_at, initialized_at);

        clone.with_conn(|conn| {
            conn.execute("INSERT INTO alert_deliveries(rule_id,timestamp_ms,status) VALUES ('throttle',1,'failed')", [])?;
            Ok(())
        }).unwrap();
        alerts
            .record_test_delivery("throttle", "success", "shared throttle")
            .unwrap();
        assert_eq!(
            *alerts.last_delivery_history_prune.lock().unwrap(),
            pruned_at
        );
        assert_eq!(clone.list_alert_deliveries(None, 100).unwrap().len(), 4);
        clone.prune_delivery_history().unwrap();
        assert_eq!(alerts.list_alert_deliveries(None, 100).unwrap().len(), 3);
        assert!(
            alerts
                .list_alert_deliveries(None, 100)
                .unwrap()
                .iter()
                .all(|row| row.timestamp_ms > 1)
        );
        drop(clone);
        drop(alerts);
        cleanup_rule_config_db(Path::new(&path));
    }

    #[test]
    fn retention_batch_prunes_after_all_writes_and_retries_failed_commit() {
        let (path, alerts) = temp_db();
        let now = now_ms();
        let mut events = Vec::new();
        for id in ["batch-first", "batch-second"] {
            alerts.create_alert_rule(metric_rule(id)).unwrap();
            events.push(
                alerts
                    .evaluate_condition_at(id, true, Some(11.0), now)
                    .unwrap()
                    .unwrap(),
            );
        }
        alerts.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO alert_deliveries(rule_id,timestamp_ms,status) VALUES ('expired',1,'failed');
                 CREATE TRIGGER no_early_prune BEFORE INSERT ON alert_deliveries
                 WHEN NEW.rule_id = 'batch-second' AND NOT EXISTS (
                     SELECT 1 FROM alert_deliveries WHERE rule_id = 'expired')
                 BEGIN SELECT RAISE(ABORT, 'pruned before the batch finished'); END;
                 CREATE TABLE retention_guard_parent(id INTEGER PRIMARY KEY);
                 CREATE TABLE retention_guard(child INTEGER REFERENCES retention_guard_parent(id)
                     DEFERRABLE INITIALLY DEFERRED);
                 CREATE TRIGGER reject_prune_commit AFTER DELETE ON alert_deliveries
                 WHEN OLD.rule_id = 'expired'
                 BEGIN INSERT INTO retention_guard(child) VALUES (1); END;"
            )
        }).unwrap();
        *alerts.last_delivery_history_prune.lock().unwrap() = None;
        let failure = alerts
            .fail_notification_deliveries(&events, "shutdown", 0, now + 1)
            .unwrap_err();
        assert!(
            matches!(failure, AppError::Database(rusqlite::Error::SqliteFailure(code, _))
            if code.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY)
        );
        assert!(alerts.last_delivery_history_prune.lock().unwrap().is_none());
        let rows = alerts.list_alert_deliveries(None, 100).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].rule_id, "expired");
        for event in &events {
            assert!(alerts.notification_is_current(event).unwrap());
        }
        alerts
            .with_conn(|conn| conn.execute_batch("DROP TRIGGER reject_prune_commit"))
            .unwrap();
        assert_eq!(
            alerts
                .fail_notification_deliveries(&events, "shutdown", 0, now + 2)
                .unwrap(),
            2
        );
        assert!(alerts.last_delivery_history_prune.lock().unwrap().is_some());
        let rows = alerts.list_alert_deliveries(None, 100).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .all(|row| row.rule_id.starts_with("batch-") && row.status == "failed")
        );
        for event in &events {
            let state = alerts.get_alert_state(&event.rule_id).unwrap();
            assert!(state.pending_notification_id.is_none());
            assert_eq!(state.retry_notification_kind.as_deref(), Some("firing"));
        }
        drop(alerts);
        cleanup_rule_config_db(Path::new(&path));
    }

    #[test]
    fn history_retention_runs_after_write_and_on_initialization() {
        let (path, alerts) = temp_db();
        alerts.create_alert_rule(metric_rule("retention")).unwrap();
        let now = now_ms();
        alerts.with_conn(|conn| {
            conn.execute("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<20001)
                INSERT INTO alert_deliveries(rule_id,timestamp_ms,status) SELECT 'retention',?1,'success' FROM n", params![now as i64])?;
            conn.execute("INSERT INTO alert_deliveries(rule_id,timestamp_ms,status) VALUES ('retention',1,'failed')", [])?;
            Ok(())
        }).unwrap();
        alerts
            .record_test_delivery("retention", "success", "throttled")
            .unwrap();
        alerts
            .with_conn(|conn| {
                let count: i64 =
                    conn.query_row("SELECT COUNT(*) FROM alert_deliveries", [], |row| {
                        row.get(0)
                    })?;
                assert_eq!(
                    count, 20003,
                    "the cap may be exceeded until maintenance is due"
                );
                Ok(())
            })
            .unwrap();
        *alerts.last_delivery_history_prune.lock().unwrap() =
            Some(Instant::now() - DELIVERY_HISTORY_PRUNE_INTERVAL);
        alerts
            .record_test_delivery("retention", "success", "latest")
            .unwrap();
        let check = |alerts: &Alerts| {
            alerts
                .with_conn(|conn| {
                    let count: i64 =
                        conn.query_row("SELECT COUNT(*) FROM alert_deliveries", [], |r| r.get(0))?;
                    assert_eq!(count, 20000);
                    let old: i64 = conn.query_row(
                        "SELECT COUNT(*) FROM alert_deliveries WHERE timestamp_ms=1",
                        [],
                        |r| r.get(0),
                    )?;
                    assert_eq!(old, 0);
                    Ok(())
                })
                .unwrap();
        };
        check(&alerts);
        assert_eq!(
            alerts.list_alert_deliveries(None, 1).unwrap()[0].message,
            "latest"
        );
        alerts.with_conn(|conn| {
            conn.execute("INSERT INTO alert_deliveries(rule_id,timestamp_ms,status) VALUES ('retention',1,'failed'),('retention',?1,'success')", params![now_ms() as i64])?;
            Ok(())
        }).unwrap();
        drop(alerts);
        let reopened = Alerts::new(&path).unwrap();
        check(&reopened);
        reopened.delete_rule("retention").unwrap();
        assert!(
            reopened
                .list_alert_deliveries(None, 100)
                .unwrap()
                .is_empty()
        );
        drop(reopened);
        cleanup_rule_config_db(Path::new(&path));
    }
}
#[allow(dead_code)]
mod rule_config_crypto {
    use std::fmt;
    use std::fs::{File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use aes_gcm::aead::{Aead, KeyInit, Payload};
    use aes_gcm::{Aes256Gcm, Nonce};
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use rand::RngCore;

    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    pub const RULE_CONFIG_PREFIX: &str = "embypanel:alert-rule-config:v1";

    const KEY_LEN: usize = 32;
    const NONCE_LEN: usize = 12;
    const GCM_TAG_LEN: usize = 16;
    const ENVELOPE_SEP: char = ':';
    const KEY_CREATE_RETRY_LIMIT: u32 = 64;
    const KEY_CREATE_RETRY_SLEEP: Duration = Duration::from_millis(1);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum CryptoError {
        KeyNotFound,
        InvalidKey,
        Io,
        Parse,
        Auth,
        Encrypt,
    }

    impl fmt::Display for CryptoError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(match self {
                Self::KeyNotFound => "encryption key was not found",
                Self::InvalidKey => "encryption key is invalid",
                Self::Io => "key file operation failed",
                Self::Parse => "ciphertext format is invalid",
                Self::Auth => "decryption failed",
                Self::Encrypt => "encryption failed",
            })
        }
    }

    impl std::error::Error for CryptoError {}

    #[derive(Debug)]
    enum KeyReadError {
        NotFound,
        Incomplete,
        Invalid,
        Io,
    }

    impl From<KeyReadError> for CryptoError {
        fn from(error: KeyReadError) -> Self {
            match error {
                KeyReadError::NotFound => Self::KeyNotFound,
                KeyReadError::Incomplete | KeyReadError::Invalid => Self::InvalidKey,
                KeyReadError::Io => Self::Io,
            }
        }
    }

    pub fn key_path(db_path: &Path) -> PathBuf {
        let mut raw = db_path.as_os_str().to_os_string();
        raw.push(".key");
        PathBuf::from(raw)
    }

    pub fn load_existing_key(path: &Path) -> Result<[u8; KEY_LEN], CryptoError> {
        read_key(path).map_err(CryptoError::from)
    }

    pub fn load_or_create_key(path: &Path) -> Result<[u8; KEY_LEN], CryptoError> {
        match read_key(path) {
            Ok(key) => return Ok(key),
            Err(KeyReadError::NotFound) | Err(KeyReadError::Incomplete) => {}
            Err(error) => return Err(error.into()),
        }

        match try_create_key(path) {
            Ok(key) => return Ok(key),
            Err(CreateError::AlreadyExists) => {}
            Err(CreateError::Io) => return Err(CryptoError::Io),
        }

        for _ in 0..KEY_CREATE_RETRY_LIMIT {
            match read_key(path) {
                Ok(key) => return Ok(key),
                Err(KeyReadError::NotFound) | Err(KeyReadError::Incomplete) => {
                    std::thread::yield_now();
                    std::thread::sleep(KEY_CREATE_RETRY_SLEEP);
                }
                Err(error) => return Err(error.into()),
            }
        }

        Err(CryptoError::InvalidKey)
    }

    pub fn encrypt_rule_config(
        plaintext: &str,
        key: &[u8; KEY_LEN],
    ) -> Result<String, CryptoError> {
        if plaintext.is_empty() {
            return Ok(String::new());
        }

        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::Encrypt)?;
        let mut nonce_bytes = [0_u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from(nonce_bytes);
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: RULE_CONFIG_PREFIX.as_bytes(),
                },
            )
            .map_err(|_| CryptoError::Encrypt)?;

        Ok(format!(
            "{RULE_CONFIG_PREFIX}:{nonce}:{ciphertext}",
            nonce = URL_SAFE_NO_PAD.encode(nonce_bytes),
            ciphertext = URL_SAFE_NO_PAD.encode(ciphertext)
        ))
    }

    pub fn decrypt_rule_config(stored: &str, key: &[u8; KEY_LEN]) -> Result<String, CryptoError> {
        if stored.is_empty() {
            return Ok(String::new());
        }

        let (nonce_bytes, ciphertext) = parse_envelope(stored)?;
        let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| CryptoError::Auth)?;
        let nonce = Nonce::from(nonce_bytes);
        let plaintext = cipher
            .decrypt(
                &nonce,
                Payload {
                    msg: &ciphertext,
                    aad: RULE_CONFIG_PREFIX.as_bytes(),
                },
            )
            .map_err(|_| CryptoError::Auth)?;
        String::from_utf8(plaintext).map_err(|_| CryptoError::Auth)
    }

    pub fn parse_envelope(stored: &str) -> Result<([u8; NONCE_LEN], Vec<u8>), CryptoError> {
        let Some(rest) = stored.strip_prefix(RULE_CONFIG_PREFIX) else {
            return Err(CryptoError::Parse);
        };
        let Some(rest) = rest.strip_prefix(ENVELOPE_SEP) else {
            return Err(CryptoError::Parse);
        };

        let mut parts = rest.split(ENVELOPE_SEP);
        let Some(nonce_b64) = parts.next() else {
            return Err(CryptoError::Parse);
        };
        let Some(ciphertext_b64) = parts.next() else {
            return Err(CryptoError::Parse);
        };
        if parts.next().is_some() || nonce_b64.is_empty() || ciphertext_b64.is_empty() {
            return Err(CryptoError::Parse);
        }
        if !is_b64url_nopad(nonce_b64) || !is_b64url_nopad(ciphertext_b64) {
            return Err(CryptoError::Parse);
        }

        let nonce_raw = URL_SAFE_NO_PAD
            .decode(nonce_b64)
            .map_err(|_| CryptoError::Parse)?;
        let ciphertext = URL_SAFE_NO_PAD
            .decode(ciphertext_b64)
            .map_err(|_| CryptoError::Parse)?;
        let nonce_bytes: [u8; NONCE_LEN] = nonce_raw.try_into().map_err(|_| CryptoError::Parse)?;
        if ciphertext.len() < GCM_TAG_LEN {
            return Err(CryptoError::Parse);
        }
        Ok((nonce_bytes, ciphertext))
    }

    fn is_b64url_nopad(value: &str) -> bool {
        !value.is_empty()
            && value.bytes().all(|byte| {
                matches!(
                    byte,
                    b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'
                )
            })
    }

    fn read_key(path: &Path) -> Result<[u8; KEY_LEN], KeyReadError> {
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(KeyReadError::NotFound);
            }
            Err(_) => return Err(KeyReadError::Io),
        };
        if !metadata.file_type().is_file() {
            return Err(KeyReadError::Invalid);
        }
        #[cfg(unix)]
        {
            if metadata.permissions().mode() & 0o777 != 0o600 {
                return Err(KeyReadError::Invalid);
            }
        }

        let mut file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(KeyReadError::NotFound);
            }
            Err(_) => return Err(KeyReadError::Io),
        };
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).map_err(|_| KeyReadError::Io)?;
        if buf.len() == KEY_LEN {
            let mut key = [0_u8; KEY_LEN];
            key.copy_from_slice(&buf);
            Ok(key)
        } else if buf.len() < KEY_LEN {
            Err(KeyReadError::Incomplete)
        } else {
            Err(KeyReadError::Invalid)
        }
    }

    enum CreateError {
        AlreadyExists,
        Io,
    }

    fn try_create_key(path: &Path) -> Result<[u8; KEY_LEN], CreateError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }

        let mut file = match options.open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(CreateError::AlreadyExists);
            }
            Err(_) => return Err(CreateError::Io),
        };

        let mut key = [0_u8; KEY_LEN];
        rand::rng().fill_bytes(&mut key);
        file.write_all(&key).map_err(|_| CreateError::Io)?;
        file.sync_all().map_err(|_| CreateError::Io)?;
        Ok(key)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        use std::fs;
        use std::path::{Path, PathBuf};
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::sync::{Arc, Barrier};
        use std::thread;
        use std::time::{SystemTime, UNIX_EPOCH};

        static TEMP_DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

        struct TestTempDir {
            path: PathBuf,
        }

        impl TestTempDir {
            fn new() -> std::io::Result<Self> {
                let counter = TEMP_DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos();
                let path = std::env::temp_dir().join(format!(
                    "embypanel-alert-crypto-{}-{timestamp}-{counter}",
                    std::process::id()
                ));
                fs::create_dir(&path)?;
                Ok(Self { path })
            }

            fn path(&self) -> &Path {
                &self.path
            }
        }

        impl Drop for TestTempDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.path);
            }
        }

        const KEY: [u8; 32] = [0x11; 32];
        const SECRET_PLAINTEXT: &str = "LEAK_PLAINTEXT_XYZ_DO_NOT_ECHO";

        fn encode(bytes: &[u8]) -> String {
            URL_SAFE_NO_PAD.encode(bytes)
        }

        fn envelope_from_parts(nonce: &[u8], ciphertext: &[u8]) -> String {
            format!(
                "{RULE_CONFIG_PREFIX}:{}:{}",
                encode(nonce),
                encode(ciphertext)
            )
        }

        fn unix_mode(path: &Path) -> u32 {
            #[cfg(unix)]
            {
                fs::metadata(path).unwrap().permissions().mode() & 0o777
            }
            #[cfg(not(unix))]
            {
                let _ = path;
                0o600
            }
        }

        #[test]
        fn nonempty_roundtrip() {
            let stored = encrypt_rule_config(SECRET_PLAINTEXT, &KEY).unwrap();
            assert!(stored.starts_with(&format!("{RULE_CONFIG_PREFIX}:")));
            assert_eq!(
                decrypt_rule_config(&stored, &KEY).unwrap(),
                SECRET_PLAINTEXT
            );
        }

        #[test]
        fn same_plaintext_yields_distinct_envelopes() {
            let first = encrypt_rule_config(SECRET_PLAINTEXT, &KEY).unwrap();
            let second = encrypt_rule_config(SECRET_PLAINTEXT, &KEY).unwrap();
            assert_ne!(first, second);
            assert_eq!(decrypt_rule_config(&first, &KEY).unwrap(), SECRET_PLAINTEXT);
            assert_eq!(
                decrypt_rule_config(&second, &KEY).unwrap(),
                SECRET_PLAINTEXT
            );
        }

        #[test]
        fn tampered_ciphertext_fails_authentication() {
            let stored = encrypt_rule_config(SECRET_PLAINTEXT, &KEY).unwrap();
            let (nonce, mut ciphertext) = parse_envelope(&stored).unwrap();
            ciphertext[0] ^= 0x01;
            let tampered = envelope_from_parts(&nonce, &ciphertext);
            assert!(decrypt_rule_config(&tampered, &KEY).is_err());
        }

        #[test]
        fn empty_passthrough() {
            assert_eq!(encrypt_rule_config("", &KEY).unwrap(), "");
            assert_eq!(decrypt_rule_config("", &KEY).unwrap(), "");
        }

        #[test]
        fn malformed_table_is_rejected() {
            let valid = encrypt_rule_config("ok", &KEY).unwrap();
            let (nonce, ciphertext) = parse_envelope(&valid).unwrap();
            let nonce_b64 = encode(&nonce);
            let ct_b64 = encode(&ciphertext);
            let valid_nonce = [0_u8; 12];
            let short_ct = [0_u8; 15];
            let ok_ct = [0_u8; 16];

            let cases = [
                format!("wrongprefix:{nonce_b64}:{ct_b64}"),
                format!("embypanel:alert-rule-config:v2:{nonce_b64}:{ct_b64}"),
                RULE_CONFIG_PREFIX.to_string(),
                format!("{RULE_CONFIG_PREFIX}:{nonce_b64}"),
                format!("{RULE_CONFIG_PREFIX}:{nonce_b64}:{ct_b64}:extra"),
                format!("{RULE_CONFIG_PREFIX}::{ct_b64}"),
                format!("{RULE_CONFIG_PREFIX}:{nonce_b64}:"),
                format!("{RULE_CONFIG_PREFIX}::"),
                format!("{RULE_CONFIG_PREFIX}:{nonce_b64}={ct_b64}"),
                format!("{RULE_CONFIG_PREFIX}:{nonce_b64}:{ct_b64}="),
                format!("{RULE_CONFIG_PREFIX}:AAAA+AAA:{ct_b64}"),
                format!("{RULE_CONFIG_PREFIX}:{nonce_b64}:AAAA/AAAAAAAAAAAAAAA"),
                format!("{RULE_CONFIG_PREFIX}:{nonce_b64}:AAAA*AAAAAAAAAAAAAAA"),
                format!("{RULE_CONFIG_PREFIX}:{nonce_b64} :{ct_b64}"),
                envelope_from_parts(&[0_u8; 11], &ok_ct),
                envelope_from_parts(&[0_u8; 13], &ok_ct),
                envelope_from_parts(&valid_nonce, &short_ct),
            ];

            for case in cases {
                assert!(
                    decrypt_rule_config(&case, &KEY).is_err(),
                    "expected rejection for {case}"
                );
                if !case.is_empty() {
                    assert!(parse_envelope(&case).is_err(), "parser accepted {case}");
                }
            }
        }

        #[test]
        fn key_path_appends_suffix_without_replacing_extension() {
            assert_eq!(
                key_path(Path::new("/x/a.db")).as_os_str(),
                Path::new("/x/a.db.key").as_os_str()
            );
            assert_eq!(
                key_path(Path::new("/x/foo.bar.db")).as_os_str(),
                Path::new("/x/foo.bar.db.key").as_os_str()
            );
            assert_eq!(
                key_path(Path::new("a.db")).as_os_str(),
                Path::new("a.db.key").as_os_str()
            );
        }

        #[test]
        fn key_create_is_32_bytes_and_unix_0600() {
            let dir = TestTempDir::new().unwrap();
            let path = dir.path().join("panel.db.key");
            let key = load_or_create_key(&path).unwrap();
            assert_eq!(key.len(), 32);
            assert_eq!(fs::read(&path).unwrap(), key);
            assert_eq!(unix_mode(&path), 0o600);
        }

        #[test]
        fn key_survives_reload_and_missing_existing_does_not_create() {
            let dir = TestTempDir::new().unwrap();
            let path = dir.path().join("panel.db.key");
            let created = load_or_create_key(&path).unwrap();
            let reloaded = load_existing_key(&path).unwrap();
            let created_again = load_or_create_key(&path).unwrap();
            assert_eq!(created, reloaded);
            assert_eq!(created, created_again);

            let missing = dir.path().join("missing.db.key");
            assert!(load_existing_key(&missing).is_err());
            assert!(!missing.exists());
        }

        #[test]
        fn wrong_length_is_rejected_and_not_overwritten() {
            let dir = TestTempDir::new().unwrap();
            let path = dir.path().join("short.key");
            fs::write(&path, [0_u8; 16]).unwrap();
            #[cfg(unix)]
            {
                let mut permissions = fs::metadata(&path).unwrap().permissions();
                permissions.set_mode(0o600);
                fs::set_permissions(&path, permissions).unwrap();
            }
            assert!(load_existing_key(&path).is_err());
            assert!(load_or_create_key(&path).is_err());
            assert_eq!(fs::read(&path).unwrap().len(), 16);
        }

        #[test]
        fn non_regular_file_is_rejected() {
            let dir = TestTempDir::new().unwrap();
            assert!(load_existing_key(dir.path()).is_err());
            assert!(load_or_create_key(dir.path()).is_err());
        }

        #[cfg(unix)]
        #[test]
        fn symlink_is_rejected() {
            let dir = TestTempDir::new().unwrap();
            let real = dir.path().join("real.key");
            let link = dir.path().join("link.key");
            let key = load_or_create_key(&real).unwrap();
            std::os::unix::fs::symlink(&real, &link).unwrap();
            assert!(load_existing_key(&link).is_err());
            assert!(load_or_create_key(&link).is_err());
            assert_eq!(load_existing_key(&real).unwrap(), key);
        }

        #[cfg(unix)]
        #[test]
        fn wrong_unix_mode_is_rejected_and_not_overwritten() {
            let dir = TestTempDir::new().unwrap();
            let path = dir.path().join("mode.key");
            let original = [0x5A_u8; 32];
            fs::write(&path, original).unwrap();
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o644);
            fs::set_permissions(&path, permissions).unwrap();
            assert!(load_existing_key(&path).is_err());
            assert!(load_or_create_key(&path).is_err());
            assert_eq!(fs::read(&path).unwrap(), original);
        }

        #[test]
        fn concurrent_create_returns_identical_key() {
            let dir = TestTempDir::new().unwrap();
            let path = Arc::new(dir.path().join("race.key"));
            let barrier = Arc::new(Barrier::new(16));
            let workers: Vec<_> = (0..16)
                .map(|_| {
                    let path = Arc::clone(&path);
                    let barrier = Arc::clone(&barrier);
                    thread::spawn(move || {
                        barrier.wait();
                        load_or_create_key(&path).unwrap()
                    })
                })
                .collect();
            let keys: Vec<[u8; 32]> = workers.into_iter().map(|h| h.join().unwrap()).collect();
            let expected = keys[0];
            for key in &keys {
                assert_eq!(*key, expected);
            }
            assert_eq!(fs::read(path.as_ref()).unwrap(), expected);
            assert_eq!(unix_mode(path.as_ref()), 0o600);
        }

        #[test]
        fn crypto_error_display_is_safe() {
            let dir = TestTempDir::new().unwrap();
            let leak_path = dir.path().join("LEAK_PATH_MARKER.db.key");
            let missing = load_existing_key(&leak_path).unwrap_err();
            let missing_text = missing.to_string();
            assert!(!missing_text.contains("LEAK_PATH_MARKER"));
            assert!(!missing_text.contains(leak_path.to_string_lossy().as_ref()));
            assert!(!missing_text.contains("os error"));
            assert!(!missing_text.contains("No such file"));

            let stored = encrypt_rule_config(SECRET_PLAINTEXT, &KEY).unwrap();
            let parse_err = decrypt_rule_config(
                "embypanel:alert-rule-config:v1:LEAK_CT_FRAGMENT:LEAK_CT_FRAGMENT",
                &KEY,
            )
            .unwrap_err();
            let parse_text = parse_err.to_string();
            assert!(!parse_text.contains("LEAK_CT_FRAGMENT"));
            assert!(!parse_text.contains(SECRET_PLAINTEXT));
            assert!(!parse_text.contains(&stored));

            fs::write(&leak_path, b"short-key-bytes").unwrap();
            let invalid = load_or_create_key(&leak_path).unwrap_err();
            let invalid_text = invalid.to_string();
            assert!(!invalid_text.contains("short-key-bytes"));
            assert!(!invalid_text.contains("LEAK_PATH_MARKER"));
        }

        #[test]
        fn aad_is_bound_into_ciphertext() {
            let mut nonce_bytes = [0_u8; 12];
            nonce_bytes[0] = 7;
            let cipher = Aes256Gcm::new_from_slice(&KEY).unwrap();
            let ciphertext = cipher
                .encrypt(
                    &Nonce::from(nonce_bytes),
                    Payload {
                        msg: b"bound",
                        aad: b"",
                    },
                )
                .unwrap();
            let stored = envelope_from_parts(&nonce_bytes, &ciphertext);
            assert!(decrypt_rule_config(&stored, &KEY).is_err());
        }

        #[test]
        fn parse_envelope_accepts_valid_roundtrip() {
            let stored = encrypt_rule_config("cfg", &KEY).unwrap();
            let (nonce, ciphertext) = parse_envelope(&stored).unwrap();
            assert_eq!(nonce.len(), 12);
            assert!(ciphertext.len() >= 16);
        }
    }
}
