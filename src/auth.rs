use std::{
    collections::{HashMap, HashSet, VecDeque},
    env,
    net::SocketAddr,
    time::{SystemTime, UNIX_EPOCH},
};

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    AppState,
    crypto_api::EncryptedRequest,
    db::{AdminServerGrant, AdminTotpSecretRow, SetupStatus},
    error::{AppError, AppResult},
};

const SESSION_TTL_SECONDS: i64 = 86400 * 30;
const LOGIN_FAILURE_WINDOW_SECONDS: i64 = 300;
const LOGIN_FAILURE_LIMIT: usize = 8;
const LOGIN_FAILURE_KEY_CAPACITY: usize = 2048;
pub const TOKEN_COOKIE_NAME: &str = "embypanel_token";
pub const ADMIN_SECOND_FACTOR_REQUIRED: &str = "second factor required";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthKind {
    Session,
    ApiToken,
}

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user_id: i64,
    pub kind: AuthKind,
    pub scopes: Option<Vec<String>>,
    pub server_ids: Option<Vec<String>>,
    pub role_keys: Vec<String>,
    pub permission_keys: Vec<String>,
    pub server_grants: Vec<AdminServerGrant>,
}

#[derive(Debug, Deserialize)]
pub struct SetupRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub totp_code: Option<String>,
    #[serde(default)]
    pub recovery_code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProfileRequest {
    pub username: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct PasswordChangedResponse {
    pub changed: bool,
    pub token: String,
}

#[derive(Debug, Serialize)]
pub struct LogoutResponse {
    pub logged_out: bool,
}

#[derive(Debug, Serialize)]
pub struct ProfileResponse {
    pub username: String,
}

pub async fn setup_status(State(state): State<AppState>) -> AppResult<Json<SetupStatus>> {
    Ok(Json(SetupStatus {
        initialized: state.settings_store.has_admin()?,
    }))
}

pub async fn setup(
    State(state): State<AppState>,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Response> {
    let payload: SetupRequest = state.crypto_keys.decrypt_named(&request, "credentials")?;
    if state.settings_store.has_admin()? {
        return Err(AppError::Unauthorized(
            "admin already initialized".to_string(),
        ));
    }
    validate_credentials(&payload.username, &payload.password)?;
    let password_hash = hash_password_async(&state, payload.password).await?;
    let token = random_token();
    let token_hash = hash_token(&token);
    let Some(admin_user_id) = state.settings_store.create_initial_admin_with_session(
        payload.username.trim(),
        &password_hash,
        &token_hash,
        now_ts() + SESSION_TTL_SECONDS,
    )?
    else {
        return Err(AppError::Unauthorized(
            "admin already initialized".to_string(),
        ));
    };
    state.settings_store.record_audit_best_effort(
        Some(admin_user_id),
        "account.setup",
        "初始化管理员账户",
        "success",
    );
    Ok(Json(AuthResponse { token }).into_response())
}

pub async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Response> {
    let login_key = peer_addr.ip().to_string();
    enforce_login_attempt_limit(&state, &login_key).await?;
    let payload: LoginRequest = state.crypto_keys.decrypt_named(&request, "credentials")?;
    let Some(admin) = state
        .settings_store
        .admin_password_hash(payload.username.trim())?
    else {
        record_login_failure(&state, &login_key).await;
        return Err(AppError::Unauthorized(
            "invalid username or password".to_string(),
        ));
    };
    let current_password_hash = admin.password_hash;
    if let Err(err) = verify_login_password_async(
        &state,
        &login_key,
        payload.password,
        current_password_hash.clone(),
    )
    .await
    {
        if matches!(err, AppError::Unauthorized(_)) {
            record_login_failure(&state, &login_key).await;
        }
        return Err(err);
    }
    if let Some(identity) = state
        .settings_store
        .admin_security_identity(admin.admin_user_id)?
        && identity.totp_enabled
    {
        if !admin_second_factor_code_present(
            payload.totp_code.as_deref(),
            payload.recovery_code.as_deref(),
        ) {
            return Err(AppError::Validation(
                ADMIN_SECOND_FACTOR_REQUIRED.to_string(),
            ));
        }
        if let Err(err) = verify_admin_second_factor(
            &state,
            admin.admin_user_id,
            payload.totp_code.as_deref(),
            payload.recovery_code.as_deref(),
        ) {
            if matches!(err, AppError::Unauthorized(_)) {
                record_login_failure(&state, &login_key).await;
            }
            return Err(err);
        }
    }
    let token = random_token();
    let token_hash = hash_token(&token);
    if !state.settings_store.create_session_if_password_matches(
        admin.admin_user_id,
        &current_password_hash,
        &token_hash,
        now_ts() + SESSION_TTL_SECONDS,
    )? {
        record_login_failure(&state, &login_key).await;
        return Err(AppError::Unauthorized(
            "invalid username or password".to_string(),
        ));
    }
    clear_login_failures(&state, &login_key).await;
    Ok(Json(AuthResponse { token }).into_response())
}

pub async fn change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<PasswordChangedResponse>> {
    let ctx = require_auth_context(&state, &headers).await?;
    if ctx.kind != AuthKind::Session {
        return Err(AppError::Forbidden(
            "password changes require a session".to_string(),
        ));
    }
    let admin_user_id = ctx.user_id;
    let payload: ChangePasswordRequest = state.crypto_keys.decrypt_named(&request, "password")?;
    if payload.new_password.is_empty() {
        return Err(AppError::Validation(
            "new password cannot be empty".to_string(),
        ));
    }
    let Some(admin) = state
        .settings_store
        .admin_password_hash_by_id(admin_user_id)?
    else {
        return Err(AppError::Unauthorized(
            "invalid or expired session".to_string(),
        ));
    };
    let current_password_hash = admin.password_hash;
    match verify_password_async(
        &state,
        payload.current_password,
        current_password_hash.clone(),
    )
    .await
    {
        Ok(()) => {}
        Err(AppError::Unauthorized(_)) => {
            return Err(AppError::Validation(
                "current password is incorrect".to_string(),
            ));
        }
        Err(err) => return Err(err),
    }
    let password_hash = hash_password_async(&state, payload.new_password).await?;
    let current_token_hash = hash_token(auth_token(&headers)?);
    let token = random_token();
    let token_hash = hash_token(&token);
    let updated = state
        .settings_store
        .update_admin_password_and_replace_sessions(
            admin_user_id,
            &current_password_hash,
            &password_hash,
            &current_token_hash,
            &token_hash,
            now_ts(),
            now_ts() + SESSION_TTL_SECONDS,
        )?;
    if !updated {
        return Err(AppError::Unauthorized(
            "invalid or expired session".to_string(),
        ));
    }
    state.settings_store.record_audit_best_effort(
        Some(admin_user_id),
        "account.password",
        "修改管理员密码",
        "success",
    );
    Ok(Json(PasswordChangedResponse {
        changed: true,
        token,
    }))
}

pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<LogoutResponse>> {
    let token_hash = hash_token(auth_token(&headers)?);
    let admin_user_id = state
        .settings_store
        .revoke_session(&token_hash, now_ts())?
        .ok_or_else(|| AppError::Unauthorized("invalid or expired session".to_string()))?;
    state.settings_store.record_audit_best_effort(
        Some(admin_user_id),
        "account.logout",
        "管理员退出登录",
        "success",
    );
    Ok(Json(LogoutResponse { logged_out: true }))
}

pub async fn get_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<ProfileResponse>> {
    let admin_user_id = require_auth_user_id(&state, &headers).await?;
    let username = state
        .settings_store
        .admin_username_by_id(admin_user_id)?
        .ok_or_else(|| AppError::Unauthorized("invalid or expired session".to_string()))?;
    Ok(Json(ProfileResponse { username }))
}

pub async fn update_profile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<ProfileResponse>> {
    let ctx = require_auth_context(&state, &headers).await?;
    if ctx.kind != AuthKind::Session {
        return Err(AppError::Forbidden(
            "profile changes require a session".to_string(),
        ));
    }
    let admin_user_id = ctx.user_id;
    let payload: UpdateProfileRequest = state.crypto_keys.decrypt_named(&request, "profile")?;
    let username = payload.username.trim();
    if username.is_empty() {
        let username = state
            .settings_store
            .admin_username_by_id(admin_user_id)?
            .ok_or_else(|| AppError::Unauthorized("invalid or expired session".to_string()))?;
        return Ok(Json(ProfileResponse { username }));
    }
    state
        .settings_store
        .update_admin_username(admin_user_id, username)?;
    state.settings_store.record_audit(
        Some(admin_user_id),
        "account.profile",
        "修改账户资料",
        "success",
    )?;
    Ok(Json(ProfileResponse {
        username: username.to_string(),
    }))
}

pub async fn require_auth(state: &AppState, headers: &HeaderMap) -> AppResult<()> {
    require_auth_user_id(state, headers).await.map(|_| ())
}

pub async fn require_auth_user_id(state: &AppState, headers: &HeaderMap) -> AppResult<i64> {
    let ctx = require_auth_context(state, headers).await?;
    Ok(ctx.user_id)
}

/// Resolve full AuthContext for session or admin API token (P2 least-privilege).
/// Fail-closed on collision, disabled, expired, revoked, or unknown credentials.
pub async fn require_auth_context(state: &AppState, headers: &HeaderMap) -> AppResult<AuthContext> {
    if !state.settings_store.has_admin()? {
        return Err(AppError::Unauthorized(
            "admin is not initialized".to_string(),
        ));
    }
    let token = auth_token(headers)?;
    let token_hash = hash_token(token);
    let now = now_ts();

    let session_meta = state
        .settings_store
        .admin_session_auth_by_hash(&token_hash, now)?;
    let token_meta = state
        .settings_store
        .admin_api_token_auth_by_hash(&token_hash, now)?;

    if session_meta.is_some() && token_meta.is_some() {
        return Err(AppError::Unauthorized(
            "authentication collision".to_string(),
        ));
    }

    if let Some(meta) = session_meta {
        let identity = state
            .settings_store
            .admin_security_identity(meta.admin_user_id)?
            .ok_or_else(|| AppError::Unauthorized("admin identity not found".to_string()))?;
        if !identity.enabled {
            return Err(AppError::Unauthorized("admin is disabled".to_string()));
        }
        let _ = state
            .settings_store
            .touch_admin_session_last_seen(meta.session_id, now, 60)?;
        return Ok(AuthContext {
            user_id: meta.admin_user_id,
            kind: AuthKind::Session,
            scopes: None,
            server_ids: None,
            role_keys: identity.role_keys,
            permission_keys: identity.permission_keys,
            server_grants: identity.server_grants,
        });
    }

    if let Some(meta) = token_meta {
        let identity = state
            .settings_store
            .admin_security_identity(meta.admin_user_id)?
            .ok_or_else(|| AppError::Unauthorized("admin identity not found".to_string()))?;
        if !identity.enabled {
            return Err(AppError::Unauthorized("admin is disabled".to_string()));
        }
        if let Some(expires_at) = meta.expires_at {
            if expires_at <= now {
                return Err(AppError::Unauthorized("token expired".to_string()));
            }
        }
        let _ = state
            .settings_store
            .touch_admin_api_token_last_used(meta.id, now)?;
        return Ok(AuthContext {
            user_id: meta.admin_user_id,
            kind: AuthKind::ApiToken,
            scopes: Some(meta.scopes),
            server_ids: meta.server_ids,
            role_keys: identity.role_keys,
            permission_keys: identity.permission_keys,
            server_grants: identity.server_grants,
        });
    }

    Err(AppError::Unauthorized(
        "invalid or expired session".to_string(),
    ))
}

/// Check whether the context grants the named permission (fail-closed).
pub fn has_permission(ctx: &AuthContext, permission: &str) -> bool {
    let is_super = ctx.role_keys.iter().any(|r| r == "super_admin");
    match ctx.kind {
        AuthKind::Session => {
            if is_super {
                true
            } else {
                ctx.permission_keys.iter().any(|p| p == permission)
            }
        }
        AuthKind::ApiToken => {
            let scope_ok = match &ctx.scopes {
                Some(scopes) if !scopes.is_empty() => scopes.iter().any(|s| s == permission),
                _ => false,
            };
            if !scope_ok {
                return false;
            }
            if is_super {
                true
            } else {
                ctx.permission_keys.iter().any(|p| p == permission)
            }
        }
    }
}

/// Require the named permission or return Forbidden.
pub fn require_permission(ctx: &AuthContext, permission: &str) -> AppResult<()> {
    if has_permission(ctx, permission) {
        Ok(())
    } else {
        Err(AppError::Forbidden(format!(
            "missing permission: {permission}"
        )))
    }
}

/// Check whether the context belongs to a super administrator.
pub fn is_super_admin(ctx: &AuthContext) -> bool {
    ctx.role_keys.iter().any(|role| role == "super_admin")
}

/// Global settings and credentials are never available to API tokens.
pub fn require_super_admin_session(ctx: &AuthContext) -> AppResult<()> {
    if ctx.kind == AuthKind::Session && is_super_admin(ctx) {
        Ok(())
    } else {
        Err(AppError::Forbidden(
            "super administrator session required".to_string(),
        ))
    }
}

/// Return the finite server allow-list for restricted identities.
/// `None` means the context is a super administrator and may access all servers.
pub fn allowed_server_ids(ctx: &AuthContext) -> Option<HashSet<String>> {
    let granted = ctx
        .server_grants
        .iter()
        .map(|grant| grant.server_id.clone())
        .collect::<HashSet<_>>();
    match (&ctx.kind, &ctx.server_ids, is_super_admin(ctx)) {
        // Session super-admins have no finite server allow-list.
        (AuthKind::Session, _, true) => None,
        // API tokens remain bounded by their explicit server_ids even when the
        // underlying identity has the super_admin role.
        (AuthKind::ApiToken, Some(token_server_ids), true) => {
            Some(token_server_ids.iter().cloned().collect())
        }
        (AuthKind::ApiToken, None, true) => None,
        (AuthKind::ApiToken, Some(token_server_ids), false) => {
            let token_server_ids = token_server_ids.iter().collect::<HashSet<_>>();
            Some(
                granted
                    .into_iter()
                    .filter(|server_id| token_server_ids.contains(server_id))
                    .collect(),
            )
        }
        _ => Some(granted),
    }
}

/// Normalize an optional server filter and enforce access when it names one server.
/// Empty values and the legacy `all` selector mean no explicit server filter.
pub fn require_server_filter<'a>(
    ctx: &AuthContext,
    server_id: Option<&'a str>,
) -> AppResult<Option<&'a str>> {
    let server_id = server_id
        .map(str::trim)
        .filter(|server_id| !server_id.is_empty() && !server_id.eq_ignore_ascii_case("all"));
    if let Some(server_id) = server_id {
        require_server_access(ctx, server_id)?;
    }
    Ok(server_id)
}

/// Check whether the context can access the given server_id (fail-closed).
pub fn has_server_access(ctx: &AuthContext, server_id: &str) -> bool {
    let is_super = is_super_admin(ctx);
    match ctx.kind {
        AuthKind::Session => {
            if is_super {
                true
            } else {
                ctx.server_grants.iter().any(|g| g.server_id == server_id)
            }
        }
        AuthKind::ApiToken => {
            let token_ok = match &ctx.server_ids {
                Some(ids) if !ids.is_empty() => ids.iter().any(|id| id == server_id),
                Some(_) => false,
                None => true, // inherit identity grants
            };
            if !token_ok {
                return false;
            }
            if is_super {
                true
            } else {
                ctx.server_grants.iter().any(|g| g.server_id == server_id)
            }
        }
    }
}

/// Require server access or return Forbidden.
pub fn require_server_access(ctx: &AuthContext, server_id: &str) -> AppResult<()> {
    if has_server_access(ctx, server_id) {
        Ok(())
    } else {
        Err(AppError::Forbidden(format!(
            "missing server access: {server_id}"
        )))
    }
}

pub(crate) fn validate_credentials(username: &str, password: &str) -> AppResult<()> {
    if username.trim().is_empty() {
        return Err(AppError::Validation("username cannot be empty".to_string()));
    }
    if password.is_empty() {
        return Err(AppError::Validation("password cannot be empty".to_string()));
    }
    Ok(())
}

async fn enforce_login_attempt_limit(state: &AppState, key: &str) -> AppResult<()> {
    let now = now_ts();
    let mut attempts = state.login_failures.lock().await;
    prune_login_failure_key(&mut attempts, key, now);
    if !attempts.contains_key(key) && attempts.len() >= LOGIN_FAILURE_KEY_CAPACITY {
        prune_login_failures(&mut attempts, now);
        if attempts.len() >= LOGIN_FAILURE_KEY_CAPACITY {
            return Err(AppError::RateLimited(
                "too many login attempts, please try again later".to_string(),
            ));
        }
    }
    if attempts
        .get(key)
        .is_some_and(|failures| failures.len() >= LOGIN_FAILURE_LIMIT)
    {
        return Err(AppError::RateLimited(
            "too many login attempts, please try again later".to_string(),
        ));
    }
    Ok(())
}

async fn record_login_failure(state: &AppState, key: &str) {
    let now = now_ts();
    let mut attempts = state.login_failures.lock().await;
    prune_login_failure_key(&mut attempts, key, now);
    if !attempts.contains_key(key) && attempts.len() >= LOGIN_FAILURE_KEY_CAPACITY {
        prune_login_failures(&mut attempts, now);
        if attempts.len() >= LOGIN_FAILURE_KEY_CAPACITY {
            return;
        }
    }
    attempts.entry(key.to_string()).or_default().push_back(now);
}

async fn clear_login_failures(state: &AppState, key: &str) {
    state.login_failures.lock().await.remove(key);
}

fn prune_login_failures(attempts: &mut HashMap<String, VecDeque<i64>>, now: i64) {
    attempts.retain(|_, failures| {
        while failures
            .front()
            .is_some_and(|timestamp| now.saturating_sub(*timestamp) > LOGIN_FAILURE_WINDOW_SECONDS)
        {
            failures.pop_front();
        }
        !failures.is_empty()
    });
}

fn prune_login_failure_key(attempts: &mut HashMap<String, VecDeque<i64>>, key: &str, now: i64) {
    let remove = attempts.get_mut(key).is_some_and(|failures| {
        while failures
            .front()
            .is_some_and(|timestamp| now.saturating_sub(*timestamp) > LOGIN_FAILURE_WINDOW_SECONDS)
        {
            failures.pop_front();
        }
        failures.is_empty()
    });
    if remove {
        attempts.remove(key);
    }
}

fn hash_password(password: &str) -> AppResult<String> {
    let mut salt_bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|err| AppError::Internal(format!("failed to encode password salt: {err}")))?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|err| AppError::Internal(format!("failed to hash password: {err}")))
}

pub(crate) async fn hash_password_async(state: &AppState, password: String) -> AppResult<String> {
    let permit = state
        .password_tasks
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| AppError::Internal("password task limiter closed".to_string()))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        hash_password(&password)
    })
    .await
    .map_err(|err| AppError::Internal(format!("password hashing task failed: {err}")))?
}

fn verify_password(password: &str, password_hash: &str) -> AppResult<()> {
    let parsed = PasswordHash::new(password_hash)
        .map_err(|err| AppError::Internal(format!("invalid password hash: {err}")))?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| AppError::Unauthorized("invalid username or password".to_string()))
}

pub(crate) async fn verify_password_async(
    state: &AppState,
    password: String,
    password_hash: String,
) -> AppResult<()> {
    let permit = state
        .password_tasks
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| AppError::Internal("password task limiter closed".to_string()))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        verify_password(&password, &password_hash)
    })
    .await
    .map_err(|err| AppError::Internal(format!("password verification task failed: {err}")))?
}

async fn verify_login_password_async(
    state: &AppState,
    login_key: &str,
    password: String,
    password_hash: String,
) -> AppResult<()> {
    let permit = state
        .password_tasks
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::RateLimited("authentication is busy; try again".to_string()))?;
    enforce_login_attempt_limit(state, login_key).await?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        verify_password(&password, &password_hash)
    })
    .await
    .map_err(|err| AppError::Internal(format!("password verification task failed: {err}")))?
}

pub(crate) fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(crate) fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

pub(crate) fn auth_token(headers: &HeaderMap) -> AppResult<&str> {
    bearer_token(headers)
        .ok_or_else(|| AppError::Unauthorized("missing authorization token".to_string()))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;
    value.strip_prefix("Bearer ")
}

pub(crate) fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

const ADMIN_SECURITY_KEY_ENV: &str = "EMBYPANEL_ADMIN_SECURITY_KEY";
const TOTP_PERIOD_SECONDS: i64 = 30;
const TOTP_DIGITS: u32 = 6;

/// Load the stable application key used to encrypt TOTP secrets at rest.
/// It is intentionally external to SQLite so a database dump alone cannot decrypt secrets.
pub(crate) fn admin_security_key() -> AppResult<[u8; 32]> {
    let raw = env::var(ADMIN_SECURITY_KEY_ENV).map_err(|_| {
        AppError::Validation(format!(
            "{ADMIN_SECURITY_KEY_ENV} must be configured for 2FA"
        ))
    })?;
    let decoded = URL_SAFE_NO_PAD
        .decode(raw.trim())
        .or_else(|_| STANDARD.decode(raw.trim()))
        .map_err(|_| AppError::Validation("invalid admin security key encoding".to_string()))?;
    if decoded.len() != 32 {
        return Err(AppError::Validation(
            "admin security key must decode to 32 bytes".to_string(),
        ));
    }
    let mut key = [0_u8; 32];
    key.copy_from_slice(&decoded);
    Ok(key)
}

pub(crate) fn encrypt_admin_totp_secret(secret: &[u8]) -> AppResult<(Vec<u8>, Vec<u8>)> {
    let key = admin_security_key()?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| AppError::Internal("invalid admin security key".to_string()))?;
    let mut nonce = [0_u8; 12];
    rand::rng().fill_bytes(&mut nonce);
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), secret)
        .map_err(|_| AppError::Internal("failed to encrypt 2FA secret".to_string()))?;
    Ok((ciphertext, nonce.to_vec()))
}

pub(crate) fn decrypt_admin_totp_secret(row: &AdminTotpSecretRow) -> AppResult<Vec<u8>> {
    if row.secret_nonce.len() != 12 {
        return Err(AppError::Unauthorized(
            "2FA secret is unavailable".to_string(),
        ));
    }
    let key = admin_security_key()?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|_| AppError::Internal("invalid admin security key".to_string()))?;
    cipher
        .decrypt(
            Nonce::from_slice(&row.secret_nonce),
            row.secret_ciphertext.as_ref(),
        )
        .map_err(|_| AppError::Unauthorized("2FA secret is unavailable".to_string()))
}

pub(crate) fn generate_totp_secret() -> String {
    let mut bytes = [0_u8; 20];
    rand::rng().fill_bytes(&mut bytes);
    base32_encode(&bytes)
}

pub(crate) fn admin_second_factor_code_present(
    totp_code: Option<&str>,
    recovery_code: Option<&str>,
) -> bool {
    [totp_code, recovery_code]
        .into_iter()
        .flatten()
        .any(|code| !code.trim().is_empty())
}

pub(crate) fn verify_admin_second_factor(
    state: &AppState,
    admin_user_id: i64,
    totp_code: Option<&str>,
    recovery_code: Option<&str>,
) -> AppResult<()> {
    let Some(row) = state.settings_store.get_admin_totp_secret(admin_user_id)? else {
        return Err(AppError::Unauthorized(
            "two-factor authentication is unavailable".to_string(),
        ));
    };
    if row.confirmed_at.is_none() {
        return Err(AppError::Unauthorized(
            "two-factor authentication is unavailable".to_string(),
        ));
    }
    if let Some(code) = totp_code.map(str::trim).filter(|code| !code.is_empty()) {
        let secret = decrypt_admin_totp_secret(&row)?;
        let secret = std::str::from_utf8(&secret)
            .map_err(|_| AppError::Unauthorized("two-factor authentication failed".to_string()))?;
        let now = now_ts();
        let current_step = now.div_euclid(row.period_seconds.max(1));
        let previous_step = row.last_used_step;
        for step in current_step.saturating_sub(1)..=current_step.saturating_add(1) {
            if previous_step.is_some_and(|last| step <= last) {
                continue;
            }
            if verify_totp_code(secret, code, step, row.digits as u32, &row.algo) {
                if state.settings_store.advance_admin_totp_last_used_step(
                    admin_user_id,
                    step,
                    now,
                )? {
                    return Ok(());
                }
                return Err(AppError::Unauthorized(
                    "two-factor authentication failed".to_string(),
                ));
            }
        }
    }
    if let Some(code) = recovery_code.map(str::trim).filter(|code| !code.is_empty()) {
        if state.settings_store.consume_admin_recovery_code(
            admin_user_id,
            &hash_token(code),
            now_ts(),
        )? {
            return Ok(());
        }
    }
    Err(AppError::Unauthorized(
        "two-factor authentication failed".to_string(),
    ))
}

/// Verify a pending TOTP setup without consuming a replay-protection step.
/// The caller must atomically confirm the pending secret after this succeeds.
pub(crate) fn verify_admin_totp_setup_code(
    row: &AdminTotpSecretRow,
    secret: &str,
    code: &str,
    now: i64,
) -> bool {
    if !matches!(row.algo.to_ascii_uppercase().as_str(), "SHA1" | "SHA256")
        || row.digits != TOTP_DIGITS as i64
        || row.period_seconds != TOTP_PERIOD_SECONDS
    {
        return false;
    }
    let current_step = now.div_euclid(row.period_seconds);
    (current_step.saturating_sub(1)..=current_step.saturating_add(1))
        .any(|step| verify_totp_code(secret, code.trim(), step, row.digits as u32, &row.algo))
}

#[cfg(test)]
pub(crate) fn totp_code_for_secret(secret: &str, now: i64) -> AppResult<String> {
    let bytes = base32_decode(secret)?;
    let step = now.div_euclid(TOTP_PERIOD_SECONDS);
    format_totp_code(&bytes, step, TOTP_DIGITS, "SHA1")
        .ok_or_else(|| AppError::Internal("unsupported 2FA algorithm".to_string()))
}

fn verify_totp_code(secret: &str, code: &str, step: i64, digits: u32, algo: &str) -> bool {
    if digits != TOTP_DIGITS || code.len() != digits as usize {
        return false;
    }
    if !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Ok(secret) = base32_decode(secret) else {
        return false;
    };
    format_totp_code(&secret, step, digits, algo).is_some_and(|expected| expected == code)
}

fn format_totp_code(secret: &[u8], step: i64, digits: u32, algo: &str) -> Option<String> {
    let counter = step.max(0).to_be_bytes();
    let digest = match algo.to_ascii_uppercase().as_str() {
        "SHA1" => hmac_sha1(secret, &counter).to_vec(),
        // Keep accepting secrets created before P2 switched new enrollment to RFC 6238 SHA-1.
        "SHA256" => hmac_sha256(secret, &counter).to_vec(),
        _ => return None,
    };
    let offset = (digest.last()? & 0x0f) as usize;
    let window = digest.get(offset..offset + 4)?;
    let binary = (u32::from(window[0]) & 0x7f) << 24
        | u32::from(window[1]) << 16
        | u32::from(window[2]) << 8
        | u32::from(window[3]);
    let modulo = 10_u32.checked_pow(digits)?;
    Some(format!(
        "{:0digits$}",
        binary % modulo,
        digits = digits as usize
    ))
}

fn hmac_sha1(key: &[u8], message: &[u8]) -> [u8; 20] {
    let mut normalized = [0_u8; 64];
    if key.len() > 64 {
        normalized[..20].copy_from_slice(&sha1_digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36_u8; 64];
    let mut opad = [0x5c_u8; 64];
    for index in 0..64 {
        ipad[index] ^= normalized[index];
        opad[index] ^= normalized[index];
    }
    let mut inner = Vec::with_capacity(64 + message.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(message);
    let inner_digest = sha1_digest(&inner);
    let mut outer = Vec::with_capacity(64 + inner_digest.len());
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&inner_digest);
    sha1_digest(&outer)
}

fn sha1_digest(message: &[u8]) -> [u8; 20] {
    let mut padded = Vec::with_capacity(message.len().saturating_add(72));
    padded.extend_from_slice(message);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&(message.len() as u64).wrapping_mul(8).to_be_bytes());

    let mut h0 = 0x6745_2301_u32;
    let mut h1 = 0xefcd_ab89_u32;
    let mut h2 = 0x98ba_dcfe_u32;
    let mut h3 = 0x1032_5476_u32;
    let mut h4 = 0xc3d2_e1f0_u32;

    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 80];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }
        for index in 16..80 {
            words[index] =
                (words[index - 3] ^ words[index - 8] ^ words[index - 14] ^ words[index - 16])
                    .rotate_left(1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;
        for (index, word) in words.iter().enumerate() {
            let (function, constant) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5a82_7999),
                20..=39 => (b ^ c ^ d, 0x6ed9_eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1b_bcdc),
                _ => (b ^ c ^ d, 0xca62_c1d6),
            };
            let next = a
                .rotate_left(5)
                .wrapping_add(function)
                .wrapping_add(e)
                .wrapping_add(constant)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = next;
        }
        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
    }

    let mut output = [0_u8; 20];
    for (chunk, value) in output.chunks_exact_mut(4).zip([h0, h1, h2, h3, h4]) {
        chunk.copy_from_slice(&value.to_be_bytes());
    }
    output
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    let mut normalized = [0_u8; 64];
    if key.len() > 64 {
        normalized[..32].copy_from_slice(Sha256::digest(key).as_slice());
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36_u8; 64];
    let mut opad = [0x5c_u8; 64];
    for index in 0..64 {
        ipad[index] ^= normalized[index];
        opad[index] ^= normalized[index];
    }
    let mut inner = Vec::with_capacity(64 + message.len());
    inner.extend_from_slice(&ipad);
    inner.extend_from_slice(message);
    let inner_digest = Sha256::digest(&inner);
    let mut outer = Vec::with_capacity(64 + inner_digest.len());
    outer.extend_from_slice(&opad);
    outer.extend_from_slice(&inner_digest);
    Sha256::digest(&outer).into()
}

fn base32_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut output = String::new();
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            output.push(ALPHABET[((buffer >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        output.push(ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize] as char);
    }
    output
}

fn base32_decode(value: &str) -> AppResult<Vec<u8>> {
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    let mut output = Vec::new();
    for byte in value
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace() && *byte != b'=')
    {
        let upper = byte.to_ascii_uppercase();
        let value = match upper {
            b'A'..=b'Z' => upper - b'A',
            b'2'..=b'7' => upper - b'2' + 26,
            _ => {
                return Err(AppError::Validation(
                    "invalid 2FA secret encoding".to_string(),
                ));
            }
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    if output.len() < 10 {
        return Err(AppError::Validation("2FA secret is too short".to_string()));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActivityLogStore;
    use crate::BlockLogStore;
    use crate::Config;
    use crate::CryptoKeys;
    use crate::DirectLinkCache;
    use crate::FileLogStore;
    use crate::PlaybackPathState;
    use crate::Semaphore;
    use crate::StdRwLock;
    use crate::TelemetryWriteQueue;
    use crate::build_http_client;
    use crate::build_proxy_http_client;
    use crate::connectivity::ConnectivityMonitor;
    use crate::db::{AdminServerGrant, AdminServerGrantInput};
    use crate::ip_location::IpLocationStore;
    use crate::playback_user_cache;
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    static AUTH_TEST_DB_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn test_auth_db_path(name: &str) -> PathBuf {
        let counter = AUTH_TEST_DB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid = std::process::id();
        std::env::temp_dir().join(format!("embypanel-auth-{name}-{pid}-{ns}-{counter}.sqlite"))
    }

    fn cleanup_test_auth_db(path: &PathBuf) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(format!("{}-wal", path.display()));
        let _ = fs::remove_file(format!("{}-shm", path.display()));
    }

    use crate::PASSWORD_TASK_CONCURRENCY_LIMIT;
    use crate::SettingsStore;
    use crate::USER_PASSWORD_RESET_CONCURRENCY_LIMIT;
    use tokio::sync::{Mutex, RwLock};

    fn sample_grant(server_id: &str) -> AdminServerGrant {
        AdminServerGrant {
            server_id: server_id.to_string(),
            scope: "full".to_string(),
            granted_at: 0,
        }
    }

    #[test]
    fn test_admin_second_factor_code_presence() {
        assert!(!admin_second_factor_code_present(None, None));
        assert!(!admin_second_factor_code_present(Some("  "), Some("")));
        assert!(admin_second_factor_code_present(Some("123456"), None));
        assert!(admin_second_factor_code_present(
            None,
            Some(" recovery-code "),
        ));
    }

    #[test]
    fn test_rfc6238_totp_vectors_for_sha1_and_sha256() {
        let sha1_secret = b"12345678901234567890";
        let sha256_secret = b"12345678901234567890123456789012";
        let vectors = [
            (59_i64, "94287082", "46119246"),
            (1_111_111_109, "07081804", "68084774"),
            (1_111_111_111, "14050471", "67062674"),
            (1_234_567_890, "89005924", "91819424"),
            (2_000_000_000, "69279037", "90698825"),
            (20_000_000_000, "65353130", "77737706"),
        ];

        for (timestamp, expected_sha1, expected_sha256) in vectors {
            let step = timestamp.div_euclid(TOTP_PERIOD_SECONDS);
            assert_eq!(
                format_totp_code(sha1_secret, step, 8, "SHA1").as_deref(),
                Some(expected_sha1)
            );
            assert_eq!(
                format_totp_code(sha256_secret, step, 8, "SHA256").as_deref(),
                Some(expected_sha256)
            );
        }
    }

    #[test]
    fn test_totp_verification_accepts_new_sha1_and_legacy_sha256() {
        let step = 1_234_567_890_i64.div_euclid(TOTP_PERIOD_SECONDS);
        let sha1_secret = base32_encode(b"12345678901234567890");
        let sha256_secret = base32_encode(b"12345678901234567890123456789012");
        let sha1_code = format_totp_code(b"12345678901234567890", step, TOTP_DIGITS, "SHA1")
            .expect("SHA1 is supported");
        let sha256_code = format_totp_code(
            b"12345678901234567890123456789012",
            step,
            TOTP_DIGITS,
            "SHA256",
        )
        .expect("SHA256 is supported");

        assert!(verify_totp_code(
            &sha1_secret,
            &sha1_code,
            step,
            TOTP_DIGITS,
            "sha1"
        ));
        assert!(verify_totp_code(
            &sha256_secret,
            &sha256_code,
            step,
            TOTP_DIGITS,
            "SHA256"
        ));
        assert!(!verify_totp_code(
            &sha1_secret,
            &sha1_code,
            step,
            TOTP_DIGITS,
            "MD5"
        ));
        assert_eq!(
            totp_code_for_secret(&sha1_secret, 1_234_567_890).unwrap(),
            sha1_code
        );
    }

    #[test]
    fn test_totp_setup_rejects_invalid_stored_parameters() {
        let secret = base32_encode(b"12345678901234567890");
        let code = totp_code_for_secret(&secret, 1_234_567_890).unwrap();
        let valid = AdminTotpSecretRow {
            admin_user_id: 1,
            secret_ciphertext: Vec::new(),
            secret_nonce: Vec::new(),
            key_version: 1,
            algo: "SHA1".to_string(),
            digits: 6,
            period_seconds: 30,
            confirmed_at: None,
            last_used_step: None,
            updated_at: 0,
        };
        assert!(verify_admin_totp_setup_code(
            &valid,
            &secret,
            &code,
            1_234_567_890
        ));
        for (algo, digits, period) in [("MD5", 6, 30), ("SHA1", 8, 30), ("SHA1", 6, 0)] {
            let mut row = valid.clone();
            row.algo = algo.to_string();
            row.digits = digits;
            row.period_seconds = period;
            assert!(!verify_admin_totp_setup_code(
                &row,
                &secret,
                &code,
                1_234_567_890
            ));
        }
    }

    #[test]
    fn test_auth_context_construction() {
        let ctx = AuthContext {
            user_id: 42,
            kind: AuthKind::Session,
            scopes: None,
            server_ids: None,
            role_keys: vec!["operator".to_string()],
            permission_keys: vec!["users.read".to_string()],
            server_grants: vec![sample_grant("srv-1")],
        };
        assert_eq!(ctx.user_id, 42);
        assert_eq!(ctx.kind, AuthKind::Session);
        assert!(ctx.scopes.is_none());
        assert!(ctx.server_ids.is_none());
        assert_eq!(ctx.role_keys, vec!["operator".to_string()]);
        assert_eq!(ctx.permission_keys, vec!["users.read".to_string()]);
        assert_eq!(ctx.server_grants.len(), 1);
        assert_eq!(ctx.server_grants[0].server_id, "srv-1");
    }

    #[test]
    fn test_has_permission_and_has_server_access() {
        let ordinary = AuthContext {
            user_id: 1,
            kind: AuthKind::Session,
            scopes: None,
            server_ids: None,
            role_keys: vec!["operator".to_string()],
            permission_keys: vec!["users.read".to_string()],
            server_grants: vec![sample_grant("srv-a")],
        };
        assert!(has_permission(&ordinary, "users.read"));
        assert!(!has_permission(&ordinary, "users.write"));
        assert!(has_server_access(&ordinary, "srv-a"));
        assert!(!has_server_access(&ordinary, "srv-b"));

        let super_admin = AuthContext {
            user_id: 2,
            kind: AuthKind::Session,
            scopes: None,
            server_ids: None,
            role_keys: vec!["super_admin".to_string()],
            permission_keys: vec![],
            server_grants: vec![],
        };
        assert!(has_permission(&super_admin, "anything"));
        assert!(has_server_access(&super_admin, "any-server"));

        let restricted_token = AuthContext {
            user_id: 3,
            kind: AuthKind::ApiToken,
            scopes: Some(vec!["users.read".to_string()]),
            server_ids: Some(vec!["srv-a".to_string()]),
            role_keys: vec!["operator".to_string()],
            permission_keys: vec!["users.read".to_string(), "users.write".to_string()],
            server_grants: vec![sample_grant("srv-a"), sample_grant("srv-b")],
        };
        assert!(has_permission(&restricted_token, "users.read"));
        assert!(!has_permission(&restricted_token, "users.write"));
        assert!(has_server_access(&restricted_token, "srv-a"));
        assert!(!has_server_access(&restricted_token, "srv-b"));

        let empty = AuthContext {
            user_id: 4,
            kind: AuthKind::Session,
            scopes: None,
            server_ids: None,
            role_keys: vec![],
            permission_keys: vec![],
            server_grants: vec![],
        };
        assert!(!has_permission(&empty, "users.read"));
        assert!(!has_server_access(&empty, "srv-a"));

        let empty_scope_super = AuthContext {
            user_id: 5,
            kind: AuthKind::ApiToken,
            scopes: Some(vec![]),
            server_ids: None,
            role_keys: vec!["super_admin".to_string()],
            permission_keys: vec![],
            server_grants: vec![],
        };
        assert!(!has_permission(&empty_scope_super, "users.read"));
        assert!(has_server_access(&empty_scope_super, "any"));
    }

    #[test]
    fn test_allowed_server_ids_and_server_filter() {
        let restricted = AuthContext {
            user_id: 10,
            kind: AuthKind::Session,
            scopes: None,
            server_ids: None,
            role_keys: vec!["operator".to_string()],
            permission_keys: vec![],
            server_grants: vec![sample_grant("srv-a")],
        };
        let allowed = allowed_server_ids(&restricted).expect("restricted identities have a set");
        assert!(allowed.contains("srv-a"));
        assert!(!allowed.contains("srv-b"));
        assert_eq!(
            require_server_filter(&restricted, Some("srv-a")).unwrap(),
            Some("srv-a")
        );
        assert!(require_server_filter(&restricted, Some("srv-b")).is_err());
        assert_eq!(
            require_server_filter(&restricted, Some("all")).unwrap(),
            None
        );

        let super_admin = AuthContext {
            user_id: 11,
            kind: AuthKind::Session,
            scopes: None,
            server_ids: None,
            role_keys: vec!["super_admin".to_string()],
            permission_keys: vec![],
            server_grants: vec![],
        };
        assert!(allowed_server_ids(&super_admin).is_none());
        assert_eq!(
            require_server_filter(&super_admin, Some("srv-b")).unwrap(),
            Some("srv-b")
        );
    }

    #[tokio::test]
    async fn sensitive_settings_handlers_reject_tokens_and_non_super_sessions_before_payloads() {
        use crate::settings_api;
        use axum::extract::Path;
        let fixture = TestAuthFixture::new_super_admin();
        let scopes = crate::db::permission_key_catalog()
            .iter()
            .map(|p| p.to_string())
            .collect::<Vec<_>>();
        fixture
            .store()
            .create_admin_api_token(
                fixture.admin_id,
                "token",
                "prefix",
                &hash_token("restricted"),
                &scopes,
                Some(&["srv-a".into()]),
                None,
                None,
                now_ts(),
            )
            .unwrap();
        let viewer = fixture
            .store()
            .create_admin_with_roles("viewer", "hash", None, &["viewer"], Some(fixture.admin_id))
            .unwrap();
        fixture
            .store()
            .create_session(viewer, &hash_token("viewer-session"), now_ts() + 3600)
            .unwrap();
        for credential in ["restricted", "viewer-session"] {
            let headers = bearer_headers(credential);
            let state = fixture.app_state().clone();
            let request = || {
                Json(EncryptedRequest {
                    encrypted_key: "invalid".into(),
                    fields: HashMap::new(),
                })
            };
            let responses = [
                settings_api::reveal_emby_api_key(
                    State(state.clone()),
                    Path("srv-b".into()),
                    headers.clone(),
                )
                .await
                .err()
                .unwrap()
                .into_response(),
                settings_api::update_settings(State(state.clone()), headers.clone(), request())
                    .await
                    .err()
                    .unwrap()
                    .into_response(),
                settings_api::validate_settings(State(state.clone()), headers.clone(), request())
                    .await
                    .err()
                    .unwrap()
                    .into_response(),
                settings_api::export_backup(State(state.clone()), headers.clone(), request())
                    .await
                    .err()
                    .unwrap()
                    .into_response(),
                settings_api::import_backup(State(state.clone()), headers.clone(), request())
                    .await
                    .err()
                    .unwrap()
                    .into_response(),
                settings_api::get_log_config(State(state), headers)
                    .await
                    .err()
                    .unwrap()
                    .into_response(),
            ];
            assert!(
                responses
                    .iter()
                    .all(|response| response.status() == axum::http::StatusCode::FORBIDDEN)
            );
        }
        let Json(security) = settings_api::list_admins(
            State(fixture.app_state().clone()),
            bearer_headers("viewer-session"),
        )
        .await
        .unwrap();
        assert_eq!(security.admins.len(), 1);
        assert_eq!(security.admins[0].id, viewer);
        assert!(security.roles.iter().any(|role| role.key == "viewer"));
        assert!(security.roles.iter().any(|role| role.key == "operator"));
        // A valid session survives forbidden operations and remains usable for permitted reads.
        assert!(
            require_auth_context(fixture.app_state(), &bearer_headers("viewer-session"))
                .await
                .is_ok()
        );
        assert!(
            settings_api::get_settings(
                State(fixture.app_state().clone()),
                bearer_headers("viewer-session")
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn get_settings_consumes_read_scope_and_filters_servers_before_returning_options() {
        use crate::settings_api;
        let fixture = TestAuthFixture::new_super_admin();
        let mut config = Config::default_runtime();
        config.emby_host = "http://hidden-server".into();
        config.emby_api_key = "global-canary".into();
        config.openlist_token = Some("openlist-canary".into());
        config.servers = ["srv-a", "srv-b"].into_iter().map(|id| serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "emby_host": format!("http://{id}"), "emby_api_key": "key-canary", "port": 8096,
            "openlist_addr_override": "http://openlist", "openlist_token_override": "override-canary",
        })).unwrap()).collect();
        *fixture.app_state().config.write().await = config;
        for (credential, scope) in [
            ("allowed-settings", "settings.read"),
            ("unrelated-settings", "users.read"),
        ] {
            fixture
                .store()
                .create_admin_api_token(
                    fixture.admin_id,
                    credential,
                    "prefix",
                    &hash_token(credential),
                    &[scope.into()],
                    Some(&["srv-a".into()]),
                    None,
                    None,
                    now_ts(),
                )
                .unwrap();
        }
        let Json(config) = settings_api::get_settings(
            State(fixture.app_state().clone()),
            bearer_headers("allowed-settings"),
        )
        .await
        .unwrap();
        assert_eq!(config.servers.len(), 1);
        assert_eq!(config.servers[0].id, "srv-a");
        assert_eq!(config.emby_host, "http://srv-a");
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("srv-b"));
        assert!(!json.contains("canary"));
        assert_eq!(
            settings_api::get_settings(
                State(fixture.app_state().clone()),
                bearer_headers("unrelated-settings")
            )
            .await
            .err()
            .unwrap()
            .into_response()
            .status(),
            axum::http::StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn test_require_helpers() {
        let allowed = AuthContext {
            user_id: 1,
            kind: AuthKind::Session,
            scopes: None,
            server_ids: None,
            role_keys: vec!["operator".to_string()],
            permission_keys: vec!["users.read".to_string()],
            server_grants: vec![sample_grant("srv-1")],
        };
        assert!(require_permission(&allowed, "users.read").is_ok());
        assert!(require_server_access(&allowed, "srv-1").is_ok());

        let denied = require_permission(&allowed, "users.write");
        assert!(denied.is_err());
        match denied {
            Err(AppError::Forbidden(msg)) => {
                assert!(msg.contains("missing permission"));
            }
            other => panic!("expected Forbidden, got {:?}", other),
        }

        let denied_srv = require_server_access(&allowed, "srv-missing");
        assert!(denied_srv.is_err());
        match denied_srv {
            Err(AppError::Forbidden(msg)) => {
                assert!(msg.contains("missing server access"));
            }
            other => panic!("expected Forbidden, got {:?}", other),
        }
    }

    struct TestAuthFixture {
        store: Option<SettingsStore>,
        admin_id: i64,
        app_state: Option<AppState>,
        db_path: PathBuf,
    }

    impl TestAuthFixture {
        fn store(&self) -> &SettingsStore {
            self.store.as_ref().expect("store already dropped")
        }

        fn app_state(&self) -> &AppState {
            self.app_state.as_ref().expect("app_state already dropped")
        }

        fn new() -> Self {
            Self::create("admin", false)
        }

        fn new_super_admin() -> Self {
            Self::create("super", true)
        }

        fn create(db_label: &str, super_admin: bool) -> Self {
            let db_path = test_auth_db_path(db_label);
            let store = SettingsStore::open_for_test(db_path.clone()).unwrap();
            let admin_id = if super_admin {
                store
                    .create_initial_admin("superuser", "hash123")
                    .unwrap()
                    .expect("bootstrap super_admin must be created")
            } else {
                let role_id = store
                    .create_admin_role_for_test("test-role", "Test Role")
                    .unwrap();
                store
                    .replace_role_permissions(
                        role_id,
                        &["users.read".to_string(), "users.write".to_string()],
                    )
                    .unwrap();
                let admin_id = store
                    .create_admin_with_roles("testuser", "hash123", None, &["test-role"], None)
                    .unwrap();
                store
                    .replace_admin_server_grants(
                        admin_id,
                        &[AdminServerGrantInput {
                            server_id: "srv-a".to_string(),
                            scope: "full".to_string(),
                        }],
                        None,
                        now_ts(),
                    )
                    .unwrap();
                admin_id
            };

            let config = Config {
                emby_host: "http://127.0.0.1:8096".to_string(),
                emby_api_key: "test".to_string(),
                servers: vec![],
                openlist_addr: Some("http://127.0.0.1:8097".to_string()),
                openlist_token: Some("test".to_string()),
                port: 8096,
                cache_ttl_seconds: 180,
                cache_max_capacity: 10000,
                cache_enabled: true,
                cache_domain_filter_mode: "off".to_string(),
                cache_domain_whitelist: String::new(),
                cache_domain_blacklist: String::new(),
                enable_internal_redirect: false,
                internal_redirect_timeout_seconds: 15,
                strm_url_mappings: String::new(),
                strm_url_mapping_enabled: false,
                connectivity_check_enabled: false,
                connectivity_check_interval_seconds: 30,
                connectivity_check_timeout_seconds: 10,
                connectivity_auto_restart_seconds: 300,
                strm_url_mapping_rules: vec![],
            };
            let client = build_http_client().unwrap();
            let proxy_client = build_proxy_http_client().unwrap();
            let telemetry_writes = TelemetryWriteQueue::start(store.clone()).unwrap();
            let alerts = crate::alerts::Alerts::new(&db_path).unwrap();
            let app_state = AppState {
                config: Arc::new(RwLock::new(config)),
                config_updates: Arc::new(Mutex::new(())),
                client,
                proxy_client,
                cache: Arc::new(RwLock::new(DirectLinkCache::new(true, 180, 10_000))),
                file_log: Arc::new(FileLogStore::new(&store)),
                settings_store: store.clone(),
                telemetry_writes,
                alerts,
                alert_runtime: crate::alerts_runtime::AlertRuntime::disabled(),
                client_control_cache: Arc::new(StdRwLock::new(None)),
                user_policies_cache: Arc::new(StdRwLock::new(None)),
                rate_limit_updates: Arc::new(Mutex::new(())),
                playback_sessions_cache: Arc::new(Mutex::new(HashMap::new())),
                password_tasks: Arc::new(Semaphore::new(PASSWORD_TASK_CONCURRENCY_LIMIT)),
                user_password_tasks: Arc::new(Semaphore::new(
                    USER_PASSWORD_RESET_CONCURRENCY_LIMIT,
                )),
                crypto_keys: CryptoKeys::generate().unwrap(),
                activity_log: Arc::new(ActivityLogStore::new(100)),
                block_log: Arc::new(BlockLogStore::new()),
                ip_location: Arc::new(IpLocationStore::new()),
                connectivity: Arc::new(ConnectivityMonitor::new()),
                proxy_manager: None,
                proxy_server_id: None,
                proxy_metrics: None,
                playback_paths: Arc::new(Mutex::new(PlaybackPathState::default())),
                playback_users: playback_user_cache(),
                playback_rate_hits: Arc::new(Mutex::new(HashMap::new())),
                playback_rate_recent_events: Arc::new(Mutex::new(HashMap::new())),
                playback_rate_bans: Arc::new(Mutex::new(HashMap::new())),
                playback_rate_ip_bans: Arc::new(Mutex::new(HashMap::new())),
                login_failures: Arc::new(Mutex::new(HashMap::new())),
                update_check: Arc::new(Mutex::new(None)),
            };
            TestAuthFixture {
                store: Some(store),
                admin_id,
                app_state: Some(app_state),
                db_path,
            }
        }
    }

    impl Drop for TestAuthFixture {
        fn drop(&mut self) {
            if let Some(state) = self.app_state.take() {
                let shutdown_res = state.telemetry_writes.shutdown();
                if let Err(err) = shutdown_res {
                    eprintln!("Warning: telemetry shutdown failed: {}", err);
                }
                drop(state);
            }
            let _ = self.store.take();
            cleanup_test_auth_db(&self.db_path);
        }
    }

    fn bearer_headers(plain_token: &str) -> HeaderMap {
        HeaderMap::from_iter(vec![(
            header::AUTHORIZATION,
            format!("Bearer {plain_token}").parse().unwrap(),
        )])
    }

    fn create_test_api_token(
        store: &SettingsStore,
        admin_id: i64,
        name: &str,
        prefix: &str,
        plain_token: &str,
        scopes: &[String],
        server_ids: Option<&[String]>,
    ) -> i64 {
        let token_hash = hash_token(plain_token);
        store
            .create_admin_api_token(
                admin_id,
                name,
                prefix,
                &token_hash,
                scopes,
                server_ids,
                None,
                None,
                now_ts(),
            )
            .unwrap()
    }

    fn assert_forbidden(result: AppResult<()>, expected_fragment: &str) {
        match result {
            Err(AppError::Forbidden(msg)) => {
                assert!(
                    msg.contains(expected_fragment),
                    "unexpected forbidden message: {msg}"
                );
            }
            other => panic!("expected Forbidden, got {other:?}"),
        }
    }

    fn assert_require_auth_unauthorized(result: AppResult<AuthContext>) {
        match result {
            Err(AppError::Unauthorized(_)) => {}
            other => panic!("expected Unauthorized, got {other:?}"),
        }
    }

    fn create_live_session(store: &SettingsStore, admin_id: i64, plain_token: &str) {
        store
            .create_session(admin_id, &hash_token(plain_token), now_ts() + 3600)
            .unwrap();
    }

    async fn assert_session_auth_ok(state: &AppState, admin_id: i64, plain_token: &str) {
        let ctx = require_auth_context(state, &bearer_headers(plain_token))
            .await
            .unwrap();
        assert_eq!(ctx.user_id, admin_id);
        assert_eq!(ctx.kind, AuthKind::Session);
    }

    async fn assert_api_token_auth_ok(state: &AppState, admin_id: i64, plain_token: &str) {
        let ctx = require_auth_context(state, &bearer_headers(plain_token))
            .await
            .unwrap();
        assert_eq!(ctx.user_id, admin_id);
        assert_eq!(ctx.kind, AuthKind::ApiToken);
    }

    #[tokio::test]
    async fn test_ordinary_admin_session_auth_success() {
        let fixture = TestAuthFixture::new();
        let state = fixture.app_state();
        let store = fixture.store();
        let token = "test-session-token";
        let token_hash = hash_token(token);
        let future_expires_at = now_ts() + 3600 * 24;
        store
            .create_session(fixture.admin_id, &token_hash, future_expires_at)
            .unwrap();
        let header = HeaderMap::from_iter(vec![(
            header::AUTHORIZATION,
            format!("Bearer {}", token).parse().unwrap(),
        )]);
        let ctx = require_auth_context(state, &header).await.unwrap();
        assert_eq!(ctx.user_id, fixture.admin_id);
        assert_eq!(ctx.kind, AuthKind::Session);
        assert!(ctx.scopes.is_none());
        assert!(ctx.server_ids.is_none());
        assert_eq!(ctx.role_keys, vec!["test-role".to_string()]);
        assert_eq!(
            ctx.permission_keys,
            vec!["users.read".to_string(), "users.write".to_string()]
        );
        assert_eq!(ctx.server_grants.len(), 1);
        assert_eq!(ctx.server_grants[0].server_id, "srv-a");
        assert_eq!(ctx.server_grants[0].scope, "full");
        assert!(has_permission(&ctx, "users.read"));
        assert!(has_permission(&ctx, "users.write"));
        assert!(has_server_access(&ctx, "srv-a"));
        assert!(!has_server_access(&ctx, "srv-b"));
        assert!(!has_permission(&ctx, "alerts.write"));
    }

    #[tokio::test]
    async fn test_ordinary_admin_api_token_auth_success() {
        let fixture = TestAuthFixture::new();
        let state = fixture.app_state();
        let store = fixture.store();
        let plain_token = "test-api-token";
        let token_hash = hash_token(plain_token);
        let scopes = vec!["users.read".to_string()];
        let server_ids = vec!["srv-a".to_string()];
        let token_id = store
            .create_admin_api_token(
                fixture.admin_id,
                "test-api",
                "embyp_ab",
                &token_hash,
                &scopes,
                Some(&server_ids),
                None,
                None,
                now_ts(),
            )
            .unwrap();
        assert!(token_id > 0);
        let header = HeaderMap::from_iter(vec![(
            header::AUTHORIZATION,
            format!("Bearer {}", plain_token).parse().unwrap(),
        )]);
        let ctx = require_auth_context(state, &header).await.unwrap();
        assert_eq!(ctx.user_id, fixture.admin_id);
        assert_eq!(ctx.kind, AuthKind::ApiToken);
        assert_eq!(ctx.scopes, Some(scopes.clone()));
        assert_eq!(ctx.server_ids, Some(server_ids.clone()));
        assert_eq!(ctx.role_keys, vec!["test-role".to_string()]);
        assert_eq!(
            ctx.permission_keys,
            vec!["users.read".to_string(), "users.write".to_string()]
        );
        assert_eq!(ctx.server_grants.len(), 1);
        assert_eq!(ctx.server_grants[0].server_id, "srv-a");
        assert_eq!(ctx.server_grants[0].scope, "full");
        assert!(has_permission(&ctx, "users.read"));
        assert!(!has_permission(&ctx, "users.write"));
        assert!(has_server_access(&ctx, "srv-a"));
        assert!(!has_server_access(&ctx, "srv-b"));
    }
    #[tokio::test]
    async fn test_ordinary_api_token_permission_boundaries() {
        let fixture = TestAuthFixture::new();
        let state = fixture.app_state();
        let store = fixture.store();

        let session_token = "b2-ordinary-session-perm";
        store
            .create_session(
                fixture.admin_id,
                &hash_token(session_token),
                now_ts() + 3600,
            )
            .unwrap();
        let identity_ctx = require_auth_context(state, &bearer_headers(session_token))
            .await
            .unwrap();
        assert_eq!(identity_ctx.kind, AuthKind::Session);
        assert!(identity_ctx.scopes.is_none());
        assert!(identity_ctx.server_ids.is_none());
        assert_eq!(identity_ctx.role_keys, vec!["test-role".to_string()]);
        assert!(
            identity_ctx
                .permission_keys
                .iter()
                .any(|permission| permission == "users.read")
        );
        assert!(
            identity_ctx
                .permission_keys
                .iter()
                .any(|permission| permission == "users.write")
        );
        assert!(has_permission(&identity_ctx, "users.read"));
        assert!(has_permission(&identity_ctx, "users.write"));
        assert!(!has_permission(&identity_ctx, "alerts.write"));

        let narrow_token = "b2-ordinary-token-narrow";
        let narrow_scopes = vec!["users.read".to_string(), "alerts.write".to_string()];
        let narrow_id = create_test_api_token(
            store,
            fixture.admin_id,
            "b2-perm-narrow",
            "embyp_b2n",
            narrow_token,
            &narrow_scopes,
            None,
        );
        assert!(narrow_id > 0);
        let narrow_ctx = require_auth_context(state, &bearer_headers(narrow_token))
            .await
            .unwrap();
        assert_eq!(narrow_ctx.user_id, fixture.admin_id);
        assert_eq!(narrow_ctx.kind, AuthKind::ApiToken);
        assert_eq!(narrow_ctx.scopes, Some(narrow_scopes.clone()));
        assert!(narrow_ctx.server_ids.is_none());
        assert_eq!(narrow_ctx.role_keys, vec!["test-role".to_string()]);
        assert_eq!(
            narrow_ctx.permission_keys,
            vec!["users.read".to_string(), "users.write".to_string()]
        );
        assert!(has_permission(&narrow_ctx, "users.read"));
        assert!(!has_permission(&narrow_ctx, "users.write"));
        assert!(!has_permission(&narrow_ctx, "alerts.write"));
        assert!(has_permission(&narrow_ctx, "users.read"));
        assert!(!has_permission(&narrow_ctx, "Users.Read"));
        assert!(require_permission(&narrow_ctx, "users.read").is_ok());
        assert_forbidden(
            require_permission(&narrow_ctx, "users.write"),
            "missing permission",
        );
        assert_forbidden(
            require_permission(&narrow_ctx, "alerts.write"),
            "missing permission",
        );
        assert_forbidden(
            require_permission(&narrow_ctx, "Users.Read"),
            "missing permission",
        );

        let literal_token = "b2-ordinary-token-literals";
        let literal_scopes = vec!["all".to_string(), "*".to_string()];
        let literal_id = create_test_api_token(
            store,
            fixture.admin_id,
            "b2-perm-wildcard-literals",
            "embyp_b2w",
            literal_token,
            &literal_scopes,
            None,
        );
        assert!(literal_id > 0);
        let literal_ctx = require_auth_context(state, &bearer_headers(literal_token))
            .await
            .unwrap();
        assert_eq!(literal_ctx.kind, AuthKind::ApiToken);
        assert_eq!(literal_ctx.scopes, Some(literal_scopes.clone()));
        assert_eq!(literal_ctx.role_keys, vec!["test-role".to_string()]);
        assert!(!has_permission(&literal_ctx, "users.read"));
        assert_forbidden(
            require_permission(&literal_ctx, "users.read"),
            "missing permission",
        );
    }

    #[tokio::test]
    async fn test_api_token_server_id_boundaries() {
        let fixture = TestAuthFixture::new();
        let state = fixture.app_state();
        let store = fixture.store();
        store
            .replace_admin_server_grants(
                fixture.admin_id,
                &[
                    AdminServerGrantInput {
                        server_id: "srv-a".to_string(),
                        scope: "full".to_string(),
                    },
                    AdminServerGrantInput {
                        server_id: "srv-b".to_string(),
                        scope: "full".to_string(),
                    },
                ],
                None,
                now_ts(),
            )
            .unwrap();

        let session_token = "b2-ordinary-session-servers";
        store
            .create_session(
                fixture.admin_id,
                &hash_token(session_token),
                now_ts() + 3600,
            )
            .unwrap();
        let identity_ctx = require_auth_context(state, &bearer_headers(session_token))
            .await
            .unwrap();
        assert_eq!(identity_ctx.kind, AuthKind::Session);
        assert_eq!(identity_ctx.server_grants.len(), 2);
        assert!(
            identity_ctx
                .server_grants
                .iter()
                .any(|grant| grant.server_id == "srv-a")
        );
        assert!(
            identity_ctx
                .server_grants
                .iter()
                .any(|grant| grant.server_id == "srv-b")
        );
        assert!(has_server_access(&identity_ctx, "srv-a"));
        assert!(has_server_access(&identity_ctx, "srv-b"));
        assert!(!has_server_access(&identity_ctx, "srv-c"));

        let inherit_token = "b2-ordinary-token-inherit-servers";
        let inherit_id = create_test_api_token(
            store,
            fixture.admin_id,
            "b2-srv-inherit",
            "embyp_b2i",
            inherit_token,
            &["users.read".to_string()],
            None,
        );
        assert!(inherit_id > 0);
        let inherit_ctx = require_auth_context(state, &bearer_headers(inherit_token))
            .await
            .unwrap();
        assert_eq!(inherit_ctx.kind, AuthKind::ApiToken);
        assert!(inherit_ctx.server_ids.is_none());
        assert_eq!(inherit_ctx.server_grants.len(), 2);
        assert!(has_server_access(&inherit_ctx, "srv-a"));
        assert!(has_server_access(&inherit_ctx, "srv-b"));
        assert!(!has_server_access(&inherit_ctx, "srv-c"));
        assert!(require_server_access(&inherit_ctx, "srv-a").is_ok());
        assert!(require_server_access(&inherit_ctx, "srv-b").is_ok());
        assert_forbidden(
            require_server_access(&inherit_ctx, "srv-c"),
            "missing server access",
        );

        let empty_token = "b2-ordinary-token-empty-servers";
        let empty_ids: Vec<String> = vec![];
        let empty_id = create_test_api_token(
            store,
            fixture.admin_id,
            "b2-srv-empty",
            "embyp_b2e",
            empty_token,
            &["users.read".to_string()],
            Some(empty_ids.as_slice()),
        );
        assert!(empty_id > 0);
        let empty_ctx = require_auth_context(state, &bearer_headers(empty_token))
            .await
            .unwrap();
        assert_eq!(empty_ctx.kind, AuthKind::ApiToken);
        assert_eq!(empty_ctx.server_ids, Some(vec![]));
        assert!(!has_server_access(&empty_ctx, "srv-a"));
        assert!(!has_server_access(&empty_ctx, "srv-b"));
        assert!(!has_server_access(&empty_ctx, "srv-c"));
        assert_forbidden(
            require_server_access(&empty_ctx, "srv-a"),
            "missing server access",
        );

        let narrow_token = "b2-ordinary-token-narrow-servers";
        let narrow_ids = vec!["srv-a".to_string(), "srv-c".to_string()];
        let narrow_id = create_test_api_token(
            store,
            fixture.admin_id,
            "b2-srv-narrow",
            "embyp_b2s",
            narrow_token,
            &["users.read".to_string()],
            Some(narrow_ids.as_slice()),
        );
        assert!(narrow_id > 0);
        let narrow_ctx = require_auth_context(state, &bearer_headers(narrow_token))
            .await
            .unwrap();
        assert_eq!(narrow_ctx.kind, AuthKind::ApiToken);
        assert_eq!(narrow_ctx.server_ids, Some(narrow_ids.clone()));
        assert!(has_server_access(&narrow_ctx, "srv-a"));
        assert!(!has_server_access(&narrow_ctx, "srv-b"));
        assert!(!has_server_access(&narrow_ctx, "srv-c"));
        assert!(require_server_access(&narrow_ctx, "srv-a").is_ok());
        assert_forbidden(
            require_server_access(&narrow_ctx, "srv-b"),
            "missing server access",
        );
        assert_forbidden(
            require_server_access(&narrow_ctx, "srv-c"),
            "missing server access",
        );
    }

    #[tokio::test]
    async fn test_super_admin_session_bypasses_permission_and_server_limits() {
        let fixture = TestAuthFixture::new_super_admin();
        let state = fixture.app_state();
        let store = fixture.store();
        let session_token = "b2-super-session";
        store
            .create_session(
                fixture.admin_id,
                &hash_token(session_token),
                now_ts() + 3600,
            )
            .unwrap();
        let ctx = require_auth_context(state, &bearer_headers(session_token))
            .await
            .unwrap();
        assert_eq!(ctx.user_id, fixture.admin_id);
        assert_eq!(ctx.kind, AuthKind::Session);
        assert!(ctx.scopes.is_none());
        assert!(ctx.server_ids.is_none());
        assert!(ctx.role_keys.iter().any(|role| role == "super_admin"));
        assert!(has_permission(&ctx, "never.configured.permission"));
        assert!(has_permission(&ctx, "alerts.write"));
        assert!(has_server_access(&ctx, "srv-never-granted"));
        assert!(has_server_access(&ctx, "srv-z"));
        assert!(require_permission(&ctx, "never.configured.permission").is_ok());
        assert!(require_server_access(&ctx, "srv-never-granted").is_ok());
    }

    #[tokio::test]
    async fn test_super_admin_api_token_is_still_token_restricted() {
        let fixture = TestAuthFixture::new_super_admin();
        let state = fixture.app_state();
        let store = fixture.store();

        let restricted_token = "b2-super-token-restricted";
        let restricted_scopes = vec!["users.read".to_string()];
        let restricted_servers = vec!["srv-a".to_string()];
        let restricted_id = create_test_api_token(
            store,
            fixture.admin_id,
            "b2-super-restricted",
            "embyp_b2r",
            restricted_token,
            &restricted_scopes,
            Some(restricted_servers.as_slice()),
        );
        assert!(restricted_id > 0);
        let restricted_ctx = require_auth_context(state, &bearer_headers(restricted_token))
            .await
            .unwrap();
        assert_eq!(restricted_ctx.user_id, fixture.admin_id);
        assert_eq!(restricted_ctx.kind, AuthKind::ApiToken);
        assert_eq!(restricted_ctx.scopes, Some(restricted_scopes.clone()));
        assert_eq!(restricted_ctx.server_ids, Some(restricted_servers.clone()));
        assert!(
            restricted_ctx
                .role_keys
                .iter()
                .any(|role| role == "super_admin")
        );
        assert!(has_permission(&restricted_ctx, "users.read"));
        assert!(!has_permission(&restricted_ctx, "users.write"));
        assert!(has_server_access(&restricted_ctx, "srv-a"));
        assert!(!has_server_access(&restricted_ctx, "srv-b"));
        let allowed = allowed_server_ids(&restricted_ctx).expect("token server scope is finite");
        assert_eq!(allowed, ["srv-a".to_string()].into_iter().collect());
        assert!(require_permission(&restricted_ctx, "users.read").is_ok());
        assert_forbidden(
            require_permission(&restricted_ctx, "users.write"),
            "missing permission",
        );
        assert!(require_server_access(&restricted_ctx, "srv-a").is_ok());
        assert_forbidden(
            require_server_access(&restricted_ctx, "srv-b"),
            "missing server access",
        );

        let closed_token = "b2-super-token-failclosed";
        let empty_scopes: Vec<String> = vec![];
        let empty_servers: Vec<String> = vec![];
        let closed_id = create_test_api_token(
            store,
            fixture.admin_id,
            "b2-super-failclosed",
            "embyp_b2f",
            closed_token,
            &empty_scopes,
            Some(empty_servers.as_slice()),
        );
        assert!(closed_id > 0);
        let closed_ctx = require_auth_context(state, &bearer_headers(closed_token))
            .await
            .unwrap();
        assert_eq!(closed_ctx.kind, AuthKind::ApiToken);
        assert_eq!(closed_ctx.scopes, Some(vec![]));
        assert_eq!(closed_ctx.server_ids, Some(vec![]));
        assert!(
            closed_ctx
                .role_keys
                .iter()
                .any(|role| role == "super_admin")
        );
        assert!(!has_permission(&closed_ctx, "users.read"));
        assert!(!has_permission(&closed_ctx, "users.write"));
        assert!(!has_server_access(&closed_ctx, "srv-a"));
        assert!(!has_server_access(&closed_ctx, "srv-b"));
        assert_forbidden(
            require_permission(&closed_ctx, "users.read"),
            "missing permission",
        );
        assert_forbidden(
            require_server_access(&closed_ctx, "srv-a"),
            "missing server access",
        );
    }

    #[tokio::test]
    async fn test_server_grant_scope_is_orthogonal_to_server_access() {
        let fixture = TestAuthFixture::new();
        let state = fixture.app_state();
        let store = fixture.store();
        store
            .replace_admin_server_grants(
                fixture.admin_id,
                &[AdminServerGrantInput {
                    server_id: "srv-readonly".to_string(),
                    scope: "read_only".to_string(),
                }],
                None,
                now_ts(),
            )
            .unwrap();

        let session_token = "b2-ordinary-session-grant-scope";
        store
            .create_session(
                fixture.admin_id,
                &hash_token(session_token),
                now_ts() + 3600,
            )
            .unwrap();
        let ctx = require_auth_context(state, &bearer_headers(session_token))
            .await
            .unwrap();
        assert_eq!(ctx.kind, AuthKind::Session);
        assert!(ctx.scopes.is_none());
        assert!(ctx.server_ids.is_none());
        assert_eq!(ctx.role_keys, vec!["test-role".to_string()]);
        assert_eq!(ctx.server_grants.len(), 1);
        assert_eq!(ctx.server_grants[0].server_id, "srv-readonly");
        assert_eq!(ctx.server_grants[0].scope, "read_only");
        assert!(has_server_access(&ctx, "srv-readonly"));
        assert!(!has_server_access(&ctx, "srv-a"));
        assert!(!has_server_access(&ctx, "read_only"));
        assert!(!has_permission(&ctx, "read_only"));
        assert!(require_server_access(&ctx, "srv-readonly").is_ok());
        assert_forbidden(
            require_server_access(&ctx, "srv-a"),
            "missing server access",
        );
        assert_forbidden(require_permission(&ctx, "read_only"), "missing permission");
    }

    #[tokio::test]
    async fn test_disabled_credentials_fail_closed() {
        let fixture = TestAuthFixture::new();
        let state = fixture.app_state();
        let store = fixture.store();

        let session_token = "b3-disabled-session";
        create_live_session(store, fixture.admin_id, session_token);
        assert_session_auth_ok(state, fixture.admin_id, session_token).await;

        let api_token = "b3-disabled-api";
        let api_token_id = create_test_api_token(
            store,
            fixture.admin_id,
            "b3-disabled-api",
            "embyp_b3d",
            api_token,
            &["users.read".to_string()],
            None,
        );
        assert!(api_token_id > 0);
        assert_api_token_auth_ok(state, fixture.admin_id, api_token).await;

        assert!(
            store
                .set_admin_disabled(fixture.admin_id, true, now_ts())
                .unwrap()
        );

        assert_require_auth_unauthorized(
            require_auth_context(state, &bearer_headers(session_token)).await,
        );
        assert_require_auth_unauthorized(
            require_auth_context(state, &bearer_headers(api_token)).await,
        );
    }

    #[tokio::test]
    async fn test_expired_credentials_fail_closed() {
        let fixture = TestAuthFixture::new();
        let state = fixture.app_state();
        let store = fixture.store();

        let live_session = "b3-expired-live-session";
        create_live_session(store, fixture.admin_id, live_session);
        assert_session_auth_ok(state, fixture.admin_id, live_session).await;

        let live_api = "b3-expired-live-api";
        let live_api_id = create_test_api_token(
            store,
            fixture.admin_id,
            "b3-expired-live-api",
            "embyp_b3el",
            live_api,
            &["users.read".to_string()],
            None,
        );
        assert!(live_api_id > 0);
        assert_api_token_auth_ok(state, fixture.admin_id, live_api).await;

        let past = now_ts() - 3600;
        let expired_session = "b3-expired-session";
        store
            .create_session(fixture.admin_id, &hash_token(expired_session), past)
            .unwrap();
        assert_require_auth_unauthorized(
            require_auth_context(state, &bearer_headers(expired_session)).await,
        );

        let expired_api = "b3-expired-api";
        let expired_api_id = store
            .create_admin_api_token(
                fixture.admin_id,
                "b3-expired-api",
                "embyp_b3ex",
                &hash_token(expired_api),
                &["users.read".to_string()],
                None,
                Some(past),
                None,
                now_ts(),
            )
            .unwrap();
        assert!(expired_api_id > 0);
        assert_require_auth_unauthorized(
            require_auth_context(state, &bearer_headers(expired_api)).await,
        );
    }

    #[tokio::test]
    async fn test_revoked_credentials_fail_closed() {
        let fixture = TestAuthFixture::new();
        let state = fixture.app_state();
        let store = fixture.store();

        let session_token = "b3-revoked-session";
        create_live_session(store, fixture.admin_id, session_token);
        assert_session_auth_ok(state, fixture.admin_id, session_token).await;
        let sessions_before = store.list_admin_sessions(fixture.admin_id).unwrap();
        assert_eq!(sessions_before.len(), 1);
        let session_id = sessions_before[0].id;
        assert!(sessions_before[0].revoked_at.is_none());

        let api_token = "b3-revoked-api";
        let api_token_id = create_test_api_token(
            store,
            fixture.admin_id,
            "b3-revoked-api",
            "embyp_b3r",
            api_token,
            &["users.read".to_string()],
            None,
        );
        assert!(api_token_id > 0);
        assert_api_token_auth_ok(state, fixture.admin_id, api_token).await;
        let tokens_before = store.list_admin_api_tokens(fixture.admin_id).unwrap();
        let token_before = tokens_before
            .iter()
            .find(|item| item.id == api_token_id)
            .expect("api token row must exist before revoke");
        assert_eq!(token_before.name, "b3-revoked-api");
        assert_eq!(token_before.token_prefix, "embyp_b3r");
        assert!(token_before.revoked_at.is_none());

        let now = now_ts();
        assert_eq!(
            store
                .revoke_session(&hash_token(session_token), now)
                .unwrap(),
            Some(fixture.admin_id)
        );
        assert!(store.revoke_admin_api_token(api_token_id, now).unwrap());

        assert_require_auth_unauthorized(
            require_auth_context(state, &bearer_headers(session_token)).await,
        );
        assert_require_auth_unauthorized(
            require_auth_context(state, &bearer_headers(api_token)).await,
        );

        let sessions_after = store.list_admin_sessions(fixture.admin_id).unwrap();
        let session_after = sessions_after
            .iter()
            .find(|item| item.id == session_id)
            .expect("session row must remain after revoke");
        assert!(session_after.revoked_at.is_some());

        let tokens_after = store.list_admin_api_tokens(fixture.admin_id).unwrap();
        let token_after = tokens_after
            .iter()
            .find(|item| item.id == api_token_id)
            .expect("api token row must remain after revoke");
        assert_eq!(token_after.name, "b3-revoked-api");
        assert_eq!(token_after.token_prefix, "embyp_b3r");
        assert!(token_after.revoked_at.is_some());
    }

    #[tokio::test]
    async fn test_orphaned_credentials_fail_closed_and_rows_remain() {
        let fixture = TestAuthFixture::new();
        let state = fixture.app_state();
        let store = fixture.store();
        assert!(store.has_admin().unwrap());

        let orphan_admin_id = store
            .create_admin_with_roles("b3-orphan-user", "hash123", None, &["test-role"], None)
            .unwrap();
        assert_ne!(orphan_admin_id, fixture.admin_id);

        let session_token = "b3-orphan-session";
        create_live_session(store, orphan_admin_id, session_token);
        assert_session_auth_ok(state, orphan_admin_id, session_token).await;
        let sessions_before = store.list_admin_sessions(orphan_admin_id).unwrap();
        assert_eq!(sessions_before.len(), 1);
        let session_id = sessions_before[0].id;
        assert_eq!(sessions_before[0].admin_user_id, orphan_admin_id);

        let api_token = "b3-orphan-api";
        let api_token_id = create_test_api_token(
            store,
            orphan_admin_id,
            "b3-orphan-api",
            "embyp_b3o",
            api_token,
            &["users.read".to_string()],
            None,
        );
        assert!(api_token_id > 0);
        assert_api_token_auth_ok(state, orphan_admin_id, api_token).await;
        let tokens_before = store.list_admin_api_tokens(orphan_admin_id).unwrap();
        let token_before = tokens_before
            .iter()
            .find(|item| item.id == api_token_id)
            .expect("api token row must exist before orphan");
        assert_eq!(token_before.name, "b3-orphan-api");
        assert_eq!(token_before.token_prefix, "embyp_b3o");
        assert_eq!(token_before.admin_user_id, orphan_admin_id);

        store.orphan_admin_user_for_test(orphan_admin_id).unwrap();
        assert!(store.has_admin().unwrap());

        let sessions_after = store.list_admin_sessions(orphan_admin_id).unwrap();
        assert!(
            sessions_after
                .iter()
                .any(|item| item.id == session_id && item.admin_user_id == orphan_admin_id),
            "orphaned session row must remain listable"
        );
        let tokens_after = store.list_admin_api_tokens(orphan_admin_id).unwrap();
        assert!(
            tokens_after.iter().any(|item| {
                item.id == api_token_id
                    && item.name == "b3-orphan-api"
                    && item.token_prefix == "embyp_b3o"
                    && item.admin_user_id == orphan_admin_id
            }),
            "orphaned api token row must remain listable"
        );

        assert_require_auth_unauthorized(
            require_auth_context(state, &bearer_headers(session_token)).await,
        );
        assert_require_auth_unauthorized(
            require_auth_context(state, &bearer_headers(api_token)).await,
        );
    }

    #[tokio::test]
    async fn test_session_and_api_token_hash_collision_is_rejected() {
        let fixture = TestAuthFixture::new();
        let state = fixture.app_state();
        let store = fixture.store();

        let plain_token = "b3-collision-shared";
        let token_hash = hash_token(plain_token);
        store
            .create_session(fixture.admin_id, &token_hash, now_ts() + 3600)
            .unwrap();
        let api_token_id = store
            .create_admin_api_token(
                fixture.admin_id,
                "b3-collision-api",
                "embyp_b3c",
                &token_hash,
                &["users.read".to_string()],
                None,
                None,
                None,
                now_ts(),
            )
            .unwrap();
        assert!(api_token_id > 0);

        let sessions = store.list_admin_sessions(fixture.admin_id).unwrap();
        assert_eq!(sessions.len(), 1);
        assert!(sessions[0].revoked_at.is_none());
        assert!(sessions[0].expires_at > now_ts());
        let tokens = store.list_admin_api_tokens(fixture.admin_id).unwrap();
        let token_meta = tokens
            .iter()
            .find(|item| item.id == api_token_id)
            .expect("collision api token must remain listable");
        assert!(token_meta.revoked_at.is_none());
        assert!(token_meta.expires_at.is_none() || token_meta.expires_at.unwrap() > now_ts());

        match require_auth_context(state, &bearer_headers(plain_token)).await {
            Err(AppError::Unauthorized(message)) => {
                assert_eq!(message, "authentication collision");
            }
            other => panic!("expected authentication collision, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_auth_database_error_is_preserved() {
        let fixture = TestAuthFixture::new();
        let store = fixture.store();

        let session_token = "b3-db-error-session";
        create_live_session(store, fixture.admin_id, session_token);
        assert_session_auth_ok(fixture.app_state(), fixture.admin_id, session_token).await;

        let counter = AUTH_TEST_DB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let bad_dir = std::env::temp_dir().join(format!(
            "embypanel-auth-b3-bad-{}-{}-{}",
            std::process::id(),
            ns,
            counter
        ));
        fs::create_dir_all(&bad_dir).unwrap();

        let mut state = fixture.app_state().clone();
        let mut bad_store = state.settings_store.clone();
        bad_store.replace_path_for_test(bad_dir.clone());
        state.settings_store = bad_store;

        let result = require_auth_context(&state, &bearer_headers(session_token)).await;
        let _ = fs::remove_dir_all(&bad_dir);
        match result {
            Err(AppError::Database(_)) => {}
            other => panic!("expected Database error, got {other:?}"),
        }
    }
}
