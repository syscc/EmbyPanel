//! Diagnostics HTTP free handlers for main router mounting.
//! GET /api/diagnostics  — list/filter diagnosis records (auth required)
//! POST /api/diagnostics/clean — clear memory and storage (admin + confirm + audit)

use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{
    AppState, auth,
    db::{DIAGNOSTICS_PERSIST_MAX_RECORDS, DIAGNOSTICS_RETENTION_DAYS, diagnostics_cutoff_ms},
    diagnostics::{self, DIAGNOSTICS_CAPACITY, DiagnosisRecord, sanitize_diagnosis_record},
    error::{AppError, AppResult},
};

/// Query for listing diagnosis records. All fields optional; empty result is valid.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DiagnosticsQuery {
    /// Exact request_id match (max 128 chars, no whitespace/controls)
    pub request_id: Option<String>,
    /// Exact server_id match (max 128 chars, no whitespace/controls)
    pub server_id: Option<String>,
    /// Exact media_id match (max 128 chars, no whitespace/controls)
    pub media_id: Option<String>,
    /// Reserved legacy filter. Current diagnosis records do not expose session IDs.
    pub session_id: Option<String>,
    pub outcome: Option<String>,
    /// Case-insensitive substring match on the collected user name.
    pub user: Option<String>,
    /// Case-insensitive substring search over explicitly selected sanitized fields.
    pub keyword: Option<String>,
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    pub min_duration_ms: Option<u64>,
    /// Page size, default 50, max 100
    pub limit: Option<usize>,
    /// 1-based page, default 1
    pub page: Option<usize>,
}

const MAX_LIMIT: usize = 100;
const DEFAULT_LIMIT: usize = 50;
const MAX_ID_LEN: usize = 128;
const MAX_PAGE: usize = 1_000_000;
const MAX_TEXT_FILTER_LEN: usize = 256;

fn validate_query(query: &DiagnosticsQuery) -> AppResult<()> {
    for (name, value) in [
        ("request_id", &query.request_id),
        ("server_id", &query.server_id),
        ("media_id", &query.media_id),
        ("session_id", &query.session_id),
    ] {
        if let Some(value) = value {
            validate_id(name, value)?;
        }
    }
    if query.session_id.is_some() {
        return Err(AppError::Validation(
            "session_id filter is not supported".to_string(),
        ));
    }
    if query.outcome.as_deref().is_some_and(|value| {
        !matches!(
            value,
            "redirect" | "proxy" | "failed" | "cancelled" | "blocked"
        )
    }) {
        return Err(AppError::Validation("invalid outcome filter".to_string()));
    }
    for (name, value) in [("user", &query.user), ("keyword", &query.keyword)] {
        if value.as_ref().is_some_and(|value| {
            value.trim().is_empty()
                || value.len() > MAX_TEXT_FILTER_LEN
                || value.chars().any(char::is_control)
        }) {
            return Err(AppError::Validation(format!("invalid {name} filter")));
        }
    }
    if query.since_ms.is_some_and(|value| value <= 0)
        || query.until_ms.is_some_and(|value| value <= 0)
        || matches!((query.since_ms, query.until_ms), (Some(since), Some(until)) if since > until)
    {
        return Err(AppError::Validation(
            "invalid diagnostics time range".to_string(),
        ));
    }
    Ok(())
}

fn validate_id(name: &str, value: &str) -> AppResult<()> {
    if value.is_empty() || value.len() > MAX_ID_LEN {
        return Err(AppError::Validation(format!(
            "invalid {name}: empty or longer than {MAX_ID_LEN}"
        )));
    }
    if value
        .chars()
        .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err(AppError::Validation(format!(
            "invalid {name}: whitespace or control characters are not allowed"
        )));
    }
    // Reject values that clearly look like raw credentials in filters.
    let lower = value.to_ascii_lowercase();
    if lower.contains("://")
        || lower.starts_with("bearer ")
        || lower.starts_with("basic ")
        || value.contains('=')
        || value.contains(':')
    {
        return Err(AppError::Validation(format!(
            "invalid {name}: credential-like filter values are not allowed"
        )));
    }
    Ok(())
}

fn page_limit(query: &DiagnosticsQuery) -> (usize, usize) {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let page = query.page.unwrap_or(1).clamp(1, MAX_PAGE);
    (page, limit)
}

fn page_start(page: usize, limit: usize) -> usize {
    // Saturating math avoids overflow on extreme page*limit.
    (page - 1).saturating_mul(limit)
}

/// Stable response envelope for diagnosis records and caller capabilities.
#[derive(Debug, Serialize)]
pub struct DiagnosticsResponse {
    pub records: Vec<DiagnosisRecord>,
    pub can_global_cleanup: bool,
    pub total: usize,
    pub has_more: bool,
    pub storage: DiagnosticsStorage,
}

#[derive(Debug, Serialize)]
pub struct DiagnosticsStorage {
    pub persistent: bool,
    pub retention_days: i64,
    pub max_records: usize,
    pub memory_capacity: usize,
}

fn diagnostics_response(
    records: Vec<DiagnosisRecord>,
    auth_context: &auth::AuthContext,
) -> DiagnosticsResponse {
    DiagnosticsResponse {
        total: records.len(),
        has_more: false,
        records,
        can_global_cleanup: can_clean_global_diagnostics(auth_context),
        storage: DiagnosticsStorage {
            persistent: true,
            retention_days: DIAGNOSTICS_RETENTION_DAYS,
            max_records: DIAGNOSTICS_PERSIST_MAX_RECORDS,
            memory_capacity: DIAGNOSTICS_CAPACITY,
        },
    }
}

fn matches_keyword(record: &DiagnosisRecord, keyword: &str) -> bool {
    // Search values, never serialized JSON keys or uncollected/raw request material.
    let fields = [
        Some(record.request_id.as_str()),
        Some(record.server_id.as_str()),
        Some(record.media_id.as_str()),
        Some(record.ua.as_str()),
        record.context.server_name.as_deref(),
        record.context.media_name.as_deref(),
        record.context.series_name.as_deref(),
        record.context.user_name.as_deref(),
        record.context.client_name.as_deref(),
        record.context.device_name.as_deref(),
        record.context.method.as_deref(),
        record.context.request_path.as_deref(),
        record.result.outcome.as_deref(),
        record.result.target_host.as_deref(),
        record.result.reason.as_deref(),
        record.result.error_kind.as_deref(),
        record.cache.status.as_deref(),
        record.cache.reason.as_deref(),
        record.paths.original.as_deref(),
        record.paths.mapped.as_deref(),
        record.paths.resolved.as_deref(),
        record.paths.final_url.as_deref(),
        record.failure_node.as_deref(),
    ];
    fields
        .into_iter()
        .flatten()
        .any(|value| value.to_lowercase().contains(keyword))
        || record.steps.iter().any(|step| {
            [
                Some(step.name.as_str()),
                Some(step.details.as_str()),
                step.reason.as_deref(),
                step.error_kind.as_deref(),
                step.state.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|value| value.to_lowercase().contains(keyword))
        })
}

fn filter_and_paginate(
    persisted: Vec<DiagnosisRecord>,
    memory: Vec<DiagnosisRecord>,
    query: &DiagnosticsQuery,
    auth_context: &auth::AuthContext,
) -> AppResult<DiagnosticsResponse> {
    validate_query(query)?;
    let server_id = auth::require_server_filter(auth_context, query.server_id.as_deref())?;
    let cutoff = diagnostics_cutoff_ms();
    let mut merged = HashMap::new();
    // Memory is a newer snapshot and replaces persisted duplicates. Authorize each
    // source before merging so inaccessible records cannot affect counts or pages.
    for record in persisted.into_iter().chain(memory) {
        if !auth::has_server_access(auth_context, &record.server_id)
            || server_id.is_some_and(|server_id| record.server_id != server_id)
        {
            continue;
        }
        let record = sanitize_diagnosis_record(record);
        merged.insert(record.request_id.clone(), record);
    }
    let user = query.user.as_ref().map(|value| value.to_lowercase());
    let keyword = query.keyword.as_ref().map(|value| value.to_lowercase());
    let mut records = merged
        .into_values()
        .filter(|record| {
            record.started_at_ms.is_none_or(|time| time >= cutoff)
                && query
                    .request_id
                    .as_ref()
                    .is_none_or(|id| record.request_id == *id)
                && query
                    .media_id
                    .as_ref()
                    .is_none_or(|id| record.media_id == *id)
                && query
                    .outcome
                    .as_ref()
                    .is_none_or(|outcome| record.result.outcome.as_ref() == Some(outcome))
                && user.as_ref().is_none_or(|user| {
                    record
                        .context
                        .user_name
                        .as_ref()
                        .is_some_and(|value| value.to_lowercase().contains(user))
                })
                && keyword
                    .as_ref()
                    .is_none_or(|keyword| matches_keyword(record, keyword))
                && query
                    .since_ms
                    .is_none_or(|since| record.started_at_ms.is_some_and(|time| time >= since))
                && query
                    .until_ms
                    .is_none_or(|until| record.started_at_ms.is_some_and(|time| time <= until))
                && query
                    .min_duration_ms
                    .is_none_or(|min| record.duration_ms.is_some_and(|duration| duration >= min))
        })
        .collect::<Vec<_>>();
    // Unknown times stay unknown and sort last; request_id provides a stable tie-break.
    records.sort_by(|a, b| {
        b.started_at_ms
            .cmp(&a.started_at_ms)
            .then_with(|| b.request_id.cmp(&a.request_id))
    });
    let total = records.len();
    let (page, limit) = page_limit(query);
    let start = page_start(page, limit);
    let records = records.into_iter().skip(start).take(limit).collect();
    let mut response = diagnostics_response(records, auth_context);
    response.total = total;
    response.has_more = start.saturating_add(response.records.len()) < total;
    Ok(response)
}

/// Free handler: list diagnosis records with filter/page/limit.
/// Mount as: .route("/api/diagnostics", get(diagnostics_api::get_diagnostics))
pub async fn get_diagnostics(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<DiagnosticsQuery>,
) -> AppResult<Json<DiagnosticsResponse>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;

    validate_query(&query)?;
    let requested_server_id =
        auth::require_server_filter(&auth_context, query.server_id.as_deref())?;
    let allowed_servers = requested_server_id
        .map(|server_id| [server_id.to_string()].into_iter().collect())
        .or_else(|| auth::allowed_server_ids(&auth_context));
    let store = state.settings_store.clone();
    // Fail closed on storage failure: no success envelope claiming persistence or
    // a partial/global total. SQLite work does not block the async runtime.
    let persisted = tokio::task::spawn_blocking(move || {
        store.list_diagnosis_records_scoped(allowed_servers.as_ref())
    })
    .await
    .map_err(|_| AppError::Internal("diagnostics read task failed".to_string()))??;
    Ok(Json(filter_and_paginate(
        persisted,
        diagnostics::get_diagnosis_records(),
        &query,
        &auth_context,
    )?))
}

/// Body for clear/reset. Requires explicit confirm == "yes".
#[derive(Debug, Deserialize)]
pub struct CleanDiagnosticsRequest {
    pub confirm: String,
}

/// Free handler: clear persisted and in-memory diagnoses. Admin + confirm + audit.
/// Mount as: .route("/api/diagnostics/clean", post(diagnostics_api::clean_diagnostics))
fn can_clean_global_diagnostics(ctx: &auth::AuthContext) -> bool {
    ctx.kind == auth::AuthKind::Session && auth::is_super_admin(ctx)
}

pub async fn clean_diagnostics(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CleanDiagnosticsRequest>,
) -> AppResult<Json<()>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;
    auth::require_super_admin_session(&auth_context)?;
    let admin_user_id = auth_context.user_id;

    if body.confirm != "yes" {
        return Err(AppError::Validation(
            "confirmation required for cleanup".to_string(),
        ));
    }

    let queue = state.telemetry_writes.clone();
    tokio::task::spawn_blocking(move || queue.clear_diagnosis_records())
        .await
        .map_err(|_| AppError::Internal("diagnostics cleanup task failed".to_string()))??;
    diagnostics::clean_diagnostics();

    // Audit must not include request filters or any potentially sensitive query values.
    state.settings_store.record_audit(
        Some(admin_user_id),
        "diagnostics.clean",
        "清空内存与持久化诊断记录",
        "success",
    )?;

    Ok(Json(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::AdminServerGrant,
        diagnostics::{
            DiagnosisRecord, DiagnosisStep, REDACTED_PLACEHOLDER, create_diagnosis_record,
            sanitize_diagnosis_record,
        },
    };

    fn sample_record(id: &str, server: &str) -> DiagnosisRecord {
        DiagnosisRecord {
            request_id: id.to_string(),
            server_id: server.to_string(),
            media_id: "m1".to_string(),
            ua: "ua".to_string(),
            steps: vec![DiagnosisStep {
                name: "openlist".to_string(),
                duration_ms: 1,
                details: "ok".to_string(),
                ..Default::default()
            }],
            cache_hit: false,
            failure_node: None,
            sanitized_errors: vec![],
            ..Default::default()
        }
    }

    fn auth_context(kind: auth::AuthKind, roles: &[&str]) -> auth::AuthContext {
        auth::AuthContext {
            user_id: 1,
            kind,
            scopes: None,
            server_ids: None,
            role_keys: roles.iter().map(|role| (*role).to_string()).collect(),
            permission_keys: Vec::new(),
            server_grants: Vec::new(),
        }
    }

    fn restricted_session_context() -> auth::AuthContext {
        let mut context = auth_context(auth::AuthKind::Session, &["operator"]);
        context.server_grants = vec![AdminServerGrant {
            server_id: "srv-1".to_string(),
            scope: "full".to_string(),
            granted_at: 0,
        }];
        context
    }

    #[test]
    fn get_diagnostics_response_exposes_cleanup_capability_fail_closed() {
        let super_admin_session = auth_context(auth::AuthKind::Session, &["super_admin"]);
        let response = diagnostics_response(
            vec![sample_record("diag-super-admin", "srv-1")],
            &super_admin_session,
        );
        assert_eq!(response.records.len(), 1);
        assert!(response.can_global_cleanup);
        let response_json = serde_json::to_value(&response).unwrap();
        assert!(response_json["records"].is_array());
        assert_eq!(
            response_json["can_global_cleanup"],
            serde_json::Value::Bool(true)
        );

        let ordinary_session = auth_context(auth::AuthKind::Session, &["operator"]);
        let restricted_session = restricted_session_context();
        let super_admin_api_token = auth_context(auth::AuthKind::ApiToken, &["super_admin"]);
        for context in [ordinary_session, restricted_session, super_admin_api_token] {
            let response = diagnostics_response(Vec::new(), &context);
            assert!(!response.can_global_cleanup);
        }
    }

    #[test]
    fn clean_authorization_requires_super_admin_session() {
        assert!(can_clean_global_diagnostics(&auth_context(
            auth::AuthKind::Session,
            &["super_admin"],
        )));
        assert!(!can_clean_global_diagnostics(&auth_context(
            auth::AuthKind::Session,
            &["operator"],
        )));
        assert!(!can_clean_global_diagnostics(&restricted_session_context()));
        assert!(!can_clean_global_diagnostics(&auth_context(
            auth::AuthKind::ApiToken,
            &["super_admin"],
        )));
    }

    #[test]
    fn validate_id_rejects_empty_long_whitespace_controls_and_credentials() {
        assert!(validate_id("request_id", "").is_err());
        assert!(validate_id("request_id", &"x".repeat(MAX_ID_LEN + 1)).is_err());
        assert!(validate_id("request_id", "ok-id").is_ok());
        assert!(validate_id("request_id", "bad id").is_err());
        assert!(validate_id("request_id", "tab\tid").is_err());
        assert!(validate_id("request_id", "nul\u{0000}id").is_err());
        assert!(validate_id("server_id", "Bearer zz").is_err());
        assert!(validate_id("media_id", "https://x").is_err());
        assert!(validate_id("session_id", "a=b").is_err());
    }

    #[test]
    fn limit_and_page_clamp_without_overflow() {
        let q = DiagnosticsQuery {
            limit: Some(0),
            page: Some(0),
            ..Default::default()
        };
        let (page, limit) = page_limit(&q);
        assert_eq!(limit, 1);
        assert_eq!(page, 1);

        let q = DiagnosticsQuery {
            limit: Some(9999),
            page: Some(usize::MAX),
            ..Default::default()
        };
        let (page, limit) = page_limit(&q);
        assert_eq!(limit, MAX_LIMIT);
        assert_eq!(page, MAX_PAGE);
        // Extreme page must not overflow.
        let start = page_start(page, limit);
        assert_eq!(start, (MAX_PAGE - 1).saturating_mul(MAX_LIMIT));
    }

    fn timed_sample(id: &str, server: &str, time: i64) -> DiagnosisRecord {
        let mut record = sample_record(id, server);
        record.started_at_ms = Some(time);
        record.duration_ms = Some(50);
        record.context.user_name = Some("Alice 测试".into());
        record.context.media_name = Some("Synthetic Film".into());
        record.result.outcome = Some("redirect".into());
        record
    }

    #[test]
    fn diagnosis_query_validates_enum_text_and_numeric_ranges() {
        for query in [
            DiagnosticsQuery {
                outcome: Some("success".into()),
                ..Default::default()
            },
            DiagnosticsQuery {
                user: Some(" ".into()),
                ..Default::default()
            },
            DiagnosticsQuery {
                keyword: Some("x".repeat(MAX_TEXT_FILTER_LEN + 1)),
                ..Default::default()
            },
            DiagnosticsQuery {
                keyword: Some("bad\ntext".into()),
                ..Default::default()
            },
            DiagnosticsQuery {
                since_ms: Some(0),
                ..Default::default()
            },
            DiagnosticsQuery {
                until_ms: Some(-1),
                ..Default::default()
            },
            DiagnosticsQuery {
                since_ms: Some(2),
                until_ms: Some(1),
                ..Default::default()
            },
            DiagnosticsQuery {
                session_id: Some("legacy".into()),
                ..Default::default()
            },
        ] {
            assert!(validate_query(&query).is_err(), "{query:?}");
        }
        assert!(
            validate_query(&DiagnosticsQuery {
                min_duration_ms: Some(0),
                since_ms: Some(1),
                until_ms: Some(i64::MAX),
                ..Default::default()
            })
            .is_ok()
        );
        for value in ["redirect", "proxy", "failed", "cancelled", "blocked"] {
            assert!(
                validate_query(&DiagnosticsQuery {
                    outcome: Some(value.into()),
                    ..Default::default()
                })
                .is_ok()
            );
        }
        // Integer deserialization rejects negative durations and fractional timestamps.
        assert!(
            serde_json::from_value::<DiagnosticsQuery>(serde_json::json!({"min_duration_ms": -1}))
                .is_err()
        );
        assert!(
            serde_json::from_value::<DiagnosticsQuery>(serde_json::json!({"since_ms": 1.5}))
                .is_err()
        );
    }

    #[test]
    fn diagnosis_merge_authorizes_deduplicates_and_paginates_exact_total() {
        let time = diagnostics_cutoff_ms() + 86_400_000;
        let persisted = vec![
            timed_sample("a", "srv-1", time),
            timed_sample("b", "srv-1", time + 1),
            timed_sample("private", "srv-2", time + 100),
            sample_record("legacy", "srv-1"),
        ];
        let mut newer = timed_sample("a", "srv-1", time + 3);
        newer.context.media_name = Some("Memory version".into());
        let memory = vec![newer, timed_sample("c", "srv-1", time + 1)];
        let query = DiagnosticsQuery {
            limit: Some(2),
            ..Default::default()
        };
        let context = restricted_session_context();
        let response =
            filter_and_paginate(persisted.clone(), memory.clone(), &query, &context).unwrap();
        assert_eq!(response.total, 4);
        assert!(response.has_more);
        assert_eq!(
            response
                .records
                .iter()
                .map(|r| r.request_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "c"]
        );
        assert_eq!(
            response.records[0].context.media_name.as_deref(),
            Some("Memory version")
        );
        assert!(!response.can_global_cleanup);
        assert!(response.storage.persistent);
        assert_eq!(response.storage.retention_days, 7);
        assert_eq!(response.storage.max_records, 10000);
        assert_eq!(response.storage.memory_capacity, 1000);
        let query = DiagnosticsQuery {
            page: Some(2),
            ..query
        };
        let response =
            filter_and_paginate(persisted.clone(), memory.clone(), &query, &context).unwrap();
        assert_eq!(response.total, 4);
        assert!(!response.has_more);
        assert_eq!(
            response
                .records
                .iter()
                .map(|r| r.request_id.as_str())
                .collect::<Vec<_>>(),
            ["b", "legacy"]
        );
        assert_eq!(response.records[1].started_at_ms, None);
        let response = filter_and_paginate(
            persisted,
            memory,
            &DiagnosticsQuery {
                page: Some(99),
                ..query
            },
            &context,
        )
        .unwrap();
        assert!(response.records.is_empty());
        assert_eq!(response.total, 4);
        assert!(!response.has_more);
    }

    #[test]
    fn diagnosis_filters_apply_before_pagination_and_unknown_values_do_not_match() {
        let context = restricted_session_context();
        let time = diagnostics_cutoff_ms() + 86_400_000;
        let mut wrong_user = timed_sample("wrong-user", "srv-1", time + 1);
        wrong_user.context.user_name = Some("Bob".into());
        let mut wrong_outcome = timed_sample("wrong-outcome", "srv-1", time + 2);
        wrong_outcome.result.outcome = Some("failed".into());
        let records = vec![
            timed_sample("match", "srv-1", time),
            wrong_user,
            wrong_outcome,
            sample_record("legacy", "srv-1"),
            timed_sample("expired", "srv-1", diagnostics_cutoff_ms() - 1),
        ];
        let query = DiagnosticsQuery {
            user: Some("aLIce".into()),
            keyword: Some("fILm".into()),
            outcome: Some("redirect".into()),
            since_ms: Some(time),
            until_ms: Some(time),
            min_duration_ms: Some(50),
            limit: Some(1),
            ..Default::default()
        };
        let response = filter_and_paginate(records.clone(), vec![], &query, &context).unwrap();
        assert_eq!(response.total, 1);
        assert_eq!(response.records[0].request_id, "match");
        assert!(!response.has_more);
        for query in [
            DiagnosticsQuery {
                user: Some("nobody".into()),
                ..Default::default()
            },
            DiagnosticsQuery {
                min_duration_ms: Some(51),
                ..Default::default()
            },
            DiagnosticsQuery {
                keyword: Some("not-present".into()),
                ..Default::default()
            },
            DiagnosticsQuery {
                media_id: Some("other".into()),
                ..Default::default()
            },
            DiagnosticsQuery {
                request_id: Some("absent".into()),
                ..Default::default()
            },
        ] {
            assert_eq!(
                filter_and_paginate(records.clone(), vec![], &query, &context)
                    .unwrap()
                    .total,
                0
            );
        }
        let query = DiagnosticsQuery {
            server_id: Some("srv-2".into()),
            ..Default::default()
        };
        assert!(matches!(
            filter_and_paginate(records, vec![], &query, &context),
            Err(AppError::Forbidden(_))
        ));
    }

    #[test]
    fn diagnosis_keyword_search_is_sanitized_and_excludes_json_keys() {
        let context = restricted_session_context();
        let mut record = sample_record("synthetic", "srv-1");
        record.steps[0].details = "token=synthetic-removed-value".into();
        for keyword in ["synthetic-removed-value", "started_at_ms"] {
            let query = DiagnosticsQuery {
                keyword: Some(keyword.into()),
                ..Default::default()
            };
            assert_eq!(
                filter_and_paginate(vec![record.clone()], vec![], &query, &context)
                    .unwrap()
                    .total,
                0
            );
        }
        let query = DiagnosticsQuery {
            keyword: Some("REDACTED".into()),
            ..Default::default()
        };
        assert_eq!(
            filter_and_paginate(vec![record], vec![], &query, &context)
                .unwrap()
                .total,
            1
        );
    }

    #[test]
    fn diagnosis_empty_grants_and_token_scope_fail_closed() {
        let records = vec![sample_record("one", "srv-1"), sample_record("two", "srv-2")];
        let query = DiagnosticsQuery::default();
        let no_grants = auth_context(auth::AuthKind::Session, &["operator"]);
        assert_eq!(
            filter_and_paginate(records.clone(), vec![], &query, &no_grants)
                .unwrap()
                .total,
            0
        );
        let mut token = auth_context(auth::AuthKind::ApiToken, &["super_admin"]);
        token.server_ids = Some(vec!["srv-1".into()]);
        let response = filter_and_paginate(records, vec![], &query, &token).unwrap();
        assert_eq!(response.total, 1);
        assert_eq!(response.records[0].server_id, "srv-1");
        assert!(!response.can_global_cleanup);
    }

    #[test]
    fn filter_and_page_preserves_order_and_empty() {
        let records = vec![
            sample_record("diag-1", "s1"),
            sample_record("diag-2", "s1"),
            sample_record("diag-3", "s2"),
        ];
        let filtered: Vec<_> = records
            .into_iter()
            .filter(|r| r.server_id == "s1")
            .collect();
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].request_id, "diag-1");
        assert_eq!(filtered[1].request_id, "diag-2");

        let start = page_start(3, 5);
        let page: Vec<_> = filtered.into_iter().skip(start).take(5).collect();
        assert!(page.is_empty());
    }

    #[test]
    fn clean_request_requires_yes() {
        let body = CleanDiagnosticsRequest {
            confirm: "no".to_string(),
        };
        assert_ne!(body.confirm, "yes");
        let body = CleanDiagnosticsRequest {
            confirm: "yes".to_string(),
        };
        assert_eq!(body.confirm, "yes");
    }

    #[test]
    fn api_egress_sanitizes_non_empty_dto_secrets() {
        let dirty = DiagnosisRecord {
            request_id: "diag-9".into(),
            server_id: "s1".into(),
            media_id: "m1".into(),
            ua: "ua".into(),
            steps: vec![DiagnosisStep {
                name: "openlist".into(),
                duration_ms: 2,
                details: "Authorization: Bearer SHOULD-NOT-LEAK api_key=XYZSECRET".into(),
                started_at_ms: Some(1_700_000_000_123),
                finished_at_ms: Some(1_700_000_000_125),
                ..Default::default()
            }],
            cache_hit: false,
            failure_node: None,
            sanitized_errors: vec![],
            started_at_ms: Some(1_700_000_000_123),
            finished_at_ms: Some(1_700_000_000_169),
            duration_ms: Some(46),
            ..Default::default()
        };
        let clean = sanitize_diagnosis_record(dirty);
        let json = serde_json::to_string(&clean).unwrap();
        assert!(!json.contains("SHOULD-NOT-LEAK"), "{json}");
        assert!(!json.contains("XYZSECRET"), "{json}");
        assert!(json.contains(REDACTED_PLACEHOLDER), "{json}");
        let response = diagnostics_response(
            vec![clean],
            &auth_context(auth::AuthKind::Session, &["super_admin"]),
        );
        let value = serde_json::to_value(response).unwrap();
        assert_eq!(value["records"][0]["started_at_ms"], 1_700_000_000_123_i64);
        assert_eq!(value["records"][0]["finished_at_ms"], 1_700_000_000_169_i64);
        assert_eq!(value["records"][0]["duration_ms"], 46);
        assert_eq!(
            value["records"][0]["steps"][0]["finished_at_ms"],
            1_700_000_000_125_i64
        );
    }

    #[test]
    fn create_path_and_query_helpers_agree_on_safe_shape() {
        let record = create_diagnosis_record(
            "srv",
            "media",
            "ua",
            vec![DiagnosisStep {
                name: "cache".into(),
                duration_ms: 1,
                details: "Cookie: sid=secretcookie path=/Movies".into(),
                ..Default::default()
            }],
            false,
            None,
        );
        let json = serde_json::to_string(&vec![record]).unwrap();
        assert!(!json.contains("secretcookie"), "{json}");
        assert!(json.contains("path=/Movies") || json.contains(REDACTED_PLACEHOLDER));
    }
}
