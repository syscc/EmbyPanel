use std::{
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
    let password_hash = hash_password(&payload.password)?;
    let admin_user_id = state
        .settings_store
        .create_admin(payload.username.trim(), &password_hash)?;
    state.settings_store.record_audit(
        Some(admin_user_id),
        "account.setup",
        "初始化管理员账户",
        "success",
    )?;
    create_session_response(&state, admin_user_id).await
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
    if let Err(err) = verify_password(&payload.password, &admin.password_hash) {
        record_login_failure(&state, &login_key).await;
        return Err(err);
    }
    clear_login_failures(&state, &login_key).await;
    create_session_response(&state, admin.admin_user_id).await
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
    match verify_password(&payload.current_password, &admin.password_hash) {
        Ok(()) => {}
        Err(AppError::Unauthorized(_)) => {
            return Err(AppError::Validation(
                "current password is incorrect".to_string(),
            ));
        }
        Err(err) => return Err(err),
    }
    let password_hash = hash_password(&payload.new_password)?;
    state
        .settings_store
        .update_admin_password(admin_user_id, &password_hash)?;
    state.settings_store.record_audit(
        Some(admin_user_id),
        "account.password",
        "修改管理员密码",
        "success",
    )?;
    Ok(Json(PasswordChangedResponse { changed: true }))
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
    let failures = attempts.entry(key.to_string()).or_default();
    while failures
        .front()
        .is_some_and(|timestamp| now.saturating_sub(*timestamp) > LOGIN_FAILURE_WINDOW_SECONDS)
    {
        failures.pop_front();
    }
    if failures.len() >= LOGIN_FAILURE_LIMIT {
        return Err(AppError::RateLimited(
            "too many login attempts, please try again later".to_string(),
        ));
    }
    Ok(())
}

async fn record_login_failure(state: &AppState, key: &str) {
    let now = now_ts();
    let mut attempts = state.login_failures.lock().await;
    let failures = attempts.entry(key.to_string()).or_default();
    while failures
        .front()
        .is_some_and(|timestamp| now.saturating_sub(*timestamp) > LOGIN_FAILURE_WINDOW_SECONDS)
    {
        failures.pop_front();
    }
    failures.push_back(now);
}

async fn clear_login_failures(state: &AppState, key: &str) {
    state.login_failures.lock().await.remove(key);
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

fn verify_password(password: &str, password_hash: &str) -> AppResult<()> {
    let parsed = PasswordHash::new(password_hash)
        .map_err(|err| AppError::Internal(format!("invalid password hash: {err}")))?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| AppError::Unauthorized("invalid username or password".to_string()))
}

async fn create_session_response(state: &AppState, admin_user_id: i64) -> AppResult<Response> {
    let token = random_token();
    let token_hash = hash_token(&token);
    state.settings_store.create_session(
        admin_user_id,
        &token_hash,
        now_ts() + SESSION_TTL_SECONDS,
    )?;
    Ok(Json(AuthResponse { token }).into_response())
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
