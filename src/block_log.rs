use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use crate::{
    db::{self, ProxyRequestDetail, SettingsStore},
    error::AppResult,
    ip_location::IpLocation,
};

const MAX_BYTES: u64 = 10 * 1024 * 1024;
const MAX_BACKUPS: u64 = 5;

pub struct BlockLogStore {
    path: PathBuf,
    lock: Mutex<()>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BlockLogEntry {
    pub id: i64,
    #[serde(default = "default_event_type")]
    pub event_type: String,
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
    #[serde(default)]
    pub ip_location_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_location: Option<IpLocation>,
    pub cache_hit: bool,
    pub blocked: bool,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct BlockLogInsert<'a> {
    pub event_type: &'a str,
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
    pub ip_location_text: &'a str,
    pub cache_hit: bool,
    pub detail: &'a str,
}

#[derive(Debug, Clone, Default)]
pub struct BlockLogFilter<'a> {
    pub server_id: Option<&'a str>,
    pub path_type: Option<&'a str>,
    pub keyword: Option<&'a str>,
    pub since_ms: Option<u128>,
    pub until_ms: Option<u128>,
    pub limit: usize,
}

impl BlockLogStore {
    pub fn new() -> Self {
        let dir = db::data_dir().join("logs");
        let _ = fs::create_dir_all(&dir);
        Self {
            path: dir.join("block.log"),
            lock: Mutex::new(()),
        }
    }

    pub fn record(&self, record: BlockLogInsert<'_>) {
        let _guard = self.lock.lock().expect("block log mutex poisoned");
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = rotate_if_needed(&self.path);
        let entry = BlockLogEntry {
            id: record.timestamp_ms as i64,
            event_type: normalize_event_type(record.event_type).to_string(),
            timestamp_ms: record.timestamp_ms,
            server_id: record.server_id.to_string(),
            server_name: record.server_name.to_string(),
            port: record.port,
            method: record.method.to_string(),
            path: record.path.to_string(),
            path_type: record.path_type.to_string(),
            status_code: record.status_code,
            outcome: record.outcome.to_string(),
            duration_ms: record.duration_ms,
            playback_user: record.playback_user.to_string(),
            playback_ip: record.playback_ip.to_string(),
            ip_location_text: record.ip_location_text.to_string(),
            ip_location: None,
            cache_hit: record.cache_hit,
            blocked: true,
            detail: record.detail.to_string(),
        };
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let line = render_log_line(&entry);
            let _ = writeln!(file, "{line}");
        }
    }

    pub fn bootstrap_from_proxy_logs(&self, settings_store: &SettingsStore) {
        if self.path.exists() {
            return;
        }
        let Ok(rows) = settings_store.list_blocked_proxy_request_details(500) else {
            return;
        };
        for row in rows.into_iter().rev() {
            self.record(proxy_detail_to_insert(&row));
        }
    }

    pub fn list(&self, filter: BlockLogFilter<'_>) -> AppResult<Vec<BlockLogEntry>> {
        let _guard = self.lock.lock().expect("block log mutex poisoned");
        let keyword = filter
            .keyword
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase);
        let mut entries = Vec::new();
        for path in log_paths(&self.path) {
            if !path.exists() {
                continue;
            }
            let file = File::open(path)?;
            let mut file_entries = BufReader::new(file)
                .lines()
                .map_while(Result::ok)
                .filter_map(|line| parse_log_line(&line))
                .collect::<Vec<_>>();
            file_entries.sort_by(|left, right| {
                right
                    .timestamp_ms
                    .cmp(&left.timestamp_ms)
                    .then_with(|| right.id.cmp(&left.id))
            });
            for entry in file_entries {
                if !matches_filter(&entry, &filter, keyword.as_deref()) {
                    continue;
                }
                entries.push(entry);
                if entries.len() >= filter.limit.clamp(1, 500) {
                    return Ok(entries);
                }
            }
        }
        Ok(entries)
    }
}

fn proxy_detail_to_insert(row: &ProxyRequestDetail) -> BlockLogInsert<'_> {
    BlockLogInsert {
        timestamp_ms: row.timestamp_ms,
        event_type: "request",
        server_id: &row.server_id,
        server_name: &row.server_name,
        port: row.port,
        method: &row.method,
        path: &row.path,
        path_type: &row.path_type,
        status_code: row.status_code,
        outcome: &row.outcome,
        duration_ms: row.duration_ms,
        playback_user: &row.playback_user,
        playback_ip: &row.playback_ip,
        ip_location_text: "",
        cache_hit: row.cache_hit,
        detail: &row.detail,
    }
}

fn parse_log_line(line: &str) -> Option<BlockLogEntry> {
    if let Ok(entry) = serde_json::from_str::<BlockLogEntry>(line) {
        return Some(entry);
    }
    let raw_fields = line.split('\t').collect::<Vec<_>>();
    let mut fields = raw_fields.iter().copied();
    let timestamp_ms = fields.next()?.parse::<u128>().ok()?;
    let has_ip_location_text = raw_fields.len() >= 18;
    let event_type = fields.next().unwrap_or("request").to_string();
    let server_id = fields.next().unwrap_or("").to_string();
    let server_name = fields.next().unwrap_or("--").to_string();
    let port = fields
        .next()
        .unwrap_or("0")
        .parse::<u16>()
        .unwrap_or_default();
    let playback_user = fields.next().unwrap_or("--").to_string();
    let playback_ip = fields.next().unwrap_or("--").to_string();
    let ip_location_text = if has_ip_location_text {
        fields.next().unwrap_or("").to_string()
    } else {
        String::new()
    };
    Some(BlockLogEntry {
        id: timestamp_ms as i64,
        timestamp_ms,
        event_type,
        server_id,
        server_name,
        port,
        playback_user,
        playback_ip,
        ip_location_text,
        method: fields.next().unwrap_or("").to_string(),
        path: fields.next().unwrap_or("").to_string(),
        path_type: fields.next().unwrap_or("proxy").to_string(),
        status_code: fields
            .next()
            .unwrap_or("0")
            .parse::<u16>()
            .unwrap_or_default(),
        outcome: fields.next().unwrap_or("--").to_string(),
        duration_ms: fields
            .next()
            .unwrap_or("0")
            .parse::<u128>()
            .unwrap_or_default(),
        cache_hit: fields.next().unwrap_or("false") == "true",
        blocked: fields.next().unwrap_or("true") != "false",
        detail: fields.next().unwrap_or("").to_string(),
        ip_location: None,
    })
}

fn render_log_line(entry: &BlockLogEntry) -> String {
    [
        entry.timestamp_ms.to_string(),
        entry.event_type.clone(),
        entry.server_id.clone(),
        entry.server_name.clone(),
        entry.port.to_string(),
        entry.playback_user.clone(),
        entry.playback_ip.clone(),
        entry.ip_location_text.clone(),
        entry.method.clone(),
        entry.path.clone(),
        entry.path_type.clone(),
        entry.status_code.to_string(),
        entry.outcome.clone(),
        entry.duration_ms.to_string(),
        entry.cache_hit.to_string(),
        entry.blocked.to_string(),
        entry.detail.clone(),
        human_log_summary(entry),
    ]
    .into_iter()
    .map(sanitize_field)
    .collect::<Vec<_>>()
    .join("\t")
}

fn human_log_summary(entry: &BlockLogEntry) -> String {
    let ip = if entry.playback_ip.trim().is_empty() {
        "--"
    } else if !entry.ip_location_text.trim().is_empty() {
        return human_log_summary_with_ip(
            entry,
            &format!("{} · {}", entry.playback_ip, entry.ip_location_text),
        );
    } else {
        &entry.playback_ip
    };
    human_log_summary_with_ip(entry, ip)
}

fn human_log_summary_with_ip(entry: &BlockLogEntry, ip: &str) -> String {
    let time = format_time(entry.timestamp_ms);
    let user = if entry.playback_user.trim().is_empty() {
        "--"
    } else {
        &entry.playback_user
    };
    match entry.event_type.as_str() {
        "block" => format!(
            "{time} 封禁动作 服务器 {server} 用户 {user} IP {ip} 方式 {outcome} {detail}",
            server = entry.server_name,
            outcome = entry.outcome,
            detail = entry.detail
        ),
        "unblock" => format!(
            "{time} 解除封禁 服务器 {server} 用户 {user} IP {ip} {detail}",
            server = entry.server_name,
            detail = entry.detail
        ),
        _ => format!(
            "{time} 已拦截 服务器 {server} 用户 {user} IP {ip} {method} {path} {outcome} {path_type} HTTP {status} {duration}ms {cache} {detail}",
            server = entry.server_name,
            method = entry.method,
            path = entry.path,
            outcome = entry.outcome,
            path_type = path_type_label(&entry.path_type),
            status = entry.status_code,
            duration = entry.duration_ms,
            cache = if entry.cache_hit {
                "缓存命中"
            } else {
                "未命中"
            },
            detail = entry.detail
        ),
    }
}

fn sanitize_field(value: String) -> String {
    value.replace(['\t', '\r', '\n'], " ")
}

fn format_time(timestamp_ms: u128) -> String {
    let offset = crate::tz_offset_seconds(&std::env::var("TZ").unwrap_or_default()) as i64;
    let time = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(timestamp_ms as i64)
        .unwrap_or_default()
        + chrono::Duration::seconds(offset);
    time.format("%Y/%-m/%-d %H:%M:%S").to_string()
}

fn path_type_label(path_type: &str) -> &'static str {
    match path_type {
        "video_stream" => "视频流",
        "playback_info" => "播放信息",
        "system_info" => "系统信息",
        "base_html_player" => "播放器脚本",
        "rate_limit_action" => "封禁动作",
        _ => "普通代理",
    }
}

fn matches_filter(
    entry: &BlockLogEntry,
    filter: &BlockLogFilter<'_>,
    keyword: Option<&str>,
) -> bool {
    if filter
        .server_id
        .is_some_and(|server_id| entry.server_id != server_id)
    {
        return false;
    }
    if filter
        .path_type
        .is_some_and(|path_type| entry.path_type != path_type)
    {
        return false;
    }
    if filter
        .since_ms
        .is_some_and(|since| entry.timestamp_ms < since)
    {
        return false;
    }
    if filter
        .until_ms
        .is_some_and(|until| entry.timestamp_ms > until)
    {
        return false;
    }
    if let Some(keyword) = keyword {
        let haystack = format!(
            "{} {} {} {} {} {} {} {} {}",
            entry.event_type,
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
            return false;
        }
    }
    true
}

fn default_event_type() -> String {
    "request".to_string()
}

fn normalize_event_type(event_type: &str) -> &'static str {
    match event_type {
        "block" => "block",
        "unblock" => "unblock",
        _ => "request",
    }
}

fn rotate_if_needed(path: &Path) -> std::io::Result<()> {
    if fs::metadata(path)
        .map(|metadata| metadata.len() < MAX_BYTES)
        .unwrap_or(true)
    {
        return Ok(());
    }
    for index in (1..=MAX_BACKUPS).rev() {
        let from = backup_path(path, index);
        let to = backup_path(path, index + 1);
        if index == MAX_BACKUPS {
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

fn log_paths(path: &Path) -> Vec<PathBuf> {
    std::iter::once(path.to_path_buf())
        .chain((1..=MAX_BACKUPS).map(|index| backup_path(path, index)))
        .collect()
}

fn backup_path(path: &Path, index: u64) -> PathBuf {
    PathBuf::from(format!("{}.{}", path.display(), index))
}
