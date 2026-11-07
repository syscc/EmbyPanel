use std::collections::{BTreeMap, HashSet};

use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use chrono::{Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    AppState, auth,
    db::{ProxyAnalyticsSample, RequestStatsRangeRow, SettingsStore},
    error::{AppError, AppResult},
};

const DEFAULT_HISTORY_DAYS: i64 = 7;
const MAX_HISTORY_DAYS: i64 = 365;
const MAX_ANALYTICS_SAMPLE_DAYS: i64 = 7;
const DEFAULT_METRIC: &str = "requests";
const DEFAULT_DIMENSION: &str = "server";
const MAX_FILTER_LEN: usize = 128;
const MAX_PAGE_LIMIT: usize = 365;
const HISTORY_METRICS: &[&str] = &[
    "requests",
    "redirects",
    "cache_hits",
    "blocks",
    "errors",
    "redirect_success_rate",
    "latency_p95",
    "transcode_rate",
    "bandwidth",
];
const HISTORY_DIMENSIONS: &[&str] = &["server", "user", "media", "client"];

#[derive(Clone)]
pub struct AnalyticsApi {
    db: SettingsStore,
}

impl AnalyticsApi {
    pub fn new(db: SettingsStore) -> Self {
        Self { db }
    }

    pub fn list_history(
        &self,
        metric: &str,
        dimension: &str,
        server_id: Option<&str>,
        user: Option<&str>,
        client: Option<&str>,
        days: i64,
    ) -> AppResult<HistoryResponse> {
        self.list_history_scoped(metric, dimension, server_id, user, client, days, None)
    }

    /// List history while restricting unscoped queries to the supplied server grants.
    /// `None` keeps the existing unrestricted/super-admin behavior.
    pub fn list_history_scoped(
        &self,
        metric: &str,
        dimension: &str,
        server_id: Option<&str>,
        user: Option<&str>,
        client: Option<&str>,
        days: i64,
        allowed_server_ids: Option<&HashSet<String>>,
    ) -> AppResult<HistoryResponse> {
        let metric = normalize_metric(Some(metric))?;
        let dimension = normalize_dimension(Some(dimension))?;
        // Daily counters are the only source with 90-day retention. Keep this
        // path for the global/server counter view instead of deriving zeros from
        // the shorter proxy log retention window.
        if dimension == "server"
            && matches!(
                metric,
                "requests" | "redirects" | "cache_hits" | "blocks" | "errors"
            )
        {
            let rows = if let Some(allowed_server_ids) = allowed_server_ids {
                let mut server_ids = allowed_server_ids.iter().collect::<Vec<_>>();
                server_ids.sort_unstable();
                let mut rows = Vec::new();
                for granted_server_id in server_ids {
                    if server_id.is_some_and(|requested| requested != granted_server_id) {
                        continue;
                    }
                    rows.extend(
                        self.db
                            .list_request_stats_range(days, Some(granted_server_id))?,
                    );
                }
                rows.sort_by(|left, right| {
                    left.date
                        .cmp(&right.date)
                        .then_with(|| left.server_id.cmp(&right.server_id))
                });
                rows
            } else {
                self.db.list_request_stats_range(days, server_id)?
            };
            let points = rows
                .into_iter()
                .map(|row| history_point_from_daily(row, metric))
                .collect::<Vec<_>>();
            return Ok(response_with_points(
                metric,
                dimension,
                points,
                "request_stats_daily",
                90,
                None,
            ));
        }

        let sample_days = days.min(MAX_ANALYTICS_SAMPLE_DAYS);
        let sample_kind = if matches!(metric, "transcode_rate" | "bandwidth")
            || (dimension == "client" && metric == "requests")
        {
            "playback"
        } else {
            "request"
        };
        let samples = self.db.list_proxy_analytics_samples_for_dimension(
            sample_days,
            server_id,
            allowed_server_ids,
            Some(sample_kind),
            metric == "bandwidth",
            if dimension == "user" { user } else { None },
            if dimension == "client" { client } else { None },
            Some(dimension),
            20_000,
        )?;
        let (points, sample_count) = aggregate_samples(&samples, metric, dimension, user, client);
        let caveat = if dimension == "client" && metric == "requests" {
            Some("客户端请求趋势按活跃播放会话采样统计")
        } else if days > MAX_ANALYTICS_SAMPLE_DAYS {
            Some("该维度仅保留最近 7 天的代理/播放样本")
        } else if metric == "redirect_success_rate" {
            Some("该比例表示网关发出的 302 响应占请求样本的比例，不代表下游播放器已成功跟随")
        } else {
            None
        };
        let caveat = if samples.len() >= 20_000 {
            Some(format!(
                "{}历史样本达到查询上限，仅展示最新 20000 条有效样本，可能不覆盖完整保留窗口",
                caveat.map(|text| format!("{text}；")).unwrap_or_default()
            ))
        } else {
            caveat.map(str::to_string)
        };
        Ok(response_with_points(
            metric,
            dimension,
            points,
            "proxy_analytics_samples",
            7,
            if sample_count == 0 {
                Some(("no_samples", "当前保留窗口没有可用历史样本"))
            } else {
                caveat.as_deref().map(|value| ("retention_caveat", value))
            },
        ))
    }
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub dimension: Option<String>,
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default)]
    pub server: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub client: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub page: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryResponse {
    pub metric: String,
    pub dimension: String,
    pub points: Vec<HistoryPoint>,
    pub capability: HistoryCapability,
    pub sample_count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryCapability {
    pub available: bool,
    pub source: Option<String>,
    pub retention_days: Option<i64>,
    pub reason_code: Option<String>,
    pub reason: Option<String>,
    pub caveat: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HistoryPoint {
    pub date: String,
    pub metric: String,
    pub value: f64,
    pub dimension_key: Option<String>,
    pub dimension_label: Option<String>,
    pub server_id: Option<String>,
    pub server_name: Option<String>,
    pub sample_count: u64,
    pub numerator: Option<u64>,
    pub denominator: Option<u64>,
    pub updated_at_ms: u128,
}

pub async fn get_history(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<HistoryQuery>,
) -> AppResult<Json<HistoryResponse>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;
    let metric = normalize_metric(query.metric.as_deref())?;
    let days = normalize_days(query.days)?;
    let dimension = normalize_dimension(query.dimension.as_deref())?;
    let server_id = normalize_filter(query.server.as_deref(), true)?;
    let requested_server_id = auth::require_server_filter(&auth_context, server_id)?;
    let allowed_server_ids = if requested_server_id.is_none() {
        auth::allowed_server_ids(&auth_context)
    } else {
        None
    };
    let user = normalize_filter(query.user.as_deref(), false)?;
    let client = normalize_filter(query.client.as_deref(), false)?;
    let (limit, page) = normalize_limit_page(query.limit, query.page)?;
    let api = AnalyticsApi::new(state.settings_store.clone());
    let mut response = api.list_history_scoped(
        metric,
        dimension,
        requested_server_id,
        user,
        client,
        days,
        allowed_server_ids.as_ref(),
    )?;
    if let Some(limit) = limit {
        let start = (page - 1).saturating_mul(limit);
        response.points = response
            .points
            .into_iter()
            .skip(start)
            .take(limit)
            .collect();
    }
    response.sample_count = response.points.iter().map(|point| point.sample_count).sum();
    Ok(Json(response))
}

fn response_with_points(
    metric: &str,
    dimension: &str,
    points: Vec<HistoryPoint>,
    source: &str,
    retention_days: i64,
    status: Option<(&str, &str)>,
) -> HistoryResponse {
    let sample_count = points.iter().map(|point| point.sample_count).sum();
    let (reason_code, reason, caveat) = match status {
        Some((code, text)) if code == "retention_caveat" => (None, None, Some(text.to_string())),
        Some((code, text)) => (Some(code.to_string()), Some(text.to_string()), None),
        None if points.is_empty() => (
            Some("no_samples".to_string()),
            Some("当前保留窗口没有可用历史样本".to_string()),
            None,
        ),
        None => (None, None, None),
    };
    HistoryResponse {
        metric: metric.to_string(),
        dimension: dimension.to_string(),
        points,
        capability: HistoryCapability {
            available: true,
            source: Some(source.to_string()),
            retention_days: Some(retention_days),
            reason_code,
            reason,
            caveat,
        },
        sample_count,
    }
}

fn aggregate_samples(
    samples: &[ProxyAnalyticsSample],
    metric: &str,
    dimension: &str,
    user_filter: Option<&str>,
    client_filter: Option<&str>,
) -> (Vec<HistoryPoint>, u64) {
    let mut groups: BTreeMap<(String, String), Vec<&ProxyAnalyticsSample>> = BTreeMap::new();
    for sample in samples {
        let is_playback = sample.sample_kind == "playback";
        let metric_uses_playback = matches!(metric, "transcode_rate" | "bandwidth")
            || (dimension == "client" && metric == "requests");
        if metric_uses_playback != is_playback
            || (metric == "bandwidth" && sample.bandwidth.is_none())
        {
            continue;
        }
        let key = match dimension {
            "server" => (sample.server_id.clone(), String::new()),
            "user" => {
                let user = sample.playback_user.trim();
                if user.is_empty()
                    || user == "--"
                    || user_filter.is_some_and(|filter| filter != user)
                {
                    continue;
                }
                (user.to_string(), user.to_string())
            }
            "media" => {
                let Some(item_id) = crate::db::parse_analytics_media_id(&sample.path) else {
                    continue;
                };
                (item_id.clone(), item_id)
            }
            "client" => {
                let client = sample.client.trim();
                if client.is_empty()
                    || client == "--"
                    || client_filter.is_some_and(|filter| filter != client)
                {
                    continue;
                }
                (client.to_string(), client.to_string())
            }
            _ => continue,
        };
        groups.entry(key).or_default().push(sample);
    }
    let mut points = Vec::new();
    let mut total = 0_u64;
    for ((key, label), group) in groups {
        let mut by_date: BTreeMap<String, Vec<&ProxyAnalyticsSample>> = BTreeMap::new();
        for sample in group {
            by_date
                .entry(sample_date(sample.timestamp_ms))
                .or_default()
                .push(sample);
        }
        for (date, day_samples) in by_date {
            let (value, numerator, denominator) = metric_from_samples(metric, &day_samples);
            let count = day_samples.len() as u64;
            total += count;
            let latest_sample = day_samples
                .iter()
                .copied()
                .max_by_key(|sample| sample.timestamp_ms);
            let latest = latest_sample
                .map(|sample| sample.timestamp_ms)
                .unwrap_or_default();
            points.push(HistoryPoint {
                date,
                metric: metric.to_string(),
                value,
                dimension_key: Some(key.clone()),
                dimension_label: if dimension == "server" {
                    latest_sample.map(|sample| sample.server_name.clone())
                } else {
                    Some(label.clone())
                },
                server_id: latest_sample.map(|sample| sample.server_id.clone()),
                server_name: latest_sample.map(|sample| sample.server_name.clone()),
                sample_count: count,
                numerator,
                denominator,
                updated_at_ms: latest,
            });
        }
    }
    points.sort_by(|left, right| {
        left.date
            .cmp(&right.date)
            .then_with(|| left.dimension_key.cmp(&right.dimension_key))
    });
    (points, total)
}

fn metric_from_samples(
    metric: &str,
    samples: &[&ProxyAnalyticsSample],
) -> (f64, Option<u64>, Option<u64>) {
    let denominator = samples.len() as u64;
    let numerator = match metric {
        "requests" => denominator,
        "redirects" => samples
            .iter()
            .filter(|sample| (300..400).contains(&sample.status_code))
            .count() as u64,
        "redirect_success_rate" => samples
            .iter()
            .filter(|sample| sample.status_code == 302)
            .count() as u64,
        "cache_hits" => samples.iter().filter(|sample| sample.cache_hit).count() as u64,
        "blocks" => samples.iter().filter(|sample| sample.blocked).count() as u64,
        "errors" => samples
            .iter()
            .filter(|sample| sample.status_code >= 400)
            .count() as u64,
        "transcode_rate" => samples
            .iter()
            .filter(|sample| sample.transcoding == Some(true))
            .count() as u64,
        "bandwidth" => {
            let values = samples
                .iter()
                .filter_map(|sample| sample.bandwidth)
                .collect::<Vec<_>>();
            if values.is_empty() {
                return (f64::NAN, None, Some(0));
            }
            let total = values.iter().copied().fold(0_u64, u64::saturating_add);
            return (
                total as f64 / values.len() as f64,
                Some(total),
                Some(values.len() as u64),
            );
        }
        "latency_p95" => {
            let mut durations = samples
                .iter()
                .map(|sample| sample.duration_ms as f64)
                .collect::<Vec<_>>();
            durations.sort_by(f64::total_cmp);
            let index = ((durations.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);
            return (durations[index], None, Some(denominator));
        }
        _ => 0,
    };
    if matches!(metric, "redirect_success_rate" | "transcode_rate") {
        (
            if denominator == 0 {
                f64::NAN
            } else {
                numerator as f64 * 100.0 / denominator as f64
            },
            Some(numerator),
            Some(denominator),
        )
    } else {
        (numerator as f64, Some(numerator), Some(denominator))
    }
}

fn history_point_from_daily(row: RequestStatsRangeRow, metric: &str) -> HistoryPoint {
    let value = match metric {
        "requests" => row.requests,
        "redirects" => row.redirects,
        "cache_hits" => row.cache_hits,
        "blocks" => row.blocks,
        "errors" => row.errors,
        _ => 0,
    };
    HistoryPoint {
        date: row.date,
        metric: metric.to_string(),
        value: value as f64,
        dimension_key: row.server_id.clone(),
        dimension_label: row.server_name.clone(),
        server_id: row.server_id,
        server_name: row.server_name,
        sample_count: value.max(0) as u64,
        numerator: Some(value.max(0) as u64),
        denominator: None,
        updated_at_ms: row.updated_at_ms,
    }
}

fn sample_date(timestamp_ms: u128) -> String {
    Utc.timestamp_millis_opt(timestamp_ms.min(i64::MAX as u128) as i64)
        .single()
        .map(|value| (value + Duration::hours(8)).format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn normalize_metric(raw: Option<&str>) -> AppResult<&str> {
    let metric = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_METRIC);
    if HISTORY_METRICS.contains(&metric) {
        Ok(metric)
    } else {
        Err(AppError::Validation(
            "unsupported history metric".to_string(),
        ))
    }
}

fn normalize_dimension(raw: Option<&str>) -> AppResult<&str> {
    let dimension = raw
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_DIMENSION);
    if HISTORY_DIMENSIONS.contains(&dimension) {
        Ok(dimension)
    } else {
        Err(AppError::Validation(
            "unsupported history dimension".to_string(),
        ))
    }
}

fn normalize_days(raw: Option<i64>) -> AppResult<i64> {
    let days = raw.unwrap_or(DEFAULT_HISTORY_DAYS);
    if (1..=MAX_HISTORY_DAYS).contains(&days) {
        Ok(days)
    } else {
        Err(AppError::Validation(
            "days must be between 1 and 365".to_string(),
        ))
    }
}

fn normalize_filter(raw: Option<&str>, _allow_all: bool) -> AppResult<Option<&str>> {
    let Some(value) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.eq_ignore_ascii_case("all") {
        return Ok(None);
    }
    if value.len() > MAX_FILTER_LEN || value.chars().any(char::is_control) {
        return Err(AppError::Validation("invalid filter value".to_string()));
    }
    Ok(Some(value))
}

fn normalize_limit_page(
    limit: Option<usize>,
    page: Option<usize>,
) -> AppResult<(Option<usize>, usize)> {
    let page = page.unwrap_or(1);
    if page == 0 {
        return Err(AppError::Validation("page must be >= 1".to_string()));
    }
    if let Some(limit) = limit {
        if !(1..=MAX_PAGE_LIMIT).contains(&limit) {
            return Err(AppError::Validation(
                "limit must be between 1 and 365".to_string(),
            ));
        }
        Ok((Some(limit), page))
    } else if page != 1 {
        Err(AppError::Validation(
            "page requires limit to be set".to_string(),
        ))
    } else {
        Ok((None, 1))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        AnalyticsApi, aggregate_samples, normalize_dimension, normalize_limit_page,
        normalize_metric, sample_date,
    };
    use crate::db::{
        ProxyAnalyticsSample, ProxyRequestDetailWrite, SettingsStore, TelemetryWriteQueue,
    };

    #[test]
    fn accepts_supported_history_contract() {
        assert_eq!(
            normalize_metric(Some("latency_p95")).unwrap(),
            "latency_p95"
        );
        assert_eq!(normalize_dimension(Some("media")).unwrap(), "media");
    }

    #[test]
    fn validates_bounded_point_pagination() {
        assert_eq!(
            normalize_limit_page(Some(25), Some(2)).unwrap(),
            (Some(25), 2)
        );
        assert!(normalize_limit_page(Some(0), Some(1)).is_err());
        assert!(normalize_limit_page(Some(366), Some(1)).is_err());
        assert!(normalize_limit_page(None, Some(2)).is_err());
    }

    #[test]
    fn parses_media_id_without_exposing_path() {
        assert_eq!(
            crate::db::parse_analytics_media_id("/Videos/abc/stream").as_deref(),
            Some("abc")
        );
        assert_eq!(
            crate::db::parse_analytics_media_id("/Items/xyz/PlaybackInfo").as_deref(),
            Some("xyz")
        );
        assert_eq!(
            crate::db::parse_analytics_media_id("/emby/vIdEoS/\u{2003}abc\u{3000}/stream")
                .as_deref(),
            Some("abc")
        );
        assert_eq!(
            crate::db::parse_analytics_media_id("//legacy//iTeMs/xyz/PlaybackInfo").as_deref(),
            Some("xyz")
        );
        assert!(crate::db::parse_analytics_media_id("/Videos/\u{2003}\u{3000}/stream").is_none());
        assert!(crate::db::parse_analytics_media_id("/Items/bad\nid/stream").is_none());
        assert!(
            crate::db::parse_analytics_media_id(&format!("/Videos/{}/stream", "a".repeat(129)))
                .is_none()
        );
        assert!(crate::db::parse_analytics_media_id("/Users/foo").is_none());
    }

    #[test]
    fn converts_sample_timestamp_to_business_date() {
        assert_eq!(sample_date(0), "1970-01-01");
    }

    #[test]
    fn aggregates_real_rate_and_p95_samples_without_fabricated_values() {
        let samples = vec![
            ProxyAnalyticsSample {
                timestamp_ms: 1_000_000,
                server_id: "srv".to_string(),
                server_name: "Server".to_string(),
                path: "/Videos/a/stream".to_string(),
                client: "Player".to_string(),
                sample_kind: "request".to_string(),
                transcoding: None,
                bandwidth: None,
                status_code: 302,
                duration_ms: 10,
                playback_user: "alice".to_string(),
                cache_hit: true,
                blocked: false,
            },
            ProxyAnalyticsSample {
                timestamp_ms: 1_000_001,
                server_id: "srv".to_string(),
                server_name: "Server".to_string(),
                path: "/Videos/a/stream".to_string(),
                client: "Player".to_string(),
                sample_kind: "request".to_string(),
                transcoding: None,
                bandwidth: None,
                status_code: 200,
                duration_ms: 100,
                playback_user: "alice".to_string(),
                cache_hit: false,
                blocked: false,
            },
            ProxyAnalyticsSample {
                timestamp_ms: 1_000_002,
                server_id: "srv".to_string(),
                server_name: "Server".to_string(),
                path: "/Videos/a/stream".to_string(),
                client: "Player".to_string(),
                sample_kind: "request".to_string(),
                transcoding: None,
                bandwidth: None,
                status_code: 500,
                duration_ms: 50,
                playback_user: "alice".to_string(),
                cache_hit: false,
                blocked: true,
            },
        ];
        let (rate, _) = aggregate_samples(&samples, "redirect_success_rate", "media", None, None);
        assert_eq!(rate.len(), 1);
        assert!((rate[0].value - (100.0 / 3.0)).abs() < 0.001);
        assert_eq!(rate[0].numerator, Some(1));
        assert_eq!(rate[0].denominator, Some(3));
        let (p95, _) = aggregate_samples(&samples, "latency_p95", "user", None, None);
        assert_eq!(p95.len(), 1);
        assert_eq!(p95[0].value, 100.0);
    }

    #[test]
    fn server_rename_keeps_one_weighted_point_and_latest_label() {
        let sample = |timestamp_ms, name: &str, status_code, duration_ms| ProxyAnalyticsSample {
            timestamp_ms,
            server_id: "same".into(),
            server_name: name.into(),
            path: "/Videos/a".into(),
            client: "Player".into(),
            sample_kind: "request".into(),
            transcoding: None,
            bandwidth: None,
            status_code,
            duration_ms,
            playback_user: "alice".into(),
            cache_hit: false,
            blocked: false,
        };
        // Input order is deliberately reversed; labels follow time, not traversal order.
        let samples = vec![sample(2001, "New", 200, 100), sample(2000, "Old", 302, 10)];
        let (points, count) =
            aggregate_samples(&samples, "redirect_success_rate", "server", None, None);
        assert_eq!(count, 2);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].value, 50.0);
        assert_eq!(points[0].numerator, Some(1));
        assert_eq!(points[0].denominator, Some(2));
        assert_eq!(points[0].dimension_key.as_deref(), Some("same"));
        assert_eq!(points[0].dimension_label.as_deref(), Some("New"));
        assert_eq!(points[0].server_name.as_deref(), Some("New"));
        let (p95, _) = aggregate_samples(&samples, "latency_p95", "server", None, None);
        assert_eq!(p95.len(), 1);
        assert_eq!(p95[0].value, 100.0);
    }

    #[test]
    fn history_filters_kind_grants_and_valid_bandwidth_before_latest_cap() {
        let path = std::env::temp_dir().join(format!(
            "embypanel-history-cap-{}-{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<20000)
             INSERT INTO proxy_analytics_samples (timestamp_ms,sample_kind,server_id,server_name,status_code,duration_ms,playback_user,created_at_ms)
             SELECT ?1-100,'request','a','A',200,10,'bulk',?1 FROM n", [now]
        ).unwrap();
        conn.execute(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<20000)
             INSERT INTO proxy_analytics_samples (timestamp_ms,sample_kind,server_id,server_name,session_key,transcoding,created_at_ms)
             SELECT ?1-90,'playback','a','A',CAST(x AS TEXT),0,?1 FROM n", [now]
        ).unwrap();
        conn.execute(
            "INSERT INTO proxy_analytics_samples (timestamp_ms,sample_kind,server_id,server_name,session_key,transcoding,bandwidth,created_at_ms)
             VALUES (?1-80,'playback','z','Z','last',1,NULL,?1)", [now]
        ).unwrap();
        // This valid bandwidth observation is older than 20000 NULL-bandwidth
        // playback rows, so filtering after LIMIT would incorrectly lose it.
        conn.execute(
            "INSERT INTO proxy_analytics_samples (timestamp_ms,sample_kind,server_id,server_name,session_key,transcoding,bandwidth,created_at_ms)
             VALUES (?1-95,'playback','z','Z','bandwidth-old',1,8000,?1)", [now]
        ).unwrap();
        conn.execute(
            "INSERT INTO proxy_analytics_samples (timestamp_ms,sample_kind,server_id,server_name,status_code,duration_ms,playback_user,created_at_ms)
             VALUES (?1-70,'request','z','Z',302,100,'wanted',?1)", [now]
        ).unwrap();
        conn.execute(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<20001)
             INSERT INTO proxy_analytics_samples (timestamp_ms,sample_kind,server_id,server_name,status_code,duration_ms,playback_user,created_at_ms)
             SELECT ?1-10,'request','hidden','Hidden',500,200,'bulk',?1 FROM n", [now]
        ).unwrap();
        let api = AnalyticsApi::new(store);
        let allowed = HashSet::from(["a".into(), "z".into()]);
        let read = |metric, dimension, user| {
            api.list_history_scoped(metric, dimension, None, user, None, 7, Some(&allowed))
                .unwrap()
        };
        let rate = read("redirect_success_rate", "server", None);
        assert_eq!(rate.sample_count, 20000);
        assert_eq!(rate.points.len(), 2);
        assert_eq!(
            rate.points
                .iter()
                .find(|point| point.server_id.as_deref() == Some("z"))
                .unwrap()
                .value,
            100.0
        );
        assert!(rate.capability.caveat.as_deref().unwrap().contains("20000"));
        let transcode = read("transcode_rate", "server", None);
        assert_eq!(transcode.sample_count, 20000);
        assert_eq!(transcode.points.len(), 2);
        assert_eq!(
            transcode
                .points
                .iter()
                .find(|point| point.server_id.as_deref() == Some("z"))
                .unwrap()
                .value,
            100.0
        );
        let bandwidth = read("bandwidth", "server", None);
        assert_eq!(bandwidth.sample_count, 1);
        assert_eq!(bandwidth.points[0].value, 8000.0);
        let filtered = read("requests", "user", Some("wanted"));
        assert_eq!(filtered.sample_count, 1);
        assert_eq!(filtered.points[0].dimension_key.as_deref(), Some("wanted"));
        let empty = api
            .list_history_scoped(
                "transcode_rate",
                "server",
                None,
                None,
                None,
                7,
                Some(&HashSet::new()),
            )
            .unwrap();
        assert!(empty.points.is_empty());
        assert_eq!(empty.capability.reason_code.as_deref(), Some("no_samples"));
        drop(conn);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn playback_samples_flow_from_storage_to_history_aggregation() {
        let path = std::env::temp_dir().join(format!(
            "embypanel-history-evidence-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis();
        store
            .record_playback_analytics_samples(&[
                crate::db::PlaybackAnalyticsSampleWrite {
                    timestamp_ms: now.saturating_sub(1000),
                    server_id: "srv".to_string(),
                    server_name: "Server".to_string(),
                    session_key: "session-a".to_string(),
                    media_id: "media-a".to_string(),
                    playback_user: "alice".to_string(),
                    client: "Infuse".to_string(),
                    transcoding: true,
                    bandwidth: Some(8_000),
                },
                crate::db::PlaybackAnalyticsSampleWrite {
                    timestamp_ms: now.saturating_sub(500),
                    server_id: "srv".to_string(),
                    server_name: "Server".to_string(),
                    session_key: "session-b".to_string(),
                    media_id: "media-b".to_string(),
                    playback_user: "bob".to_string(),
                    client: "Kodi".to_string(),
                    transcoding: false,
                    bandwidth: Some(12_000),
                },
                crate::db::PlaybackAnalyticsSampleWrite {
                    timestamp_ms: now.saturating_sub(250),
                    server_id: "srv-other".to_string(),
                    server_name: "Other".to_string(),
                    session_key: "session-c".to_string(),
                    media_id: "media-c".to_string(),
                    playback_user: "mallory".to_string(),
                    client: "OtherClient".to_string(),
                    transcoding: true,
                    bandwidth: Some(16_000),
                },
            ])
            .unwrap();
        let queue = TelemetryWriteQueue::start(store.clone()).unwrap();
        queue.proxy_request_detail(ProxyRequestDetailWrite {
            timestamp_ms: now.saturating_sub(2_000),
            server_id: "srv".to_string(),
            server_name: "Server".to_string(),
            port: 8096,
            method: "GET".to_string(),
            path: "/Videos/request-media/stream".to_string(),
            path_type: "video_stream".to_string(),
            status_code: 302,
            outcome: "redirect".to_string(),
            duration_ms: 42,
            playback_user: "carol".to_string(),
            playback_ip: "--".to_string(),
            cache_hit: false,
            blocked: false,
            detail: String::new(),
        });
        queue.shutdown().unwrap();
        let api = AnalyticsApi::new(store.clone());
        let requests = api
            .list_history("requests", "user", None, Some("carol"), None, 7)
            .unwrap();
        assert_eq!(requests.points.len(), 1);
        assert_eq!(requests.points[0].value, 1.0);
        let transcode = api
            .list_history("transcode_rate", "client", None, None, None, 7)
            .unwrap();
        assert_eq!(transcode.points.len(), 3);
        assert!(transcode.points.iter().any(|point| point.value == 100.0));
        let allowed = HashSet::from(["srv".to_string()]);
        let scoped = api
            .list_history_scoped(
                "transcode_rate",
                "client",
                None,
                None,
                None,
                7,
                Some(&allowed),
            )
            .unwrap();
        assert_eq!(scoped.points.len(), 2);
        assert!(
            scoped
                .points
                .iter()
                .all(|point| point.server_id.as_deref() == Some("srv"))
        );
        let client_requests = api
            .list_history("requests", "client", None, None, None, 7)
            .unwrap();
        assert_eq!(client_requests.points.len(), 3);
        assert!(client_requests.capability.available);
        let bandwidth = api
            .list_history("bandwidth", "client", None, None, None, 7)
            .unwrap();
        assert_eq!(bandwidth.points.len(), 3);
        assert!(bandwidth.points.iter().any(|point| point.value == 8_000.0));
        let media = api
            .list_history("transcode_rate", "media", None, None, None, 7)
            .unwrap();
        assert_eq!(media.points.len(), 3);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn history_dimension_invalid_rows_do_not_consume_the_latest_sample_budget() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let path = std::env::temp_dir().join(format!(
            "embypanel-history-dimension-budget-{}-{}.sqlite",
            std::process::id(),
            now.as_nanos()
        ));
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let at = now.as_millis() as i64;
        // One usable observation precedes all invalid rows.
        // Raw paths represent legacy request rows; playback stores a plain media ID.
        conn.execute(
            "INSERT INTO proxy_analytics_samples
             (timestamp_ms,sample_kind,server_id,server_name,media_id,playback_user,client,
              status_code,transcoding,session_key,created_at_ms)
             VALUES (?1-172800000,'request','a','A','/Videos/movie/stream','alice','',302,NULL,'',?1),
                    (?1-172800000,'playback','a','A','movie','alice','Player',NULL,1,'first',?1),
                    (?1-1,'request','hidden','Hidden','/Items/hidden','hidden','',302,NULL,'',?1),
                    (?1-1,'playback','hidden','Hidden','hidden','','Hidden',NULL,1,'hidden',?1)",
            [at],
        ).unwrap();
        conn.execute(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<20000)
             INSERT INTO proxy_analytics_samples
             (timestamp_ms,sample_kind,server_id,server_name,media_id,playback_user,created_at_ms)
             SELECT ?1-10000,'request','a','A',
                    CASE x%4 WHEN 0 THEN '' WHEN 1 THEN '/Users/ignored'
                         WHEN 2 THEN '/Videos/\u{2003}\u{3000}/stream' ELSE '/Items/' END,
                    CASE x%3 WHEN 0 THEN '' WHEN 1 THEN '\u{2003}\u{3000}' ELSE ' -- ' END,?1
             FROM n",
            [at],
        )
        .unwrap();
        conn.execute(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<20000)
             INSERT INTO proxy_analytics_samples
             (timestamp_ms,sample_kind,server_id,server_name,session_key,client,transcoding,created_at_ms)
             SELECT ?1-10000,'playback','z','Z',CAST(x AS TEXT),
                    CASE x%3 WHEN 0 THEN '' WHEN 1 THEN '\u{00a0}\u{202f}' ELSE '\u{3000}--\u{2003}' END,1,?1
             FROM n",
            [at],
        ).unwrap();
        let allowed = HashSet::from(["a".to_string(), "z".to_string()]);
        let api = AnalyticsApi::new(store.clone());
        for (dimension, metric, kind, key) in [
            ("media", "redirect_success_rate", "request", "movie"),
            ("user", "redirect_success_rate", "request", "alice"),
            ("media", "transcode_rate", "playback", "movie"),
            ("user", "transcode_rate", "playback", "alice"),
            ("client", "transcode_rate", "playback", "Player"),
        ] {
            let history = api
                .list_history_scoped(metric, dimension, None, None, None, 7, Some(&allowed))
                .unwrap();
            assert_eq!(history.sample_count, 1, "{dimension}");
            assert_eq!(history.points.len(), 1, "{dimension}");
            let point = &history.points[0];
            assert_eq!(point.date, sample_date((at - 172800000) as u128));
            assert_eq!(point.dimension_key.as_deref(), Some(key));
            assert_eq!(point.value, 100.0);
            assert_eq!(point.numerator, Some(1));
            assert_eq!(point.denominator, Some(1));
            assert_eq!(point.sample_count, 1);
            assert_eq!(point.server_id.as_deref(), Some("a"));
            // A small storage limit must count usable rows rather than scanned rows.
            let latest = store
                .list_proxy_analytics_samples_for_dimension(
                    7,
                    None,
                    Some(&allowed),
                    Some(kind),
                    false,
                    None,
                    None,
                    Some(dimension),
                    1,
                )
                .unwrap();
            assert_eq!(latest.len(), 1, "{dimension}");
            assert_eq!(latest[0].server_id, "a");
            assert_eq!(latest[0].timestamp_ms, (at - 172800000) as u128);
            let scoped = api
                .list_history_scoped(metric, dimension, Some("a"), None, None, 7, Some(&allowed))
                .unwrap();
            assert_eq!(scoped.sample_count, 1);
            assert_eq!(scoped.points.len(), 1);
            assert_eq!(scoped.points[0].value, 100.0);
            assert_eq!(scoped.points[0].numerator, Some(1));
            assert_eq!(scoped.points[0].denominator, Some(1));
        }
        let requests = api
            .list_history_scoped("requests", "client", None, None, None, 7, Some(&allowed))
            .unwrap();
        assert_eq!(requests.sample_count, 1);
        assert_eq!(requests.points.len(), 1);
        assert!(requests.points.iter().all(|point| point.value == 1.0
            && point.numerator == Some(1)
            && point.denominator == Some(1)));
    }

    #[test]
    fn history_dimension_budget_caps_usable_samples_and_preserves_date_order() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let path = std::env::temp_dir().join(format!(
            "embypanel-history-valid-budget-{}-{}.sqlite",
            std::process::id(),
            now.as_nanos()
        ));
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let conn = rusqlite::Connection::open(&path).unwrap();
        let at = now.as_millis() as i64;
        // Anchor inside the previous business day so all 20001 rows share a date.
        let base = at - (at + 8 * 3600000) % 86400000 - 86400000 + 3600000;
        conn.execute(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<20001),
             kinds(kind) AS (VALUES ('request'),('playback'))
             INSERT INTO proxy_analytics_samples
             (timestamp_ms,sample_kind,server_id,server_name,media_id,playback_user,client,
              session_key,status_code,transcoding,created_at_ms)
             SELECT ?1+x,kind,'a','A',CASE kind WHEN 'request' THEN '/Items/movie' ELSE 'movie' END,
                    'alice','Player',CAST(x AS TEXT),CASE WHEN x=1 THEN 302 ELSE 200 END,
                    x=1,?1+x FROM n CROSS JOIN kinds",
            [base],
        )
        .unwrap();
        let api = AnalyticsApi::new(store.clone());
        for (dimension, metric, kind) in [
            ("media", "redirect_success_rate", "request"),
            ("user", "redirect_success_rate", "request"),
            ("media", "transcode_rate", "playback"),
            ("user", "transcode_rate", "playback"),
            ("client", "transcode_rate", "playback"),
        ] {
            let history = api
                .list_history(metric, dimension, None, None, None, 7)
                .unwrap();
            assert_eq!(history.sample_count, 20000, "{dimension}");
            assert_eq!(history.points.len(), 1);
            let point = &history.points[0];
            assert_eq!(point.date, sample_date(base as u128));
            assert_eq!(point.value, 0.0);
            assert_eq!(point.sample_count, 20000);
            assert_eq!(point.numerator, Some(0));
            assert_eq!(point.denominator, Some(20000));
            assert_eq!(point.updated_at_ms, (base + 20001) as u128);
            assert!(
                history
                    .capability
                    .caveat
                    .as_deref()
                    .unwrap()
                    .contains("20000")
            );
            let latest = store
                .list_proxy_analytics_samples_for_dimension(
                    7,
                    None,
                    None,
                    Some(kind),
                    false,
                    None,
                    None,
                    Some(dimension),
                    3,
                )
                .unwrap();
            assert_eq!(
                latest
                    .iter()
                    .map(|sample| sample.timestamp_ms)
                    .collect::<Vec<_>>(),
                vec![
                    (base + 20001) as u128,
                    (base + 20000) as u128,
                    (base + 19999) as u128
                ]
            );
        }
    }

    #[test]
    fn normalized_user_and_client_filters_match_legacy_whitespace_before_limit() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        let path = std::env::temp_dir().join(format!(
            "embypanel-analytics-trim-{}-{}.sqlite",
            std::process::id(),
            now.as_nanos()
        ));
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        let at = now.as_millis();
        let samples = [
            ("first", " alice ", " Player ", true),
            (
                "second",
                "\u{2003}alice\u{3000}",
                "\u{00a0}Player\u{202f}",
                false,
            ),
            ("latest-other", "bob", "Other", true),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (session, user, client, transcoding))| {
            crate::db::PlaybackAnalyticsSampleWrite {
                timestamp_ms: at.saturating_sub(1000) + index as u128,
                server_id: "srv".into(),
                server_name: "Server".into(),
                session_key: session.into(),
                media_id: "movie".into(),
                playback_user: user.into(),
                client: client.into(),
                transcoding,
                bandwidth: Some(1000),
            }
        })
        .collect::<Vec<_>>();
        store.record_playback_analytics_samples(&samples).unwrap();
        let api = AnalyticsApi::new(store.clone());
        let user = api
            .list_history("transcode_rate", "user", None, Some("alice"), None, 7)
            .unwrap();
        assert_eq!(user.points.len(), 1);
        assert_eq!(user.points[0].value, 50.0);
        assert_eq!(user.points[0].denominator, Some(2));
        let client = api
            .list_history("transcode_rate", "client", None, None, Some("Player"), 7)
            .unwrap();
        assert_eq!(client.points.len(), 1);
        assert_eq!(client.points[0].value, 50.0);
        assert_eq!(client.points[0].denominator, Some(2));
        let capped = store
            .list_proxy_analytics_samples_filtered(
                7,
                None,
                None,
                Some("playback"),
                false,
                Some("alice"),
                Some("Player"),
                1,
            )
            .unwrap();
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].playback_user.trim(), "alice");
        assert_eq!(capped[0].client.trim(), "Player");
        drop(api);
        drop(store);
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}
