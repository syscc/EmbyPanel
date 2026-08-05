use axum::{
    Json,
    extract::State,
    http::{HeaderMap, header},
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use url::Url;

use crate::{
    AppState, auth,
    config::Config,
    crypto_api::EncryptedRequest,
    emby,
    error::{AppError, AppResult, safe_error_message},
    ip_location::IpLocation,
};

const SETTING_KEY: &str = "client_control";
const PLAYBACK_RATE_RECENT_EVENT_TTL_SECONDS: u64 = 5;
const PLAYBACK_RATE_STATE_CAPACITY: usize = 4096;
const PLAYBACK_RATE_MAX_WINDOW_SECONDS: u64 = 24 * 60 * 60;
const PLAYBACK_RATE_MAX_REQUESTS: u64 = 1000;
const PLAYBACK_RATE_MAX_BLOCK_SECONDS: u64 = 31 * 24 * 60 * 60;
const CONCURRENT_PLAYBACK_MAX: u64 = 100;

#[derive(Debug, Clone)]
pub struct PlaybackRateHit {
    pub timestamp: u64,
    pub user_name: String,
}

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

pub struct ConcurrentPlaybackLimitInput<'a> {
    pub runtime_config: &'a Config,
    pub server_id: &'a str,
    pub server_name: &'a str,
    pub playback_user: &'a str,
    pub playback_ip: &'a str,
    pub item_id: &'a str,
}

struct RateLimitNotification<'a> {
    server_id: &'a str,
    server_name: &'a str,
    action: &'a str,
    playback_user: &'a str,
    playback_ip: &'a str,
    ip_location_text: &'a str,
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
    pub concurrent_playback_limit_enabled: bool,
    #[serde(default = "default_concurrent_playback_limit_max")]
    pub concurrent_playback_limit_max: u64,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_location: Option<IpLocation>,
}

#[derive(Debug, Serialize)]
pub struct PlaybackRateWindowStatus {
    pub server_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_action: Option<String>,
    pub user_name: String,
    pub ip: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_location: Option<IpLocation>,
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
    pub concurrent_playback_limit_enabled: bool,
    #[serde(default)]
    pub concurrent_playback_limit_max: u64,
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
    Ok(Json(
        enrich_client_control_response(&state, load_or_default(&state)?).await,
    ))
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
        config.playback_rate_limit_window_seconds = payload
            .playback_rate_limit_window_seconds
            .min(PLAYBACK_RATE_MAX_WINDOW_SECONDS);
    }
    if payload.playback_rate_limit_max_requests > 0 {
        config.playback_rate_limit_max_requests = payload
            .playback_rate_limit_max_requests
            .min(PLAYBACK_RATE_MAX_REQUESTS);
    }
    if payload.playback_rate_limit_block_seconds > 0 {
        config.playback_rate_limit_block_seconds = payload
            .playback_rate_limit_block_seconds
            .min(PLAYBACK_RATE_MAX_BLOCK_SECONDS);
    }
    config.playback_rate_limit_action =
        normalize_playback_rate_limit_action(&payload.playback_rate_limit_action);
    config.concurrent_playback_limit_enabled = payload.concurrent_playback_limit_enabled;
    if payload.concurrent_playback_limit_max > 0 {
        config.concurrent_playback_limit_max = payload
            .concurrent_playback_limit_max
            .min(CONCURRENT_PLAYBACK_MAX);
    }
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
    Ok(Json(enrich_client_control_response(&state, config).await))
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
    let user_agent = normalize_manual_user_agent_rule(&payload.user_agent);
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
    Ok(Json(enrich_client_control_response(&state, config).await))
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
    Ok(Json(enrich_client_control_response(&state, config).await))
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
    Ok(Json(enrich_client_control_response(&state, config).await))
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
    let server_name = record.server_name.clone();
    let action = normalize_playback_rate_limit_action(&record.action);
    let ip = record.ip.clone();
    let user_name = record.user_name.clone();
    let blocked_until = record.blocked_until.clone();
    state.playback_rate_ip_bans.lock().await.remove(&ip);
    state.playback_rate_bans.lock().await.remove(&user_name);

    if action_disables_user(&action) && !user_name.trim().is_empty() && user_name != "--" {
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
        &format!(
            "服务器: {server_name}({server_id}); 类型: {}; 用户: {}; IP: {}; 原到期: {}",
            rate_limit_action_label(&action),
            empty_audit_value(&user_name),
            empty_audit_value(&ip),
            blocked_until
        ),
    )?;
    let ip_location_text = state
        .ip_location
        .lookup(&ip)
        .await
        .map(|location| location.display_text())
        .unwrap_or_default();
    state.block_log.record(crate::block_log::BlockLogInsert {
        event_type: "unblock",
        timestamp_ms: now_millis(),
        server_id: &server_id,
        server_name: &server_name,
        port: 0,
        method: "ACTION",
        path: "rate_limit/unblock",
        path_type: "rate_limit_action",
        status_code: 200,
        outcome: "解除封禁",
        duration_ms: 0,
        playback_user: &user_name,
        playback_ip: &ip,
        ip_location_text: &ip_location_text,
        cache_hit: false,
        detail: &format!(
            "方式: {}; 原到期: {}",
            rate_limit_action_label(&action),
            blocked_until
        ),
    });
    Ok(Json(enrich_client_control_response(&state, config).await))
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
    let mut rows = {
        let hits = state.playback_rate_hits.lock().await;
        let mut rows = Vec::new();
        for (key, timestamps) in hits.iter() {
            let Some((server_id, ip)) = key.split_once(':') else {
                continue;
            };
            let active = timestamps
                .iter()
                .filter(|hit| now.saturating_sub(hit.timestamp) < window)
                .collect::<Vec<_>>();
            if active.is_empty() {
                continue;
            }
            let oldest = active.first().map(|hit| hit.timestamp).unwrap_or(now);
            let user_name = active
                .iter()
                .rev()
                .find_map(|hit| {
                    let user_name = hit.user_name.trim();
                    (!user_name.is_empty() && user_name != "--").then(|| user_name.to_string())
                })
                .or_else(|| {
                    blocks.iter().find_map(|record| {
                        let blocked_until = record.blocked_until.parse::<u64>().unwrap_or_default();
                        let user_name = record.user_name.trim();
                        (record.enabled
                            && record.server_id == server_id
                            && record.ip == ip
                            && blocked_until > now
                            && !user_name.is_empty()
                            && user_name != "--")
                            .then(|| user_name.to_string())
                    })
                })
                .unwrap_or_else(|| "--".to_string());
            let current_count = active.len() as u64;
            let block_record = blocks.iter().find(|record| {
                record.enabled
                    && record.server_id == server_id
                    && record.ip == ip
                    && record.blocked_until.parse::<u64>().unwrap_or_default() > now
            });
            rows.push(PlaybackRateWindowStatus {
                server_id: server_id.to_string(),
                block_id: block_record.map(|record| record.id.clone()),
                block_action: block_record.map(|record| record.action.clone()),
                user_name,
                ip: ip.to_string(),
                ip_location: None,
                current_count,
                threshold,
                remaining: threshold.saturating_sub(current_count),
                window_seconds: window,
                reset_at: oldest.saturating_add(window).to_string(),
                blocked: block_record.is_some(),
            });
        }
        rows
    };
    let mut row_keys = HashSet::new();
    for row in &mut rows {
        row.ip_location = state.ip_location.lookup(&row.ip).await;
        row_keys.insert((row.server_id.clone(), row.ip.clone()));
    }
    for record in blocks.iter() {
        let blocked_until = record.blocked_until.parse::<u64>().unwrap_or_default();
        if !record.enabled || blocked_until <= now || record.ip.trim().is_empty() {
            continue;
        }
        if !row_keys.insert((record.server_id.clone(), record.ip.clone())) {
            continue;
        }
        rows.push(PlaybackRateWindowStatus {
            server_id: record.server_id.clone(),
            block_id: Some(record.id.clone()),
            block_action: Some(record.action.clone()),
            user_name: normalize_value(&record.user_name),
            ip: normalize_value(&record.ip),
            ip_location: state.ip_location.lookup(&record.ip).await,
            current_count: 0,
            threshold,
            remaining: threshold,
            window_seconds: window,
            reset_at: blocked_until.to_string(),
            blocked: true,
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
    let user_agent = normalize_auto_user_agent_rule(&user_agent);
    if user_agent.is_empty() {
        return Ok(());
    }
    let client_name = normalize_value(&client_name);
    let device_name = normalize_value(&device_name);
    let user_name = normalize_value(&user_name);
    let mut config = load_or_default(state)?;
    let changed = if let Some(existing) = config
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
        changed
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
        true
    };
    if changed {
        state
            .settings_store
            .save_setting_json(SETTING_KEY, &config)?;
    }
    Ok(())
}

fn load_or_default(state: &AppState) -> AppResult<ClientControlConfig> {
    let revision = state.settings_store.settings_revision();
    if let Some(mut config) = state
        .client_control_cache
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .filter(|(cached_revision, _)| *cached_revision == revision)
        .map(|(_, config)| config.clone())
    {
        if prune_inactive_rate_limit_blocks(&mut config, now_seconds()) {
            state
                .settings_store
                .save_setting_json(SETTING_KEY, &config)?;
            let revision = state.settings_store.settings_revision();
            *state
                .client_control_cache
                .write()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some((revision, config.clone()));
        }
        return Ok(config);
    }

    let mut config = state
        .settings_store
        .load_setting_json(SETTING_KEY)?
        .unwrap_or_else(default_config);
    migrate_webhook_config(&mut config);
    let mut changed = normalize_client_rule_records(&mut config);
    changed |= normalize_client_control_limits(&mut config);
    changed |= prune_inactive_rate_limit_blocks(&mut config, now_seconds());
    if changed {
        state
            .settings_store
            .save_setting_json(SETTING_KEY, &config)?;
    }
    let revision = state.settings_store.settings_revision();
    *state
        .client_control_cache
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((revision, config.clone()));
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
        concurrent_playback_limit_enabled: false,
        concurrent_playback_limit_max: default_concurrent_playback_limit_max(),
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

fn default_concurrent_playback_limit_max() -> u64 {
    3
}

fn normalize_client_control_limits(config: &mut ClientControlConfig) -> bool {
    let previous = (
        config.playback_rate_limit_window_seconds,
        config.playback_rate_limit_max_requests,
        config.playback_rate_limit_block_seconds,
        config.concurrent_playback_limit_max,
    );
    config.playback_rate_limit_window_seconds = config
        .playback_rate_limit_window_seconds
        .clamp(1, PLAYBACK_RATE_MAX_WINDOW_SECONDS);
    config.playback_rate_limit_max_requests = config
        .playback_rate_limit_max_requests
        .clamp(1, PLAYBACK_RATE_MAX_REQUESTS);
    config.playback_rate_limit_block_seconds = config
        .playback_rate_limit_block_seconds
        .clamp(1, PLAYBACK_RATE_MAX_BLOCK_SECONDS);
    config.concurrent_playback_limit_max = config
        .concurrent_playback_limit_max
        .clamp(1, CONCURRENT_PLAYBACK_MAX);
    previous
        != (
            config.playback_rate_limit_window_seconds,
            config.playback_rate_limit_max_requests,
            config.playback_rate_limit_block_seconds,
            config.concurrent_playback_limit_max,
        )
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

async fn enrich_client_control_response(
    state: &AppState,
    config: ClientControlConfig,
) -> ClientControlConfig {
    let mut config = redact_webhook_secrets(config);
    for record in &mut config.rate_limit_blocks {
        record.ip_location = state.ip_location.lookup(&record.ip).await;
    }
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
    ip_location_text: &str,
    window: u64,
    max_requests: u64,
    block_seconds: u64,
) -> (String, String) {
    let action_label = rate_limit_action_label(action);
    let title = format!("播放频率限制 - {action_label}");
    let ip_location_line = if ip_location_text.trim().is_empty() {
        String::new()
    } else {
        format!("\n归属地：{}", ip_location_text.trim())
    };
    let text = format!(
        "服务器：{server_name}\n用户：{}\nIP：{}{}\n策略：{action_label}\n窗口：{window}s\n阈值：{max_requests} 次\n处理时长：{block_seconds}s",
        normalize_value(playback_user),
        normalize_value(playback_ip),
        ip_location_line,
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
        notification.ip_location_text,
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

async fn notify_concurrent_playback_block(
    state: &AppState,
    webhooks: Vec<WebhookNotifyConfig>,
    notification: RateLimitNotification<'_>,
    active_count: u64,
) {
    let action_label = rate_limit_action_label(notification.action);
    let title = format!("同时播放限制 - {action_label}");
    let ip_location_line = if notification.ip_location_text.trim().is_empty() {
        String::new()
    } else {
        format!("\n归属地：{}", notification.ip_location_text.trim())
    };
    let text = format!(
        "服务器：{}\n用户：{}\nIP：{}{}\n策略：{action_label}\n当前播放：{active_count} 路\n允许同时播放：{} 路\n处理时长：{}s",
        notification.server_name,
        normalize_value(notification.playback_user),
        normalize_value(notification.playback_ip),
        ip_location_line,
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
        "mixed" | "both" | "all" | "disable_user_and_block_ip" => "mixed".to_string(),
        _ => "block_ip".to_string(),
    }
}

fn action_requires_user(action: &str) -> bool {
    action == "disable_user"
}

fn action_disables_user(action: &str) -> bool {
    action == "disable_user" || action == "mixed"
}

fn action_blocks_ip(action: &str) -> bool {
    action == "block_ip" || action == "mixed"
}

fn action_uses_user_key(action: &str) -> bool {
    action == "disable_user"
}

fn rate_limit_action_label(action: &str) -> &'static str {
    match action {
        "disable_user" => "禁用用户",
        "mixed" => "混合模式",
        _ => "屏蔽 IP",
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
    if config.rate_limit_blocks.len() > PLAYBACK_RATE_STATE_CAPACITY {
        config
            .rate_limit_blocks
            .sort_by_key(|record| record.blocked_until.parse::<u64>().unwrap_or_default());
        let excess = config.rate_limit_blocks.len() - PLAYBACK_RATE_STATE_CAPACITY;
        config.rate_limit_blocks.drain(..excess);
    }
    config.rate_limit_blocks.len() != before_len
}

fn make_room_for_rate_state<T>(entries: &mut HashMap<String, T>, key: &str) {
    if entries.contains_key(key) || entries.len() < PLAYBACK_RATE_STATE_CAPACITY {
        return;
    }
    if let Some(evicted) = entries.keys().next().cloned() {
        entries.remove(&evicted);
    }
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
    upsert_rate_limit_block_with_note(
        config,
        server_id,
        server_name,
        action,
        ip,
        user_name,
        blocked_until,
        rate_limit_block_note(action, ip, user_name),
    );
}

fn upsert_rate_limit_block_with_note(
    config: &mut ClientControlConfig,
    server_id: &str,
    server_name: &str,
    action: &str,
    ip: &str,
    user_name: &str,
    blocked_until: u64,
    note: String,
) {
    let now = chrono_like_now();
    if let Some(existing) = config.rate_limit_blocks.iter_mut().find(|record| {
        record.enabled
            && record.server_id == server_id
            && record.action == action
            && if action_uses_user_key(action) {
                record.user_name == user_name
            } else {
                record.ip == ip
            }
    }) {
        existing.blocked_until = blocked_until.to_string();
        existing.note = note;
        return;
    }
    if config.rate_limit_blocks.len() >= PLAYBACK_RATE_STATE_CAPACITY
        && let Some((index, _)) = config
            .rate_limit_blocks
            .iter()
            .enumerate()
            .min_by_key(|(_, record)| record.blocked_until.parse::<u64>().unwrap_or_default())
    {
        config.rate_limit_blocks.remove(index);
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
        note,
        ip_location: None,
    });
}

fn rate_limit_block_note(action: &str, ip: &str, user_name: &str) -> String {
    match action {
        "disable_user" => format!("频率超限禁用用户 {user_name}"),
        "mixed" => format!("频率超限禁用用户 {user_name} 并屏蔽 IP {ip}"),
        _ => format!("频率超限屏蔽 IP {ip}"),
    }
}

fn concurrent_limit_block_note(ip: &str, user_name: &str) -> String {
    format!("同时播放超限屏蔽用户 {user_name} 的 IP {ip}")
}

fn empty_audit_value(value: &str) -> &str {
    if value.trim().is_empty() { "--" } else { value }
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

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
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

pub async fn notify_connectivity_issue(
    state: &AppState,
    server_id: &str,
    server_name: &str,
    title: &str,
    text: &str,
) {
    let Ok(config) = load_or_default(state) else {
        return;
    };
    if !config.notify_enabled {
        return;
    }

    for webhook in active_webhooks(config.webhooks) {
        if let Err(err) = send_webhook(
            &state.client,
            &webhook.url,
            Some(webhook.secret.as_str()),
            title,
            text,
        )
        .await
        {
            state.activity_log.record(
                crate::activity_log::ActivityKind::General,
                crate::activity_log::ActivityLevel::Warn,
                Some(server_id),
                server_name,
                "连通性巡检通知发送失败",
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
    let has_playback_user = !playback_user.is_empty() && playback_user != "--";
    if action_requires_user(&action) && !has_playback_user {
        return Ok(false);
    }

    let now = now_seconds();
    let mut changed = prune_inactive_rate_limit_blocks(&mut config, now);
    if config.rate_limit_blocks.iter().any(|record| {
        record.enabled
            && record.server_id == input.server_id
            && record.blocked_until.parse::<u64>().unwrap_or_default() > now
            && if action_uses_user_key(&action) {
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
        if bans
            .get(playback_user)
            .is_some_and(|blocked_until| *blocked_until <= now)
        {
            bans.remove(playback_user);
        }
        if let Some(blocked_until) = bans.get(playback_user).copied() {
            if blocked_until > now {
                return Ok(true);
            }
        }
    }
    {
        let mut ip_bans = state.playback_rate_ip_bans.lock().await;
        if ip_bans
            .get(playback_ip)
            .is_some_and(|blocked_until| *blocked_until <= now)
        {
            ip_bans.remove(playback_ip);
        }
        if let Some(blocked_until) = ip_bans.get(playback_ip).copied() {
            if blocked_until > now {
                return Ok(true);
            }
        }
    }

    let window = config.playback_rate_limit_window_seconds.max(1);
    let max_requests = config.playback_rate_limit_max_requests.max(1);
    let event_key = input.playback_event.trim();
    if !event_key.is_empty() {
        let recent_key = format!("{}:{playback_ip}:{event_key}", input.server_id);
        let mut recent_events = state.playback_rate_recent_events.lock().await;
        if recent_events.get(&recent_key).is_some_and(|timestamp| {
            now.saturating_sub(*timestamp) >= PLAYBACK_RATE_RECENT_EVENT_TTL_SECONDS
        }) {
            recent_events.remove(&recent_key);
        }
        let has_recent_event = recent_events.get(&recent_key).is_some_and(|timestamp| {
            now.saturating_sub(*timestamp) < PLAYBACK_RATE_RECENT_EVENT_TTL_SECONDS
        });
        if input.skip_recent_event && has_recent_event {
            return Ok(false);
        }
        if input.record_recent_event {
            if !recent_events.contains_key(&recent_key)
                && recent_events.len() >= PLAYBACK_RATE_STATE_CAPACITY
            {
                recent_events.retain(|_, timestamp| {
                    now.saturating_sub(*timestamp) < PLAYBACK_RATE_RECENT_EVENT_TTL_SECONDS
                });
            }
            make_room_for_rate_state(&mut recent_events, &recent_key);
            recent_events.insert(recent_key, now);
        }
    } else if input.skip_recent_event || input.record_recent_event {
        let recent_key = format!("{}:{playback_ip}:playback-start", input.server_id);
        let mut recent_events = state.playback_rate_recent_events.lock().await;
        if recent_events.get(&recent_key).is_some_and(|timestamp| {
            now.saturating_sub(*timestamp) >= PLAYBACK_RATE_RECENT_EVENT_TTL_SECONDS
        }) {
            recent_events.remove(&recent_key);
        }
        let has_recent_event = recent_events.get(&recent_key).is_some_and(|timestamp| {
            now.saturating_sub(*timestamp) < PLAYBACK_RATE_RECENT_EVENT_TTL_SECONDS
        });
        if input.skip_recent_event && has_recent_event {
            return Ok(false);
        }
        if input.record_recent_event {
            if !recent_events.contains_key(&recent_key)
                && recent_events.len() >= PLAYBACK_RATE_STATE_CAPACITY
            {
                recent_events.retain(|_, timestamp| {
                    now.saturating_sub(*timestamp) < PLAYBACK_RATE_RECENT_EVENT_TTL_SECONDS
                });
            }
            make_room_for_rate_state(&mut recent_events, &recent_key);
            recent_events.insert(recent_key, now);
        }
    }

    let key = format!("{}:{}", input.server_id, playback_ip);
    let should_block = {
        let mut hits = state.playback_rate_hits.lock().await;
        if !hits.contains_key(&key) && hits.len() >= PLAYBACK_RATE_STATE_CAPACITY {
            hits.retain(|_, timestamps| {
                while timestamps
                    .front()
                    .is_some_and(|hit| now.saturating_sub(hit.timestamp) >= window)
                {
                    timestamps.pop_front();
                }
                !timestamps.is_empty()
            });
        }
        make_room_for_rate_state(&mut hits, &key);
        let timestamps = hits.entry(key).or_default();
        while timestamps
            .front()
            .is_some_and(|hit| now.saturating_sub(hit.timestamp) >= window)
        {
            timestamps.pop_front();
        }
        timestamps.push_back(PlaybackRateHit {
            timestamp: now,
            user_name: playback_user.to_string(),
        });
        timestamps.len() as u64 > max_requests
    };

    if should_block {
        let block_seconds = config.playback_rate_limit_block_seconds.max(1);
        let blocked_until = now.saturating_add(block_seconds);
        let notify_enabled = config.notify_enabled;
        let webhooks = config.webhooks.clone();
        if action_disables_user(&action) && has_playback_user {
            if let Err(err) =
                disable_emby_user_by_name(input.client, input.runtime_config, playback_user).await
            {
                state.activity_log.record(
                    crate::activity_log::ActivityKind::General,
                    crate::activity_log::ActivityLevel::Warn,
                    Some(input.server_id),
                    "播放频率限制",
                    "禁用用户失败",
                    format!(
                        "用户 {} 调用 Emby API 失败: {}",
                        playback_user,
                        err.safe_log_message()
                    ),
                );
            } else {
                state.activity_log.record(
                    crate::activity_log::ActivityKind::General,
                    crate::activity_log::ActivityLevel::Warn,
                    Some(input.server_id),
                    "播放频率限制",
                    if action == "mixed" {
                        "混合封禁"
                    } else {
                        "禁用用户"
                    },
                    format!(
                        "用户 {playback_user} 在 IP {playback_ip} 的 {window}s 窗口内超过 {max_requests} 次播放请求，调用 Emby API 禁用用户"
                    ),
                );
            }
            let mut bans = state.playback_rate_bans.lock().await;
            if !bans.contains_key(playback_user) && bans.len() >= PLAYBACK_RATE_STATE_CAPACITY {
                bans.retain(|_, blocked_until| *blocked_until > now);
            }
            make_room_for_rate_state(&mut bans, playback_user);
            bans.insert(playback_user.to_string(), blocked_until);
        }
        if action_blocks_ip(&action) {
            let mut ip_bans = state.playback_rate_ip_bans.lock().await;
            if !ip_bans.contains_key(playback_ip) && ip_bans.len() >= PLAYBACK_RATE_STATE_CAPACITY {
                ip_bans.retain(|_, blocked_until| *blocked_until > now);
            }
            make_room_for_rate_state(&mut ip_bans, playback_ip);
            ip_bans.insert(playback_ip.to_string(), blocked_until);
            state.activity_log.record(
                crate::activity_log::ActivityKind::General,
                crate::activity_log::ActivityLevel::Warn,
                Some(input.server_id),
                "播放频率限制",
                if action == "mixed" {
                    "混合封禁"
                } else {
                    "屏蔽 IP"
                },
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
        let ip_location_text = state
            .ip_location
            .lookup(playback_ip)
            .await
            .map(|location| location.display_text())
            .unwrap_or_default();
        state.block_log.record(crate::block_log::BlockLogInsert {
            event_type: "block",
            timestamp_ms: now as u128 * 1000,
            server_id: input.server_id,
            server_name: input.server_name,
            port: input.runtime_config.port,
            method: "ACTION",
            path: "rate_limit/block",
            path_type: "rate_limit_action",
            status_code: 429,
            outcome: rate_limit_action_label(&action),
            duration_ms: 0,
            playback_user,
            playback_ip,
            ip_location_text: &ip_location_text,
            cache_hit: false,
            detail: &format!(
                "窗口: {window}s; 阈值: {max_requests}; 封禁: {block_seconds}s; 到期: {blocked_until}"
            ),
        });
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
                    ip_location_text: &ip_location_text,
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

pub async fn enforce_concurrent_playback_limit(
    state: &AppState,
    input: ConcurrentPlaybackLimitInput<'_>,
) -> AppResult<bool> {
    let mut config = load_or_default(state)?;
    if !config.concurrent_playback_limit_enabled {
        return Ok(false);
    }

    let playback_user = input.playback_user.trim();
    let playback_ip = input.playback_ip.trim();
    if playback_user.is_empty()
        || playback_user == "--"
        || playback_ip.is_empty()
        || playback_ip == "--"
    {
        return Ok(false);
    }

    let action = "block_ip";
    let now = now_seconds();
    let mut changed = prune_inactive_rate_limit_blocks(&mut config, now);
    if config.rate_limit_blocks.iter().any(|record| {
        record.enabled
            && record.server_id == input.server_id
            && record.action == action
            && record.ip == playback_ip
            && record.blocked_until.parse::<u64>().unwrap_or_default() > now
    }) {
        if changed {
            state
                .settings_store
                .save_setting_json(SETTING_KEY, &config)?;
        }
        return Ok(true);
    }
    {
        let mut ip_bans = state.playback_rate_ip_bans.lock().await;
        if ip_bans
            .get(playback_ip)
            .is_some_and(|blocked_until| *blocked_until <= now)
        {
            ip_bans.remove(playback_ip);
        }
        if let Some(blocked_until) = ip_bans.get(playback_ip).copied() {
            if blocked_until > now {
                return Ok(true);
            }
        }
    }

    let sessions =
        match emby::get_active_playback_sessions(&state.client, input.runtime_config).await {
            Ok(sessions) => sessions,
            Err(err) => {
                state.activity_log.record(
                    crate::activity_log::ActivityKind::General,
                    crate::activity_log::ActivityLevel::Warn,
                    Some(input.server_id),
                    "同时播放限制",
                    "会话查询失败",
                    format!(
                        "查询 Emby 当前播放会话失败，已放行本次请求: {}",
                        safe_error_message(&err)
                    ),
                );
                if changed {
                    state
                        .settings_store
                        .save_setting_json(SETTING_KEY, &config)?;
                }
                return Ok(false);
            }
        };
    let current_item_id = input.item_id.trim();
    let active_count = sessions
        .iter()
        .filter(|session| {
            session.server_id == input.server_id
                && session.user_name.trim().eq_ignore_ascii_case(playback_user)
                && {
                    let session_item_id = session.item_id.trim();
                    current_item_id.is_empty()
                        || session_item_id.is_empty()
                        || session_item_id != current_item_id
                }
        })
        .count() as u64;
    let max_concurrent = config.concurrent_playback_limit_max.max(1);
    if active_count < max_concurrent {
        if changed {
            state
                .settings_store
                .save_setting_json(SETTING_KEY, &config)?;
        }
        return Ok(false);
    }

    let block_seconds = config.playback_rate_limit_block_seconds.max(1);
    let blocked_until = now.saturating_add(block_seconds);
    let notify_enabled = config.notify_enabled;
    let webhooks = config.webhooks.clone();
    {
        let mut ip_bans = state.playback_rate_ip_bans.lock().await;
        if !ip_bans.contains_key(playback_ip) && ip_bans.len() >= PLAYBACK_RATE_STATE_CAPACITY {
            ip_bans.retain(|_, blocked_until| *blocked_until > now);
        }
        make_room_for_rate_state(&mut ip_bans, playback_ip);
        ip_bans.insert(playback_ip.to_string(), blocked_until);
    }
    upsert_rate_limit_block_with_note(
        &mut config,
        input.server_id,
        input.server_name,
        action,
        playback_ip,
        playback_user,
        blocked_until,
        concurrent_limit_block_note(playback_ip, playback_user),
    );
    let ip_location_text = state
        .ip_location
        .lookup(playback_ip)
        .await
        .map(|location| location.display_text())
        .unwrap_or_default();
    state.activity_log.record(
        crate::activity_log::ActivityKind::General,
        crate::activity_log::ActivityLevel::Warn,
        Some(input.server_id),
        "同时播放限制",
        "屏蔽 IP",
        format!(
            "用户 {playback_user} 已有 {active_count} 路播放，超过允许同时播放 {max_concurrent} 路，屏蔽 IP {playback_ip} {block_seconds}s"
        ),
    );
    state.block_log.record(crate::block_log::BlockLogInsert {
        event_type: "block",
        timestamp_ms: now as u128 * 1000,
        server_id: input.server_id,
        server_name: input.server_name,
        port: input.runtime_config.port,
        method: "ACTION",
        path: "concurrent_limit/block",
        path_type: "rate_limit_action",
        status_code: 429,
        outcome: "屏蔽 IP",
        duration_ms: 0,
        playback_user,
        playback_ip,
        ip_location_text: &ip_location_text,
        cache_hit: false,
        detail: &format!(
            "当前播放: {active_count}; 允许同时播放: {max_concurrent}; 封禁: {block_seconds}s; 到期: {blocked_until}"
        ),
    });
    changed = true;

    if notify_enabled {
        notify_concurrent_playback_block(
            state,
            webhooks,
            RateLimitNotification {
                server_id: input.server_id,
                server_name: input.server_name,
                action,
                playback_user,
                playback_ip,
                ip_location_text: &ip_location_text,
                window: 0,
                max_requests: max_concurrent,
                block_seconds,
            },
            active_count,
        )
        .await;
    }

    if changed {
        state
            .settings_store
            .save_setting_json(SETTING_KEY, &config)?;
    }

    Ok(true)
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

fn normalize_manual_user_agent_rule(user_agent: &str) -> String {
    user_agent.trim().to_string()
}

fn normalize_auto_user_agent_rule(user_agent: &str) -> String {
    let value = normalize_manual_user_agent_rule(user_agent);
    if value.is_empty() {
        return String::new();
    }
    if let Some((name, _)) = value.as_str().split_once('/')
        && !name.trim().is_empty()
    {
        return name.trim().to_string();
    }
    value
}

fn normalize_client_rule_records(config: &mut ClientControlConfig) -> bool {
    let mut changed = false;
    let mut normalized = Vec::<ClientRuleRecord>::new();
    for mut record in std::mem::take(&mut config.records) {
        let normalized_ua = match record.source {
            ClientRuleSource::Auto => normalize_auto_user_agent_rule(&record.user_agent),
            ClientRuleSource::Manual => normalize_manual_user_agent_rule(&record.user_agent),
        };
        if normalized_ua.is_empty() {
            changed = true;
            continue;
        }
        if record.user_agent != normalized_ua {
            record.user_agent = normalized_ua;
            changed = true;
        }
        if let Some(existing) = normalized
            .iter_mut()
            .find(|item| item.user_agent.eq_ignore_ascii_case(&record.user_agent))
        {
            merge_client_rule_record(existing, record);
            changed = true;
        } else {
            normalized.push(record);
        }
    }
    config.records = normalized;
    changed
}

fn merge_client_rule_record(existing: &mut ClientRuleRecord, incoming: ClientRuleRecord) {
    existing.enabled |= incoming.enabled;
    if incoming.updated_at > existing.updated_at {
        existing.client_name = incoming.client_name;
        existing.device_name = incoming.device_name;
        existing.user_name = incoming.user_name;
        existing.updated_at = incoming.updated_at;
    }
    if existing.note.trim().is_empty() || existing.note == "自动记录播放设备" {
        existing.note = incoming.note;
    }
    if matches!(existing.source, ClientRuleSource::Auto)
        && matches!(incoming.source, ClientRuleSource::Manual)
    {
        existing.source = ClientRuleSource::Manual;
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

    #[test]
    fn ua_rule_does_not_match_device_or_user_only() {
        let record = ClientRuleRecord {
            id: "rule-1".to_string(),
            client_name: "网易爆米花 iOS".to_string(),
            device_name: "AppleTV14".to_string(),
            user_name: "jhoupeng".to_string(),
            user_agent: "网易爆米花 iOS/2.6.9".to_string(),
            source: ClientRuleSource::Auto,
            enabled: true,
            created_at: "0".to_string(),
            updated_at: "0".to_string(),
            note: String::new(),
        };

        assert!(!rule_matches(&record, "AppleTV14"));
        assert!(!rule_matches(&record, "jhoupeng"));
    }

    #[test]
    fn normalizes_auto_versioned_client_user_agents() {
        assert_eq!(
            normalize_auto_user_agent_rule("Infuse-Direct/8.4.5"),
            "Infuse-Direct"
        );
        assert_eq!(normalize_auto_user_agent_rule("VidHub/2.3.0"), "VidHub");
        assert_eq!(
            normalize_auto_user_agent_rule("AndroidTv/2.0.95g"),
            "AndroidTv"
        );
        assert_eq!(
            normalize_auto_user_agent_rule("网易爆米花 iOS/2.6.9"),
            "网易爆米花 iOS"
        );
        assert_eq!(
            normalize_auto_user_agent_rule("Emby for Apple TV/1.9.8 (2)"),
            "Emby for Apple TV"
        );
        assert_eq!(
            normalize_auto_user_agent_rule("Filmly/2.5.13-340"),
            "Filmly"
        );
        assert_eq!(
            normalize_auto_user_agent_rule("Filmly/2.9.21-395"),
            "Filmly"
        );
        assert_eq!(normalize_auto_user_agent_rule("Mozilla/5.0"), "Mozilla");
        assert_eq!(
            normalize_auto_user_agent_rule("CustomClient/beta"),
            "CustomClient"
        );
    }

    #[test]
    fn keeps_manual_versioned_user_agent_rules_precise() {
        assert_eq!(
            normalize_manual_user_agent_rule("VidHub/2.2.9"),
            "VidHub/2.2.9"
        );
        let precise = rule("VidHub/2.2.9", "VidHub");
        assert!(rule_matches(&precise, "VidHub/2.2.9"));
        assert!(!rule_matches(&precise, "VidHub/2.3.0"));

        let family = rule("VidHub", "VidHub");
        assert!(rule_matches(&family, "VidHub/2.2.9"));
        assert!(rule_matches(&family, "VidHub/2.3.0"));
    }

    #[test]
    fn merges_versioned_client_rule_records() {
        let mut config = default_config();
        config.records.push(ClientRuleRecord {
            id: "old".to_string(),
            client_name: "Infuse-Direct".to_string(),
            device_name: "Apple TV".to_string(),
            user_name: "Lucifinil".to_string(),
            user_agent: "Infuse-Direct/8.4.5".to_string(),
            source: ClientRuleSource::Auto,
            enabled: false,
            created_at: "100".to_string(),
            updated_at: "100".to_string(),
            note: "自动记录播放设备".to_string(),
        });
        config.records.push(ClientRuleRecord {
            id: "new".to_string(),
            client_name: "Infuse-Direct".to_string(),
            device_name: "Apple TV".to_string(),
            user_name: "王德发".to_string(),
            user_agent: "Infuse-Direct/8.4.3".to_string(),
            source: ClientRuleSource::Auto,
            enabled: true,
            created_at: "90".to_string(),
            updated_at: "200".to_string(),
            note: "自动记录播放设备".to_string(),
        });

        assert!(normalize_client_rule_records(&mut config));
        assert_eq!(config.records.len(), 1);
        assert_eq!(config.records[0].user_agent, "Infuse-Direct");
        assert_eq!(config.records[0].user_name, "王德发");
        assert!(config.records[0].enabled);
    }

    #[test]
    fn normalizes_client_control_resource_limits() {
        let mut config = default_config();
        config.playback_rate_limit_window_seconds = u64::MAX;
        config.playback_rate_limit_max_requests = u64::MAX;
        config.playback_rate_limit_block_seconds = u64::MAX;
        config.concurrent_playback_limit_max = u64::MAX;

        assert!(normalize_client_control_limits(&mut config));
        assert_eq!(
            config.playback_rate_limit_window_seconds,
            PLAYBACK_RATE_MAX_WINDOW_SECONDS
        );
        assert_eq!(
            config.playback_rate_limit_max_requests,
            PLAYBACK_RATE_MAX_REQUESTS
        );
        assert_eq!(
            config.playback_rate_limit_block_seconds,
            PLAYBACK_RATE_MAX_BLOCK_SECONDS
        );
        assert_eq!(
            config.concurrent_playback_limit_max,
            CONCURRENT_PLAYBACK_MAX
        );
    }

    #[test]
    fn rate_limit_runtime_state_stays_bounded() {
        let mut entries = HashMap::new();
        for index in 0..PLAYBACK_RATE_STATE_CAPACITY {
            entries.insert(format!("client-{index}"), index);
        }

        make_room_for_rate_state(&mut entries, "new-client");
        entries.insert("new-client".to_string(), usize::MAX);

        assert_eq!(entries.len(), PLAYBACK_RATE_STATE_CAPACITY);
        assert_eq!(entries.get("new-client"), Some(&usize::MAX));
    }
}
