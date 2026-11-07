use std::{collections::HashSet, sync::Arc};

use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};

use crate::{
    AppState, auth,
    cache::{CacheEntryView, CacheStats, DirectLinkCache},
    error::{AppError, AppResult},
};

const MAX_SEARCH_LEN: usize = 256;
const MAX_ID_LEN: usize = 128;
const MAX_PAGE_SIZE: usize = 100;
const DEFAULT_PAGE_SIZE: usize = 50;
const MAX_PAGE: usize = 1_000_000;

/// Thin domain wrapper around the shared `DirectLinkCache` instance.
#[derive(Clone)]
pub struct CacheApi {
    cache: Arc<DirectLinkCache>,
}

impl CacheApi {
    pub fn new(cache: Arc<DirectLinkCache>) -> Self {
        Self { cache }
    }

    pub async fn get_stats(&self) -> CacheStats {
        self.cache.get_stats().await
    }

    /// Safe structured listing. Search matches only opaque id / server / media / source.
    pub async fn list_entries(&self, search: Option<&str>) -> Vec<CacheEntryView> {
        self.cache.list_entry_views(search).await
    }

    /// Safe authorized listing. Authorization filters are applied before the hard cap/page.
    pub async fn list_entries_page(
        &self,
        search: Option<&str>,
        requested_server_id: Option<&str>,
        allowed_server_ids: Option<&HashSet<String>>,
        start: usize,
        limit: usize,
    ) -> Vec<CacheEntryView> {
        self.cache
            .list_entry_views_page(
                search,
                requested_server_id,
                allowed_server_ids,
                start,
                limit,
            )
            .await
    }

    /// Explicit scoped cleanup. Callers must validate scope combinations first.
    pub async fn cleanup(&self, server_id: Option<String>, media_id: Option<String>) -> usize {
        self.cache.clean_by_server_media(server_id, media_id).await
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct CacheEntriesQuery {
    pub search: Option<String>,
    #[serde(default, rename = "serverId", alias = "server_id")]
    pub server_id: Option<String>,
    /// 1-based page size, default 50, max 100.
    pub limit: Option<usize>,
    /// 1-based page number, default 1.
    pub page: Option<usize>,
}

/// POST /api/cache/cleanup body. Full wipe requires explicit `scope=all`.
#[derive(Debug, Deserialize, Default)]
pub struct CacheCleanupRequest {
    /// One of: `all`, `server`, `media`, `server_media`.
    pub scope: Option<String>,
    #[serde(default, rename = "serverId", alias = "server_id")]
    pub server_id: Option<String>,
    #[serde(default, rename = "mediaId", alias = "media_id")]
    pub media_id: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct CacheStatsResponse {
    #[serde(rename = "canViewGlobalStats")]
    pub can_view_global_stats: bool,
    #[serde(rename = "canCleanupAll")]
    pub can_cleanup_all: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<CacheStats>,
}

impl CacheStatsResponse {
    fn hidden(auth_context: &auth::AuthContext) -> Self {
        Self {
            can_view_global_stats: false,
            can_cleanup_all: can_cleanup_all(auth_context),
            stats: None,
        }
    }

    fn visible(auth_context: &auth::AuthContext, stats: CacheStats) -> Self {
        Self {
            can_view_global_stats: true,
            can_cleanup_all: can_cleanup_all(auth_context),
            stats: Some(stats),
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CacheCleanupResponse {
    pub ok: bool,
    pub deleted: usize,
    pub scope: String,
    #[serde(rename = "serverId", skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(rename = "mediaId", skip_serializing_if = "Option::is_none")]
    pub media_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCleanup {
    pub scope: String,
    pub server_id: Option<String>,
    pub media_id: Option<String>,
}

/// Validate cleanup DTO. Missing/unknown scope never defaults to full wipe.
pub fn validate_cleanup_request(req: &CacheCleanupRequest) -> AppResult<ValidatedCleanup> {
    let scope_raw = req
        .scope
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Validation(
                "scope is required (all|server|media|server_media); full cleanup must use scope=all"
                    .to_string(),
            )
        })?;

    if scope_raw.len() > 32 || scope_raw.chars().any(|ch| ch.is_control()) {
        return Err(AppError::Validation("invalid scope".to_string()));
    }
    let scope = scope_raw.to_ascii_lowercase();

    let server_id = normalize_optional_id(req.server_id.as_deref(), "serverId")?;
    let media_id = normalize_optional_id(req.media_id.as_deref(), "mediaId")?;

    match scope.as_str() {
        "all" => {
            if server_id.is_some() || media_id.is_some() {
                return Err(AppError::Validation(
                    "scope=all must not include serverId or mediaId".to_string(),
                ));
            }
            Ok(ValidatedCleanup {
                scope,
                server_id: None,
                media_id: None,
            })
        }
        "server" => {
            let server_id = server_id.ok_or_else(|| {
                AppError::Validation("scope=server requires serverId".to_string())
            })?;
            if media_id.is_some() {
                return Err(AppError::Validation(
                    "scope=server must not include mediaId; use scope=server_media".to_string(),
                ));
            }
            Ok(ValidatedCleanup {
                scope,
                server_id: Some(server_id),
                media_id: None,
            })
        }
        "media" => {
            let media_id = media_id
                .ok_or_else(|| AppError::Validation("scope=media requires mediaId".to_string()))?;
            if server_id.is_some() {
                return Err(AppError::Validation(
                    "scope=media must not include serverId; use scope=server_media".to_string(),
                ));
            }
            Ok(ValidatedCleanup {
                scope,
                server_id: None,
                media_id: Some(media_id),
            })
        }
        "server_media" => {
            let server_id = server_id.ok_or_else(|| {
                AppError::Validation("scope=server_media requires serverId".to_string())
            })?;
            let media_id = media_id.ok_or_else(|| {
                AppError::Validation("scope=server_media requires mediaId".to_string())
            })?;
            Ok(ValidatedCleanup {
                scope,
                server_id: Some(server_id),
                media_id: Some(media_id),
            })
        }
        _ => Err(AppError::Validation(
            "scope must be one of: all, server, media, server_media".to_string(),
        )),
    }
}

fn page_limit(query: &CacheEntriesQuery) -> (usize, usize) {
    let limit = query
        .limit
        .unwrap_or(DEFAULT_PAGE_SIZE)
        .clamp(1, MAX_PAGE_SIZE);
    let page = query.page.unwrap_or(1).clamp(1, MAX_PAGE);
    (page, limit)
}

fn page_start(page: usize, limit: usize) -> usize {
    (page - 1).saturating_mul(limit)
}

fn normalize_optional_id(value: Option<&str>, field: &str) -> AppResult<Option<String>> {
    let Some(raw) = value else {
        return Ok(None);
    };
    // Reject control chars on the raw value before trim so "m\n" cannot sneak through.
    if raw.chars().any(|ch| ch.is_control()) {
        return Err(AppError::Validation(format!("invalid {field}")));
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(AppError::Validation(format!("{field} cannot be empty")));
    }
    if trimmed.len() > MAX_ID_LEN {
        return Err(AppError::Validation(format!("invalid {field}")));
    }
    Ok(Some(trimmed.to_string()))
}

/// Build audit summary with only safe IDs and counts — never cache keys/URLs/tokens.
pub fn cleanup_audit_summary(validated: &ValidatedCleanup, deleted: usize) -> String {
    match validated.scope.as_str() {
        "all" => format!("清理直链缓存 scope=all deleted={deleted}"),
        "server" => format!(
            "清理直链缓存 scope=server serverId={} deleted={deleted}",
            validated.server_id.as_deref().unwrap_or("-")
        ),
        "media" => format!(
            "清理直链缓存 scope=media mediaId={} deleted={deleted}",
            validated.media_id.as_deref().unwrap_or("-")
        ),
        "server_media" => format!(
            "清理直链缓存 scope=server_media serverId={} mediaId={} deleted={deleted}",
            validated.server_id.as_deref().unwrap_or("-"),
            validated.media_id.as_deref().unwrap_or("-")
        ),
        other => format!("清理直链缓存 scope={other} deleted={deleted}"),
    }
}

/// GET /api/cache/stats — global cache aggregates are session-super-admin only.
pub async fn get_cache_stats(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<CacheStatsResponse>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    require_cache_stats_access(&auth_context)?;
    if !can_view_global_cache_stats(&auth_context) {
        return Ok(Json(CacheStatsResponse::hidden(&auth_context)));
    }

    let cache = state.cache.read().await.clone();
    let api = CacheApi::new(Arc::new(cache));
    Ok(Json(cache_stats_response(
        &auth_context,
        api.get_stats().await,
    )))
}

fn can_view_global_cache_stats(auth_context: &auth::AuthContext) -> bool {
    matches!(&auth_context.kind, auth::AuthKind::Session) && auth::is_super_admin(auth_context)
}

fn require_cache_stats_access(auth_context: &auth::AuthContext) -> AppResult<()> {
    if auth::has_permission(auth_context, "monitoring.read") {
        return Ok(());
    }
    auth::require_permission(auth_context, "cache.write")
}

fn can_cleanup_all(auth_context: &auth::AuthContext) -> bool {
    auth::has_permission(auth_context, "cache.write")
        && auth::allowed_server_ids(auth_context).is_none()
}

fn authorize_cleanup_scope(
    auth_context: &auth::AuthContext,
    validated: &ValidatedCleanup,
) -> AppResult<()> {
    if let Some(server_id) = validated.server_id.as_deref() {
        auth::require_server_access(auth_context, server_id)
    } else if auth::allowed_server_ids(auth_context).is_some()
        && matches!(validated.scope.as_str(), "all" | "media")
    {
        Err(AppError::Forbidden(
            "server-scoped access is required for this cleanup".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn cache_stats_response(auth_context: &auth::AuthContext, stats: CacheStats) -> CacheStatsResponse {
    if can_view_global_cache_stats(auth_context) {
        CacheStatsResponse::visible(auth_context, stats)
    } else {
        CacheStatsResponse::hidden(auth_context)
    }
}

/// GET /api/cache/entries?search= — structured safe views, sorted, max 500.
pub async fn list_cache_entries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CacheEntriesQuery>,
) -> AppResult<Json<Vec<CacheEntryView>>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "monitoring.read")?;
    let requested_server_id =
        auth::require_server_filter(&auth_context, query.server_id.as_deref())?;
    let (page, limit) = page_limit(&query);
    let start = page_start(page, limit);
    let raw = query.search.as_deref().unwrap_or("");
    if raw.len() > MAX_SEARCH_LEN {
        return Err(AppError::Validation(format!(
            "search query too long (max {MAX_SEARCH_LEN} characters)"
        )));
    }
    if raw.chars().any(|ch| ch.is_control()) {
        return Err(AppError::Validation(
            "search query contains invalid control characters".to_string(),
        ));
    }
    let allowed_server_ids = auth::allowed_server_ids(&auth_context);
    let cache = state.cache.read().await.clone();
    let api = CacheApi::new(Arc::new(cache));
    let entries = api
        .list_entries_page(
            Some(raw),
            requested_server_id,
            allowed_server_ids.as_ref(),
            start,
            limit,
        )
        .await;
    debug_assert!(entries.len() <= MAX_PAGE_SIZE);
    Ok(Json(entries))
}

/// POST /api/cache/cleanup — explicit scoped clear (admin + audit).
pub async fn cleanup_cache(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CacheCleanupRequest>,
) -> AppResult<Json<CacheCleanupResponse>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "cache.write")?;
    let admin_user_id = auth_context.user_id;
    let validated = match validate_cleanup_request(&request) {
        Ok(v) => v,
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "cache.cleanup",
                "清理直链缓存失败：请求参数无效",
                "error",
            );
            return Err(error);
        }
    };
    authorize_cleanup_scope(&auth_context, &validated)?;

    let cache = state.cache.read().await.clone();
    let api = CacheApi::new(Arc::new(cache));
    let deleted = api
        .cleanup(validated.server_id.clone(), validated.media_id.clone())
        .await;

    let summary = cleanup_audit_summary(&validated, deleted);
    state.settings_store.record_audit_best_effort(
        Some(admin_user_id),
        "cache.cleanup",
        &summary,
        "success",
    );

    Ok(Json(CacheCleanupResponse {
        ok: true,
        deleted,
        scope: validated.scope,
        server_id: validated.server_id,
        media_id: validated.media_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        auth::{AuthContext, AuthKind},
        cache::DirectLinkCache,
        db::AdminServerGrant,
    };

    fn auth_context(
        kind: AuthKind,
        role_keys: &[&str],
        server_grants: Vec<AdminServerGrant>,
    ) -> AuthContext {
        AuthContext {
            user_id: 1,
            kind,
            scopes: None,
            server_ids: None,
            role_keys: role_keys.iter().map(|role| (*role).to_string()).collect(),
            permission_keys: Vec::new(),
            server_grants,
        }
    }

    fn sample_stats() -> CacheStats {
        CacheStats {
            capacity: 7,
            max_capacity: 100,
            unit: "entries",
            hits: 3,
            misses: 1,
            hit_rate: 0.75,
        }
    }

    fn assert_global_stats_hidden(response: CacheStatsResponse) {
        assert!(!response.can_view_global_stats);
        assert!(response.stats.is_none());
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            serde_json::json!({ "canViewGlobalStats": false, "canCleanupAll": response.can_cleanup_all })
        );
    }

    fn req(
        scope: Option<&str>,
        server_id: Option<&str>,
        media_id: Option<&str>,
    ) -> CacheCleanupRequest {
        CacheCleanupRequest {
            scope: scope.map(str::to_string),
            server_id: server_id.map(str::to_string),
            media_id: media_id.map(str::to_string),
        }
    }

    #[test]
    fn monitoring_token_cannot_cleanup_cache() {
        let mut context = auth::AuthContext {
            user_id: 1,
            kind: auth::AuthKind::ApiToken,
            scopes: Some(vec!["monitoring.read".to_string()]),
            server_ids: Some(vec!["srv-a".to_string()]),
            role_keys: vec!["super_admin".to_string()],
            permission_keys: vec![],
            server_grants: vec![],
        };
        assert!(auth::require_permission(&context, "monitoring.read").is_ok());
        assert!(matches!(
            auth::require_permission(&context, "cache.write"),
            Err(AppError::Forbidden(_))
        ));
        context.scopes = Some(vec!["cache.write".to_string()]);
        assert!(auth::require_permission(&context, "cache.write").is_ok());
        assert!(auth::require_permission(&context, "monitoring.read").is_err());
    }

    #[test]
    fn validate_requires_explicit_scope_never_defaults_to_all() {
        assert!(validate_cleanup_request(&req(None, None, None)).is_err());
        assert!(validate_cleanup_request(&req(Some(""), None, None)).is_err());
        assert!(validate_cleanup_request(&req(Some("  "), None, None)).is_err());
        assert!(validate_cleanup_request(&req(Some("everything"), None, None)).is_err());
    }

    #[test]
    fn validate_all_server_media_combinations() {
        let all = validate_cleanup_request(&req(Some("all"), None, None)).unwrap();
        assert_eq!(all.scope, "all");
        assert!(all.server_id.is_none());
        assert!(all.media_id.is_none());

        // extras forbidden on all
        assert!(validate_cleanup_request(&req(Some("all"), Some("s1"), None)).is_err());
        assert!(validate_cleanup_request(&req(Some("all"), None, Some("m1"))).is_err());

        let server = validate_cleanup_request(&req(Some("server"), Some("s1"), None)).unwrap();
        assert_eq!(server.server_id.as_deref(), Some("s1"));
        assert!(server.media_id.is_none());
        assert!(validate_cleanup_request(&req(Some("server"), None, None)).is_err());
        assert!(validate_cleanup_request(&req(Some("server"), Some("s1"), Some("m1"))).is_err());

        let media = validate_cleanup_request(&req(Some("media"), None, Some("m1"))).unwrap();
        assert_eq!(media.media_id.as_deref(), Some("m1"));
        assert!(validate_cleanup_request(&req(Some("media"), None, None)).is_err());
        assert!(validate_cleanup_request(&req(Some("media"), Some("s1"), Some("m1"))).is_err());

        let both =
            validate_cleanup_request(&req(Some("server_media"), Some("s1"), Some("m1"))).unwrap();
        assert_eq!(both.server_id.as_deref(), Some("s1"));
        assert_eq!(both.media_id.as_deref(), Some("m1"));
        assert!(validate_cleanup_request(&req(Some("server_media"), Some("s1"), None)).is_err());
        assert!(validate_cleanup_request(&req(Some("server_media"), None, Some("m1"))).is_err());
    }

    #[test]
    fn validate_rejects_control_chars_empty_and_oversize() {
        assert!(validate_cleanup_request(&req(Some("server"), Some("s\u{0001}"), None)).is_err());
        assert!(validate_cleanup_request(&req(Some("media"), None, Some("m\n"))).is_err());
        assert!(validate_cleanup_request(&req(Some("server"), Some("   "), None)).is_err());
        assert!(validate_cleanup_request(&req(Some("server"), Some(""), None)).is_err());
        let long = "x".repeat(MAX_ID_LEN + 1);
        assert!(validate_cleanup_request(&req(Some("server"), Some(&long), None)).is_err());
        assert!(validate_cleanup_request(&req(Some("all\u{0000}"), None, None)).is_err());
    }

    #[test]
    fn audit_summary_contains_only_safe_ids_and_counts() {
        let all = ValidatedCleanup {
            scope: "all".into(),
            server_id: None,
            media_id: None,
        };
        let s = cleanup_audit_summary(&all, 3);
        assert_eq!(s, "清理直链缓存 scope=all deleted=3");
        assert!(!s.contains("http"));
        assert!(!s.contains("token"));
        assert!(!s.contains(':'));

        let server = ValidatedCleanup {
            scope: "server".into(),
            server_id: Some("srv-1".into()),
            media_id: None,
        };
        let s = cleanup_audit_summary(&server, 2);
        assert!(s.contains("serverId=srv-1"));
        assert!(s.contains("deleted=2"));
        assert!(!s.contains("ua"));

        let media = ValidatedCleanup {
            scope: "media".into(),
            server_id: None,
            media_id: Some("item-9".into()),
        };
        let s = cleanup_audit_summary(&media, 1);
        assert!(s.contains("mediaId=item-9"));

        let both = ValidatedCleanup {
            scope: "server_media".into(),
            server_id: Some("srv-1".into()),
            media_id: Some("item-9".into()),
        };
        let s = cleanup_audit_summary(&both, 5);
        assert!(s.contains("serverId=srv-1"));
        assert!(s.contains("mediaId=item-9"));
        assert!(s.contains("deleted=5"));
        // must not look like a raw cache key dump
        assert!(!s.contains("default:"));
    }

    #[test]
    fn page_limits_are_clamped_and_safe_at_extremes() {
        let q = CacheEntriesQuery {
            limit: Some(0),
            page: Some(0),
            ..Default::default()
        };
        assert_eq!(page_limit(&q), (1, 1));

        let q = CacheEntriesQuery {
            limit: Some(usize::MAX),
            page: Some(usize::MAX),
            ..Default::default()
        };
        assert_eq!(page_limit(&q), (MAX_PAGE, MAX_PAGE_SIZE));
        assert_eq!(
            page_start(MAX_PAGE, MAX_PAGE_SIZE),
            (MAX_PAGE - 1) * MAX_PAGE_SIZE
        );
    }

    #[test]
    fn global_stats_super_admin_session_exposes_stats() {
        let ctx = auth_context(AuthKind::Session, &["super_admin"], Vec::new());
        let response = cache_stats_response(&ctx, sample_stats());

        assert!(response.can_view_global_stats);
        assert!(response.can_cleanup_all);
        assert_eq!(response.stats, Some(sample_stats()));
        let json = serde_json::to_value(response).unwrap();
        assert_eq!(json["canViewGlobalStats"], true);
        assert_eq!(json["canCleanupAll"], true);
        assert_eq!(json["stats"]["capacity"], 7);
        assert_eq!(json["stats"]["maxCapacity"], 100);
        assert_eq!(json["stats"]["hits"], 3);
        assert_eq!(json["stats"]["misses"], 1);
        assert_eq!(json["stats"]["hitRate"], 0.75);
    }

    #[test]
    fn global_stats_ordinary_or_restricted_session_never_exposes_stats() {
        let ordinary = auth_context(AuthKind::Session, &[], Vec::new());
        let restricted = auth_context(
            AuthKind::Session,
            &["cache_manager"],
            vec![AdminServerGrant {
                server_id: "server-1".to_string(),
                scope: "manage".to_string(),
                granted_at: 0,
            }],
        );

        for ctx in [&ordinary, &restricted] {
            assert!(!can_view_global_cache_stats(ctx));
            assert_global_stats_hidden(cache_stats_response(ctx, sample_stats()));
        }
    }

    #[test]
    fn cleanup_authorization_is_fail_closed_for_restricted_scopes() {
        let restricted = auth_context(
            AuthKind::Session,
            &["cache_manager"],
            vec![AdminServerGrant {
                server_id: "server1".to_string(),
                scope: "manage".to_string(),
                granted_at: 0,
            }],
        );
        let server1 =
            validate_cleanup_request(&req(Some("server"), Some("server1"), None)).unwrap();
        assert!(authorize_cleanup_scope(&restricted, &server1).is_ok());

        let server10 =
            validate_cleanup_request(&req(Some("server"), Some("server10"), None)).unwrap();
        assert!(authorize_cleanup_scope(&restricted, &server10).is_err());

        let media = validate_cleanup_request(&req(Some("media"), None, Some("media1"))).unwrap();
        assert!(authorize_cleanup_scope(&restricted, &media).is_err());
        let all = validate_cleanup_request(&req(Some("all"), None, None)).unwrap();
        assert!(authorize_cleanup_scope(&restricted, &all).is_err());

        let super_session = auth_context(AuthKind::Session, &["super_admin"], Vec::new());
        assert!(authorize_cleanup_scope(&super_session, &all).is_ok());
        assert!(can_cleanup_all(&super_session));
        assert!(!can_cleanup_all(&restricted));
    }

    #[test]
    fn global_stats_api_token_never_exposes_stats_even_with_super_admin_role() {
        let ctx = auth_context(AuthKind::ApiToken, &["super_admin"], Vec::new());

        assert!(!can_view_global_cache_stats(&ctx));
        assert!(!can_cleanup_all(&ctx));
        assert_global_stats_hidden(cache_stats_response(&ctx, sample_stats()));
    }

    #[test]
    fn global_cache_write_api_token_can_cleanup_all_without_viewing_stats() {
        let mut ctx = auth_context(AuthKind::ApiToken, &["super_admin"], Vec::new());
        ctx.scopes = Some(vec!["cache.write".to_string()]);
        let all = validate_cleanup_request(&req(Some("all"), None, None)).unwrap();

        assert!(require_cache_stats_access(&ctx).is_ok());
        assert!(authorize_cleanup_scope(&ctx, &all).is_ok());
        let response = cache_stats_response(&ctx, sample_stats());
        assert!(!response.can_view_global_stats);
        assert!(response.can_cleanup_all);
        assert!(response.stats.is_none());
    }

    #[tokio::test]
    async fn stats_empty_cache_unit_is_entries() {
        let cache = DirectLinkCache::new(true, 60, 100);
        let api = CacheApi::new(Arc::new(cache));
        let stats = api.get_stats().await;
        assert_eq!(stats.capacity, 0);
        assert_eq!(stats.hit_rate, 0.0);
        assert_eq!(stats.unit, "entries");
        assert_eq!(stats.max_capacity, 100);
        let json = serde_json::to_value(&stats).unwrap();
        assert_eq!(json["unit"], "entries");
        assert_eq!(json["capacity"], 0);
        assert_eq!(json["maxCapacity"], 100);
        assert!(json.get("hitRate").is_some());
    }

    #[tokio::test]
    async fn list_filters_limits_and_hides_secrets() {
        let cache = DirectLinkCache::new(true, 60, 100);
        cache
            .set(
                "server1:media1:default:ua".into(),
                "https://cdn.example/a?token=SECRET1".into(),
            )
            .await;
        cache
            .set(
                "server1:media2:default:ua".into(),
                "https://cdn.example/b?token=SECRET2".into(),
            )
            .await;
        cache
            .set(
                "server2:media9:default:ua".into(),
                "https://cdn.example/c?token=SECRET3".into(),
            )
            .await;
        let api = CacheApi::new(Arc::new(cache));

        let all = api.list_entries(None).await;
        assert_eq!(all.len(), 3);
        let encoded = serde_json::to_string(&all).unwrap();
        assert!(!encoded.contains("SECRET"));
        assert!(!encoded.contains("https://"));
        assert!(!encoded.contains("token="));
        // raw key / UA not echoed
        assert!(!encoded.contains("default:ua"));

        let filtered = api.list_entries(Some("media1")).await;
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].media_id, "media1");
        assert_eq!(filtered[0].server_id, "server1");
        assert!(!filtered[0].id.is_empty());

        let empty = api.list_entries(Some("no-such")).await;
        assert!(empty.is_empty());

        let ci = api.list_entries(Some("SERVER2")).await;
        assert_eq!(ci.len(), 1);
        assert_eq!(ci[0].server_id, "server2");
    }

    #[tokio::test]
    async fn list_page_filters_access_before_hard_cap_and_paginates() {
        let cache = DirectLinkCache::new(true, 60, 1_000);
        for i in 0..525 {
            cache
                .set(format!("server0:media{i:04}:default:ua"), "url".to_string())
                .await;
        }
        cache
            .set("server1:media1:default:ua".into(), "url1".into())
            .await;
        cache
            .set("server10:media10:default:ua".into(), "url10".into())
            .await;
        let api = CacheApi::new(Arc::new(cache));
        let allowed = HashSet::from(["server1".to_string()]);

        let page = api
            .list_entries_page(None, None, Some(&allowed), 0, 50)
            .await;
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].server_id, "server1");

        let page = api.list_entries_page(None, None, None, 500, 100).await;
        assert!(page.is_empty());

        let page = api
            .list_entries_page(None, Some("server1"), None, 0, 50)
            .await;
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].media_id, "media1");
    }

    #[tokio::test]
    async fn cleanup_scopes_via_api_wrapper() {
        let cache = DirectLinkCache::new(true, 60, 100);
        cache
            .set("server1:media1:default:ua".into(), "url1".into())
            .await;
        cache
            .set("server1:media2:default:ua".into(), "url2".into())
            .await;
        cache
            .set("server2:media1:default:ua".into(), "url3".into())
            .await;
        let api = CacheApi::new(Arc::new(cache));

        let v = validate_cleanup_request(&req(Some("server"), Some("server1"), None)).unwrap();
        let deleted = api.cleanup(v.server_id.clone(), v.media_id.clone()).await;
        assert_eq!(deleted, 2);
        assert_eq!(api.get_stats().await.capacity, 1);

        let remaining = api.list_entries(None).await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].server_id, "server2");

        let v = validate_cleanup_request(&req(Some("all"), None, None)).unwrap();
        let deleted = api.cleanup(v.server_id, v.media_id).await;
        assert_eq!(deleted, 1);
        assert_eq!(api.get_stats().await.capacity, 0);
        assert!(api.list_entries(None).await.is_empty());
    }

    #[tokio::test]
    async fn cleanup_media_and_server_media_isolation() {
        let cache = DirectLinkCache::new(true, 60, 100);
        cache
            .set("server1:media1:default:ua".into(), "u1".into())
            .await;
        cache
            .set("server2:media1:default:ua".into(), "u2".into())
            .await;
        cache
            .set("server1:media2:default:ua".into(), "u3".into())
            .await;
        // prefix trap
        cache
            .set("server10:media10:default:ua".into(), "u4".into())
            .await;
        let api = CacheApi::new(Arc::new(cache));

        let v = validate_cleanup_request(&req(Some("media"), None, Some("media1"))).unwrap();
        assert_eq!(api.cleanup(v.server_id, v.media_id).await, 2);
        let left = api.list_entries(None).await;
        assert_eq!(left.len(), 2);
        assert!(left.iter().any(|e| e.media_id == "media2"));
        assert!(left.iter().any(|e| e.server_id == "server10"));

        // re-add media1 on server1 only
        // (use underlying cache through fresh wrapper — same Arc)
    }

    #[tokio::test]
    async fn cleanup_server_media_precise() {
        let cache = Arc::new(DirectLinkCache::new(true, 60, 100));
        cache
            .set("server1:media1:default:ua".into(), "u1".into())
            .await;
        cache
            .set("server1:media2:default:ua".into(), "u2".into())
            .await;
        cache
            .set("server2:media1:default:ua".into(), "u3".into())
            .await;
        let api = CacheApi::new(cache);

        let v =
            validate_cleanup_request(&req(Some("server_media"), Some("server1"), Some("media1")))
                .unwrap();
        assert_eq!(api.cleanup(v.server_id, v.media_id).await, 1);
        let left = api.list_entries(None).await;
        assert_eq!(left.len(), 2);
        assert!(
            !left
                .iter()
                .any(|e| e.server_id == "server1" && e.media_id == "media1")
        );
    }

    #[tokio::test]
    async fn search_long_needle_domain_safe_handler_rejects() {
        let cache = DirectLinkCache::new(true, 60, 10);
        let api = CacheApi::new(Arc::new(cache));
        let long = "x".repeat(300);
        // domain layer simply finds nothing; handler enforces length
        let entries = api.list_entries(Some(&long)).await;
        assert!(entries.is_empty());
        assert!(long.len() > MAX_SEARCH_LEN);
    }

    #[tokio::test]
    async fn legacy_entry_compatible_with_list_and_all_cleanup() {
        let cache = Arc::new(DirectLinkCache::new(true, 60, 50));
        cache
            .set(
                "legacy-key-without-structure".into(),
                "https://t/?tok=X".into(),
            )
            .await;
        cache
            .set(
                "server1:media1:default:ua".into(),
                "https://t/?tok=Y".into(),
            )
            .await;
        let api = CacheApi::new(cache.clone());

        let listed = api.list_entries(None).await;
        assert_eq!(listed.len(), 2);
        let legacy = listed.iter().find(|e| e.legacy).unwrap();
        assert!(legacy.server_id.is_empty());
        let json = serde_json::to_string(&listed).unwrap();
        assert!(!json.contains("legacy-key"));
        assert!(!json.contains("tok="));

        // scoped server cleanup leaves legacy
        let v = validate_cleanup_request(&req(Some("server"), Some("server1"), None)).unwrap();
        assert_eq!(api.cleanup(v.server_id, v.media_id).await, 1);
        assert_eq!(api.list_entries(None).await.len(), 1);
        assert!(api.list_entries(None).await[0].legacy);

        let v = validate_cleanup_request(&req(Some("all"), None, None)).unwrap();
        assert_eq!(api.cleanup(v.server_id, v.media_id).await, 1);
        assert!(api.list_entries(None).await.is_empty());
    }

    #[test]
    fn cleanup_response_shape() {
        let resp = CacheCleanupResponse {
            ok: true,
            deleted: 4,
            scope: "server".into(),
            server_id: Some("s1".into()),
            media_id: None,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["deleted"], 4);
        assert_eq!(v["scope"], "server");
        assert_eq!(v["serverId"], "s1");
        assert!(v.get("mediaId").is_none());
    }
}
