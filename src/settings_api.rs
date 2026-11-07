use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use argon2::Argon2;
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AppState, auth,
    client_control::{self, ClientControlConfig},
    config::Config,
    crypto_api::EncryptedRequest,
    direct_link_cache_from_config,
    error::{AppError, AppResult, safe_error_message},
    file_log::{SETTING_KEY as SYSTEM_LOG_SETTING_KEY, SystemLogConfig, normalize_config},
    users_api::{
        USER_POLICIES_SETTING_KEY, USER_TEMPLATES_SETTING_KEY, UserPoliciesDocument,
        UserTemplatesDocument, normalize_user_policies, normalize_user_templates,
    },
};

const CLIENT_CONTROL_SETTING_KEY: &str = "client_control";
const BACKUP_SCHEMA_VERSION: u32 = 2;
const BACKUP_ENVELOPE_VERSION: u32 = 1;
const BACKUP_SALT_BYTES: usize = 16;
const BACKUP_NONCE_BYTES: usize = 12;
const MAX_BACKUP_TEXT_BYTES: usize = 8 * 1024 * 1024;
const OPENLIST_TOKEN_OVERRIDE_MASK: &str = "********";

#[derive(Debug, Deserialize)]
pub struct RestartProxyRequest {
    server_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ToggleProxyRequest {
    server_id: String,
    enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct BackupImportRequest {
    #[serde(default)]
    password: Option<String>,
    backup: String,
}

#[derive(Debug, Deserialize)]
pub struct BackupExportRequest {
    password: String,
}

#[derive(Debug, Serialize)]
pub struct BackupExportResponse {
    backup: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct BackupEnvelope {
    version: u32,
    kdf: String,
    cipher: String,
    salt: String,
    nonce: String,
    data: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct BackupPayload {
    #[serde(default = "legacy_backup_schema_version")]
    schema_version: u32,
    #[serde(default)]
    application_version: Option<String>,
    runtime_config: Config,
    #[serde(default)]
    client_control: Option<ClientControlConfig>,
    #[serde(default)]
    system_log_config: Option<SystemLogConfig>,
    #[serde(default)]
    user_policies: Option<UserPoliciesDocument>,
    #[serde(default)]
    user_templates: Option<UserTemplatesDocument>,
}

#[derive(Debug, Serialize)]
pub struct ValidationResponse {
    ok: bool,
    results: Vec<ValidationResult>,
}

#[derive(Debug, Serialize)]
pub struct ValidationResult {
    scope: String,
    ok: bool,
    message: String,
    detail: String,
}

#[derive(Debug, Serialize)]
pub struct EmbyApiKeyResponse {
    api_key: String,
}

const ADMIN_SECURITY_PERMISSION: &str = "admin.security.read";
const ADMIN_SECURITY_WRITE_PERMISSION: &str = "admin.security.write";
// The shared authentication surface now handles the login second-factor challenge.
const ADMIN_TOTP_LOGIN_CHALLENGE_AVAILABLE: bool = true;

#[derive(Debug, Deserialize)]
pub struct AdminCreateRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub role_keys: Vec<String>,
    #[serde(default)]
    pub server_grants: Vec<AdminServerGrantPayload>,
}

#[derive(Debug, Deserialize)]
pub struct AdminRolesRequest {
    pub role_keys: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct AdminServerGrantsRequest {
    #[serde(default)]
    pub server_grants: Vec<AdminServerGrantPayload>,
}

#[derive(Debug, Deserialize)]
pub struct AdminStatusRequest {
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct AdminPasswordResetRequest {
    pub new_password: String,
}

#[derive(Debug, Deserialize)]
pub struct AdminServerGrantPayload {
    pub server_id: String,
    #[serde(default = "default_server_scope")]
    pub scope: String,
}

#[derive(Debug, Serialize)]
pub struct AdminSecurityResponse {
    pub current_user_id: i64,
    pub admins: Vec<AdminView>,
    pub roles: Vec<AdminRoleView>,
}

#[derive(Debug, Serialize)]
pub struct AdminView {
    pub id: i64,
    pub username: String,
    pub display_name: Option<String>,
    pub enabled: bool,
    pub disabled_at: Option<i64>,
    pub totp_enabled: bool,
    pub totp_pending: bool,
    pub created_at: String,
    pub updated_at: Option<i64>,
    pub role_keys: Vec<String>,
    pub permission_keys: Vec<String>,
    pub server_grants: Vec<AdminServerGrantView>,
}

#[derive(Debug, Serialize)]
pub struct AdminServerGrantView {
    pub server_id: String,
    pub scope: String,
    pub granted_at: i64,
    pub server_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AdminRoleView {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub description: String,
    pub is_system: bool,
    pub permissions: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct AdminMutationResponse {
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct AdminTokenCreateRequest {
    pub name: String,
    #[serde(default, alias = "scopes")]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub server_ids: Option<Vec<String>>,
    #[serde(default)]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct AdminTokenCreateResponse {
    pub id: i64,
    pub token: String,
    pub token_prefix: String,
    pub name: String,
    pub permissions: Vec<String>,
    pub server_ids: Option<Vec<String>>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct AdminTokensResponse {
    pub tokens: Vec<AdminTokenView>,
}

#[derive(Debug, Serialize)]
pub struct AdminTokenView {
    pub id: i64,
    pub name: String,
    pub token_prefix: String,
    pub permissions: Vec<String>,
    pub server_ids: Option<Vec<String>>,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub created_at: i64,
    pub created_by: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct AdminSessionsResponse {
    pub sessions: Vec<AdminSessionView>,
}

#[derive(Debug, Serialize)]
pub struct AdminSessionView {
    pub id: i64,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
    pub device_label: Option<String>,
    pub last_seen_at: i64,
    pub expires_at: i64,
    pub created_at: String,
    pub revoked_at: Option<i64>,
    pub auth_method: String,
}

#[derive(Debug, Serialize)]
pub struct AdminTwoFactorStatusResponse {
    pub enabled: bool,
    pub pending: bool,
    pub available: bool,
    pub login_challenge_available: bool,
    pub recovery_codes_remaining: i64,
}

#[derive(Debug, Deserialize)]
pub struct AdminTwoFactorConfirmRequest {
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub struct AdminTwoFactorCredentialRequest {
    pub password: String,
    #[serde(default)]
    pub totp_code: Option<String>,
    #[serde(default)]
    pub recovery_code: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AdminTwoFactorSetupResponse {
    pub pending: bool,
    pub secret: String,
    pub otpauth_uri: String,
}

#[derive(Debug, Serialize)]
pub struct AdminRecoveryCodesResponse {
    pub recovery_codes: Vec<String>,
}

fn default_server_scope() -> String {
    "full".to_string()
}

async fn require_security_context(
    state: &AppState,
    headers: &HeaderMap,
    permission: &str,
    session_only: bool,
) -> AppResult<auth::AuthContext> {
    let ctx = auth::require_auth_context(state, headers).await?;
    if session_only && ctx.kind != auth::AuthKind::Session {
        return Err(AppError::Forbidden(
            "administrator security changes require a session".to_string(),
        ));
    }
    auth::require_permission(&ctx, permission)?;
    Ok(ctx)
}

async fn require_super_security_session(
    state: &AppState,
    headers: &HeaderMap,
) -> AppResult<auth::AuthContext> {
    let ctx =
        require_security_context(state, headers, ADMIN_SECURITY_WRITE_PERMISSION, true).await?;
    if !ctx.role_keys.iter().any(|key| key == "super_admin") {
        return Err(AppError::Forbidden(
            "super administrator role required".to_string(),
        ));
    }
    Ok(ctx)
}

fn normalized_keys(values: &[String], field: &str) -> AppResult<Vec<String>> {
    let mut normalized = values
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    if normalized.is_empty() {
        return Err(AppError::Validation(format!("{field} must not be empty")));
    }
    if normalized.iter().any(|value| value.len() > 128) {
        return Err(AppError::Validation(format!(
            "{field} contains an oversized value"
        )));
    }
    Ok(normalized)
}

fn validate_role_keys(state: &AppState, role_keys: &[String]) -> AppResult<Vec<String>> {
    let role_keys = normalized_keys(role_keys, "role_keys")?;
    let available = state
        .settings_store
        .list_admin_roles()?
        .into_iter()
        .map(|role| role.key)
        .collect::<std::collections::HashSet<_>>();
    if let Some(missing) = role_keys.iter().find(|key| !available.contains(*key)) {
        return Err(AppError::Validation(format!(
            "admin role not found: {missing}"
        )));
    }
    Ok(role_keys)
}

async fn validate_server_grants(
    state: &AppState,
    ctx: &auth::AuthContext,
    payload: &[AdminServerGrantPayload],
) -> AppResult<Vec<crate::db::AdminServerGrantInput>> {
    let config = state.config.read().await;
    let known = config
        .servers
        .iter()
        .map(|server| server.id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let is_super = ctx.role_keys.iter().any(|key| key == "super_admin");
    let allowed = ctx
        .server_grants
        .iter()
        .map(|grant| grant.server_id.as_str())
        .collect::<std::collections::HashSet<_>>();
    let mut seen = std::collections::HashSet::new();
    let mut grants = Vec::with_capacity(payload.len());
    for grant in payload {
        let server_id = grant.server_id.trim();
        let scope = if grant.scope.trim().is_empty() {
            "full"
        } else {
            grant.scope.trim()
        };
        if server_id.is_empty() || !known.contains(server_id) {
            return Err(AppError::Validation("server_id does not exist".to_string()));
        }
        if !is_super && !allowed.contains(server_id) {
            return Err(AppError::Forbidden(
                "cannot grant an unauthorized server".to_string(),
            ));
        }
        if !seen.insert(server_id.to_string()) {
            return Err(AppError::Validation("duplicate server grant".to_string()));
        }
        if scope.len() > 64 || scope.chars().any(|ch| ch.is_control()) {
            return Err(AppError::Validation(
                "invalid server grant scope".to_string(),
            ));
        }
        grants.push(crate::db::AdminServerGrantInput {
            server_id: server_id.to_string(),
            scope: scope.to_string(),
        });
    }
    Ok(grants)
}

fn no_store_response<T: Serialize>(value: T) -> Response {
    (
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::PRAGMA, "no-cache"),
        ],
        Json(value),
    )
        .into_response()
}

pub async fn list_admins(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<AdminSecurityResponse>> {
    let ctx = require_security_context(&state, &headers, ADMIN_SECURITY_PERMISSION, false).await?;
    let mut users = state.settings_store.list_admin_users()?;
    if !auth::is_super_admin(&ctx) {
        users.retain(|user| user.id == ctx.user_id);
    }
    let config = state.config.read().await;
    let server_names = config
        .servers
        .iter()
        .map(|server| (server.id.clone(), server.name.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    drop(config);
    let mut admins = Vec::with_capacity(users.len());
    for user in users {
        let identity = state
            .settings_store
            .admin_security_identity(user.id)?
            .ok_or_else(|| AppError::Internal("admin identity not found".to_string()))?;
        admins.push(AdminView {
            id: user.id,
            username: user.username,
            display_name: user.display_name,
            enabled: user.enabled,
            disabled_at: user.disabled_at,
            totp_enabled: user.totp_enabled,
            totp_pending: user.totp_pending,
            created_at: user.created_at,
            updated_at: user.updated_at,
            role_keys: user.role_keys,
            permission_keys: identity.permission_keys,
            server_grants: identity
                .server_grants
                .into_iter()
                .filter(|grant| auth::has_server_access(&ctx, &grant.server_id))
                .map(|grant| AdminServerGrantView {
                    server_name: server_names.get(&grant.server_id).cloned(),
                    server_id: grant.server_id,
                    scope: grant.scope,
                    granted_at: grant.granted_at,
                })
                .collect(),
        });
    }
    let mut roles = Vec::new();
    for role in state.settings_store.list_admin_roles()? {
        roles.push(AdminRoleView {
            permissions: state.settings_store.list_role_permissions(role.id)?,
            id: role.id,
            key: role.key,
            name: role.name,
            description: role.description,
            is_system: role.is_system,
        });
    }
    Ok(Json(AdminSecurityResponse {
        current_user_id: ctx.user_id,
        admins,
        roles,
    }))
}

pub async fn create_admin(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<AdminView>> {
    let ctx = require_super_security_session(&state, &headers).await?;
    let payload: AdminCreateRequest = state.crypto_keys.decrypt_named(&request, "admin_create")?;
    auth::validate_credentials(&payload.username, &payload.password)?;
    let roles = validate_role_keys(&state, &payload.role_keys)?;
    let grants = validate_server_grants(&state, &ctx, &payload.server_grants).await?;
    let password_hash = auth::hash_password_async(&state, payload.password).await?;
    let role_refs = roles.iter().map(String::as_str).collect::<Vec<_>>();
    let admin_id = state
        .settings_store
        .create_admin_with_roles_and_server_grants(
            &payload.username,
            &password_hash,
            payload.display_name.as_deref(),
            &role_refs,
            &grants,
            Some(ctx.user_id),
        )?;
    state.settings_store.record_audit_best_effort(
        Some(ctx.user_id),
        "admin.create",
        &format!("创建管理员 {}", admin_id),
        "success",
    );
    let identity = state
        .settings_store
        .admin_security_identity(admin_id)?
        .ok_or_else(|| AppError::Internal("admin identity not found".to_string()))?;
    Ok(Json(admin_view_from_identity(identity)))
}

pub async fn update_admin_roles(
    State(state): State<AppState>,
    Path(admin_user_id): Path<i64>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<AdminMutationResponse>> {
    let ctx = require_super_security_session(&state, &headers).await?;
    let payload: AdminRolesRequest = state.crypto_keys.decrypt_named(&request, "admin_roles")?;
    let roles = validate_role_keys(&state, &payload.role_keys)?;
    let role_refs = roles.iter().map(String::as_str).collect::<Vec<_>>();
    state.settings_store.replace_admin_roles(
        admin_user_id,
        &role_refs,
        Some(ctx.user_id),
        auth::now_ts(),
    )?;
    state.settings_store.record_audit_best_effort(
        Some(ctx.user_id),
        "admin.update_roles",
        &format!("更新管理员 {} 的角色", admin_user_id),
        "success",
    );
    Ok(Json(AdminMutationResponse { success: true }))
}

pub async fn update_admin_server_grants(
    State(state): State<AppState>,
    Path(admin_user_id): Path<i64>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<AdminMutationResponse>> {
    let ctx = require_super_security_session(&state, &headers).await?;
    let payload: AdminServerGrantsRequest = state
        .crypto_keys
        .decrypt_named(&request, "admin_server_grants")?;
    let grants = validate_server_grants(&state, &ctx, &payload.server_grants).await?;
    state.settings_store.replace_admin_server_grants(
        admin_user_id,
        &grants,
        Some(ctx.user_id),
        auth::now_ts(),
    )?;
    state.settings_store.record_audit_best_effort(
        Some(ctx.user_id),
        "admin.update_server_grants",
        &format!("更新管理员 {} 的服务器授权", admin_user_id),
        "success",
    );
    Ok(Json(AdminMutationResponse { success: true }))
}

pub async fn set_admin_status(
    State(state): State<AppState>,
    Path(admin_user_id): Path<i64>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<AdminMutationResponse>> {
    let ctx = require_super_security_session(&state, &headers).await?;
    let payload: AdminStatusRequest = state.crypto_keys.decrypt_named(&request, "admin_status")?;
    if admin_user_id == ctx.user_id && !payload.enabled {
        return Err(AppError::Validation(
            "cannot disable the current administrator".to_string(),
        ));
    }
    if !state
        .settings_store
        .set_admin_disabled(admin_user_id, !payload.enabled, auth::now_ts())?
    {
        return Err(AppError::Validation("admin user not found".to_string()));
    }
    state.settings_store.record_audit_best_effort(
        Some(ctx.user_id),
        "admin.status",
        &format!(
            "{}管理员 {}",
            if payload.enabled { "启用" } else { "禁用" },
            admin_user_id
        ),
        "success",
    );
    Ok(Json(AdminMutationResponse { success: true }))
}

pub async fn reset_admin_password(
    State(state): State<AppState>,
    Path(admin_user_id): Path<i64>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<AdminMutationResponse>> {
    let ctx = require_super_security_session(&state, &headers).await?;
    let payload: AdminPasswordResetRequest = state
        .crypto_keys
        .decrypt_named(&request, "admin_password_reset")?;
    auth::validate_credentials("admin", &payload.new_password)?;
    let password_hash = auth::hash_password_async(&state, payload.new_password).await?;
    if !state
        .settings_store
        .reset_admin_password_and_revoke_credentials(
            admin_user_id,
            &password_hash,
            auth::now_ts(),
        )?
    {
        return Err(AppError::Validation(
            "enabled admin user not found".to_string(),
        ));
    }
    state.settings_store.record_audit_best_effort(
        Some(ctx.user_id),
        "admin.password_reset",
        &format!("重置管理员 {} 的密码并撤销凭据", admin_user_id),
        "success",
    );
    Ok(Json(AdminMutationResponse { success: true }))
}

pub async fn delete_admin(
    State(state): State<AppState>,
    Path(admin_user_id): Path<i64>,
    headers: HeaderMap,
) -> AppResult<Json<AdminMutationResponse>> {
    let ctx = require_super_security_session(&state, &headers).await?;
    if admin_user_id == ctx.user_id {
        return Err(AppError::Validation(
            "cannot delete the current administrator".to_string(),
        ));
    }
    if !state
        .settings_store
        .delete_admin_user(admin_user_id, auth::now_ts())?
    {
        return Err(AppError::Validation("admin user not found".to_string()));
    }
    state.settings_store.record_audit_best_effort(
        Some(ctx.user_id),
        "admin.delete",
        &format!("删除管理员 {}", admin_user_id),
        "success",
    );
    Ok(Json(AdminMutationResponse { success: true }))
}

pub async fn list_admin_tokens(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<AdminTokensResponse>> {
    let ctx = require_security_context(&state, &headers, ADMIN_SECURITY_PERMISSION, true).await?;
    let tokens = state
        .settings_store
        .list_admin_api_tokens(ctx.user_id)?
        .into_iter()
        .map(|token| AdminTokenView {
            id: token.id,
            name: token.name,
            token_prefix: token.token_prefix,
            permissions: token.scopes,
            server_ids: token.server_ids,
            expires_at: token.expires_at,
            last_used_at: token.last_used_at,
            revoked_at: token.revoked_at,
            created_at: token.created_at,
            created_by: token.created_by,
        })
        .collect();
    Ok(Json(AdminTokensResponse { tokens }))
}

pub async fn create_admin_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Response> {
    let ctx =
        require_security_context(&state, &headers, ADMIN_SECURITY_WRITE_PERMISSION, true).await?;
    let payload: AdminTokenCreateRequest = state
        .crypto_keys
        .decrypt_named(&request, "admin_token_create")?;
    let name = payload.name.trim();
    if name.is_empty() || name.len() > 128 {
        return Err(AppError::Validation("token name is invalid".to_string()));
    }
    let permissions = normalized_keys(&payload.permissions, "permissions")?;
    let is_super = ctx.role_keys.iter().any(|key| key == "super_admin");
    if !is_super
        && permissions.iter().any(|permission| {
            !ctx.permission_keys
                .iter()
                .any(|allowed| allowed == permission)
        })
    {
        return Err(AppError::Forbidden(
            "token permission exceeds administrator access".to_string(),
        ));
    }
    if payload
        .expires_at
        .is_some_and(|expires_at| expires_at <= auth::now_ts())
    {
        return Err(AppError::Validation(
            "token expiry must be in the future".to_string(),
        ));
    }
    let server_ids = match payload.server_ids {
        None => None,
        Some(ids) => {
            let ids = normalized_keys(&ids, "server_ids")?;
            let config = state.config.read().await;
            let known = config
                .servers
                .iter()
                .map(|server| server.id.as_str())
                .collect::<std::collections::HashSet<_>>();
            if ids
                .iter()
                .any(|server_id| !known.contains(server_id.as_str()))
            {
                return Err(AppError::Validation(
                    "token server_id does not exist".to_string(),
                ));
            }
            drop(config);
            if !is_super
                && ids.iter().any(|server_id| {
                    !ctx.server_grants
                        .iter()
                        .any(|grant| &grant.server_id == server_id)
                })
            {
                return Err(AppError::Forbidden(
                    "token server access exceeds administrator grants".to_string(),
                ));
            }
            Some(ids)
        }
    };
    let source_token_hash = auth::hash_token(auth::auth_token(&headers)?);
    for _ in 0..3 {
        let token = format!("embyp_{}", auth::random_token());
        let token_prefix = token.chars().take(16).collect::<String>();
        match state.settings_store.create_admin_api_token_from_session(
            &source_token_hash,
            ctx.user_id,
            name,
            &token_prefix,
            &auth::hash_token(&token),
            &permissions,
            server_ids.as_deref(),
            payload.expires_at,
            Some(ctx.user_id),
        ) {
            Ok(id) => {
                state.settings_store.record_audit_best_effort(
                    Some(ctx.user_id),
                    "admin.token_create",
                    &format!("创建管理员 API token {}", id),
                    "success",
                );
                return Ok(no_store_response(AdminTokenCreateResponse {
                    id,
                    token,
                    token_prefix,
                    name: name.to_string(),
                    permissions,
                    server_ids,
                    expires_at: payload.expires_at,
                }));
            }
            Err(AppError::Validation(message)) if message == "api token hash already exists" => {}
            Err(err) => return Err(err),
        }
    }
    Err(AppError::Internal(
        "failed to create unique API token".to_string(),
    ))
}

pub async fn revoke_admin_token(
    State(state): State<AppState>,
    Path(token_id): Path<i64>,
    headers: HeaderMap,
) -> AppResult<Json<AdminMutationResponse>> {
    let ctx =
        require_security_context(&state, &headers, ADMIN_SECURITY_WRITE_PERMISSION, true).await?;
    if !state.settings_store.revoke_admin_api_token_for_user(
        token_id,
        ctx.user_id,
        auth::now_ts(),
    )? {
        return Err(AppError::Validation("API token not found".to_string()));
    }
    state.settings_store.record_audit_best_effort(
        Some(ctx.user_id),
        "admin.token_revoke",
        &format!("撤销管理员 API token {}", token_id),
        "success",
    );
    Ok(Json(AdminMutationResponse { success: true }))
}

pub async fn list_admin_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<AdminSessionsResponse>> {
    let ctx = require_security_context(&state, &headers, ADMIN_SECURITY_PERMISSION, true).await?;
    let sessions = state
        .settings_store
        .list_admin_sessions(ctx.user_id)?
        .into_iter()
        .map(|session| AdminSessionView {
            id: session.id,
            ip: session.ip,
            user_agent: session.user_agent,
            device_label: session.device_label,
            last_seen_at: session.last_seen_at,
            expires_at: session.expires_at,
            created_at: session.created_at,
            revoked_at: session.revoked_at,
            auth_method: session.auth_method,
        })
        .collect();
    Ok(Json(AdminSessionsResponse { sessions }))
}

pub async fn revoke_admin_session(
    State(state): State<AppState>,
    Path(session_id): Path<i64>,
    headers: HeaderMap,
) -> AppResult<Json<AdminMutationResponse>> {
    let ctx =
        require_security_context(&state, &headers, ADMIN_SECURITY_WRITE_PERMISSION, true).await?;
    if !state.settings_store.revoke_admin_session_by_id_for_user(
        session_id,
        ctx.user_id,
        auth::now_ts(),
    )? {
        return Err(AppError::Validation("session not found".to_string()));
    }
    state.settings_store.record_audit_best_effort(
        Some(ctx.user_id),
        "admin.session_revoke",
        &format!("撤销管理员会话 {}", session_id),
        "success",
    );
    Ok(Json(AdminMutationResponse { success: true }))
}

pub async fn get_admin_2fa_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<AdminTwoFactorStatusResponse>> {
    let ctx = require_security_context(&state, &headers, ADMIN_SECURITY_PERMISSION, true).await?;
    let identity = state
        .settings_store
        .admin_security_identity(ctx.user_id)?
        .ok_or_else(|| AppError::Unauthorized("admin identity not found".to_string()))?;
    Ok(Json(AdminTwoFactorStatusResponse {
        enabled: identity.totp_enabled,
        pending: identity.totp_pending,
        available: auth::admin_security_key().is_ok(),
        login_challenge_available: ADMIN_TOTP_LOGIN_CHALLENGE_AVAILABLE,
        recovery_codes_remaining: state
            .settings_store
            .count_unused_admin_recovery_codes(ctx.user_id)?,
    }))
}

pub async fn setup_admin_2fa(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let ctx =
        require_security_context(&state, &headers, ADMIN_SECURITY_WRITE_PERMISSION, true).await?;
    if !ADMIN_TOTP_LOGIN_CHALLENGE_AVAILABLE {
        return Err(AppError::Validation(
            "2FA cannot be enabled until the login challenge is available".to_string(),
        ));
    }
    let identity = state
        .settings_store
        .admin_security_identity(ctx.user_id)?
        .ok_or_else(|| AppError::Unauthorized("admin identity not found".to_string()))?;
    if identity.totp_enabled {
        return Err(AppError::Validation("2FA is already enabled".to_string()));
    }
    let secret = auth::generate_totp_secret();
    let (ciphertext, nonce) = auth::encrypt_admin_totp_secret(secret.as_bytes())?;
    state.settings_store.upsert_admin_totp_secret(
        ctx.user_id,
        &ciphertext,
        &nonce,
        1,
        "SHA1",
        6,
        30,
        auth::now_ts(),
    )?;
    let issuer = urlencoding::encode("EmbyPanel");
    let label_value = format!("EmbyPanel:{}", identity.username);
    let label = urlencoding::encode(&label_value);
    let otpauth_uri = format!(
        "otpauth://totp/{label}?secret={secret}&issuer={issuer}&algorithm=SHA1&digits=6&period=30"
    );
    state.settings_store.record_audit_best_effort(
        Some(ctx.user_id),
        "admin.2fa_setup",
        &format!("为管理员 {} 创建 2FA 待确认配置", ctx.user_id),
        "success",
    );
    Ok(no_store_response(AdminTwoFactorSetupResponse {
        pending: true,
        secret,
        otpauth_uri,
    }))
}

pub async fn confirm_admin_2fa(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Response> {
    let ctx =
        require_security_context(&state, &headers, ADMIN_SECURITY_WRITE_PERMISSION, true).await?;
    if !ADMIN_TOTP_LOGIN_CHALLENGE_AVAILABLE {
        return Err(AppError::Validation(
            "2FA cannot be enabled until the login challenge is available".to_string(),
        ));
    }
    let payload: AdminTwoFactorConfirmRequest = state
        .crypto_keys
        .decrypt_named(&request, "admin_2fa_confirm")?;
    let row = state
        .settings_store
        .get_admin_totp_secret(ctx.user_id)?
        .ok_or_else(|| AppError::Validation("2FA setup is not pending".to_string()))?;
    if row.confirmed_at.is_some() {
        return Err(AppError::Validation("2FA is already enabled".to_string()));
    }
    let secret = auth::decrypt_admin_totp_secret(&row)?;
    let secret = std::str::from_utf8(&secret)
        .map_err(|_| AppError::Unauthorized("2FA secret is unavailable".to_string()))?;
    if !auth::verify_admin_totp_setup_code(&row, secret, &payload.code, auth::now_ts()) {
        return Err(AppError::Unauthorized("invalid 2FA code".to_string()));
    }
    let recovery_codes = (0..8).map(|_| auth::random_token()).collect::<Vec<_>>();
    let hashes = recovery_codes
        .iter()
        .map(|code| auth::hash_token(code))
        .collect::<Vec<_>>();
    if !state
        .settings_store
        .confirm_admin_totp_with_recovery_codes(
            ctx.user_id,
            &row.secret_ciphertext,
            &row.secret_nonce,
            &hashes,
            auth::now_ts(),
        )?
    {
        return Err(AppError::Validation(
            "2FA setup is no longer pending".to_string(),
        ));
    }
    state.settings_store.record_audit_best_effort(
        Some(ctx.user_id),
        "admin.2fa_confirm",
        &format!("确认管理员 {} 的 2FA", ctx.user_id),
        "success",
    );
    Ok(no_store_response(AdminRecoveryCodesResponse {
        recovery_codes,
    }))
}

async fn verify_admin_2fa_credentials(
    state: &AppState,
    ctx: &auth::AuthContext,
    payload: AdminTwoFactorCredentialRequest,
) -> AppResult<()> {
    let password_hash = state
        .settings_store
        .admin_password_hash_by_id(ctx.user_id)?
        .ok_or_else(|| AppError::Unauthorized("admin identity not found".to_string()))?;
    auth::verify_password_async(state, payload.password, password_hash.password_hash).await?;
    auth::verify_admin_second_factor(
        state,
        ctx.user_id,
        payload.totp_code.as_deref(),
        payload.recovery_code.as_deref(),
    )
}

pub async fn disable_admin_2fa(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<AdminMutationResponse>> {
    let ctx =
        require_security_context(&state, &headers, ADMIN_SECURITY_WRITE_PERMISSION, true).await?;
    let payload: AdminTwoFactorCredentialRequest = state
        .crypto_keys
        .decrypt_named(&request, "admin_2fa_disable")?;
    verify_admin_2fa_credentials(&state, &ctx, payload).await?;
    if !state
        .settings_store
        .disable_admin_totp_and_revoke_credentials(ctx.user_id, auth::now_ts())?
    {
        return Err(AppError::Validation("2FA is not enabled".to_string()));
    }
    state.settings_store.record_audit_best_effort(
        Some(ctx.user_id),
        "admin.2fa_disable",
        &format!("禁用管理员 {} 的 2FA 并撤销凭据", ctx.user_id),
        "success",
    );
    Ok(Json(AdminMutationResponse { success: true }))
}

pub async fn regenerate_admin_recovery_codes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Response> {
    let ctx =
        require_security_context(&state, &headers, ADMIN_SECURITY_WRITE_PERMISSION, true).await?;
    let payload: AdminTwoFactorCredentialRequest = state
        .crypto_keys
        .decrypt_named(&request, "admin_2fa_recovery")?;
    verify_admin_2fa_credentials(&state, &ctx, payload).await?;
    let recovery_codes = (0..8).map(|_| auth::random_token()).collect::<Vec<_>>();
    let hashes = recovery_codes
        .iter()
        .map(|code| auth::hash_token(code))
        .collect::<Vec<_>>();
    state
        .settings_store
        .replace_admin_recovery_codes(ctx.user_id, &hashes, auth::now_ts())?;
    state.settings_store.record_audit_best_effort(
        Some(ctx.user_id),
        "admin.2fa_recovery_regenerate",
        &format!("重新生成管理员 {} 的恢复码", ctx.user_id),
        "success",
    );
    Ok(no_store_response(AdminRecoveryCodesResponse {
        recovery_codes,
    }))
}

pub async fn rollback_admin_2fa_setup(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<AdminMutationResponse>> {
    let ctx =
        require_security_context(&state, &headers, ADMIN_SECURITY_WRITE_PERMISSION, true).await?;
    if !state
        .settings_store
        .rollback_admin_totp_setup(ctx.user_id)?
    {
        return Err(AppError::Validation("2FA setup is not pending".to_string()));
    }
    state.settings_store.record_audit_best_effort(
        Some(ctx.user_id),
        "admin.2fa_rollback",
        &format!("回滚管理员 {} 的 2FA 待确认配置", ctx.user_id),
        "success",
    );
    Ok(Json(AdminMutationResponse { success: true }))
}

fn admin_view_from_identity(identity: crate::db::AdminSecurityIdentity) -> AdminView {
    AdminView {
        id: identity.admin_user_id,
        username: identity.username,
        display_name: identity.display_name,
        enabled: identity.enabled,
        disabled_at: identity.disabled_at,
        totp_enabled: identity.totp_enabled,
        totp_pending: identity.totp_pending,
        created_at: String::new(),
        updated_at: None,
        role_keys: identity.role_keys,
        permission_keys: identity.permission_keys,
        server_grants: identity
            .server_grants
            .into_iter()
            .map(|grant| AdminServerGrantView {
                server_id: grant.server_id,
                scope: grant.scope,
                granted_at: grant.granted_at,
                server_name: None,
            })
            .collect(),
    }
}

pub async fn get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<Config>> {
    let ctx = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&ctx, "settings.read")?;
    Ok(Json(settings_for_context(
        state.config.read().await.clone(),
        &ctx,
    )))
}

pub async fn reveal_emby_api_key(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let ctx = auth::require_auth_context(&state, &headers).await?;
    auth::require_super_admin_session(&ctx)?;
    auth::require_server_access(&ctx, &server_id)?;
    let admin_user_id = ctx.user_id;
    let (server_name, api_key) = {
        let config = state.config.read().await;
        let server = config
            .servers
            .iter()
            .find(|server| server.id == server_id)
            .ok_or_else(|| AppError::Validation("server_id does not exist".to_string()))?;
        (server.name.clone(), server.emby_api_key.clone())
    };
    state.settings_store.record_audit(
        Some(admin_user_id),
        "settings.reveal_emby_api_key",
        &format!("查看服务器 {server_name} 的 Emby API Key"),
        "success",
    )?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(EmbyApiKeyResponse { api_key }),
    )
        .into_response())
}

pub async fn update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<Config>> {
    let ctx = auth::require_auth_context(&state, &headers).await?;
    auth::require_super_admin_session(&ctx)?;
    let admin_user_id = ctx.user_id;
    let (mut payload, raw_payload) = decrypt_settings_payload(&state, &request)?;
    let _config_update = state.config_updates.lock().await;
    let existing = state.config.read().await.clone();
    merge_server_openlist_token_overrides(&mut payload, &raw_payload, &existing);
    for server in &mut payload.servers {
        if server.emby_api_key.trim().is_empty()
            && let Some(existing_server) = existing
                .servers
                .iter()
                .find(|existing_server| existing_server.id == server.id)
        {
            server.emby_api_key = existing_server.emby_api_key.clone();
        }
    }
    if !payload.servers.is_empty() && payload.emby_api_key.trim().is_empty() {
        payload.emby_api_key = existing.emby_api_key.clone();
    } else if payload.servers.is_empty() {
        payload.emby_host.clear();
        payload.emby_api_key.clear();
    }
    if payload.openlist_addr.is_none() {
        payload.openlist_token = None;
    } else if payload
        .openlist_token
        .as_deref()
        .unwrap_or("")
        .trim()
        .is_empty()
    {
        payload.openlist_token = existing.openlist_token.clone();
    }
    payload
        .validate_for_storage()
        .map_err(|err| AppError::Validation(err.safe_log_message()))?;
    apply_runtime_config(&state, &existing, &payload).await?;
    state.settings_store.record_audit_best_effort(
        Some(admin_user_id),
        "settings.update",
        "保存服务器配置",
        "success",
    );
    Ok(Json(redact_config_secrets(payload)))
}

pub async fn restart_proxy_server(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<RestartProxyRequest>,
) -> AppResult<Json<Config>> {
    let ctx = auth::require_auth_context(&state, &headers).await?;
    auth::require_super_admin_session(&ctx)?;
    let admin_user_id = ctx.user_id;
    let server_id = payload.server_id.trim();
    if server_id.is_empty() {
        return Err(AppError::Validation("server_id is required".to_string()));
    }
    if let Some(proxy_manager) = state.proxy_manager.as_ref() {
        proxy_manager
            .restart_server(state.clone(), server_id)
            .await?;
    }
    state.settings_store.record_audit_best_effort(
        Some(admin_user_id),
        "settings.restart_proxy",
        &format!("重启反代服务器 {server_id}"),
        "success",
    );
    Ok(Json(redact_config_secrets(
        state.config.read().await.clone(),
    )))
}

pub async fn toggle_proxy_server(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ToggleProxyRequest>,
) -> AppResult<Json<Config>> {
    let ctx = auth::require_auth_context(&state, &headers).await?;
    auth::require_super_admin_session(&ctx)?;
    let admin_user_id = ctx.user_id;
    let server_id = payload.server_id.trim();
    if server_id.is_empty() {
        return Err(AppError::Validation("server_id is required".to_string()));
    }

    let _config_update = state.config_updates.lock().await;
    let existing = state.config.read().await.clone();
    let mut config = existing.clone();
    let server_name = {
        let server = config
            .servers
            .iter_mut()
            .find(|server| server.id == server_id)
            .ok_or_else(|| AppError::Validation("server_id does not exist".to_string()))?;
        server.enabled = payload.enabled;
        server.name.clone()
    };
    config
        .validate_for_storage()
        .map_err(|err| AppError::Config(err.to_string()))?;
    apply_runtime_config(&state, &existing, &config).await?;

    state.settings_store.record_audit_best_effort(
        Some(admin_user_id),
        "settings.toggle_proxy",
        &format!(
            "{}反代服务器 {}",
            if payload.enabled { "开启" } else { "关闭" },
            server_name
        ),
        "success",
    );
    Ok(Json(redact_config_secrets(config)))
}

pub async fn validate_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<ValidationResponse>> {
    let ctx = auth::require_auth_context(&state, &headers).await?;
    auth::require_super_admin_session(&ctx)?;
    let admin_user_id = ctx.user_id;
    let (mut payload, raw_payload) = decrypt_settings_payload(&state, &request)?;
    let existing = state.config.read().await.clone();
    merge_server_openlist_token_overrides(&mut payload, &raw_payload, &existing);
    for server in &mut payload.servers {
        if server.emby_api_key.trim().is_empty()
            && let Some(existing_server) = existing
                .servers
                .iter()
                .find(|existing_server| existing_server.id == server.id)
        {
            server.emby_api_key = existing_server.emby_api_key.clone();
        }
    }
    if !payload.servers.is_empty() && payload.emby_api_key.trim().is_empty() {
        payload.emby_api_key = existing.emby_api_key.clone();
    }
    if payload.openlist_addr.is_some()
        && payload
            .openlist_token
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
    {
        payload.openlist_token = existing.openlist_token.clone();
    }
    let mut results = Vec::new();
    match payload.validate_for_storage() {
        Ok(()) => results.push(ok_result("配置", "本地配置校验通过", "")),
        Err(err) => results.push(err_result("配置", "本地配置校验失败", &err.to_string())),
    }
    let current_statuses = if let Some(proxy_manager) = state.proxy_manager.as_ref() {
        proxy_manager.statuses(&existing).await
    } else {
        Vec::new()
    };
    let mut ports = std::collections::HashSet::new();
    for server in &payload.servers {
        if !ports.insert(server.port) {
            results.push(err_result(
                &server.name,
                "反代端口重复",
                &format!("端口 {}", server.port),
            ));
            continue;
        }
        let bind_addr = format!("0.0.0.0:{}", server.port);
        let current_proxy_owns_port = current_statuses.iter().any(|status| {
            status.server_id == server.id && status.port == server.port && status.listening
        });
        let bind_ok = current_proxy_owns_port || std::net::TcpListener::bind(&bind_addr).is_ok();
        results.push(if current_proxy_owns_port {
            ok_result(&server.name, "反代端口正在由当前服务监听", &bind_addr)
        } else if bind_ok {
            ok_result(&server.name, "反代端口可用", &bind_addr)
        } else {
            err_result(&server.name, "反代端口已被占用", &bind_addr)
        });
        match crate::emby::get_media_overview(
            &state.client,
            &payload.for_server_for_validation(server),
        )
        .await
        {
            Ok(_) => results.push(ok_result(
                &server.name,
                "Emby API Key 可用",
                &server.emby_host,
            )),
            Err(err) => results.push(err_result(
                &server.name,
                "Emby 连接失败",
                &safe_error_message(&err),
            )),
        }
    }

    if payload.servers.is_empty() {
        if payload.openlist_addr.is_some() {
            match crate::openlist::validate_connection(&state.client, &payload).await {
                Ok(()) => results.push(ok_result("OpenList", "OpenList 连接可用", "")),
                Err(err) => results.push(err_result(
                    "OpenList",
                    "OpenList 连接失败",
                    &safe_error_message(&err),
                )),
            }
        } else {
            results.push(ok_result("OpenList", "未配置，已跳过", ""));
        }
    } else {
        for server in &payload.servers {
            let effective_config = payload.for_server_for_validation(server);
            if effective_config.openlist_addr.is_some() {
                match crate::openlist::validate_connection(&state.client, &effective_config).await {
                    Ok(()) => results.push(ok_result(&server.name, "OpenList 连接可用", "")),
                    Err(err) => results.push(err_result(
                        &server.name,
                        "OpenList 连接失败",
                        &safe_error_message(&err),
                    )),
                }
            } else {
                results.push(ok_result(&server.name, "未配置，已跳过", ""));
            }
        }
    }

    let ok = results.iter().all(|result| result.ok);
    state.settings_store.record_audit(
        Some(admin_user_id),
        "settings.validate",
        "测试服务器配置",
        if ok { "success" } else { "warn" },
    )?;
    Ok(Json(ValidationResponse { ok, results }))
}

pub async fn get_log_config(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<SystemLogConfig>> {
    let ctx = auth::require_auth_context(&state, &headers).await?;
    auth::require_super_admin_session(&ctx)?;
    Ok(Json(state.file_log.config()))
}

pub async fn update_log_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<SystemLogConfig>> {
    let ctx = auth::require_auth_context(&state, &headers).await?;
    auth::require_super_admin_session(&ctx)?;
    let admin_user_id = ctx.user_id;
    let payload: SystemLogConfig = state.crypto_keys.decrypt_named(&request, "log_config")?;
    let _config_update = state.config_updates.lock().await;
    let config = state
        .file_log
        .update_config(&state.settings_store, payload)?;
    state.settings_store.record_audit_best_effort(
        Some(admin_user_id),
        "log_config.update",
        "保存日志配置",
        "success",
    );
    Ok(Json(config))
}

pub async fn export_backup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<BackupExportResponse>> {
    let ctx = auth::require_auth_context(&state, &headers).await?;
    auth::require_super_admin_session(&ctx)?;
    let admin_user_id = ctx.user_id;
    let payload: BackupExportRequest =
        state.crypto_keys.decrypt_named(&request, "backup_export")?;
    let password = payload.password.trim();
    validate_backup_password(password)?;
    let backup = {
        let _config_update = state.config_updates.lock().await;
        let _rate_limit_update = state.rate_limit_updates.lock().await;
        BackupPayload {
            schema_version: BACKUP_SCHEMA_VERSION,
            application_version: Some(crate::app_version()),
            runtime_config: state.config.read().await.clone(),
            client_control: Some(client_control::backup_config(&state)?),
            system_log_config: Some(state.file_log.config()),
            user_policies: Some(
                state
                    .settings_store
                    .load_setting_json(USER_POLICIES_SETTING_KEY)?
                    .unwrap_or_default(),
            ),
            user_templates: Some(
                state
                    .settings_store
                    .load_setting_json(USER_TEMPLATES_SETTING_KEY)?
                    .unwrap_or_default(),
            ),
        }
    };
    let backup = encrypt_backup(password, &backup)?;
    state.settings_store.record_audit_best_effort(
        Some(admin_user_id),
        "backup.export",
        "导出加密配置备份",
        "success",
    );
    Ok(Json(BackupExportResponse { backup }))
}

pub async fn import_backup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<Config>> {
    let ctx = auth::require_auth_context(&state, &headers).await?;
    auth::require_super_admin_session(&ctx)?;
    let admin_user_id = ctx.user_id;
    let payload: BackupImportRequest = state.crypto_keys.decrypt_named(&request, "backup")?;
    let mut backup = parse_backup_text(payload.password.as_deref(), &payload.backup)?;
    let restore_snapshot = match backup.schema_version {
        1 => false,
        BACKUP_SCHEMA_VERSION => true,
        version => {
            return Err(AppError::Validation(format!(
                "unsupported backup schema version {version}"
            )));
        }
    };
    backup
        .runtime_config
        .validate_for_storage()
        .map_err(|err| AppError::Validation(err.safe_log_message()))?;
    let restored_client_control = if restore_snapshot || backup.client_control.is_some() {
        Some(client_control::normalize_restored_config(
            backup.client_control.take(),
        ))
    } else {
        None
    };
    let restored_log_config = if restore_snapshot || backup.system_log_config.is_some() {
        Some(normalize_config(
            backup.system_log_config.take().unwrap_or_default(),
        ))
    } else {
        None
    };
    let restored_user_policies = if restore_snapshot || backup.user_policies.is_some() {
        let mut document = backup.user_policies.take().unwrap_or_default();
        normalize_user_policies(&mut document);
        Some(document)
    } else {
        None
    };
    let restored_user_templates = if restore_snapshot || backup.user_templates.is_some() {
        let mut document = backup.user_templates.take().unwrap_or_default();
        normalize_user_templates(&mut document);
        Some(document)
    } else {
        None
    };
    let client_control_changed = restored_client_control.is_some();
    let user_policies_changed = restored_user_policies.is_some();
    let _config_update = state.config_updates.lock().await;
    let _rate_limit_update = state.rate_limit_updates.lock().await;
    let existing = state.config.read().await.clone();
    let previous_client_control = state
        .settings_store
        .load_setting_json::<serde_json::Value>(CLIENT_CONTROL_SETTING_KEY)?;
    let previous_log_setting = state
        .settings_store
        .load_setting_json::<serde_json::Value>(SYSTEM_LOG_SETTING_KEY)?;
    let previous_user_policies = state
        .settings_store
        .load_setting_json::<serde_json::Value>(USER_POLICIES_SETTING_KEY)?;
    let previous_user_templates = state
        .settings_store
        .load_setting_json::<serde_json::Value>(USER_TEMPLATES_SETTING_KEY)?;
    let previous_log_config = state.file_log.config();
    let mut client_control_applied = false;
    let mut log_config_applied = false;
    let mut user_policies_applied = false;
    let mut user_templates_applied = false;

    if let Some(client_control) = restored_client_control.as_ref() {
        if let Err(err) = state
            .settings_store
            .save_setting_json(CLIENT_CONTROL_SETTING_KEY, client_control)
        {
            return Err(err);
        }
        client_control_applied = true;
    }
    if let Some(log_config) = restored_log_config.as_ref() {
        if let Err(err) = state
            .file_log
            .update_config(&state.settings_store, log_config.clone())
        {
            rollback_backup_auxiliary_settings(
                &state,
                previous_client_control.as_ref(),
                previous_log_setting.as_ref(),
                previous_user_policies.as_ref(),
                previous_user_templates.as_ref(),
                &previous_log_config,
                client_control_applied,
                false,
                false,
                false,
            )?;
            return Err(err);
        }
        log_config_applied = true;
    }
    if let Some(user_policies) = restored_user_policies.as_ref() {
        if let Err(err) = state
            .settings_store
            .save_setting_json(USER_POLICIES_SETTING_KEY, user_policies)
        {
            rollback_backup_auxiliary_settings(
                &state,
                previous_client_control.as_ref(),
                previous_log_setting.as_ref(),
                previous_user_policies.as_ref(),
                previous_user_templates.as_ref(),
                &previous_log_config,
                client_control_applied,
                log_config_applied,
                false,
                false,
            )?;
            return Err(err);
        }
        user_policies_applied = true;
    }
    if let Some(user_templates) = restored_user_templates.as_ref() {
        if let Err(err) = state
            .settings_store
            .save_setting_json(USER_TEMPLATES_SETTING_KEY, user_templates)
        {
            rollback_backup_auxiliary_settings(
                &state,
                previous_client_control.as_ref(),
                previous_log_setting.as_ref(),
                previous_user_policies.as_ref(),
                previous_user_templates.as_ref(),
                &previous_log_config,
                client_control_applied,
                log_config_applied,
                user_policies_applied,
                false,
            )?;
            return Err(err);
        }
        user_templates_applied = true;
    }
    if let Err(err) = apply_runtime_config(&state, &existing, &backup.runtime_config).await {
        if let Err(rollback_error) = rollback_backup_auxiliary_settings(
            &state,
            previous_client_control.as_ref(),
            previous_log_setting.as_ref(),
            previous_user_policies.as_ref(),
            previous_user_templates.as_ref(),
            &previous_log_config,
            client_control_applied,
            log_config_applied,
            user_policies_applied,
            user_templates_applied,
        ) {
            tracing::error!(
                import_error = %err.safe_log_message(),
                rollback_error = %rollback_error.safe_log_message(),
                "failed to rollback auxiliary backup settings"
            );
            return Err(AppError::Internal(
                "backup import failed and auxiliary settings could not be restored".to_string(),
            ));
        }
        return Err(err);
    }
    if client_control_changed || user_policies_changed {
        client_control::clear_restored_runtime_state(&state).await;
    }
    state.settings_store.record_audit_best_effort(
        Some(admin_user_id),
        "backup.import",
        "还原配置文件",
        "success",
    );
    Ok(Json(redact_config_secrets(backup.runtime_config)))
}

fn rollback_backup_auxiliary_settings(
    state: &AppState,
    previous_client_control: Option<&serde_json::Value>,
    previous_log_setting: Option<&serde_json::Value>,
    previous_user_policies: Option<&serde_json::Value>,
    previous_user_templates: Option<&serde_json::Value>,
    previous_log_config: &SystemLogConfig,
    restore_client_control: bool,
    restore_log_config: bool,
    restore_user_policies: bool,
    restore_user_templates: bool,
) -> AppResult<()> {
    let mut first_error = None;
    if restore_client_control
        && let Err(err) = restore_setting_snapshot(
            &state.settings_store,
            CLIENT_CONTROL_SETTING_KEY,
            previous_client_control,
        )
    {
        first_error = Some(err);
    }
    if restore_log_config {
        if let Err(err) = state
            .file_log
            .update_config(&state.settings_store, previous_log_config.clone())
            && first_error.is_none()
        {
            first_error = Some(err);
        }
        if let Err(err) = restore_setting_snapshot(
            &state.settings_store,
            SYSTEM_LOG_SETTING_KEY,
            previous_log_setting,
        ) && first_error.is_none()
        {
            first_error = Some(err);
        }
    }
    if restore_user_policies
        && let Err(err) = restore_setting_snapshot(
            &state.settings_store,
            USER_POLICIES_SETTING_KEY,
            previous_user_policies,
        )
        && first_error.is_none()
    {
        first_error = Some(err);
    }
    if restore_user_templates
        && let Err(err) = restore_setting_snapshot(
            &state.settings_store,
            USER_TEMPLATES_SETTING_KEY,
            previous_user_templates,
        )
        && first_error.is_none()
    {
        first_error = Some(err);
    }
    first_error.map_or(Ok(()), Err)
}

fn restore_setting_snapshot(
    settings_store: &crate::db::SettingsStore,
    key: &str,
    previous: Option<&serde_json::Value>,
) -> AppResult<()> {
    match previous {
        Some(value) => settings_store.save_setting_json(key, value),
        None => settings_store.delete_setting(key),
    }
}

async fn apply_runtime_config(state: &AppState, previous: &Config, next: &Config) -> AppResult<()> {
    state.settings_store.save_config(next)?;
    install_runtime_config(state, next).await;

    let Some(proxy_manager) = state.proxy_manager.as_ref() else {
        return Ok(());
    };
    let Err(reconcile_error) = proxy_manager.ensure_running(state.clone()).await else {
        return Ok(());
    };

    install_runtime_config(state, previous).await;
    if let Err(rollback_error) = state.settings_store.save_config(previous) {
        install_runtime_config(state, next).await;
        if let Err(recovery_error) = proxy_manager.ensure_running(state.clone()).await {
            tracing::error!(
                reconcile_error = %reconcile_error.safe_log_message(),
                recovery_error = %recovery_error.safe_log_message(),
                "failed to reconcile listeners with persisted proxy configuration"
            );
        }
        tracing::error!(
            reconcile_error = %reconcile_error.safe_log_message(),
            rollback_error = %rollback_error.safe_log_message(),
            "failed to rollback persisted proxy configuration"
        );
        return Err(AppError::Internal(
            "proxy configuration failed and persisted rollback also failed".to_string(),
        ));
    }
    if let Err(restore_error) = proxy_manager.ensure_running(state.clone()).await {
        tracing::error!(
            reconcile_error = %reconcile_error.safe_log_message(),
            restore_error = %restore_error.safe_log_message(),
            "failed to restore previous proxy listeners"
        );
        return Err(AppError::Internal(
            "proxy configuration failed and previous listeners could not be restored".to_string(),
        ));
    }
    Err(reconcile_error)
}

async fn install_runtime_config(state: &AppState, config: &Config) {
    *state.config.write().await = config.clone();
    *state.cache.write().await = direct_link_cache_from_config(config);
}

fn decrypt_settings_payload(
    state: &AppState,
    request: &EncryptedRequest,
) -> AppResult<(Config, Value)> {
    let mut fields = state.crypto_keys.decrypt_fields(request)?;
    let raw_payload = fields
        .remove("settings")
        .ok_or_else(|| AppError::Validation("missing encrypted field `settings`".to_string()))?;
    let payload = serde_json::from_value(raw_payload.clone())
        .map_err(|_| AppError::Validation("invalid encrypted payload".to_string()))?;
    Ok((payload, raw_payload))
}

fn merge_server_openlist_token_overrides(
    payload: &mut Config,
    raw_payload: &Value,
    existing: &Config,
) {
    let raw_servers = raw_payload.get("servers").and_then(Value::as_array);
    for server in &mut payload.servers {
        let existing_token = existing
            .servers
            .iter()
            .find(|existing_server| existing_server.id == server.id)
            .and_then(|existing_server| existing_server.openlist_token_override.clone());
        let token_field = raw_servers
            .and_then(|servers| {
                servers.iter().find(|raw_server| {
                    raw_server.get("id").and_then(Value::as_str) == Some(server.id.as_str())
                })
            })
            .and_then(|raw_server| raw_server.get("openlist_token_override"));

        match token_field {
            // Omitted means the redacted value was not echoed by the client; retain it.
            None => server.openlist_token_override = existing_token,
            // Null is the explicit clear operation emitted by the UI.
            Some(Value::Null) => server.openlist_token_override = None,
            // Empty values and our response mask are non-destructive placeholders.
            Some(Value::String(value))
                if value.trim().is_empty() || value.trim() == OPENLIST_TOKEN_OVERRIDE_MASK =>
            {
                server.openlist_token_override = existing_token;
            }
            Some(_) => {}
        }
    }
}

fn settings_for_context(mut config: Config, ctx: &auth::AuthContext) -> Config {
    if auth::allowed_server_ids(ctx).is_none() {
        return redact_config_secrets(config);
    }
    config
        .servers
        .retain(|server| auth::has_server_access(ctx, &server.id));
    // Legacy global host mirrors the first configured server and must not reveal
    // an out-of-scope server when only the server list has been filtered.
    if let Some(server) = config.servers.first() {
        config.emby_host = server.emby_host.clone();
    } else {
        config.emby_host.clear();
    }
    redact_config_secrets(config)
}

fn redact_config_secrets(mut config: Config) -> Config {
    config.emby_api_key.clear();
    for server in &mut config.servers {
        server.emby_api_key.clear();
        if server.openlist_token_override.is_some() {
            server.openlist_token_override = Some(OPENLIST_TOKEN_OVERRIDE_MASK.to_string());
        }
    }
    config.openlist_token = None;
    config
}

fn ok_result(scope: &str, message: &str, detail: &str) -> ValidationResult {
    ValidationResult {
        scope: scope.to_string(),
        ok: true,
        message: message.to_string(),
        detail: detail.to_string(),
    }
}

fn err_result(scope: &str, message: &str, detail: &str) -> ValidationResult {
    ValidationResult {
        scope: scope.to_string(),
        ok: false,
        message: message.to_string(),
        detail: detail.to_string(),
    }
}

fn parse_backup_text(password: Option<&str>, value: &str) -> AppResult<BackupPayload> {
    let value = value.trim();
    validate_backup_text_size(value)?;
    let root: serde_json::Value = serde_json::from_str(value)
        .map_err(|_| AppError::Validation("invalid backup config text".to_string()))?;
    if root.get("cipher").is_some() {
        let password = password
            .map(str::trim)
            .filter(|password| !password.is_empty())
            .ok_or_else(|| AppError::Validation("backup password is required".to_string()))?;
        decrypt_backup(password, value)
    } else {
        serde_json::from_value(root)
            .map_err(|_| AppError::Validation("invalid backup config".to_string()))
    }
}

fn encrypt_backup(password: &str, payload: &BackupPayload) -> AppResult<String> {
    validate_backup_password(password)?;
    let mut salt = [0_u8; BACKUP_SALT_BYTES];
    let mut nonce = [0_u8; BACKUP_NONCE_BYTES];
    rand::rng().fill_bytes(&mut salt);
    rand::rng().fill_bytes(&mut nonce);
    let key = backup_key(password, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|err| AppError::Internal(format!("backup cipher error: {err}")))?;
    let plaintext = serde_json::to_vec(payload)?;
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_ref())
        .map_err(|_| AppError::Internal("backup encryption failed".to_string()))?;
    let envelope = BackupEnvelope {
        version: BACKUP_ENVELOPE_VERSION,
        kdf: "argon2id".to_string(),
        cipher: "aes-256-gcm".to_string(),
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce),
        data: STANDARD.encode(ciphertext),
    };
    let backup = serde_json::to_string_pretty(&envelope)?;
    validate_backup_text_size(&backup)?;
    Ok(backup)
}

fn decrypt_backup(password: &str, value: &str) -> AppResult<BackupPayload> {
    validate_backup_password(password)?;
    let envelope: BackupEnvelope = serde_json::from_str(value)
        .map_err(|_| AppError::Validation("invalid encrypted backup envelope".to_string()))?;
    if envelope.version != BACKUP_ENVELOPE_VERSION
        || envelope.kdf != "argon2id"
        || envelope.cipher != "aes-256-gcm"
    {
        return Err(AppError::Validation(
            "unsupported backup format".to_string(),
        ));
    }
    let salt = STANDARD
        .decode(envelope.salt)
        .map_err(|err| AppError::Validation(format!("invalid backup salt: {err}")))?;
    let nonce = STANDARD
        .decode(envelope.nonce)
        .map_err(|err| AppError::Validation(format!("invalid backup nonce: {err}")))?;
    let data = STANDARD
        .decode(envelope.data)
        .map_err(|err| AppError::Validation(format!("invalid backup data: {err}")))?;
    if salt.len() != BACKUP_SALT_BYTES {
        return Err(AppError::Validation(
            "invalid backup salt length".to_string(),
        ));
    }
    if nonce.len() != BACKUP_NONCE_BYTES {
        return Err(AppError::Validation(
            "invalid backup nonce length".to_string(),
        ));
    }
    let key = backup_key(password, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|err| AppError::Internal(format!("backup cipher error: {err}")))?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce), data.as_ref())
        .map_err(|_| AppError::Validation("backup password is invalid".to_string()))?;
    serde_json::from_slice(&plaintext)
        .map_err(|_| AppError::Validation("invalid backup payload".to_string()))
}

fn validate_backup_password(password: &str) -> AppResult<()> {
    let length = password.chars().count();
    if !(4..=256).contains(&length) {
        return Err(AppError::Validation(
            "backup password must be between 4 and 256 characters".to_string(),
        ));
    }
    Ok(())
}

fn validate_backup_text_size(value: &str) -> AppResult<()> {
    if value.len() > MAX_BACKUP_TEXT_BYTES {
        return Err(AppError::PayloadTooLarge(
            "backup file is larger than 8 MiB".to_string(),
        ));
    }
    Ok(())
}

fn legacy_backup_schema_version() -> u32 {
    1
}

fn backup_key(password: &str, salt: &[u8]) -> AppResult<[u8; 32]> {
    let mut key = [0_u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), salt, &mut key)
        .map_err(|err| AppError::Internal(format!("backup key derivation failed: {err}")))?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        client_control::{
            ClientRuleRecord, ClientRuleSource, PlaybackRateBlockRecord, WebhookNotifyConfig,
        },
        users_api::{UserPolicyRecord, UserPolicyUpdate, UserTemplate},
    };

    #[test]
    fn malformed_backup_config_does_not_echo_secret_values() {
        let text =
            r#"{"runtime_config":{"emby_host":"http://emby","emby_api_key":982764321098765432}}"#;
        let error = parse_backup_text(None, text).err().unwrap();
        assert_eq!(error.safe_log_message(), "invalid backup config");
        assert!(!format!("{error:?}").contains("982764321098765432"));
    }

    fn test_client_control() -> ClientControlConfig {
        ClientControlConfig {
            enabled: true,
            notify_enabled: false,
            playback_rate_limit_enabled: true,
            playback_rate_limit_window_seconds: 60,
            playback_rate_limit_max_requests: 20,
            playback_rate_limit_block_seconds: 1800,
            playback_rate_limit_action: "block_ip".to_string(),
            concurrent_playback_limit_enabled: true,
            concurrent_playback_limit_max: 3,
            rate_limit_blocks: Vec::new(),
            webhook: WebhookNotifyConfig::default(),
            webhooks: Vec::new(),
            records: Vec::new(),
        }
    }

    fn test_payload() -> BackupPayload {
        BackupPayload {
            schema_version: BACKUP_SCHEMA_VERSION,
            application_version: Some("v-test".to_string()),
            runtime_config: Config::default_runtime(),
            client_control: Some(test_client_control()),
            system_log_config: Some(SystemLogConfig::default()),
            user_policies: Some(UserPoliciesDocument::default()),
            user_templates: Some(UserTemplatesDocument::default()),
        }
    }

    #[test]
    fn backup_round_trip_restores_payload() {
        let payload = test_payload();

        let backup = encrypt_backup("strong-password", &payload).unwrap();
        let restored = decrypt_backup("strong-password", &backup).unwrap();

        assert_eq!(
            restored.runtime_config.cache_ttl_seconds,
            payload.runtime_config.cache_ttl_seconds
        );
        assert_eq!(restored.schema_version, BACKUP_SCHEMA_VERSION);
        assert_eq!(restored.application_version.as_deref(), Some("v-test"));
        assert!(restored.client_control.as_ref().unwrap().enabled);
        assert!(restored.system_log_config.is_some());
    }

    #[test]
    fn cold_data_backup_restores_security_alerts_and_separate_keys() {
        use crate::{
            alerts::{AlertRule, Alerts},
            db::{AdminServerGrantInput, SettingsStore},
        };
        use std::{
            fs,
            time::{SystemTime, UNIX_EPOCH},
        };

        let root = std::env::temp_dir().join(format!(
            "embypanel-cold-restore-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let source = root.join("source");
        let backup = root.join("backup");
        let restored = root.join("restored");
        for dir in [&source, &backup, &restored] {
            fs::create_dir_all(dir).unwrap();
        }
        let source_db = source.join("embypanel.db");
        let store = SettingsStore::open_for_test(source_db.clone()).unwrap();
        let owner = store
            .create_initial_admin("backup-owner", "synthetic-password-hash")
            .unwrap()
            .unwrap();
        let viewer = store
            .create_admin_with_roles(
                "backup-viewer",
                "viewer-hash",
                None,
                &["viewer"],
                Some(owner),
            )
            .unwrap();
        store
            .replace_admin_server_grants(
                viewer,
                &[AdminServerGrantInput {
                    server_id: "srv-a".into(),
                    scope: "full".into(),
                }],
                Some(owner),
                auth::now_ts(),
            )
            .unwrap();
        store
            .create_session(
                owner,
                &auth::hash_token("revoked-backup-session"),
                auth::now_ts() + 3600,
            )
            .unwrap();
        store
            .revoke_session(&auth::hash_token("revoked-backup-session"), auth::now_ts())
            .unwrap();
        let mut config = Config::default_runtime();
        config.servers.push(
            serde_json::from_value(serde_json::json!({
                "id":"srv-a","name":"Server A","emby_host":"http://synthetic.invalid",
                "emby_api_key":"synthetic-api-key","port":18096
            }))
            .unwrap(),
        );
        config.validate_for_storage().unwrap();
        store.save_config(&config).unwrap();
        store
            .save_setting_json("backup-fixture", &serde_json::json!({"template":"fixture"}))
            .unwrap();

        // This test key stands in for the externally backed-up 2FA environment key;
        // it never reads or modifies the process environment or real credentials.
        let external_key = [23u8; 32];
        let nonce = [17u8; 12];
        let cipher = Aes256Gcm::new_from_slice(&external_key).unwrap();
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                b"SYNTHETIC_TOTP_SECRET".as_slice(),
            )
            .unwrap();
        store
            .upsert_admin_totp_secret(owner, &ciphertext, &nonce, 1, "SHA1", 6, 30, auth::now_ts())
            .unwrap();
        assert!(
            store
                .confirm_admin_totp_with_recovery_codes(
                    owner,
                    &ciphertext,
                    &nonce,
                    &[auth::hash_token("backup-recovery-code")],
                    auth::now_ts()
                )
                .unwrap()
        );
        let alerts = Alerts::new(&source_db).unwrap();
        alerts
            .create_alert_rule(AlertRule {
                id: "backup-rule".into(),
                name: "Backup rule".into(),
                enabled: true,
                rule_config: "https://synthetic.invalid/hook?token=backup-canary".into(),
                threshold: "".into(),
                silence_period: 0,
                created_at: crate::alerts::now_ms(),
                metric: "".into(),
                operator: "gte".into(),
                server_id: "srv-a".into(),
                evaluation_window_secs: 0,
            })
            .unwrap();
        alerts
            .record_test_delivery("backup-rule", "success", "backup fixture")
            .unwrap();
        // Complete cold backup: close every SQLite handle, then copy the whole directory.
        drop(alerts);
        drop(store);
        for entry in fs::read_dir(&source).unwrap() {
            let entry = entry.unwrap();
            fs::copy(entry.path(), backup.join(entry.file_name())).unwrap();
        }
        assert!(backup.join("embypanel.db.key").is_file());
        for entry in fs::read_dir(&backup).unwrap() {
            let entry = entry.unwrap();
            fs::copy(entry.path(), restored.join(entry.file_name())).unwrap();
        }
        let restored_db = restored.join("embypanel.db");
        let store = SettingsStore::open_for_test(restored_db.clone()).unwrap();
        let owner_identity = store.admin_security_identity(owner).unwrap().unwrap();
        assert!(
            owner_identity
                .role_keys
                .contains(&"super_admin".to_string())
        );
        assert!(owner_identity.totp_enabled);
        assert_eq!(
            store
                .admin_password_hash("backup-owner")
                .unwrap()
                .unwrap()
                .password_hash,
            "synthetic-password-hash"
        );
        let viewer_identity = store.admin_security_identity(viewer).unwrap().unwrap();
        assert_eq!(viewer_identity.role_keys, vec!["viewer".to_string()]);
        assert_eq!(viewer_identity.server_grants[0].server_id, "srv-a");
        assert!(
            store
                .admin_session_auth_by_hash(
                    &auth::hash_token("revoked-backup-session"),
                    auth::now_ts()
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.load_config().unwrap().unwrap().servers[0].emby_api_key,
            "synthetic-api-key"
        );
        assert_eq!(
            store
                .load_setting_json::<Value>("backup-fixture")
                .unwrap()
                .unwrap()["template"],
            "fixture"
        );
        let secret = store.get_admin_totp_secret(owner).unwrap().unwrap();
        assert_eq!(
            cipher
                .decrypt(
                    Nonce::from_slice(&secret.secret_nonce),
                    secret.secret_ciphertext.as_slice()
                )
                .unwrap(),
            b"SYNTHETIC_TOTP_SECRET"
        );
        assert_eq!(store.count_unused_admin_recovery_codes(owner).unwrap(), 1);
        let alerts = Alerts::new(&restored_db).unwrap();
        assert_eq!(
            alerts
                .get_rule_webhook_url("backup-rule")
                .unwrap()
                .as_deref(),
            Some("https://synthetic.invalid/hook?token=backup-canary")
        );
        assert_eq!(
            alerts
                .list_alert_deliveries(Some("backup-rule"), 10)
                .unwrap()
                .len(),
            1
        );
        let rendered =
            serde_json::to_string(&alerts.get_alert_rule("backup-rule").unwrap().unwrap()).unwrap();
        assert!(!rendered.contains("backup-canary"));
        drop(alerts);
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backup_round_trip_keeps_user_policies() {
        let mut payload = test_payload();
        payload.user_policies = Some(UserPoliciesDocument {
            policies: vec![UserPolicyRecord {
                server_id: "server-a".to_string(),
                user_id: "user-a".to_string(),
                rate_limit_enabled: true,
                ..UserPolicyRecord::default()
            }],
        });
        payload.user_templates = Some(UserTemplatesDocument {
            templates: vec![UserTemplate {
                id: "template-a".to_string(),
                server_id: "server-a".to_string(),
                name: "家庭用户".to_string(),
                policy: UserPolicyUpdate {
                    is_administrator: Some(false),
                    ..UserPolicyUpdate::default()
                },
            }],
        });

        let backup = encrypt_backup("strong-password", &payload).unwrap();
        let restored = decrypt_backup("strong-password", &backup).unwrap();

        let policy = &restored.user_policies.unwrap().policies[0];
        assert_eq!(policy.server_id, "server-a");
        assert_eq!(policy.user_id, "user-a");
        assert!(policy.rate_limit_enabled);
        let template = &restored.user_templates.unwrap().templates[0];
        assert_eq!(template.id, "template-a");
        assert_eq!(template.policy.is_administrator, Some(false));
    }

    #[test]
    fn backup_rejects_wrong_password() {
        let payload = test_payload();

        let backup = encrypt_backup("correct-password", &payload).unwrap();
        let err = decrypt_backup("wrong-password", &backup).unwrap_err();

        assert!(err.to_string().contains("backup password is invalid"));
    }

    #[test]
    fn legacy_plain_backup_defaults_to_schema_one() {
        let value = serde_json::json!({
            "runtime_config": Config::default_runtime()
        });

        let restored = parse_backup_text(None, &value.to_string()).unwrap();

        assert_eq!(restored.schema_version, 1);
        assert!(restored.client_control.is_none());
        assert!(restored.user_policies.is_none());
    }

    #[test]
    fn backup_rejects_short_password() {
        let err = encrypt_backup("abc", &test_payload()).unwrap_err();

        assert!(err.to_string().contains("between 4 and 256"));
    }

    #[test]
    fn backup_rejects_invalid_nonce_length() {
        let envelope = BackupEnvelope {
            version: BACKUP_ENVELOPE_VERSION,
            kdf: "argon2id".to_string(),
            cipher: "aes-256-gcm".to_string(),
            salt: STANDARD.encode([0_u8; BACKUP_SALT_BYTES]),
            nonce: STANDARD.encode([0_u8; 1]),
            data: STANDARD.encode([0_u8; 16]),
        };

        let err = decrypt_backup(
            "strong-password",
            &serde_json::to_string(&envelope).unwrap(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("invalid backup nonce length"));
    }

    #[test]
    fn backup_rejects_oversized_text() {
        let oversized = "x".repeat(MAX_BACKUP_TEXT_BYTES + 1);

        let err = parse_backup_text(None, &oversized).unwrap_err();

        assert!(matches!(err, AppError::PayloadTooLarge(_)));
    }

    #[test]
    fn server_openlist_token_override_preserves_omitted_empty_and_masked_values() {
        let existing = config_with_server_openlist_token(Some("persisted-server-token"));
        let raw_payloads = [
            serde_json::json!({"servers": [{"id": "server-a"}]}),
            serde_json::json!({
                "servers": [{"id": "server-a", "openlist_token_override": "  "}]
            }),
            serde_json::json!({
                "servers": [{
                    "id": "server-a",
                    "openlist_token_override": OPENLIST_TOKEN_OVERRIDE_MASK
                }]
            }),
        ];

        for raw_payload in raw_payloads {
            let mut payload = existing.clone();
            payload.servers[0].openlist_token_override = Some("client-placeholder".to_string());
            merge_server_openlist_token_overrides(&mut payload, &raw_payload, &existing);
            assert_eq!(
                payload.servers[0].openlist_token_override.as_deref(),
                Some("persisted-server-token")
            );
        }
    }

    #[test]
    fn server_openlist_token_override_null_explicitly_clears() {
        let existing = config_with_server_openlist_token(Some("persisted-server-token"));
        let raw_payload = serde_json::json!({
            "servers": [{"id": "server-a", "openlist_token_override": null}]
        });
        let mut payload = existing.clone();

        merge_server_openlist_token_overrides(&mut payload, &raw_payload, &existing);

        assert!(payload.servers[0].openlist_token_override.is_none());
    }

    fn config_with_server_openlist_token(token: Option<&str>) -> Config {
        let mut config = Config::default_runtime();
        config.servers.push(crate::config::EmbyServerConfig {
            id: "server-a".to_string(),
            name: "Server A".to_string(),
            openlist_addr_override: token.map(|_| "https://openlist.example.test".to_string()),
            openlist_token_override: token.map(str::to_string),
            ..crate::config::EmbyServerConfig::default()
        });
        config
    }

    #[test]
    fn settings_responses_redact_all_runtime_secrets() {
        let mut config = Config::default_runtime();
        config.emby_api_key = "top-level-key".to_string();
        config.openlist_addr = Some("https://openlist.example.test".to_string());
        config.openlist_token = Some("openlist-token".to_string());
        config.servers.push(crate::config::EmbyServerConfig {
            id: "server-a".to_string(),
            name: "Server A".to_string(),
            emby_host: "https://emby.example.test".to_string(),
            emby_api_key: "server-key".to_string(),
            openlist_addr_override: Some("https://openlist.example.test".to_string()),
            openlist_token_override: Some("server-openlist-token".to_string()),
            port: 18096,
            enabled: true,
            block_web_ui: false,
            block_web_ui_status_code: 403,
            block_web_ui_message: "Emby Web UI is disabled by EmbyPanel".to_string(),
            real_ip_mode: "auto".to_string(),
            real_ip_header: String::new(),
            trusted_proxy_cidrs: String::new(),
            trusted_proxy_networks: Vec::new(),
            ..crate::config::EmbyServerConfig::default()
        });

        let redacted = redact_config_secrets(config);

        assert!(redacted.emby_api_key.is_empty());
        assert!(
            redacted
                .servers
                .iter()
                .all(|server| server.emby_api_key.is_empty())
        );
        assert!(redacted.openlist_token.is_none());
        assert_eq!(
            redacted.servers[0].openlist_token_override.as_deref(),
            Some(OPENLIST_TOKEN_OVERRIDE_MASK)
        );
        let serialized = serde_json::to_string(&redacted).unwrap();
        assert!(!serialized.contains("server-openlist-token"));
    }

    #[test]
    fn backup_client_control_keeps_only_active_rules_and_blocks() {
        let now = 1_000;
        let active_until = now + 60;
        let mut config = test_client_control();
        config.records.push(ClientRuleRecord {
            id: "blocked".to_string(),
            client_name: "Infuse".to_string(),
            device_name: "--".to_string(),
            user_name: "--".to_string(),
            user_agent: "Infuse-Library".to_string(),
            source: ClientRuleSource::Manual,
            enabled: true,
            created_at: "1".to_string(),
            updated_at: "1".to_string(),
            note: "禁用".to_string(),
        });
        config.records.push(ClientRuleRecord {
            id: "allowed-auto".to_string(),
            client_name: "Auto".to_string(),
            device_name: "--".to_string(),
            user_name: "--".to_string(),
            user_agent: "Allowed-UA".to_string(),
            source: ClientRuleSource::Auto,
            enabled: false,
            created_at: "1".to_string(),
            updated_at: "1".to_string(),
            note: "自动记录播放设备".to_string(),
        });
        config.records.push(ClientRuleRecord {
            id: "allowed-manual".to_string(),
            client_name: "Manual".to_string(),
            device_name: "--".to_string(),
            user_name: "--".to_string(),
            user_agent: "Manual-UA".to_string(),
            source: ClientRuleSource::Manual,
            enabled: false,
            created_at: "1".to_string(),
            updated_at: "1".to_string(),
            note: "手动保留".to_string(),
        });
        config.rate_limit_blocks.push(PlaybackRateBlockRecord {
            id: "active-block".to_string(),
            server_id: "server-a".to_string(),
            server_name: "a".to_string(),
            action: "block_ip".to_string(),
            ip: "10.0.0.1".to_string(),
            user_name: "user-a".to_string(),
            blocked_until: active_until.to_string(),
            created_at: "1".to_string(),
            enabled: true,
            note: "active".to_string(),
            ip_location: None,
        });
        config.rate_limit_blocks.push(PlaybackRateBlockRecord {
            id: "expired-block".to_string(),
            server_id: "server-a".to_string(),
            server_name: "a".to_string(),
            action: "block_ip".to_string(),
            ip: "10.0.0.2".to_string(),
            user_name: "user-b".to_string(),
            blocked_until: "1".to_string(),
            created_at: "1".to_string(),
            enabled: true,
            note: "expired".to_string(),
            ip_location: None,
        });
        config.rate_limit_blocks.push(PlaybackRateBlockRecord {
            id: "disabled-block".to_string(),
            server_id: "server-a".to_string(),
            server_name: "a".to_string(),
            action: "disable_user".to_string(),
            ip: "10.0.0.3".to_string(),
            user_name: "user-c".to_string(),
            blocked_until: active_until.to_string(),
            created_at: "1".to_string(),
            enabled: false,
            note: "disabled".to_string(),
            ip_location: None,
        });

        let backup = client_control::prepare_backup_config(config, now);
        let record_ids = backup
            .records
            .iter()
            .map(|record| record.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(record_ids, vec!["blocked", "allowed-manual"]);
        assert_eq!(backup.rate_limit_blocks.len(), 1);
        assert_eq!(backup.rate_limit_blocks[0].id, "active-block");
    }
}
