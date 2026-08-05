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

use crate::{
    AppState, DirectLinkCache, auth,
    client_control::ClientControlConfig,
    config::Config,
    crypto_api::EncryptedRequest,
    error::{AppError, AppResult, safe_error_message},
    file_log::{SETTING_KEY as SYSTEM_LOG_SETTING_KEY, SystemLogConfig, normalize_config},
};

const CLIENT_CONTROL_SETTING_KEY: &str = "client_control";

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
    runtime_config: Config,
    client_control: Option<serde_json::Value>,
    system_log_config: Option<SystemLogConfig>,
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

pub async fn get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<Config>> {
    auth::require_auth(&state, &headers).await?;
    Ok(Json(redact_config_secrets(
        state.config.read().await.clone(),
    )))
}

pub async fn reveal_emby_api_key(
    State(state): State<AppState>,
    Path(server_id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
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
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let mut payload: Config = state.crypto_keys.decrypt_named(&request, "settings")?;
    let _config_update = state.config_updates.lock().await;
    let existing = state.config.read().await.clone();
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
    if let Err(err) = state.settings_store.record_audit(
        Some(admin_user_id),
        "settings.update",
        "保存服务器配置",
        "success",
    ) {
        tracing::error!(error = %err.safe_log_message(), "failed to record settings update audit");
    }
    Ok(Json(redact_config_secrets(payload)))
}

pub async fn restart_proxy_server(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<RestartProxyRequest>,
) -> AppResult<Json<Config>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let server_id = payload.server_id.trim();
    if server_id.is_empty() {
        return Err(AppError::Validation("server_id is required".to_string()));
    }
    if let Some(proxy_manager) = state.proxy_manager.as_ref() {
        proxy_manager
            .restart_server(state.clone(), server_id)
            .await?;
    }
    if let Err(err) = state.settings_store.record_audit(
        Some(admin_user_id),
        "settings.restart_proxy",
        &format!("重启反代服务器 {server_id}"),
        "success",
    ) {
        tracing::error!(error = %err.safe_log_message(), "failed to record proxy restart audit");
    }
    Ok(Json(redact_config_secrets(
        state.config.read().await.clone(),
    )))
}

pub async fn toggle_proxy_server(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ToggleProxyRequest>,
) -> AppResult<Json<Config>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
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

    if let Err(err) = state.settings_store.record_audit(
        Some(admin_user_id),
        "settings.toggle_proxy",
        &format!(
            "{}反代服务器 {}",
            if payload.enabled { "开启" } else { "关闭" },
            server_name
        ),
        "success",
    ) {
        tracing::error!(error = %err.safe_log_message(), "failed to record proxy toggle audit");
    }
    Ok(Json(redact_config_secrets(config)))
}

pub async fn validate_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<ValidationResponse>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let mut payload: Config = state.crypto_keys.decrypt_named(&request, "settings")?;
    let existing = state.config.read().await.clone();
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
    auth::require_auth(&state, &headers).await?;
    Ok(Json(state.file_log.config()))
}

pub async fn update_log_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<SystemLogConfig>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: SystemLogConfig = state.crypto_keys.decrypt_named(&request, "log_config")?;
    let config = state
        .file_log
        .update_config(&state.settings_store, payload)?;
    if let Err(err) = state.settings_store.record_audit(
        Some(admin_user_id),
        "log_config.update",
        "保存日志配置",
        "success",
    ) {
        tracing::error!(error = %err.safe_log_message(), "failed to record log config audit");
    }
    Ok(Json(config))
}

pub async fn export_backup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<BackupExportResponse>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: BackupExportRequest =
        state.crypto_keys.decrypt_named(&request, "backup_export")?;
    let password = payload.password.trim();
    if password.len() < 4 {
        return Err(AppError::Validation(
            "backup password must be at least 4 characters".to_string(),
        ));
    }
    let backup = BackupPayload {
        runtime_config: state.config.read().await.clone(),
        client_control: backup_client_control_config(&state)?,
        system_log_config: Some(state.file_log.config()),
    };
    let backup = encrypt_backup(password, &backup)?;
    state.settings_store.record_audit(
        Some(admin_user_id),
        "backup.export",
        "导出加密配置备份",
        "success",
    )?;
    Ok(Json(BackupExportResponse { backup }))
}

pub async fn import_backup(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<Config>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: BackupImportRequest = state.crypto_keys.decrypt_named(&request, "backup")?;
    let mut backup = parse_backup_text(payload.password.as_deref(), &payload.backup)?;
    backup
        .runtime_config
        .validate_for_storage()
        .map_err(|err| AppError::Validation(err.safe_log_message()))?;
    let _config_update = state.config_updates.lock().await;
    let existing = state.config.read().await.clone();
    let previous_client_control = state
        .settings_store
        .load_setting_json::<serde_json::Value>(CLIENT_CONTROL_SETTING_KEY)?;
    let previous_log_setting = state
        .settings_store
        .load_setting_json::<serde_json::Value>(SYSTEM_LOG_SETTING_KEY)?;
    let previous_log_config = state.file_log.config();
    let mut client_control_applied = false;
    let mut log_config_applied = false;

    if let Some(client_control) = backup.client_control.as_ref() {
        if let Err(err) = state
            .settings_store
            .save_setting_json(CLIENT_CONTROL_SETTING_KEY, client_control)
        {
            return Err(err);
        }
        client_control_applied = true;
    }
    if let Some(log_config) = backup.system_log_config.as_ref() {
        if let Err(err) = state
            .file_log
            .update_config(&state.settings_store, normalize_config(log_config.clone()))
        {
            rollback_backup_auxiliary_settings(
                &state,
                previous_client_control.as_ref(),
                previous_log_setting.as_ref(),
                &previous_log_config,
                client_control_applied,
                false,
            )?;
            return Err(err);
        }
        log_config_applied = true;
    }
    if let Err(err) = apply_runtime_config(&state, &existing, &backup.runtime_config).await {
        if let Err(rollback_error) = rollback_backup_auxiliary_settings(
            &state,
            previous_client_control.as_ref(),
            previous_log_setting.as_ref(),
            &previous_log_config,
            client_control_applied,
            log_config_applied,
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
    if let Err(err) = state.settings_store.record_audit(
        Some(admin_user_id),
        "backup.import",
        "还原配置文件",
        "success",
    ) {
        tracing::error!(error = %err.safe_log_message(), "failed to record backup import audit");
    }
    Ok(Json(redact_config_secrets(backup.runtime_config)))
}

fn rollback_backup_auxiliary_settings(
    state: &AppState,
    previous_client_control: Option<&serde_json::Value>,
    previous_log_setting: Option<&serde_json::Value>,
    previous_log_config: &SystemLogConfig,
    restore_client_control: bool,
    restore_log_config: bool,
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
    *state.cache.write().await = DirectLinkCache::new(
        config.cache_enabled,
        config.cache_ttl_seconds,
        config.cache_max_capacity,
    );
}

fn redact_config_secrets(mut config: Config) -> Config {
    config.emby_api_key.clear();
    for server in &mut config.servers {
        server.emby_api_key.clear();
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

fn backup_client_control_config(state: &AppState) -> AppResult<Option<serde_json::Value>> {
    let config = state
        .settings_store
        .load_setting_json::<ClientControlConfig>(CLIENT_CONTROL_SETTING_KEY)?;
    backup_client_control_value(config)
}

fn backup_client_control_value(
    config: Option<ClientControlConfig>,
) -> AppResult<Option<serde_json::Value>> {
    let Some(mut config) = config else {
        return Ok(None);
    };
    let now = now_seconds();
    config.records.retain(|record| record.enabled);
    config.rate_limit_blocks.retain(|record| {
        record.enabled && record.blocked_until.parse::<u64>().unwrap_or_default() > now
    });
    serde_json::to_value(config).map(Some).map_err(Into::into)
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn parse_backup_text(password: Option<&str>, value: &str) -> AppResult<BackupPayload> {
    match serde_json::from_str::<BackupPayload>(value) {
        Ok(payload) => Ok(payload),
        Err(plain_err) => {
            if let Some(password) = password.filter(|value| !value.trim().is_empty()) {
                decrypt_backup(password, value)
            } else {
                Err(AppError::Validation(format!(
                    "invalid backup config text: {plain_err}"
                )))
            }
        }
    }
}

fn encrypt_backup(password: &str, payload: &BackupPayload) -> AppResult<String> {
    let mut salt = [0_u8; 16];
    let mut nonce = [0_u8; 12];
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
        version: 1,
        kdf: "argon2id".to_string(),
        cipher: "aes-256-gcm".to_string(),
        salt: STANDARD.encode(salt),
        nonce: STANDARD.encode(nonce),
        data: STANDARD.encode(ciphertext),
    };
    serde_json::to_string_pretty(&envelope).map_err(Into::into)
}

fn decrypt_backup(password: &str, value: &str) -> AppResult<BackupPayload> {
    let envelope: BackupEnvelope = serde_json::from_str(value)?;
    if envelope.version != 1 || envelope.cipher != "aes-256-gcm" {
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
    let key = backup_key(password, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|err| AppError::Internal(format!("backup cipher error: {err}")))?;
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&nonce), data.as_ref())
        .map_err(|_| AppError::Validation("backup password is invalid".to_string()))?;
    serde_json::from_slice(&plaintext).map_err(Into::into)
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
    use crate::client_control::{ClientRuleRecord, ClientRuleSource, WebhookNotifyConfig};

    #[test]
    fn backup_round_trip_restores_payload() {
        let payload = BackupPayload {
            runtime_config: Config::default_runtime(),
            client_control: Some(serde_json::json!({ "enabled": true })),
            system_log_config: Some(SystemLogConfig::default()),
        };

        let backup = encrypt_backup("strong-password", &payload).unwrap();
        let restored = decrypt_backup("strong-password", &backup).unwrap();

        assert_eq!(
            restored.runtime_config.cache_ttl_seconds,
            payload.runtime_config.cache_ttl_seconds
        );
        assert_eq!(restored.client_control, payload.client_control);
        assert!(restored.system_log_config.is_some());
    }

    #[test]
    fn backup_rejects_wrong_password() {
        let payload = BackupPayload {
            runtime_config: Config::default_runtime(),
            client_control: None,
            system_log_config: None,
        };

        let backup = encrypt_backup("correct-password", &payload).unwrap();
        let err = decrypt_backup("wrong-password", &backup).unwrap_err();

        assert!(err.to_string().contains("backup password is invalid"));
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
            port: 18096,
            enabled: true,
            block_web_ui: false,
            real_ip_mode: "auto".to_string(),
            real_ip_header: String::new(),
            trusted_proxy_cidrs: String::new(),
            trusted_proxy_networks: Vec::new(),
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
    }

    #[test]
    fn backup_client_control_keeps_only_active_rules_and_blocks() {
        let mut config = ClientControlConfig {
            enabled: true,
            notify_enabled: false,
            playback_rate_limit_enabled: false,
            playback_rate_limit_window_seconds: 60,
            playback_rate_limit_max_requests: 20,
            playback_rate_limit_block_seconds: 1800,
            playback_rate_limit_action: "block_ip".to_string(),
            concurrent_playback_limit_enabled: false,
            concurrent_playback_limit_max: 3,
            rate_limit_blocks: Vec::new(),
            webhook: WebhookNotifyConfig::default(),
            webhooks: Vec::new(),
            records: Vec::new(),
        };
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
        let active_until = now_seconds() + 60;
        config
            .rate_limit_blocks
            .push(crate::client_control::PlaybackRateBlockRecord {
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
        config
            .rate_limit_blocks
            .push(crate::client_control::PlaybackRateBlockRecord {
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
        config
            .rate_limit_blocks
            .push(crate::client_control::PlaybackRateBlockRecord {
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

        let value = backup_client_control_value(Some(config)).unwrap().unwrap();
        let records = value
            .get("records")
            .and_then(|records| records.as_array())
            .unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].get("id").and_then(|id| id.as_str()),
            Some("blocked")
        );
        let blocks = value
            .get("rate_limit_blocks")
            .and_then(|blocks| blocks.as_array())
            .unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(
            blocks[0].get("id").and_then(|id| id.as_str()),
            Some("active-block")
        );
    }
}
