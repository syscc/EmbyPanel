use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Playback,
    General,
}

impl ActivityKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Playback => "playback",
            Self::General => "general",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ActivityLevel {
    Success,
    Info,
    Warn,
    Error,
}

pub struct ActivityLogFilter<'a> {
    pub server_id: Option<&'a str>,
    pub kind: Option<ActivityKind>,
    pub level: Option<&'a str>,
    pub keyword: Option<&'a str>,
    pub since_ms: Option<u128>,
    pub until_ms: Option<u128>,
    pub limit: usize,
}

impl ActivityLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ActivityLogEntry {
    pub id: u64,
    pub timestamp_ms: u128,
    pub kind: String,
    pub level: String,
    pub server_id: Option<String>,
    pub server_name: String,
    pub playback_user: Option<String>,
    pub playback_ip: Option<String>,
    pub message: String,
    pub detail: String,
}

pub struct PlaybackLogRecord<'a> {
    pub level: ActivityLevel,
    pub server_id: Option<&'a str>,
    pub server_name: &'a str,
    pub playback_user: &'a str,
    pub playback_ip: &'a str,
    pub message: String,
    pub detail: String,
}

pub struct ActivityLogStore {
    limit: usize,
    next_id: AtomicU64,
    entries: Mutex<VecDeque<ActivityLogEntry>>,
}

impl ActivityLogStore {
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            next_id: AtomicU64::new(1),
            entries: Mutex::new(VecDeque::new()),
        }
    }

    pub fn record(
        &self,
        kind: ActivityKind,
        level: ActivityLevel,
        server_id: Option<&str>,
        server_name: &str,
        message: impl Into<String>,
        detail: impl Into<String>,
    ) {
        let mut entries = self.entries.lock().expect("activity log mutex poisoned");
        while entries.len() >= self.limit {
            entries.pop_front();
        }
        entries.push_back(ActivityLogEntry {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            timestamp_ms: now_ms(),
            kind: kind.as_str().to_string(),
            level: level.as_str().to_string(),
            server_id: server_id.map(str::to_string),
            server_name: server_name.to_string(),
            playback_user: None,
            playback_ip: None,
            message: message.into(),
            detail: detail.into(),
        });
    }

    pub fn record_playback(&self, record: PlaybackLogRecord<'_>) {
        let mut entries = self.entries.lock().expect("activity log mutex poisoned");
        while entries.len() >= self.limit {
            entries.pop_front();
        }
        entries.push_back(ActivityLogEntry {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            timestamp_ms: now_ms(),
            kind: ActivityKind::Playback.as_str().to_string(),
            level: record.level.as_str().to_string(),
            server_id: record.server_id.map(str::to_string),
            server_name: record.server_name.to_string(),
            playback_user: Some(record.playback_user.to_string()),
            playback_ip: Some(record.playback_ip.to_string()),
            message: record.message,
            detail: record.detail,
        });
    }

    #[cfg(test)]
    pub fn list(
        &self,
        server_id: Option<&str>,
        kind: Option<ActivityKind>,
        limit: usize,
    ) -> Vec<ActivityLogEntry> {
        let entries = self.entries.lock().expect("activity log mutex poisoned");
        entries
            .iter()
            .rev()
            .filter(|entry| {
                server_id.is_none_or(|server_id| entry.server_id.as_deref() == Some(server_id))
                    && kind.is_none_or(|kind| entry.kind == kind.as_str())
            })
            .take(limit.clamp(1, self.limit))
            .cloned()
            .collect()
    }

    pub fn list_filtered(&self, filter: ActivityLogFilter<'_>) -> Vec<ActivityLogEntry> {
        let keyword = filter.keyword.map(str::to_ascii_lowercase);
        let level = filter.level.filter(|value| *value != "all");
        let entries = self.entries.lock().expect("activity log mutex poisoned");
        entries
            .iter()
            .rev()
            .filter(|entry| {
                filter
                    .server_id
                    .is_none_or(|server_id| entry.server_id.as_deref() == Some(server_id))
                    && filter.kind.is_none_or(|kind| entry.kind == kind.as_str())
                    && level.is_none_or(|level| entry.level == level)
                    && filter
                        .since_ms
                        .is_none_or(|since| entry.timestamp_ms >= since)
                    && filter
                        .until_ms
                        .is_none_or(|until| entry.timestamp_ms <= until)
                    && keyword.as_ref().is_none_or(|keyword| {
                        let haystack = format!(
                            "{} {} {} {} {} {}",
                            entry.server_name,
                            entry.playback_user.as_deref().unwrap_or(""),
                            entry.playback_ip.as_deref().unwrap_or(""),
                            entry.level,
                            entry.message,
                            entry.detail
                        )
                        .to_ascii_lowercase();
                        haystack.contains(keyword)
                    })
            })
            .take(filter.limit.clamp(1, self.limit))
            .cloned()
            .collect()
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_latest_entries_with_filters() {
        let store = ActivityLogStore::new(2);
        store.record(
            ActivityKind::General,
            ActivityLevel::Info,
            Some("a"),
            "A",
            "one",
            "",
        );
        store.record(
            ActivityKind::Playback,
            ActivityLevel::Info,
            Some("b"),
            "B",
            "two",
            "",
        );
        store.record(
            ActivityKind::General,
            ActivityLevel::Warn,
            Some("a"),
            "A",
            "three",
            "",
        );

        let all = store.list(None, None, 10);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].message, "three");
        assert_eq!(all[1].message, "two");
        assert_eq!(
            store.list(Some("a"), Some(ActivityKind::General), 10).len(),
            1
        );
    }
}
