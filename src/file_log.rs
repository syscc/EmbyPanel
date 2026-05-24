use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use crate::{
    db::{self, SettingsStore},
    error::AppResult,
};

pub const SETTING_KEY: &str = "system_log_config";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SystemLogConfig {
    #[serde(default)]
    pub debug_mode: bool,
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default = "default_max_size_mb")]
    pub max_size_mb: u64,
    #[serde(default = "default_max_backups")]
    pub max_backups: u64,
    #[serde(default = "default_format")]
    pub format: String,
}

impl Default for SystemLogConfig {
    fn default() -> Self {
        Self {
            debug_mode: false,
            level: default_log_level(),
            max_size_mb: default_max_size_mb(),
            max_backups: default_max_backups(),
            format: default_format(),
        }
    }
}

pub struct FileLogStore {
    path: PathBuf,
    config: Mutex<SystemLogConfig>,
}

impl FileLogStore {
    pub fn new(settings_store: &SettingsStore) -> Self {
        let config = settings_store
            .load_setting_json::<SystemLogConfig>(SETTING_KEY)
            .ok()
            .flatten()
            .map(normalize_config)
            .unwrap_or_default();
        let dir = db::data_dir().join("logs");
        let _ = fs::create_dir_all(&dir);
        Self {
            path: dir.join("embypanel.log"),
            config: Mutex::new(config),
        }
    }

    pub fn config(&self) -> SystemLogConfig {
        self.config
            .lock()
            .expect("file log config mutex poisoned")
            .clone()
    }

    pub fn update_config(
        &self,
        settings_store: &SettingsStore,
        config: SystemLogConfig,
    ) -> AppResult<SystemLogConfig> {
        let config = normalize_config(config);
        settings_store.save_setting_json(SETTING_KEY, &config)?;
        *self.config.lock().expect("file log config mutex poisoned") = config.clone();
        Ok(config)
    }

    pub fn write(&self, level: &str, message: &str, detail: &str) {
        let config = self.config();
        if !level_enabled(level, &config) {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = self.rotate_if_needed(&config);
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let line = render_line(&config, level, message, detail);
            let _ = writeln!(file, "{line}");
        }
    }

    fn rotate_if_needed(&self, config: &SystemLogConfig) -> std::io::Result<()> {
        let max_bytes = config.max_size_mb.max(1) * 1024 * 1024;
        if fs::metadata(&self.path)
            .map(|metadata| metadata.len() < max_bytes)
            .unwrap_or(true)
        {
            return Ok(());
        }
        let max_backups = config.max_backups.clamp(1, 99);
        for index in (1..=max_backups).rev() {
            let from = backup_path(&self.path, index);
            let to = backup_path(&self.path, index + 1);
            if index == max_backups {
                let _ = fs::remove_file(&from);
            } else if from.exists() {
                let _ = fs::rename(&from, &to);
            }
        }
        if self.path.exists() {
            fs::rename(&self.path, backup_path(&self.path, 1))?;
        }
        Ok(())
    }
}

fn backup_path(path: &Path, index: u64) -> PathBuf {
    PathBuf::from(format!("{}.{}", path.display(), index))
}

pub fn normalize_config(mut config: SystemLogConfig) -> SystemLogConfig {
    config.level = normalize_level(&config.level);
    config.max_size_mb = config.max_size_mb.clamp(1, 1024);
    config.max_backups = config.max_backups.clamp(1, 99);
    if config.format.trim().is_empty() {
        config.format = default_format();
    }
    config.debug_mode = config.level == "debug";
    config
}

fn render_line(config: &SystemLogConfig, level: &str, message: &str, detail: &str) -> String {
    let time = chrono::Utc::now()
        + chrono::Duration::seconds(crate::tz_offset_seconds(
            &std::env::var("TZ").unwrap_or_default(),
        ) as i64);
    let asctime = time.format("%Y-%m-%d %H:%M:%S%.3f").to_string();
    let rendered_message = format!("{message} {detail}");
    config
        .format
        .replace("%(levelname)s", &level.to_ascii_uppercase())
        .replace("%(asctime)s", &asctime)
        .replace("%(message)s", rendered_message.trim())
}

fn level_enabled(level: &str, config: &SystemLogConfig) -> bool {
    level_weight(level) >= level_weight(&config.level)
}

fn level_weight(level: &str) -> u8 {
    match normalize_level(level).as_str() {
        "debug" => 10,
        "info" => 20,
        "warning" => 30,
        "error" => 40,
        "critical" => 50,
        _ => 20,
    }
}

pub fn normalize_level(level: &str) -> String {
    match level.trim().to_ascii_lowercase().as_str() {
        "debug" => "debug".to_string(),
        "warn" | "warning" => "warning".to_string(),
        "error" => "error".to_string(),
        "critical" => "critical".to_string(),
        _ => "info".to_string(),
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_max_size_mb() -> u64 {
    5
}

fn default_max_backups() -> u64 {
    10
}

fn default_format() -> String {
    "[%(levelname)s] %(asctime)s - %(message)s".to_string()
}
