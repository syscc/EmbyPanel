use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

use crate::{config::Config, error::AppResult, ip_location::IpLocation};

const DEFAULT_DATABASE_PATH: &str = "data/embypanel.db";
const CONTAINER_DATABASE_PATH: &str = "/data/embypanel.db";

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
        let store = Self { path };
        store.ensure_schema()?;
        Ok(store)
    }

    #[cfg(test)]
    pub fn open_for_test(path: PathBuf) -> AppResult<Self> {
        let store = Self { path };
        store.ensure_schema()?;
        Ok(store)
    }

    pub fn load_or_default_config(&self) -> AppResult<Config> {
        if let Some(config) = self.load_config()? {
            return Ok(config);
        }

        let config = Config::default_runtime();
        Ok(config)
    }

    pub fn load_config(&self) -> AppResult<Option<Config>> {
        let conn = self.connection()?;
        let json = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'runtime_config'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        let Some(json) = json else {
            return Ok(None);
        };

        let mut config: Config = serde_json::from_str(&json)?;
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
        let conn = self.connection()?;
        let json = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        json.map(|value| serde_json::from_str(&value).map_err(Into::into))
            .transpose()
    }

    pub fn save_setting_json<T: Serialize>(&self, key: &str, value: &T) -> AppResult<()> {
        let conn = self.connection()?;
        let json = serde_json::to_string_pretty(value)?;
        conn.execute(
            r#"
            INSERT INTO settings (key, value, updated_at)
            VALUES (?1, ?2, CURRENT_TIMESTAMP)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at = CURRENT_TIMESTAMP
            "#,
            params![key, json],
        )?;
        Ok(())
    }

    pub fn has_admin(&self) -> AppResult<bool> {
        let conn = self.connection()?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM admin_users", [], |row| row.get(0))?;
        Ok(count > 0)
    }

    pub fn create_admin(&self, username: &str, password_hash: &str) -> AppResult<i64> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            INSERT INTO admin_users (username, password_hash, enabled)
            VALUES (?1, ?2, TRUE)
            "#,
            params![username, password_hash],
        )?;
        Ok(conn.last_insert_rowid())
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

    pub fn update_admin_password(&self, admin_user_id: i64, password_hash: &str) -> AppResult<()> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            UPDATE admin_users
            SET password_hash = ?2
            WHERE id = ?1 AND enabled = TRUE
            "#,
            params![admin_user_id, password_hash],
        )?;
        Ok(())
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

    pub fn create_session(
        &self,
        admin_user_id: i64,
        token_hash: &str,
        expires_at: i64,
    ) -> AppResult<()> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            INSERT INTO admin_sessions (admin_user_id, token_hash, expires_at)
            VALUES (?1, ?2, ?3)
            "#,
            params![admin_user_id, token_hash, expires_at],
        )?;
        Ok(())
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
        self.prune_audit_logs(&conn, 90, 20000)?;
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

    pub fn increment_request_stats(
        &self,
        server_id: &str,
        server_name: &str,
        port: u16,
        kind: RequestStatKind,
    ) -> AppResult<()> {
        let conn = self.connection()?;
        let date = local_date();
        let (requests, redirects, cache_hits, blocks, errors) = kind.delta();
        conn.execute(
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
                now_ms() as i64
            ],
        )?;
        self.prune_request_stats(&conn, 90)?;
        Ok(())
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

    pub fn record_proxy_request_detail(
        &self,
        record: ProxyRequestDetailInsert<'_>,
    ) -> AppResult<()> {
        let conn = self.connection()?;
        conn.execute(
            r#"
            INSERT INTO proxy_request_logs
                (timestamp_ms, server_id, server_name, port, method, path, path_type,
                 status_code, outcome, duration_ms, playback_user, playback_ip,
                 cache_hit, blocked, detail)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            "#,
            params![
                record.timestamp_ms as i64,
                record.server_id,
                record.server_name,
                record.port,
                record.method,
                record.path,
                record.path_type,
                record.status_code,
                record.outcome,
                record.duration_ms as i64,
                record.playback_user,
                record.playback_ip,
                record.cache_hit,
                record.blocked,
                record.detail,
            ],
        )?;
        self.prune_proxy_request_logs(&conn, 7, 20000)?;
        Ok(())
    }

    pub fn list_proxy_request_details(
        &self,
        filter: ProxyRequestDetailFilter<'_>,
    ) -> AppResult<Vec<ProxyRequestDetail>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            r#"
            SELECT id, timestamp_ms, server_id, server_name, port, method, path, path_type,
                   status_code, outcome, duration_ms, playback_user, playback_ip,
                   cache_hit, blocked, detail
            FROM proxy_request_logs
            ORDER BY timestamp_ms DESC, id DESC
            LIMIT 2000
            "#,
        )?;
        let rows = stmt.query_map([], |row| {
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
        let keyword = filter.keyword.map(str::to_ascii_lowercase);
        let mut entries = Vec::new();
        for row in rows {
            let entry = row?;
            if filter
                .server_id
                .is_some_and(|server_id| entry.server_id != server_id)
            {
                continue;
            }
            if filter
                .path_type
                .is_some_and(|path_type| entry.path_type != path_type)
            {
                continue;
            }
            if filter
                .since_ms
                .is_some_and(|since| entry.timestamp_ms < since)
            {
                continue;
            }
            if filter
                .until_ms
                .is_some_and(|until| entry.timestamp_ms > until)
            {
                continue;
            }
            if let Some(keyword) = keyword.as_ref() {
                let haystack = format!(
                    "{} {} {} {} {} {} {} {}",
                    entry.server_name,
                    entry.method,
                    entry.path,
                    entry.path_type,
                    entry.outcome,
                    entry.playback_user,
                    entry.playback_ip,
                    entry.detail
                )
                .to_ascii_lowercase();
                if !haystack.contains(keyword) {
                    continue;
                }
            }
            entries.push(entry);
            if entries.len() >= filter.limit.clamp(1, 500) {
                break;
            }
        }
        Ok(entries)
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
        let conn = Connection::open(&self.path)?;
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
        let _ = self.migrate(&conn);
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

pub struct ProxyRequestDetailInsert<'a> {
    pub timestamp_ms: u128,
    pub server_id: &'a str,
    pub server_name: &'a str,
    pub port: u16,
    pub method: &'a str,
    pub path: &'a str,
    pub path_type: &'a str,
    pub status_code: u16,
    pub outcome: &'a str,
    pub duration_ms: u128,
    pub playback_user: &'a str,
    pub playback_ip: &'a str,
    pub cache_hit: bool,
    pub blocked: bool,
    pub detail: &'a str,
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

fn local_date() -> String {
    local_date_offset(0)
}

fn local_date_offset(offset_days: i64) -> String {
    let now = chrono::Utc::now() + chrono::Duration::hours(8) + chrono::Duration::days(offset_days);
    now.format("%Y-%m-%d").to_string()
}
