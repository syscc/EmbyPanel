use axum::{
    Json,
    extract::State,
    http::{HeaderMap, header},
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

use crate::{
    AppState, auth,
    config::Config,
    crypto_api::EncryptedRequest,
    emby,
    error::{AppError, AppResult},
};

const SETTING_KEY: &str = "client_control";
const PLAYBACK_RATE_RECENT_EVENT_TTL_SECONDS: u64 = 5;

pub struct PlaybackRateLimitInput<'a> {
    pub runtime_config: &'a Config,
    pub client: &'a reqwest::Client,
    pub server_id: &'a str,
    pub server_name: &'a str,
    pub playback_user: &'a str,
    pub playback_ip: &'a str,
    pub playback_event: &'a str,
    pub skip_recent_event: bool,
    pub record_recent_event: bool,
}

struct RateLimitNotification<'a> {
    server_id: &'a str,
    server_name: &'a str,
    action: &'a str,
    playback_user: &'a str,
    playback_ip: &'a str,
    window: u64,
    max_requests: u64,
    block_seconds: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClientControlConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub notify_enabled: bool,
    #[serde(default)]
    pub playback_rate_limit_enabled: bool,
    #[serde(default = "default_playback_rate_limit_window_seconds")]
    pub playback_rate_limit_window_seconds: u64,
    #[serde(default = "default_playback_rate_limit_max_requests")]
    pub playback_rate_limit_max_requests: u64,
    #[serde(default = "default_playback_rate_limit_block_seconds")]
    pub playback_rate_limit_block_seconds: u64,
    #[serde(default = "default_playback_rate_limit_action")]
    pub playback_rate_limit_action: String,
    #[serde(default)]
    pub rate_limit_blocks: Vec<PlaybackRateBlockRecord>,
    #[serde(default)]
    pub webhook: WebhookNotifyConfig,
    #[serde(default)]
    pub webhooks: Vec<WebhookNotifyConfig>,
    #[serde(default)]
    pub records: Vec<ClientRuleRecord>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WebhookNotifyConfig {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_webhook_name")]
    pub name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub secret: String,
}

impl Default for WebhookNotifyConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            enabled: false,
            name: default_webhook_name(),
            url: String::new(),
            secret: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PlaybackRateBlockRecord {
    pub id: String,
    pub server_id: String,
    pub server_name: String,
    pub action: String,
    pub ip: String,
    pub user_name: String,
    pub blocked_until: String,
    pub created_at: String,
    pub enabled: bool,
    pub note: String,
}

#[derive(Debug, Serialize)]
pub struct PlaybackRateWindowStatus {
    pub server_id: String,
    pub ip: String,
    pub current_count: u64,
    pub threshold: u64,
    pub remaining: u64,
    pub window_seconds: u64,
    pub reset_at: String,
    pub blocked: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClientRuleRecord {
    pub id: String,
    pub client_name: String,
    pub device_name: String,
    pub user_name: String,
    pub user_agent: String,
    pub source: ClientRuleSource,
    pub enabled: bool,
    pub created_at: String,
    pub updated_at: String,
    pub note: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientRuleSource {
    Auto,
    Manual,
}

#[derive(Debug, Deserialize)]
pub struct UpdateClientControlRequest {
    pub enabled: bool,
    pub notify_enabled: bool,
    #[serde(default)]
    pub playback_rate_limit_enabled: bool,
    #[serde(default)]
    pub playback_rate_limit_window_seconds: u64,
    #[serde(default)]
    pub playback_rate_limit_max_requests: u64,
    #[serde(default)]
    pub playback_rate_limit_block_seconds: u64,
    #[serde(default)]
    pub playback_rate_limit_action: String,
    #[serde(default)]
    pub webhook: WebhookNotifyConfig,
    #[serde(default)]
    pub webhooks: Vec<WebhookNotifyConfig>,
}

#[derive(Debug, Deserialize)]
pub struct AddUserAgentRuleRequest {
    pub user_agent: String,
    pub note: Option<String>,
}

pub async fn get_client_control(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<ClientControlConfig>> {
    auth::require_auth(&state, &headers).await?;
    Ok(Json(redact_webhook_secrets(load_or_default(&state)?)))
}

pub async fn update_client_control(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<ClientControlConfig>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: UpdateClientControlRequest = state
        .crypto_keys
        .decrypt_named(&request, "client_control")?;
    let mut config = load_or_default(&state)?;
    config.enabled = payload.enabled;
    config.notify_enabled = payload.notify_enabled;
    config.playback_rate_limit_enabled = payload.playback_rate_limit_enabled;
    if payload.playback_rate_limit_window_seconds > 0 {
        config.playback_rate_limit_window_seconds = payload.playback_rate_limit_window_seconds;
    }
    if payload.playback_rate_limit_max_requests > 0 {
        config.playback_rate_limit_max_requests = payload.playback_rate_limit_max_requests;
    }
    if payload.playback_rate_limit_block_seconds > 0 {
        config.playback_rate_limit_block_seconds = payload.playback_rate_limit_block_seconds;
    }
    config.playback_rate_limit_action =
        normalize_playback_rate_limit_action(&payload.playback_rate_limit_action);
    config.webhooks = merge_existing_webhook_secrets(
        normalize_webhook_configs(payload.webhooks, payload.webhook),
        &config.webhooks,
    );
    config.webhook = config
        .webhooks
        .first()
        .cloned()
        .unwrap_or_else(WebhookNotifyConfig::default);
    state
        .settings_store
        .save_setting_json(SETTING_KEY, &config)?;
    state.settings_store.record_audit(
        Some(admin_user_id),
        "client_control.update",
        "保存客户端管控和通知配置",
        "success",
    )?;
    Ok(Json(redact_webhook_secrets(config)))
}

pub async fn add_user_agent_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<ClientControlConfig>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: AddUserAgentRuleRequest =
        state.crypto_keys.decrypt_named(&request, "client_rule")?;
    let mut config = load_or_default(&state)?;
    let user_agent = payload.user_agent.trim().to_string();
    if user_agent.is_empty() {
        return Err(AppError::Validation(
            "user_agent cannot be empty".to_string(),
        ));
    }

    let now = chrono_like_now();
    if let Some(existing) = config
        .records
        .iter_mut()
        .find(|record| record.user_agent.eq_ignore_ascii_case(&user_agent))
    {
        existing.enabled = true;
        existing.updated_at = now.clone();
        if let Some(note) = payload.note {
            let note = note.trim();
            if !note.is_empty() {
                existing.note = note.to_string();
            }
        }
    } else {
        config.records.push(ClientRuleRecord {
            id: new_id(),
            client_name: normalize_client_name(&user_agent),
            device_name: "--".to_string(),
            user_name: "--".to_string(),
            user_agent,
            source: ClientRuleSource::Manual,
            enabled: true,
            created_at: now.clone(),
            updated_at: now,
            note: payload
                .note
                .unwrap_or_else(|| "手动添加 UA 拦截".to_string()),
        });
    }
    state
        .settings_store
        .save_setting_json(SETTING_KEY, &config)?;
    state.settings_store.record_audit(
        Some(admin_user_id),
        "client_rule.add",
        "添加 UA 拦截规则",
        "success",
    )?;
    Ok(Json(redact_webhook_secrets(config)))
}

pub async fn toggle_client_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<ClientControlConfig>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: ToggleClientRuleRequest =
        state.crypto_keys.decrypt_named(&request, "client_rule")?;
    let mut config = load_or_default(&state)?;
    if let Some(record) = config
        .records
        .iter_mut()
        .find(|record| record.id == payload.id)
    {
        record.enabled = payload.enabled;
        record.updated_at = chrono_like_now();
    } else {
        return Err(AppError::Validation("client rule not found".to_string()));
    }
    state
        .settings_store
        .save_setting_json(SETTING_KEY, &config)?;
    state.settings_store.record_audit(
        Some(admin_user_id),
        "client_rule.toggle",
        "切换 UA 拦截规则状态",
        "success",
    )?;
    Ok(Json(redact_webhook_secrets(config)))
}

pub async fn delete_client_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<ClientControlConfig>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: DeleteClientRuleRequest =
        state.crypto_keys.decrypt_named(&request, "client_rule")?;
    let mut config = load_or_default(&state)?;
    let before_len = config.records.len();
    config.records.retain(|record| record.id != payload.id);
    if config.records.len() == before_len {
        return Err(AppError::Validation("client rule not found".to_string()));
    }
    state
        .settings_store
        .save_setting_json(SETTING_KEY, &config)?;
    state.settings_store.record_audit(
        Some(admin_user_id),
        "client_rule.delete",
        "删除 UA 拦截规则",
        "success",
    )?;
    Ok(Json(redact_webhook_secrets(config)))
}

pub async fn unblock_rate_limit(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<ClientControlConfig>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: UnblockRateLimitRequest = state
        .crypto_keys
        .decrypt_named(&request, "rate_limit_block")?;
    let mut config = load_or_default(&state)?;
    let Some(index) = config
        .rate_limit_blocks
        .iter()
        .position(|record| record.id == payload.id)
    else {
        return Err(AppError::Validation(
            "rate limit block not found".to_string(),
        ));
    };

    let record = config.rate_limit_blocks.remove(index);
    let server_id = record.server_id.clone();
    let action = normalize_playback_rate_limit_action(&record.action);
    let ip = record.ip.clone();
    let user_name = record.user_name.clone();
    state.playback_rate_ip_bans.lock().await.remove(&ip);
    state.playback_rate_bans.lock().await.remove(&user_name);

    if action == "disable_user" && !user_name.trim().is_empty() && user_name != "--" {
        let runtime_config = state
            .config
            .read()
            .await
            .proxy_config_for_server(Some(&server_id));
        if let Some(user) =
            emby::find_user_by_name(&state.client, &runtime_config, &user_name).await?
        {
            emby::set_user_disabled(&state.client, &runtime_config, &user.id, false).await?;
        }
    }

    state
        .settings_store
        .save_setting_json(SETTING_KEY, &config)?;
    state.settings_store.record_audit(
        Some(admin_user_id),
        "rate_limit.unblock",
        "解除播放频率封禁",
        "success",
    )?;
    Ok(Json(redact_webhook_secrets(config)))
}

pub async fn rate_limit_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<PlaybackRateWindowStatus>>> {
    auth::require_auth(&state, &headers).await?;
    let config = load_or_default(&state)?;
    let now = now_seconds();
    let window = config.playback_rate_limit_window_seconds.max(1);
    let threshold = config.playback_rate_limit_max_requests.max(1);
    let blocks = config.rate_limit_blocks;
    let hits = state.playback_rate_hits.lock().await;
    let mut rows = Vec::new();
    for (key, timestamps) in hits.iter() {
        let Some((server_id, ip)) = key.split_once(':') else {
            continue;
        };
        let active = timestamps
            .iter()
            .copied()
            .filter(|timestamp| now.saturating_sub(*timestamp) < window)
            .collect::<Vec<_>>();
        if active.is_empty() {
            continue;
        }
        let oldest = active.first().copied().unwrap_or(now);
        let current_count = active.len() as u64;
        let reset_at = oldest + window;
        let blocked = blocks.iter().any(|record| {
            record.enabled
                && record.server_id == server_id
                && record.ip == ip
                && record.blocked_until.parse::<u64>().unwrap_or_default() > now
        });
        rows.push(PlaybackRateWindowStatus {
            server_id: server_id.to_string(),
            ip: ip.to_string(),
            current_count,
            threshold,
            remaining: threshold.saturating_sub(current_count),
            window_seconds: window,
            reset_at: reset_at.to_string(),
            blocked,
        });
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.current_count));
    Ok(Json(rows))
}

pub async fn test_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<WebhookTestResponse>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: WebhookTestRequest = state.crypto_keys.decrypt_named(&request, "webhook_test")?;
    let title = normalize_webhook_test_text(&payload.title, "EmbyPanel 通知测试");
    let text = normalize_webhook_test_text(&payload.text, "Webhook POST 测试成功");
    send_webhook(
        &state.client,
        payload.url.trim(),
        payload.secret.as_deref().map(str::trim),
        &title,
        &text,
    )
    .await?;
    state.settings_store.record_audit(
        Some(admin_user_id),
        "webhook.test",
        "测试 Webhook 通知",
        "success",
    )?;
    Ok(Json(WebhookTestResponse { ok: true }))
}

#[derive(Debug, Deserialize)]
pub struct ToggleClientRuleRequest {
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct DeleteClientRuleRequest {
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct UnblockRateLimitRequest {
    pub id: String,
}

#[derive(Debug, Serialize)]
pub struct WebhookTestResponse {
    pub ok: bool,
}

#[derive(Debug, Deserialize)]
pub struct WebhookTestRequest {
    pub url: String,
    pub secret: Option<String>,
    pub title: String,
    pub text: String,
}

pub fn record_client_event(
    state: &AppState,
    client_name: String,
    device_name: String,
    user_name: String,
    user_agent: String,
) -> AppResult<()> {
    let user_agent = user_agent.trim().to_string();
    if user_agent.is_empty() {
        return Ok(());
    }
    let client_name = normalize_value(&client_name);
    let device_name = normalize_value(&device_name);
    let user_name = normalize_value(&user_name);
    let mut config = load_or_default(state)?;
    if let Some(existing) = config
        .records
        .iter_mut()
        .find(|record| record.user_agent.eq_ignore_ascii_case(&user_agent))
    {
        let mut changed = false;
        if existing.client_name != client_name {
            existing.client_name = client_name;
            changed = true;
        }
        if existing.device_name != device_name {
            existing.device_name = device_name;
            changed = true;
        }
        if existing.user_name != user_name {
            existing.user_name = user_name;
            changed = true;
        }
        if changed {
            existing.updated_at = chrono_like_now();
        }
    } else {
        let now = chrono_like_now();
        config.records.push(ClientRuleRecord {
            id: new_id(),
            client_name,
            device_name,
            user_name,
            user_agent,
            source: ClientRuleSource::Auto,
            enabled: false,
            created_at: now.clone(),
            updated_at: now,
            note: "自动记录播放设备".to_string(),
        });
    }
    state
        .settings_store
        .save_setting_json(SETTING_KEY, &config)?;
    Ok(())
}

fn load_or_default(state: &AppState) -> AppResult<ClientControlConfig> {
    let mut config = state
        .settings_store
        .load_setting_json(SETTING_KEY)?
        .unwrap_or_else(default_config);
    migrate_webhook_config(&mut config);
    if prune_inactive_rate_limit_blocks(&mut config, now_seconds()) {
        state
            .settings_store
            .save_setting_json(SETTING_KEY, &config)?;
    }
    Ok(config)
}

fn default_config() -> ClientControlConfig {
    ClientControlConfig {
        enabled: false,
        notify_enabled: false,
        playback_rate_limit_enabled: false,
        playback_rate_limit_window_seconds: default_playback_rate_limit_window_seconds(),
        playback_rate_limit_max_requests: default_playback_rate_limit_max_requests(),
        playback_rate_limit_block_seconds: default_playback_rate_limit_block_seconds(),
        playback_rate_limit_action: default_playback_rate_limit_action(),
        rate_limit_blocks: Vec::new(),
        webhook: WebhookNotifyConfig::default(),
        webhooks: Vec::new(),
        records: Vec::new(),
    }
}

fn default_playback_rate_limit_window_seconds() -> u64 {
    60
}

fn default_playback_rate_limit_max_requests() -> u64 {
    20
}

fn default_playback_rate_limit_block_seconds() -> u64 {
    1800
}

fn default_playback_rate_limit_action() -> String {
    "block_ip".to_string()
}

fn default_webhook_name() -> String {
    "新建 Webhook".to_string()
}

fn normalize_webhook_config(mut webhook: WebhookNotifyConfig) -> WebhookNotifyConfig {
    webhook.id = webhook.id.trim().to_string();
    if webhook.id.is_empty() {
        webhook.id = new_id();
    }
    webhook.name = webhook.name.trim().to_string();
    if webhook.name.is_empty() {
        webhook.name = default_webhook_name();
    }
    webhook.url = webhook.url.trim().to_string();
    webhook.secret = webhook.secret.trim().to_string();
    webhook
}

fn normalize_webhook_configs(
    webhooks: Vec<WebhookNotifyConfig>,
    legacy_webhook: WebhookNotifyConfig,
) -> Vec<WebhookNotifyConfig> {
    let mut normalized = webhooks
        .into_iter()
        .map(normalize_webhook_config)
        .filter(|webhook| {
            webhook.enabled || !webhook.url.is_empty() || webhook.name != default_webhook_name()
        })
        .collect::<Vec<_>>();
    if normalized.is_empty() && (legacy_webhook.enabled || !legacy_webhook.url.trim().is_empty()) {
        normalized.push(normalize_webhook_config(legacy_webhook));
    }
    if normalized.is_empty() {
        normalized.push(WebhookNotifyConfig::default());
    }
    normalized
}

fn migrate_webhook_config(config: &mut ClientControlConfig) {
    config.webhooks = normalize_webhook_configs(config.webhooks.clone(), config.webhook.clone());
    config.webhook = config
        .webhooks
        .first()
        .cloned()
        .unwrap_or_else(WebhookNotifyConfig::default);
}

fn merge_existing_webhook_secrets(
    mut webhooks: Vec<WebhookNotifyConfig>,
    existing: &[WebhookNotifyConfig],
) -> Vec<WebhookNotifyConfig> {
    for webhook in &mut webhooks {
        if webhook.secret.trim().is_empty()
            && let Some(existing_webhook) = existing.iter().find(|item| item.id == webhook.id)
        {
            webhook.secret = existing_webhook.secret.clone();
        }
    }
    webhooks
}

fn redact_webhook_secrets(mut config: ClientControlConfig) -> ClientControlConfig {
    for webhook in &mut config.webhooks {
        webhook.secret.clear();
    }
    config.webhook.secret.clear();
    config
}

fn normalize_webhook_test_text(value: &str, fallback: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

async fn send_webhook(
    client: &reqwest::Client,
    url: &str,
    secret: Option<&str>,
    title: &str,
    text: &str,
) -> AppResult<()> {
    let url = url.trim();
    if url.is_empty() {
        return Err(AppError::Validation(
            "webhook url cannot be empty".to_string(),
        ));
    }
    let parsed_url = Url::parse(url)
        .map_err(|err| AppError::Validation(format!("invalid webhook url: {err}")))?;
    if !matches!(parsed_url.scheme(), "http" | "https") {
        return Err(AppError::Validation(
            "webhook url must use http or https".to_string(),
        ));
    }

    let mut request = client
        .post(parsed_url)
        .header(header::CONTENT_TYPE, "application/json")
        .json(&json!({
            "title": title,
            "text": text,
        }));
    if let Some(secret) = secret.map(str::trim).filter(|value| !value.is_empty()) {
        request = request.header("X-Webhook-Secret", secret);
    }
    tokio::time::timeout(std::time::Duration::from_secs(5), request.send())
        .await
        .map_err(|_| AppError::BadGateway("webhook request timed out".to_string()))??
        .error_for_status()?;
    Ok(())
}

fn rate_limit_webhook_payload(
    server_name: &str,
    action: &str,
    playback_user: &str,
    playback_ip: &str,
    window: u64,
    max_requests: u64,
    block_seconds: u64,
) -> (String, String) {
    let action_label = if action == "disable_user" {
        "禁用用户"
    } else {
        "屏蔽 IP"
    };
    let title = format!("播放频率限制 - {action_label}");
    let text = format!(
        "服务器：{server_name}\n用户：{}\nIP：{}\n策略：{action_label}\n窗口：{window}s\n阈值：{max_requests} 次\n处理时长：{block_seconds}s",
        normalize_value(playback_user),
        normalize_value(playback_ip),
    );
    (title, text)
}

async fn notify_rate_limit_block(
    state: &AppState,
    webhooks: Vec<WebhookNotifyConfig>,
    notification: RateLimitNotification<'_>,
) {
    let (title, text) = rate_limit_webhook_payload(
        notification.server_name,
        notification.action,
        notification.playback_user,
        notification.playback_ip,
        notification.window,
        notification.max_requests,
        notification.block_seconds,
    );
    for webhook in active_webhooks(webhooks) {
        if let Err(err) = send_webhook(
            &state.client,
            &webhook.url,
            Some(webhook.secret.as_str()),
            &title,
            &text,
        )
        .await
        {
            state.activity_log.record(
                crate::activity_log::ActivityKind::General,
                crate::activity_log::ActivityLevel::Warn,
                Some(notification.server_id),
                "Webhook 通知",
                "发送失败",
                format!("{} 发送失败: {err}", webhook.name),
            );
        }
    }
}

fn active_webhooks(webhooks: Vec<WebhookNotifyConfig>) -> Vec<WebhookNotifyConfig> {
    webhooks
        .into_iter()
        .map(normalize_webhook_config)
        .filter(|webhook| webhook.enabled && !webhook.url.is_empty())
        .collect()
}

fn normalize_playback_rate_limit_action(action: &str) -> String {
    match action.trim().to_ascii_lowercase().as_str() {
        "disable_user" | "user" | "disable" => "disable_user".to_string(),
        _ => "block_ip".to_string(),
    }
}

async fn disable_emby_user_by_name(
    client: &reqwest::Client,
    config: &Config,
    user_name: &str,
) -> AppResult<()> {
    let user_name = user_name.trim();
    if user_name.is_empty() || user_name == "--" {
        return Ok(());
    }
    let Some(user) = emby::find_user_by_name(client, config, user_name).await? else {
        return Err(AppError::Validation(format!(
            "Emby user `{user_name}` not found"
        )));
    };
    emby::set_user_disabled(client, config, &user.id, true).await?;
    Ok(())
}

fn prune_inactive_rate_limit_blocks(config: &mut ClientControlConfig, now: u64) -> bool {
    let before_len = config.rate_limit_blocks.len();
    config.rate_limit_blocks.retain(|record| {
        record.enabled && record.blocked_until.parse::<u64>().unwrap_or_default() > now
    });
    config.rate_limit_blocks.len() != before_len
}

fn upsert_rate_limit_block(
    config: &mut ClientControlConfig,
    server_id: &str,
    server_name: &str,
    action: &str,
    ip: &str,
    user_name: &str,
    blocked_until: u64,
) {
    let now = chrono_like_now();
    if let Some(existing) = config.rate_limit_blocks.iter_mut().find(|record| {
        record.enabled
            && record.server_id == server_id
            && record.action == action
            && if action == "disable_user" {
                record.user_name == user_name
            } else {
                record.ip == ip
            }
    }) {
        existing.blocked_until = blocked_until.to_string();
        existing.note = rate_limit_block_note(action, ip, user_name);
        return;
    }
    config.rate_limit_blocks.push(PlaybackRateBlockRecord {
        id: new_id(),
        server_id: server_id.to_string(),
        server_name: server_name.to_string(),
        action: action.to_string(),
        ip: ip.to_string(),
        user_name: user_name.to_string(),
        blocked_until: blocked_until.to_string(),
        created_at: now,
        enabled: true,
        note: rate_limit_block_note(action, ip, user_name),
    });
}

fn rate_limit_block_note(action: &str, ip: &str, user_name: &str) -> String {
    if action == "disable_user" {
        format!("频率超限禁用用户 {user_name}")
    } else {
        format!("频率超限屏蔽 IP {ip}")
    }
}

fn new_id() -> String {
    let mut bytes = [0_u8; 4];
    rand::rng().fill_bytes(&mut bytes);
    format!(
        "client-{}-{:02x}{:02x}{:02x}{:02x}",
        chrono_like_now(),
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3]
    )
}

fn chrono_like_now() -> String {
    now_seconds().to_string()
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn normalize_client_name(user_agent: &str) -> String {
    let ua = user_agent.to_ascii_lowercase();
    if ua.contains("android tv") || ua.contains("shield") || ua.contains("kodi") {
        "Android TV".to_string()
    } else if ua.contains("iphone") || ua.contains("ipad") || ua.contains("ios") {
        "iOS".to_string()
    } else if ua.contains("apple tv") || ua.contains("tvos") {
        "Apple TV".to_string()
    } else if ua.contains("mac") || ua.contains("windows") || ua.contains("linux") {
        "桌面端".to_string()
    } else {
        "其它".to_string()
    }
}

pub fn record_from_headers(
    state: &AppState,
    headers: &HeaderMap,
    user_name: Option<&str>,
) -> AppResult<()> {
    let user_agent = extract_user_agent(headers);
    if user_agent.is_empty() {
        return Ok(());
    }
    let client_name = extract_client_name(headers, &user_agent);
    let device_name = extract_header(headers, "X-Emby-Device-Name")
        .or_else(|| extract_header(headers, "X-Emby-Device-Id"))
        .unwrap_or_else(|| "--".to_string());
    record_client_event(
        state,
        client_name,
        device_name,
        user_name.unwrap_or("--").to_string(),
        user_agent,
    )
}

pub fn matched_block_rule(
    state: &AppState,
    headers: &HeaderMap,
) -> AppResult<Option<ClientRuleRecord>> {
    let config = load_or_default(state)?;
    if !config.enabled {
        return Ok(None);
    }

    let user_agent = extract_user_agent(headers);
    if user_agent.is_empty() {
        return Ok(None);
    }

    Ok(config
        .records
        .iter()
        .find(|record| record.enabled && rule_matches(record, &user_agent))
        .cloned())
}

pub async fn notify_client_rule_hit(
    state: &AppState,
    record: &ClientRuleRecord,
    server_id: &str,
    server_name: &str,
    playback_ip: &str,
    method: &str,
    path: &str,
) {
    let Ok(config) = load_or_default(state) else {
        return;
    };
    if !config.notify_enabled {
        return;
    }

    let title = format!("{server_name} 客户端命中拦截");
    let text = format!(
        "服务器：{server_name}\n用户：{}\nIP：{}\n客户端：{}\n设备：{}\n规则：{}\n请求：{method} {path}",
        normalize_value(&record.user_name),
        normalize_value(playback_ip),
        normalize_value(&record.client_name),
        normalize_value(&record.device_name),
        normalize_value(&record.user_agent),
    );
    for webhook in active_webhooks(config.webhooks) {
        if let Err(err) = send_webhook(
            &state.client,
            &webhook.url,
            Some(webhook.secret.as_str()),
            &title,
            &text,
        )
        .await
        {
            state.activity_log.record(
                crate::activity_log::ActivityKind::General,
                crate::activity_log::ActivityLevel::Warn,
                Some(server_id),
                "Webhook 通知",
                "客户端命中通知发送失败",
                format!("{} 发送失败: {err}", webhook.name),
            );
        }
    }
}

pub async fn enforce_playback_rate_limit(
    state: &AppState,
    input: PlaybackRateLimitInput<'_>,
) -> AppResult<bool> {
    let mut config = load_or_default(state)?;
    if !config.playback_rate_limit_enabled {
        return Ok(false);
    }

    let action = normalize_playback_rate_limit_action(&config.playback_rate_limit_action);
    let playback_user = input.playback_user.trim();
    let playback_ip = input.playback_ip.trim();
    if playback_ip.is_empty() || playback_ip == "--" {
        return Ok(false);
    }
    if action == "disable_user" && (playback_user.is_empty() || playback_user == "--") {
        return Ok(false);
    }

    let now = now_seconds();
    let mut changed = prune_inactive_rate_limit_blocks(&mut config, now);
    if config.rate_limit_blocks.iter().any(|record| {
        record.enabled
            && record.server_id == input.server_id
            && record.blocked_until.parse::<u64>().unwrap_or_default() > now
            && if action == "disable_user" {
                record.user_name == playback_user
            } else {
                record.ip == playback_ip
            }
    }) {
        if changed {
            state
                .settings_store
                .save_setting_json(SETTING_KEY, &config)?;
        }
        return Ok(true);
    }
    {
        let mut bans = state.playback_rate_bans.lock().await;
        if let Some(blocked_until) = bans.get(playback_user).copied() {
            if blocked_until > now {
                return Ok(true);
            }
            bans.remove(playback_user);
        }
    }
    {
        let mut ip_bans = state.playback_rate_ip_bans.lock().await;
        if let Some(blocked_until) = ip_bans.get(playback_ip).copied() {
            if blocked_until > now {
                return Ok(true);
            }
            ip_bans.remove(playback_ip);
        }
    }

    let window = config.playback_rate_limit_window_seconds.max(1);
    let max_requests = config.playback_rate_limit_max_requests.max(1);
    let event_key = input.playback_event.trim();
    if !event_key.is_empty() {
        let recent_key = format!("{}:{playback_ip}:{event_key}", input.server_id);
        let mut recent_events = state.playback_rate_recent_events.lock().await;
        recent_events.retain(|_, timestamp| {
            now.saturating_sub(*timestamp) < PLAYBACK_RATE_RECENT_EVENT_TTL_SECONDS
        });
        let has_recent_event = recent_events.get(&recent_key).is_some_and(|timestamp| {
            now.saturating_sub(*timestamp) < PLAYBACK_RATE_RECENT_EVENT_TTL_SECONDS
        });
        if input.skip_recent_event && has_recent_event {
            return Ok(false);
        }
        if input.record_recent_event {
            recent_events.insert(recent_key, now);
        }
    } else if input.skip_recent_event || input.record_recent_event {
        let recent_key = format!("{}:{playback_ip}:playback-start", input.server_id);
        let mut recent_events = state.playback_rate_recent_events.lock().await;
        recent_events.retain(|_, timestamp| {
            now.saturating_sub(*timestamp) < PLAYBACK_RATE_RECENT_EVENT_TTL_SECONDS
        });
        let has_recent_event = recent_events.get(&recent_key).is_some_and(|timestamp| {
            now.saturating_sub(*timestamp) < PLAYBACK_RATE_RECENT_EVENT_TTL_SECONDS
        });
        if input.skip_recent_event && has_recent_event {
            return Ok(false);
        }
        if input.record_recent_event {
            recent_events.insert(recent_key, now);
        }
    }

    let key = format!("{}:{}", input.server_id, playback_ip);
    let should_block = {
        let mut hits = state.playback_rate_hits.lock().await;
        let timestamps = hits.entry(key).or_default();
        while timestamps
            .front()
            .is_some_and(|timestamp| now.saturating_sub(*timestamp) >= window)
        {
            timestamps.pop_front();
        }
        timestamps.push_back(now);
        timestamps.len() as u64 > max_requests
    };

    if should_block {
        let block_seconds = config.playback_rate_limit_block_seconds.max(1);
        let blocked_until = now + block_seconds;
        let notify_enabled = config.notify_enabled;
        let webhooks = config.webhooks.clone();
        if action == "disable_user" {
            if let Err(err) =
                disable_emby_user_by_name(input.client, input.runtime_config, playback_user).await
            {
                state.activity_log.record(
                    crate::activity_log::ActivityKind::General,
                    crate::activity_log::ActivityLevel::Warn,
                    Some(input.server_id),
                    "播放频率限制",
                    "禁用用户失败",
                    format!("用户 {playback_user} 调用 Emby API 失败: {err}"),
                );
            } else {
                state.activity_log.record(
                    crate::activity_log::ActivityKind::General,
                    crate::activity_log::ActivityLevel::Warn,
                    Some(input.server_id),
                    "播放频率限制",
                    "禁用用户",
                    format!(
                        "用户 {playback_user} 在 IP {playback_ip} 的 {window}s 窗口内超过 {max_requests} 次播放请求，调用 Emby API 禁用用户"
                    ),
                );
            }
            state
                .playback_rate_bans
                .lock()
                .await
                .insert(playback_user.to_string(), blocked_until);
        } else {
            state
                .playback_rate_ip_bans
                .lock()
                .await
                .insert(playback_ip.to_string(), blocked_until);
            state.activity_log.record(
                crate::activity_log::ActivityKind::General,
                crate::activity_log::ActivityLevel::Warn,
                Some(input.server_id),
                "播放频率限制",
                "屏蔽 IP",
                format!(
                    "IP {playback_ip} 在 {window}s 窗口内超过 {max_requests} 次播放请求，屏蔽 {block_seconds}s"
                ),
            );
        }
        upsert_rate_limit_block(
            &mut config,
            input.server_id,
            input.server_name,
            &action,
            playback_ip,
            playback_user,
            blocked_until,
        );
        changed = true;

        if notify_enabled {
            notify_rate_limit_block(
                state,
                webhooks,
                RateLimitNotification {
                    server_id: input.server_id,
                    server_name: input.server_name,
                    action: &action,
                    playback_user,
                    playback_ip,
                    window,
                    max_requests,
                    block_seconds,
                },
            )
            .await;
        }
    }

    if changed {
        state
            .settings_store
            .save_setting_json(SETTING_KEY, &config)?;
    }

    Ok(should_block)
}

pub fn extract_user_agent(headers: &HeaderMap) -> String {
    headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .trim()
        .to_string()
}

pub fn extract_client_name(headers: &HeaderMap, user_agent: &str) -> String {
    extract_header(headers, "X-Emby-Client").unwrap_or_else(|| normalize_client_name(user_agent))
}

fn extract_header(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn normalize_value(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        "--".to_string()
    } else {
        value.to_string()
    }
}

fn rule_matches(record: &ClientRuleRecord, user_agent: &str) -> bool {
    let rule_ua = record.user_agent.trim().to_ascii_lowercase();
    if rule_ua.is_empty() {
        return false;
    }
    let request_ua = user_agent.trim().to_ascii_lowercase();
    !request_ua.is_empty() && request_ua.contains(&rule_ua)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(user_agent: &str, client_name: &str) -> ClientRuleRecord {
        ClientRuleRecord {
            id: "rule-1".to_string(),
            client_name: client_name.to_string(),
            device_name: "--".to_string(),
            user_name: "--".to_string(),
            user_agent: user_agent.to_string(),
            source: ClientRuleSource::Manual,
            enabled: true,
            created_at: "0".to_string(),
            updated_at: "0".to_string(),
            note: String::new(),
        }
    }

    #[test]
    fn ua_rule_matches_case_insensitive_keyword() {
        let record = rule("infuse-library", "Infuse-Direct");

        assert!(rule_matches(&record, "Mozilla/5.0 Infuse-Library/8.0"));
    }

    #[test]
    fn ua_rule_does_not_match_client_name_only() {
        let record = rule("infuse-library", "Infuse-Direct");

        assert!(!rule_matches(&record, "Infuse-Direct/8.0"));
    }
}
