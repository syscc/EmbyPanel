use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

use crate::diagnostics::decode_log_detail_url;
use crate::ip_location::IpLocation;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_location: Option<IpLocation>,
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
            ip_location: None,
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
            ip_location: None,
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
        self.list_filtered_scoped(filter, None)
    }

    pub fn list_filtered_scoped(
        &self,
        filter: ActivityLogFilter<'_>,
        allowed_servers: Option<&HashSet<String>>,
    ) -> Vec<ActivityLogEntry> {
        let keyword = filter.keyword.map(str::to_ascii_lowercase);
        let level = filter.level.filter(|value| *value != "all");
        let entries = self.entries.lock().expect("activity log mutex poisoned");
        entries
            .iter()
            .rev()
            .filter(|entry| {
                allowed_servers.is_none_or(|allowed| {
                    entry
                        .server_id
                        .as_ref()
                        .is_none_or(|id| allowed.contains(id))
                }) && filter
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
                        // The detail is stored percent-encoded while the UI shows
                        // it decoded, so a search has to match both forms.
                        haystack.contains(keyword)
                            || decode_log_detail_url(&entry.detail)
                                .to_ascii_lowercase()
                                .contains(keyword)
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
    fn scoped_logs_apply_grants_before_limit_and_keep_global_entries() {
        let store = ActivityLogStore::new(20);
        for id in [Some("a"), None, Some("b"), Some("b")] {
            store.record(
                ActivityKind::General,
                ActivityLevel::Info,
                id,
                "server",
                "entry",
                "",
            );
        }
        let filter = || ActivityLogFilter {
            server_id: None,
            kind: None,
            level: None,
            keyword: None,
            since_ms: None,
            until_ms: None,
            limit: 2,
        };
        let allowed = HashSet::from(["a".to_string()]);
        let rows = store.list_filtered_scoped(filter(), Some(&allowed));
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].server_id, None);
        assert_eq!(rows[1].server_id.as_deref(), Some("a"));
        let empty = HashSet::new();
        let global = store.list_filtered_scoped(filter(), Some(&empty));
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].server_id, None);
        assert!(
            store
                .list_filtered(filter())
                .iter()
                .all(|row| row.server_id.as_deref() == Some("b"))
        );
        let mut explicit = filter();
        explicit.server_id = Some("b");
        assert!(
            store
                .list_filtered_scoped(explicit, Some(&allowed))
                .is_empty()
        );
    }

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

#[cfg(test)]
mod decode_tests {
    use super::{
        ActivityKind, ActivityLevel, ActivityLogFilter, ActivityLogStore, decode_log_detail_url,
    };

    #[test]
    fn log_detail_url_decodes_percent_encoded_file_names() {
        assert_eq!(
            decode_log_detail_url(
                "https://cdn.example.com/hash/%E9%87%8D%E5%99%A8.S01E21.1080p.WEB-DL.H264.DDP%205.1-Pure%40HDSWEB.mkv"
            ),
            "https://cdn.example.com/hash/重器.S01E21.1080p.WEB-DL.H264.DDP 5.1-Pure@HDSWEB.mkv"
        );
    }

    #[test]
    fn log_detail_url_keeps_the_correlation_marker_and_decodes_the_url() {
        assert_eq!(
            decode_log_detail_url(
                "request_id=diag-0123456789abcdef0123456789abcdef https://cdn.example.com/%E9%87%8D%E5%99%A8.mkv"
            ),
            "request_id=diag-0123456789abcdef0123456789abcdef https://cdn.example.com/重器.mkv"
        );
        assert_eq!(
            decode_log_detail_url("request_id=diag-0123456789abcdef0123456789abcdef"),
            "request_id=diag-0123456789abcdef0123456789abcdef"
        );
        assert_eq!(
            decode_log_detail_url("request_id=diag-abc https://cdn.example.com/a%20b.mkv"),
            "request_id=diag-abc https://cdn.example.com/a b.mkv"
        );
        // Any whitespace separates the marker, matching the frontend rule.
        assert_eq!(
            decode_log_detail_url("request_id=diag-abc\thttps://cdn.example.com/a%20b.mkv"),
            "request_id=diag-abc\thttps://cdn.example.com/a b.mkv"
        );
        assert_eq!(
            decode_log_detail_url("request_id=diag-abc  https://cdn.example.com/a%20b.mkv"),
            "request_id=diag-abc  https://cdn.example.com/a b.mkv"
        );
    }

    #[test]
    fn log_detail_url_keeps_credential_separators_encoded() {
        assert_eq!(
            decode_log_detail_url("https://cdn.example.com/x/api_key%3DSECRET.mkv"),
            "https://cdn.example.com/x/api_key%3DSECRET.mkv"
        );
        assert_eq!(
            decode_log_detail_url("https://cdn.example.com/x/a%3Ab.mkv"),
            "https://cdn.example.com/x/a%3Ab.mkv"
        );
    }

    #[test]
    fn log_detail_url_keeps_malformed_segments_fully_encoded() {
        assert_eq!(
            decode_log_detail_url("https://cdn.example.com/a%ZZb%20c.mkv"),
            "https://cdn.example.com/a%ZZb%20c.mkv"
        );
    }

    #[test]
    fn log_detail_url_keeps_query_and_fragment_encoded() {
        assert_eq!(
            decode_log_detail_url("https://cdn.example.com/a%20b.mkv?token=%E9%87%8D#f"),
            "https://cdn.example.com/a b.mkv?token=%E9%87%8D#f"
        );
    }

    #[test]
    fn log_detail_url_leaves_unsafe_and_malformed_escapes_alone() {
        assert_eq!(
            decode_log_detail_url("https://cdn.example.com/dir/a%2Fb.mkv"),
            "https://cdn.example.com/dir/a%2Fb.mkv"
        );
        assert_eq!(
            decode_log_detail_url("https://cdn.example.com/bad%ZZ.mkv"),
            "https://cdn.example.com/bad%ZZ.mkv"
        );
        assert_eq!(
            decode_log_detail_url("https://cdn.example.com/trunc%E9%87.mkv"),
            "https://cdn.example.com/trunc%E9%87.mkv"
        );
        assert_eq!(
            decode_log_detail_url("https://%65xample.com/a.mkv"),
            "https://%65xample.com/a.mkv"
        );
    }

    #[test]
    fn log_detail_url_ignores_non_url_text() {
        assert_eq!(
            decode_log_detail_url("重器 S01E21 直接播放"),
            "重器 S01E21 直接播放"
        );
        assert_eq!(decode_log_detail_url(""), "");
        assert_eq!(
            decode_log_detail_url("ftp://cdn.example.com/a%20b.mkv"),
            "ftp://cdn.example.com/a%20b.mkv"
        );
    }

    #[test]
    fn log_detail_url_keeps_control_characters_encoded() {
        assert_eq!(
            decode_log_detail_url("https://cdn.example.com/a%0Ab.mkv"),
            "https://cdn.example.com/a%0Ab.mkv"
        );
        assert_eq!(
            decode_log_detail_url("https://cdn.example.com/a%0D%0Ab.mkv"),
            "https://cdn.example.com/a%0D%0Ab.mkv"
        );
        assert_eq!(
            decode_log_detail_url("https://cdn.example.com/a%09b.mkv"),
            "https://cdn.example.com/a%09b.mkv"
        );
    }

    fn filter(keyword: &str) -> ActivityLogFilter<'_> {
        ActivityLogFilter {
            server_id: None,
            kind: None,
            level: None,
            keyword: Some(keyword),
            since_ms: None,
            until_ms: None,
            limit: 20,
        }
    }

    #[test]
    fn keyword_matches_the_decoded_detail_as_well_as_the_stored_form() {
        let store = ActivityLogStore::new(20);
        store.record(
            ActivityKind::Playback,
            ActivityLevel::Success,
            Some("srv-a"),
            "Emby",
            "直链跳转",
            "request_id=diag-0123456789abcdef0123456789abcdef https://cdn.example.com/%E9%87%8D%E5%99%A8.S01E21.mkv",
        );

        // The decoded form is what the UI shows.
        assert_eq!(store.list_filtered(filter("重器")).len(), 1);
        // The stored percent-encoded form keeps matching as before.
        assert_eq!(store.list_filtered(filter("%E9%87%8D")).len(), 1);
        // Unrelated keywords do not match.
        assert_eq!(store.list_filtered(filter("不存在")).len(), 0);
    }

    #[test]
    fn keyword_matches_plain_fields() {
        let store = ActivityLogStore::new(20);
        store.record(
            ActivityKind::General,
            ActivityLevel::Info,
            Some("srv-a"),
            "Emby",
            "反代监听启动",
            "http://127.0.0.1:18090",
        );
        assert_eq!(store.list_filtered(filter("反代监听")).len(), 1);
        assert_eq!(store.list_filtered(filter("18090")).len(), 1);
    }
}
