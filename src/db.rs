use std::{fs, path::PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

use crate::{config::Config, error::AppResult};

const DEFAULT_DATABASE_PATH: &str = "data/embypanel.db";
const CONTAINER_DATABASE_PATH: &str = "/data/embypanel.db";

pub fn database_path() -> PathBuf {
    if PathBuf::from("/data").is_dir() {
        PathBuf::from(CONTAINER_DATABASE_PATH)
    } else {
        PathBuf::from(DEFAULT_DATABASE_PATH)
    }
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

    fn ensure_schema(&self) -> AppResult<()> {
        let conn = Connection::open(&self.path)?;
        if self.schema_missing(&conn)? {
            self.migrate(&conn)?;
        }
        Ok(())
    }

    fn schema_missing(&self, conn: &Connection) -> AppResult<bool> {
        let count: i64 = conn.query_row(
            r#"
            SELECT COUNT(*)
            FROM sqlite_master
            WHERE type = 'table'
              AND name IN ('settings', 'admin_users', 'admin_sessions')
            "#,
            [],
            |row| row.get(0),
        )?;
        Ok(count < 3)
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
            "#,
        )?;
        Ok(())
    }

    fn connection(&self) -> Result<Connection, rusqlite::Error> {
        let conn = Connection::open(&self.path)?;
        if let Ok(true) = self.schema_missing(&conn) {
            let _ = self.migrate(&conn);
        }
        Ok(conn)
    }
}

pub struct AdminPasswordHash {
    pub admin_user_id: i64,
    pub password_hash: String,
}

#[derive(Debug, Serialize)]
pub struct SetupStatus {
    pub initialized: bool,
}
