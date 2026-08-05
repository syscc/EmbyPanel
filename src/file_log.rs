use std::{
    fs::{self, File, OpenOptions},
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
    state: Mutex<FileLogState>,
}

struct FileLogState {
    config: SystemLogConfig,
    file: Option<File>,
    size: u64,
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
        let path = dir.join("embypanel.log");
        let size = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        Self {
            path,
            state: Mutex::new(FileLogState {
                config,
                file: None,
                size,
            }),
        }
    }

    pub fn config(&self) -> SystemLogConfig {
        self.state
            .lock()
            .expect("file log state mutex poisoned")
            .config
            .clone()
    }

    pub fn update_config(
        &self,
        settings_store: &SettingsStore,
        config: SystemLogConfig,
    ) -> AppResult<SystemLogConfig> {
        let config = normalize_config(config);
        settings_store.save_setting_json(SETTING_KEY, &config)?;
        self.state
            .lock()
            .expect("file log state mutex poisoned")
            .config = config.clone();
        Ok(config)
    }

    pub fn write(&self, level: &str, message: &str, detail: &str) {
        let mut state = self.state.lock().expect("file log state mutex poisoned");
        if !level_enabled(level, &state.config) {
            return;
        }
        let max_bytes = state.config.max_size_mb.max(1) * 1024 * 1024;
        if state.size >= max_bytes {
            state.file.take();
            if rotate_log_files(&self.path, state.config.max_backups).is_ok() {
                state.size = 0;
            } else {
                state.size = fs::metadata(&self.path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
            }
        }
        if state.file.is_none() {
            state.file = open_log_file(&self.path).ok();
        }
        let mut line = render_line(&state.config, level, message, detail);
        line.push('\n');
        let written = state
            .file
            .as_mut()
            .is_some_and(|file| file.write_all(line.as_bytes()).is_ok());
        if written {
            state.size = state.size.saturating_add(line.len() as u64);
        } else {
            state.file = None;
            state.size = fs::metadata(&self.path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
        }
    }
}

fn open_log_file(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

fn rotate_log_files(path: &Path, max_backups: u64) -> std::io::Result<()> {
    let max_backups = max_backups.clamp(1, 99);
    for index in (1..=max_backups).rev() {
        let from = backup_path(path, index);
        let to = backup_path(path, index + 1);
        if index == max_backups {
            let _ = fs::remove_file(&from);
        } else if from.exists() {
            let _ = fs::rename(&from, &to);
        }
    }
    if path.exists() {
        fs::rename(path, backup_path(path, 1))?;
    }
    Ok(())
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
