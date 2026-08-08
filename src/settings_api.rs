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
    client_control::{self, ClientControlConfig},
    config::Config,
    crypto_api::EncryptedRequest,
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
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
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
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
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

fn parse_backup_text(password: Option<&str>, value: &str) -> AppResult<BackupPayload> {
    let value = value.trim();
    validate_backup_text_size(value)?;
    let root: serde_json::Value = serde_json::from_str(value)
        .map_err(|err| AppError::Validation(format!("invalid backup config text: {err}")))?;
    if root.get("cipher").is_some() {
        let password = password
            .map(str::trim)
            .filter(|password| !password.is_empty())
            .ok_or_else(|| AppError::Validation("backup password is required".to_string()))?;
        decrypt_backup(password, value)
    } else {
        serde_json::from_value(root)
            .map_err(|err| AppError::Validation(format!("invalid backup config: {err}")))
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
        .map_err(|err| AppError::Validation(format!("invalid backup payload: {err}")))
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
