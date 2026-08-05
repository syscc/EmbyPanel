use std::{
    collections::{HashMap, VecDeque},
    net::SocketAddr,
    time::{SystemTime, UNIX_EPOCH},
};

use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::{HeaderMap, header},
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    AppState,
    crypto_api::EncryptedRequest,
    db::SetupStatus,
    error::{AppError, AppResult},
};

const SESSION_TTL_SECONDS: i64 = 86400 * 30;
const LOGIN_FAILURE_WINDOW_SECONDS: i64 = 300;
const LOGIN_FAILURE_LIMIT: usize = 8;
const LOGIN_FAILURE_KEY_CAPACITY: usize = 2048;
pub const TOKEN_COOKIE_NAME: &str = "embypanel_token";

#[derive(Debug, Deserialize)]
pub struct SetupRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
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
    if let Err(err) = state.settings_store.record_audit(
        Some(admin_user_id),
        "account.setup",
        "初始化管理员账户",
        "success",
    ) {
        tracing::error!(error = %err.safe_log_message(), "failed to record account setup audit");
    }
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
    let admin_user_id = require_auth_user_id(&state, &headers).await?;
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
    if let Err(err) = state.settings_store.record_audit(
        Some(admin_user_id),
        "account.password",
        "修改管理员密码",
        "success",
    ) {
        tracing::error!(error = %err.safe_log_message(), "failed to record password change audit");
    }
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
    if let Err(err) = state.settings_store.record_audit(
        Some(admin_user_id),
        "account.logout",
        "管理员退出登录",
        "success",
    ) {
        tracing::error!(error = %err.safe_log_message(), "failed to record logout audit");
    }
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
    let admin_user_id = require_auth_user_id(&state, &headers).await?;
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
    if !state.settings_store.has_admin()? {
        return Err(AppError::Unauthorized(
            "admin is not initialized".to_string(),
        ));
    }
    let token = auth_token(headers)?;
    let token_hash = hash_token(token);
    state
        .settings_store
        .session_admin_user_id(&token_hash, now_ts())?
        .ok_or_else(|| AppError::Unauthorized("invalid or expired session".to_string()))
}

fn validate_credentials(username: &str, password: &str) -> AppResult<()> {
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

async fn hash_password_async(state: &AppState, password: String) -> AppResult<String> {
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

async fn verify_password_async(
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

fn random_token() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

fn auth_token(headers: &HeaderMap) -> AppResult<&str> {
    bearer_token(headers)
        .ok_or_else(|| AppError::Unauthorized("missing authorization token".to_string()))
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;
    value.strip_prefix("Bearer ")
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}
