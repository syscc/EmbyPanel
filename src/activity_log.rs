use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use serde::Serialize;

use crate::diagnostics::{decode_log_detail_url, sanitize_diagnostic_text, sanitize_media_label};
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

#[derive(Debug, Clone, Default, Serialize)]
pub struct PlaybackMedia {
    pub media_id: String,
    pub name: Option<String>,
    pub series_name: Option<String>,
    pub season_number: Option<i32>,
    pub episode_number: Option<i32>,
}

impl PlaybackMedia {
    pub fn sanitized(mut self) -> Self {
        fn clean(value: &str) -> String {
            sanitize_diagnostic_text(value)
                .trim()
                .chars()
                .take(256)
                .collect()
        }
        self.media_id = clean(&self.media_id);
        self.name = self
            .name
            .map(|name| sanitize_media_label(&name))
            .filter(|name| !name.is_empty());
        self.series_name = self
            .series_name
            .map(|name| sanitize_media_label(&name))
            .filter(|name| !name.is_empty());
        self
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
    pub playback_group_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_location: Option<IpLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media: Option<PlaybackMedia>,
    pub message: String,
    pub detail: String,
}

pub struct PlaybackLogRecord<'a> {
    pub level: ActivityLevel,
    pub server_id: Option<&'a str>,
    pub server_name: &'a str,
    pub playback_user: &'a str,
    pub playback_ip: &'a str,
    pub playback_group_id: Option<&'a str>,
    pub media: Option<PlaybackMedia>,
    pub request_path: Option<&'a str>,
    pub range: Option<&'a str>,
    pub message: String,
    pub detail: String,
}

fn playback_range_for_log(value: &str) -> String {
    // Do not log arbitrary header text, even when it looks partly like a byte range.
    let valid = value.len() <= 256
        && !value.chars().any(char::is_control)
        && value.strip_prefix("bytes=").is_some_and(|ranges| {
            ranges.split(',').all(|range| {
                let Some((start, end)) = range.trim().split_once('-') else {
                    return false;
                };
                let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
                (!start.is_empty() || !end.is_empty()) && digits(start) && digits(end)
            })
        });
    if valid {
        value.to_string()
    } else {
        "[invalid]".to_string()
    }
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
            playback_group_id: None,
            ip_location: None,
            media: None,
            message: message.into(),
            detail: detail.into(),
        });
    }

    pub fn record_playback(&self, record: PlaybackLogRecord<'_>) {
        let mut detail = record.detail;
        if let Some(path) = record.request_path {
            let path: String = crate::diagnostics::sanitize_path(path)
                .chars()
                .take(512)
                .collect();
            if !path.is_empty() {
                detail.push_str(&format!(" path={path}"));
            }
        }
        if let Some(range) = record.range {
            detail.push_str(&format!(" range={}", playback_range_for_log(range)));
        }
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
            playback_group_id: record
                .playback_group_id
                .filter(|value| {
                    value.len() == 69
                        && value.strip_prefix("play-").is_some_and(|digest| {
                            digest
                                .bytes()
                                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                        })
                })
                .map(str::to_string),
            ip_location: None,
            media: record.media.map(PlaybackMedia::sanitized),
            message: record.message,
            detail,
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
                            "{} {} {} {} {} {} {} {} {}",
                            entry
                                .media
                                .as_ref()
                                .map_or("", |media| media.media_id.as_str()),
                            entry
                                .media
                                .as_ref()
                                .and_then(|media| media.name.as_deref())
                                .unwrap_or(""),
                            entry
                                .media
                                .as_ref()
                                .and_then(|media| media.series_name.as_deref())
                                .unwrap_or(""),
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
    fn playback_request_context_is_sanitized_at_storage() {
        let oversized_range = format!("bytes=0-{}", "9".repeat(256));
        for (range, expected) in [
            (None, None),
            (Some("bytes=0-1023"), Some("bytes=0-1023")),
            (Some("bytes=1024-"), Some("bytes=1024-")),
            (Some("bytes=-512"), Some("bytes=-512")),
            (Some("bytes=0-1, 4-5"), Some("bytes=0-1, 4-5")),
            (Some("bytes=-"), Some("[invalid]")),
            (Some("bytes=0-1,token=range-canary"), Some("[invalid]")),
            (
                Some("bytes=0-1\r\nAuthorization: range-canary"),
                Some("[invalid]"),
            ),
            (Some("bytes=0-1\t"), Some("[invalid]")),
            (Some("Bearer range-canary"), Some("[invalid]")),
            (Some(oversized_range.as_str()), Some("[invalid]")),
        ] {
            let store = ActivityLogStore::new(1);
            store.record_playback(PlaybackLogRecord {
                level: ActivityLevel::Info,
                server_id: None,
                server_name: "Fixture",
                playback_user: "Fixture",
                playback_ip: "127.0.0.1",
                media: None,
                request_path: Some("/Videos/id/stream\n?api_key=query-canary&opaque=hidden"),
                range,
                playback_group_id: None,
                message: "Playback".into(),
                detail: "request_id=diag-fixture status=206 upstream_response_headers_received media_stream_not_measured".into(),
            });
            let logs = store.list(None, None, 1);
            let detail = &logs[0].detail;
            assert!(detail.starts_with("request_id=diag-fixture status=206"));
            assert!(detail.contains(" path=/Videos/id/stream"));
            assert!(!detail.contains('?'));
            assert!(!detail.contains("canary"));
            assert!(!detail.chars().any(char::is_control));
            match expected {
                Some(value) => assert!(detail.ends_with(&format!(" range={value}"))),
                None => assert!(!detail.contains(" range=")),
            }
        }
    }

    #[test]
    fn playback_group_ids_are_validated_without_dropping_raw_records() {
        let store = ActivityLogStore::new(20);
        let valid = format!("play-{}", "a1".repeat(32));
        let invalid = [
            String::new(),
            "session-device-token".into(),
            format!("play-{}", "A".repeat(64)),
            format!("play-{}", "a".repeat(63)),
            format!("{valid}\n"),
        ];
        for group in [None, Some(valid.as_str())]
            .into_iter()
            .chain(invalid.iter().map(|value| Some(value.as_str())))
        {
            store.record_playback(PlaybackLogRecord {
                level: ActivityLevel::Info,
                server_id: Some("fixture"),
                server_name: "Fixture",
                playback_user: "Fixture",
                playback_ip: "127.0.0.1",
                playback_group_id: group,
                media: None,
                request_path: None,
                range: None,
                message: "Playback".into(),
                detail: "raw detail".into(),
            });
            let rows = store.list(None, None, 20);
            assert_eq!(
                rows[0].playback_group_id.as_deref(),
                group.filter(|value| *value == valid)
            );
            assert_eq!(rows[0].detail, "raw detail");
            let json = serde_json::to_value(&rows[0]).unwrap();
            assert_eq!(
                json.get("playback_group_id").is_some(),
                group == Some(valid.as_str())
            );
        }
        assert_eq!(store.list(None, None, 20).len(), 7);
        store.record(
            ActivityKind::General,
            ActivityLevel::Info,
            None,
            "Fixture",
            "General",
            "",
        );
        assert!(store.list(None, None, 1)[0].playback_group_id.is_none());
    }

    #[test]
    fn playback_request_path_is_redacted_and_bounded_without_changing_redirects() {
        let store = ActivityLogStore::new(1);
        let long_path = format!("/Videos/{}/stream", "a".repeat(600));
        for (path, expected) in [
            (
                None,
                "request_id=diag-fixture https://media.invalid/movie".to_string(),
            ),
            (
                Some("/Videos/token=path-canary/stream"),
                "request_id=diag-fixture https://media.invalid/movie path=/Videos/token=[REDACTED]"
                    .to_string(),
            ),
            (
                Some(long_path.as_str()),
                format!(
                    "request_id=diag-fixture https://media.invalid/movie path={}",
                    long_path.chars().take(512).collect::<String>()
                ),
            ),
        ] {
            store.record_playback(PlaybackLogRecord {
                level: ActivityLevel::Info,
                server_id: None,
                server_name: "Fixture",
                playback_user: "Fixture",
                playback_ip: "127.0.0.1",
                media: None,
                request_path: path,
                range: None,
                playback_group_id: None,
                message: "Playback".into(),
                detail: "request_id=diag-fixture https://media.invalid/movie".into(),
            });
            let logs = store.list(None, None, 1);
            assert_eq!(logs[0].detail, expected);
            assert!(!logs[0].detail.contains("path-canary"));
        }
    }

    #[test]
    fn quoted_media_credentials_never_reach_storage_or_search() {
        let store = ActivityLogStore::new(10);
        for title in [
            r#"Authorization="Bearer quote_canary_42""#,
            "Authorization='Basic quote_canary_42'",
        ] {
            store.record_playback(PlaybackLogRecord {
                level: ActivityLevel::Info,
                server_id: Some("fixture"),
                server_name: "Fixture",
                playback_user: "Fixture",
                playback_ip: "127.0.0.1",
                message: "Playback".into(),
                detail: String::new(),
                request_path: None,
                range: None,
                playback_group_id: None,
                media: Some(PlaybackMedia {
                    media_id: "42".into(),
                    name: Some(title.into()),
                    series_name: Some(title.into()),
                    ..Default::default()
                }),
            });
        }
        for entry in store.list(None, None, 10) {
            assert!(
                !serde_json::to_string(&entry)
                    .unwrap()
                    .contains("quote_canary_42")
            );
        }
        assert!(
            store
                .list_filtered(ActivityLogFilter {
                    server_id: None,
                    kind: None,
                    level: None,
                    keyword: Some("quote_canary_42"),
                    since_ms: None,
                    until_ms: None,
                    limit: 10,
                })
                .is_empty()
        );
    }

    #[test]
    fn review_media_titles_survive_storage_and_search() {
        for title in ["Basic Instinct", "The Secret: Dare to Dream"] {
            let store = ActivityLogStore::new(10);
            store.record_playback(PlaybackLogRecord {
                level: ActivityLevel::Info,
                server_id: Some("fixture"),
                server_name: "Fixture",
                playback_user: "Fixture",
                playback_ip: "127.0.0.1",
                message: "Playback".into(),
                detail: String::new(),
                request_path: None,
                range: None,
                playback_group_id: None,
                media: Some(PlaybackMedia {
                    media_id: "42".into(),
                    name: Some(title.into()),
                    series_name: Some(title.into()),
                    ..Default::default()
                }),
            });
            let entries = store.list(None, None, 10);
            let media = entries[0].media.as_ref().unwrap();
            assert_eq!(media.name.as_deref(), Some(title));
            assert_eq!(media.series_name.as_deref(), Some(title));
            assert_eq!(
                store
                    .list_filtered(ActivityLogFilter {
                        server_id: None,
                        kind: None,
                        level: None,
                        keyword: Some(title),
                        since_ms: None,
                        until_ms: None,
                        limit: 10,
                    })
                    .len(),
                1
            );
        }
    }

    #[test]
    fn playback_media_is_sanitized_and_searchable_without_changing_scope() {
        let store = ActivityLogStore::new(20);
        store.record_playback(PlaybackLogRecord {
            level: ActivityLevel::Info,
            server_id: Some("a"),
            server_name: "A",
            playback_user: "Fixture",
            playback_ip: "127.0.0.1",
            message: "播放".into(),
            detail: "technical".into(),
            playback_group_id: None,
            request_path: None,
            range: None,
            media: Some(PlaybackMedia {
                media_id: "media-42".into(),
                name: Some(format!(
                    "电影\n token=synthetic-secret {}",
                    "名".repeat(400)
                )),
                series_name: Some("剧名".into()),
                season_number: Some(0),
                episode_number: Some(0),
            }),
        });
        let rows = store.list(None, None, 20);
        let media = rows[0].media.as_ref().unwrap();
        let title = media.name.as_ref().unwrap();
        assert!(!title.contains("synthetic-secret"));
        assert!(!title.chars().any(char::is_control));
        assert!(title.chars().count() <= 256);
        assert_eq!(media.season_number, Some(0));
        for keyword in ["电影", "剧名", "media-42"] {
            let filter = ActivityLogFilter {
                server_id: None,
                kind: Some(ActivityKind::Playback),
                level: None,
                keyword: Some(keyword),
                since_ms: None,
                until_ms: None,
                limit: 1,
            };
            assert_eq!(store.list_filtered(filter).len(), 1);
            let filter = ActivityLogFilter {
                server_id: None,
                kind: None,
                level: None,
                keyword: Some(keyword),
                since_ms: None,
                until_ms: None,
                limit: 1,
            };
            assert!(
                store
                    .list_filtered_scoped(filter, Some(&HashSet::new()))
                    .is_empty()
            );
        }
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
