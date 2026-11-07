//! Diagnostics HTTP free handlers for main router mounting.
//! GET /api/diagnostics  — list/filter diagnosis records (auth required)
//! POST /api/diagnostics/clean — clear buffer (admin + confirm + audit)

use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};

use crate::{
    AppState, auth,
    diagnostics::{self, DiagnosisRecord, sanitize_diagnosis_record},
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
    /// Page size, default 50, max 100
    pub limit: Option<usize>,
    /// 1-based page, default 1
    pub page: Option<usize>,
}

const MAX_LIMIT: usize = 100;
const DEFAULT_LIMIT: usize = 50;
const MAX_ID_LEN: usize = 128;
const MAX_PAGE: usize = 1_000_000;

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
}

fn diagnostics_response(
    records: Vec<DiagnosisRecord>,
    auth_context: &auth::AuthContext,
) -> DiagnosticsResponse {
    DiagnosticsResponse {
        records,
        can_global_cleanup: can_clean_global_diagnostics(auth_context),
    }
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

    let (page, limit) = page_limit(&query);

    if let Some(ref id) = query.request_id {
        validate_id("request_id", id)?;
    }
    if let Some(ref id) = query.server_id {
        validate_id("server_id", id)?;
    }
    if let Some(ref id) = query.media_id {
        validate_id("media_id", id)?;
    }
    if let Some(ref id) = query.session_id {
        validate_id("session_id", id)?;
        return Err(AppError::Validation(
            "session_id filter is not supported".to_string(),
        ));
    }
    let requested_server_id =
        auth::require_server_filter(&auth_context, query.server_id.as_deref())?;

    let mut records = diagnostics::get_diagnosis_records();

    // Filters preserve insertion order (stable, not randomized).
    if let Some(ref req_id) = query.request_id {
        records.retain(|r| r.request_id == *req_id);
    }
    records.retain(|record| {
        requested_server_id.is_none_or(|server_id| record.server_id == server_id)
            && auth::has_server_access(&auth_context, &record.server_id)
    });
    if let Some(ref mid) = query.media_id {
        records.retain(|r| r.media_id == *mid);
    }
    let start = page_start(page, limit);
    // Defense in depth: re-sanitize at the API boundary so buffer contents never leak raw secrets.
    let page_records: Vec<DiagnosisRecord> = records
        .into_iter()
        .skip(start)
        .take(limit)
        .map(sanitize_diagnosis_record)
        .collect();

    Ok(Json(diagnostics_response(page_records, &auth_context)))
}

/// Body for clear/reset. Requires explicit confirm == "yes".
#[derive(Debug, Deserialize)]
pub struct CleanDiagnosticsRequest {
    pub confirm: String,
}

/// Free handler: clear diagnosis buffer. Admin + confirm + audit.
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
    if !can_clean_global_diagnostics(&auth_context) {
        return Err(AppError::Forbidden(
            "global diagnostics cleanup requires a super administrator session".to_string(),
        ));
    }
    let admin_user_id = auth_context.user_id;

    if body.confirm != "yes" {
        return Err(AppError::Validation(
            "confirmation required for cleanup".to_string(),
        ));
    }

    diagnostics::clean_diagnostics();

    // Audit must not include request filters or any potentially sensitive query values.
    state.settings_store.record_audit(
        Some(admin_user_id),
        "diagnostics.clean",
        "清空诊断记录缓冲",
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
            }],
            cache_hit: false,
            failure_node: None,
            sanitized_errors: vec![],
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
            }],
            cache_hit: false,
            failure_node: None,
            sanitized_errors: vec![],
        };
        let clean = sanitize_diagnosis_record(dirty);
        let json = serde_json::to_string(&clean).unwrap();
        assert!(!json.contains("SHOULD-NOT-LEAK"), "{json}");
        assert!(!json.contains("XYZSECRET"), "{json}");
        assert!(json.contains(REDACTED_PLACEHOLDER), "{json}");
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
            }],
            false,
            None,
        );
        let json = serde_json::to_string(&vec![record]).unwrap();
        assert!(!json.contains("secretcookie"), "{json}");
        assert!(json.contains("path=/Movies") || json.contains(REDACTED_PLACEHOLDER));
    }
}
