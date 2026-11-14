use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use rand::RngCore;
use serde::Deserialize;
use std::time::Duration;

use crate::{
    AppState,
    alerts::{
        AlertDelivery, AlertRule, AlertRuleView, Alerts, MAX_DELIVERY_LIMIT,
        MAX_DELIVERY_MESSAGE_LEN, MAX_RETRY_COUNT, MAX_RULE_CONFIG_LEN, MAX_RULE_ID_LEN,
        MAX_RULE_NAME_LEN, MAX_SILENCE_PERIOD_SECS, MAX_THRESHOLD_LEN, RuleConfigUpdate, now_ms,
        sanitize_delivery_message,
    },
    auth,
    crypto_api::EncryptedRequest,
    error::{AppError, AppResult, safe_error_message},
};

/// Request body for create. Server generates `id` and `created_at`.
#[derive(Debug, Deserialize)]
struct CreateAlertRuleRequest {
    pub name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Decrypted webhook URL plain or `{"url":"..."}` only. Never metric JSON.
    #[serde(default)]
    pub rule_config: String,
    /// Free-form when metric empty; finite number when metric set.
    #[serde(default)]
    pub threshold: String,
    /// Cooldown seconds after a firing notification (not manual silence).
    #[serde(default)]
    pub silence_period: u64,
    /// Empty = manual/test only. Whitelist: proxy.requests|redirects|cache_hits|blocks|errors.
    #[serde(default)]
    pub metric: String,
    /// gt|gte|lt|lte|eq ; empty defaults to gte.
    #[serde(default)]
    pub operator: String,
    /// Empty = all servers aggregated by runtime.
    #[serde(default)]
    pub server_id: String,
    /// 0 = previous runtime evaluation interval; >0 = rolling runtime window.
    #[serde(default)]
    pub evaluation_window_secs: u64,
    /// Ignored if present; kept only for frontend payload compatibility.
    #[serde(default)]
    pub id: Option<String>,
    /// Ignored if present; server assigns created_at.
    #[serde(default)]
    pub created_at: Option<u128>,
}

/// Request body for update. Path id wins; created_at is preserved server-side.
#[derive(Debug, Deserialize)]
struct UpdateAlertRuleRequest {
    pub name: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Omitted preserves the existing secret; an explicit value replaces it.
    #[serde(default)]
    pub rule_config: Option<String>,
    /// Explicitly clears the stored rule configuration when no replacement is present.
    #[serde(default)]
    pub clear_rule_config: bool,
    /// Free-form when metric empty; finite number when metric set.
    #[serde(default)]
    pub threshold: String,
    /// Cooldown seconds after a firing notification (not manual silence).
    #[serde(default)]
    pub silence_period: u64,
    /// Empty = manual/test only. Whitelist: proxy.requests|redirects|cache_hits|blocks|errors.
    #[serde(default)]
    pub metric: String,
    /// gt|gte|lt|lte|eq ; empty defaults to gte.
    #[serde(default)]
    pub operator: String,
    /// Empty = all servers aggregated by runtime.
    #[serde(default)]
    pub server_id: String,
    /// 0 = previous runtime evaluation interval; >0 = rolling runtime window.
    #[serde(default)]
    pub evaluation_window_secs: u64,
    /// Ignored; path param is authoritative.
    #[serde(default)]
    pub id: Option<String>,
    /// Ignored; existing created_at is preserved.
    #[serde(default)]
    pub created_at: Option<u128>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct ToggleAlertRuleRequest {
    pub enabled: bool,
}

/// Frontend sends `{ until: string }` where until is an epoch-ms decimal string.
/// Empty / "0" / "null" / "none" clears silence; non-zero timestamps must be future.
#[derive(Debug, Deserialize)]
pub struct SilentAlertRuleRequest {
    pub until: String,
}

#[derive(Debug, Deserialize)]
pub struct AlertDeliveriesQuery {
    pub rule_id: Option<String>,
    pub limit: Option<usize>,
}

/// Thin facade used by unit/integration tests without AppState.
pub struct AlertsApi {
    alerts: Alerts,
}

impl AlertsApi {
    pub fn new(alerts: Alerts) -> Self {
        Self { alerts }
    }

    pub async fn get_alert_rules(&self) -> AppResult<Vec<AlertRuleView>> {
        self.alerts.get_alert_rules()
    }

    pub async fn create_alert_rule(&self, rule: AlertRule) -> AppResult<()> {
        self.alerts.create_alert_rule(rule)
    }

    pub async fn list_alert_deliveries(
        &self,
        rule_id: Option<&str>,
    ) -> AppResult<Vec<AlertDelivery>> {
        self.alerts
            .list_alert_deliveries(rule_id, MAX_DELIVERY_LIMIT)
    }

    pub async fn test_alert_delivery(&self, rule_id: &str, payload: &str) -> AppResult<()> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| AppError::Internal("failed to build http client".to_string()))?;
        deliver_webhook(
            &self.alerts,
            &client,
            rule_id,
            payload,
            "test delivery success",
        )
        .await
    }

    pub async fn toggle_rule(&self, id: &str, enabled: bool) -> AppResult<()> {
        self.alerts.toggle_rule(id, enabled)
    }

    pub async fn set_silent(&self, id: &str, until: &str) -> AppResult<()> {
        let (silent, until_ms) = parse_silent_until(until)?;
        self.alerts.update_silence(id, silent, until_ms)
    }

    pub async fn delete_rule(&self, id: &str) -> AppResult<()> {
        self.alerts.delete_rule(id)
    }
}

const DEFAULT_TEST_PAYLOAD: &str =
    r#"{"title":"EmbyPanel alert test","text":"Webhook test delivery"}"#;
const DELIVERY_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_BACKOFF_BASE_MS: u64 = 100;
const RETRY_BACKOFF_MAX_MS: u64 = 1_000;

fn new_rule_id() -> String {
    let mut bytes = [0_u8; 4];
    rand::rng().fill_bytes(&mut bytes);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "alert-{}-{:02x}{:02x}{:02x}{:02x}",
        now, bytes[0], bytes[1], bytes[2], bytes[3]
    )
}

fn reject_control_chars(label: &str, value: &str) -> AppResult<()> {
    if value.chars().any(|ch| ch.is_control()) {
        return Err(AppError::Validation(format!(
            "{label} contains control characters"
        )));
    }
    Ok(())
}

fn validate_path_rule_id(rule_id: &str) -> AppResult<&str> {
    let id = rule_id.trim();
    if id.is_empty() || id.len() > MAX_RULE_ID_LEN {
        return Err(AppError::Validation("invalid rule id".to_string()));
    }
    reject_control_chars("rule id", id)?;
    Ok(id)
}

fn validate_rule_config_input(rule_config: &str) -> AppResult<()> {
    if rule_config.len() > MAX_RULE_CONFIG_LEN {
        return Err(AppError::Validation("rule config too long".to_string()));
    }
    reject_control_chars("rule config", rule_config)?;
    if !rule_config.trim().is_empty() {
        let _ = Alerts::parse_webhook_url(rule_config)?;
    }
    Ok(())
}

fn build_rule_from_create(req: CreateAlertRuleRequest) -> AppResult<AlertRule> {
    let name = req.name.trim().to_string();
    let rule_config = req.rule_config.trim().to_string();
    let threshold = req.threshold.trim().to_string();
    if name.is_empty() || name.len() > MAX_RULE_NAME_LEN {
        return Err(AppError::Validation(
            "rule name is required and must be <= 128 chars".to_string(),
        ));
    }
    if threshold.len() > MAX_THRESHOLD_LEN {
        return Err(AppError::Validation("threshold too long".to_string()));
    }
    if req.silence_period > MAX_SILENCE_PERIOD_SECS {
        return Err(AppError::Validation(
            "silence_period exceeds maximum".to_string(),
        ));
    }
    reject_control_chars("rule name", &name)?;
    validate_rule_config_input(&rule_config)?;
    // Never trust client id / created_at.
    let _ = req.id;
    let _ = req.created_at;
    let rule = AlertRule {
        id: new_rule_id(),
        name,
        enabled: req.enabled,
        rule_config,
        threshold,
        silence_period: req.silence_period,
        created_at: now_ms(),
        metric: req.metric,
        operator: req.operator,
        server_id: req.server_id,
        evaluation_window_secs: req.evaluation_window_secs,
    };
    let rule = Alerts::normalize_rule(rule)?;
    Alerts::validate_rule_fields(&rule)?;
    Ok(rule)
}

fn map_rule_config_update(
    rule_config: Option<String>,
    clear_rule_config: bool,
) -> AppResult<RuleConfigUpdate> {
    match (rule_config, clear_rule_config) {
        (Some(_), true) => Err(AppError::Validation(
            "rule_config and clear_rule_config cannot be used together".to_string(),
        )),
        (Some(value), false) => Ok(RuleConfigUpdate::Replace(value)),
        (None, true) => Ok(RuleConfigUpdate::Clear),
        (None, false) => Ok(RuleConfigUpdate::Preserve),
    }
}

fn build_rule_from_update(
    path_id: &str,
    req: UpdateAlertRuleRequest,
    existing: &AlertRuleView,
) -> AppResult<(AlertRule, RuleConfigUpdate)> {
    let name = req.name.trim().to_string();
    let threshold = req.threshold.trim().to_string();
    if name.is_empty() || name.len() > MAX_RULE_NAME_LEN {
        return Err(AppError::Validation(
            "rule name is required and must be <= 128 chars".to_string(),
        ));
    }
    if threshold.len() > MAX_THRESHOLD_LEN {
        return Err(AppError::Validation("threshold too long".to_string()));
    }
    if req.silence_period > MAX_SILENCE_PERIOD_SECS {
        return Err(AppError::Validation(
            "silence_period exceeds maximum".to_string(),
        ));
    }
    reject_control_chars("rule name", &name)?;
    let config_update = map_rule_config_update(req.rule_config, req.clear_rule_config)?;
    if let RuleConfigUpdate::Replace(value) = &config_update {
        validate_rule_config_input(value.trim())?;
    }
    let _ = req.id;
    let _ = req.created_at;
    let rule = AlertRule {
        id: path_id.to_string(),
        name,
        enabled: req.enabled,
        // Preserve/replace/clear is carried separately so the API never reads or
        // reuses the existing secret from a public view.
        rule_config: String::new(),
        threshold,
        silence_period: req.silence_period,
        created_at: existing.created_at,
        metric: req.metric,
        operator: req.operator,
        server_id: req.server_id,
        evaluation_window_secs: req.evaluation_window_secs,
    };
    let rule = Alerts::normalize_rule(rule)?;
    Alerts::validate_rule_fields(&rule)?;
    Ok((rule, config_update))
}

/// Parse frontend silent `until` string.
/// Accepted:
/// - decimal epoch milliseconds (e.g. "1730000000000") strictly after the current time
/// - empty / "0" / "null" / "none" => clear silence
pub fn parse_silent_until(until: &str) -> AppResult<(bool, Option<u128>)> {
    let trimmed = until.trim();
    if trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("0")
        || trimmed.eq_ignore_ascii_case("null")
        || trimmed.eq_ignore_ascii_case("none")
    {
        return Ok((false, None));
    }
    if trimmed.len() > 32 || trimmed.chars().any(|ch| ch.is_control()) {
        return Err(AppError::Validation(
            "invalid silent until timestamp".to_string(),
        ));
    }
    let until_ms = trimmed
        .parse::<u128>()
        .map_err(|_| AppError::Validation("invalid silent until timestamp".to_string()))?;
    // Reject absurdly large values that would overflow i64 storage.
    if until_ms > i64::MAX as u128 {
        return Err(AppError::Validation(
            "silent until timestamp out of range".to_string(),
        ));
    }
    if until_ms <= now_ms() {
        return Err(AppError::Validation(
            "silent until timestamp must be in the future".to_string(),
        ));
    }
    Ok((true, Some(until_ms)))
}

fn clamp_delivery_limit(limit: Option<usize>) -> usize {
    limit
        .unwrap_or(MAX_DELIVERY_LIMIT)
        .clamp(1, MAX_DELIVERY_LIMIT)
}

fn audit_rule_name(name: &str) -> String {
    // Audit must never include webhook URL / secrets from rule_config.
    let mut out = String::new();
    for ch in name.chars().take(64) {
        if !ch.is_control() {
            out.push(ch);
        }
    }
    if out.is_empty() {
        "unnamed".to_string()
    } else {
        out
    }
}

#[derive(Debug)]
pub(crate) struct WebhookDeliverySuccess {
    pub retry_count: u32,
}

#[derive(Debug)]
pub(crate) struct WebhookDeliveryFailure {
    pub error: AppError,
    pub retry_count: u32,
    pub message: String,
}

fn webhook_failure(error: AppError, retry_count: u32, message: &str) -> WebhookDeliveryFailure {
    WebhookDeliveryFailure {
        error,
        retry_count: retry_count.min(MAX_RETRY_COUNT),
        message: sanitize_delivery_message(message),
    }
}

/// Attempt a Webhook with bounded retries. This function never holds the Alerts
/// mutex across network I/O and never persists state by itself, allowing runtime
/// notification state and delivery history to be finalized atomically afterwards.
pub(crate) async fn attempt_webhook(
    alerts: &Alerts,
    client: &reqwest::Client,
    rule_id: &str,
    payload: &str,
) -> Result<WebhookDeliverySuccess, WebhookDeliveryFailure> {
    attempt_webhook_for_notification(alerts, client, rule_id, payload, None).await
}

pub(crate) async fn attempt_webhook_for_notification(
    alerts: &Alerts,
    client: &reqwest::Client,
    rule_id: &str,
    payload: &str,
    notification: Option<&crate::alerts::AlertNotifyEvent>,
) -> Result<WebhookDeliverySuccess, WebhookDeliveryFailure> {
    if payload.len() > MAX_DELIVERY_MESSAGE_LEN * 16 {
        return Err(webhook_failure(
            AppError::Validation("payload too large".to_string()),
            0,
            "payload too large",
        ));
    }

    let rule_config = alerts
        .get_rule_webhook_url(rule_id)
        .map_err(|error| webhook_failure(error, 0, "webhook configuration unavailable"))?;
    let url = match Alerts::parse_webhook_url(rule_config.as_deref().unwrap_or_default()) {
        Ok(Some(url)) => url,
        Ok(None) => {
            return Err(webhook_failure(
                AppError::Validation("no webhook URL configured".to_string()),
                0,
                "no webhook URL configured",
            ));
        }
        Err(_) => {
            return Err(webhook_failure(
                AppError::Validation("invalid rule config".to_string()),
                0,
                "invalid rule config",
            ));
        }
    };

    let mut retries = 0_u32;
    let last_error = loop {
        if retries > 0 {
            let shift = (retries - 1).min(4);
            let backoff =
                (RETRY_BACKOFF_BASE_MS.saturating_mul(1_u64 << shift)).min(RETRY_BACKOFF_MAX_MS);
            tokio::time::sleep(Duration::from_millis(backoff)).await;
        }

        // Check after backoff, immediately before each HTTP attempt. An HTTP request
        // already handed to the transport cannot be recalled by a rule update.
        if let Some(event) = notification {
            let current = alerts.notification_is_current(event).map_err(|error| {
                webhook_failure(error, retries, "notification validation failed")
            })?;
            if !current {
                return Err(webhook_failure(
                    AppError::Validation("notification invalidated".into()),
                    retries,
                    "notification invalidated",
                ));
            }
        }

        let attempt_error = match client
            .post(&url)
            .timeout(DELIVERY_TIMEOUT)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload.to_string())
            .send()
            .await
        {
            Ok(response) if response.status().is_success() => {
                return Ok(WebhookDeliverySuccess {
                    retry_count: retries,
                });
            }
            Ok(response) => {
                format!("webhook returned HTTP {}", response.status().as_u16())
            }
            Err(_) => String::from("network delivery failed"),
        };

        if retries >= MAX_RETRY_COUNT {
            break attempt_error;
        }
        retries = retries.saturating_add(1);
    };

    let safe_message = sanitize_delivery_message(&last_error);
    Err(webhook_failure(
        AppError::BadGateway(safe_message.clone()),
        retries,
        &safe_message,
    ))
}

async fn deliver_webhook(
    alerts: &Alerts,
    client: &reqwest::Client,
    rule_id: &str,
    payload: &str,
    success_message: &str,
) -> AppResult<()> {
    match attempt_webhook(alerts, client, rule_id, payload).await {
        Ok(success) => {
            alerts.record_delivery(rule_id, "success", success_message, success.retry_count)?;
            Ok(())
        }
        Err(failure) => {
            alerts.record_delivery(rule_id, "failed", &failure.message, failure.retry_count)?;
            Err(failure.error)
        }
    }
}

fn decrypt_alert_rule_request<T: serde::de::DeserializeOwned>(
    state: &AppState,
    request: &EncryptedRequest,
) -> AppResult<T> {
    state
        .crypto_keys
        .decrypt_named(request, "alert_rule")
        .map_err(|_| AppError::Validation("invalid encrypted alert rule request".to_string()))
}

const ALERTS_READ_PERMISSION: &str = "alerts.read";
const ALERTS_WRITE_PERMISSION: &str = "alerts.write";
const ALERT_RULE_ACCESS_DENIED: &str = "alert rule access denied";

/// Alert rules intentionally use stricter server authorization than the generic
/// server helper: a scoped rule requires an account grant even for a super-admin
/// session, while an API token is bounded by the intersection of token servers
/// and the account's grants. Global rules are session-super-admin only.
fn has_alert_rule_server_access(context: &auth::AuthContext, server_id: &str) -> bool {
    let server_id = server_id.trim();
    if server_id.is_empty() {
        return context.kind == auth::AuthKind::Session && auth::is_super_admin(context);
    }

    let account_grant = context
        .server_grants
        .iter()
        .any(|grant| grant.server_id == server_id);
    if !account_grant {
        return false;
    }

    match context.kind {
        auth::AuthKind::Session => true,
        auth::AuthKind::ApiToken => match &context.server_ids {
            Some(token_servers) => token_servers.iter().any(|id| id == server_id),
            None => true,
        },
    }
}

fn require_alert_rule_server_access(
    context: &auth::AuthContext,
    server_id: &str,
    permission: &str,
) -> AppResult<()> {
    auth::require_permission(context, permission)?;
    if has_alert_rule_server_access(context, server_id) {
        Ok(())
    } else {
        Err(AppError::Forbidden(ALERT_RULE_ACCESS_DENIED.to_string()))
    }
}

fn require_alert_rule_access(
    context: &auth::AuthContext,
    rule: &AlertRuleView,
    permission: &str,
) -> AppResult<()> {
    require_alert_rule_server_access(context, &rule.server_id, permission)
}

async fn require_alerts_read(
    state: &AppState,
    headers: &HeaderMap,
) -> AppResult<auth::AuthContext> {
    let context = auth::require_auth_context(state, headers).await?;
    auth::require_permission(&context, ALERTS_READ_PERMISSION)?;
    Ok(context)
}

async fn require_alerts_write(
    state: &AppState,
    headers: &HeaderMap,
) -> AppResult<auth::AuthContext> {
    let context = auth::require_auth_context(state, headers).await?;
    auth::require_permission(&context, ALERTS_WRITE_PERMISSION)?;
    Ok(context)
}

fn authorized_alert_rule(
    alerts: &Alerts,
    context: &auth::AuthContext,
    rule_id: &str,
    permission: &str,
) -> AppResult<AlertRuleView> {
    let rule = alerts
        .get_alert_rule(rule_id)?
        .ok_or_else(|| AppError::Validation("alert rule not found".to_string()))?;
    require_alert_rule_access(context, &rule, permission)?;
    Ok(rule)
}

pub async fn list_alert_rules(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<Json<Vec<AlertRuleView>>> {
    let context = require_alerts_read(&state, &headers).await?;
    let rules = state
        .alerts
        .get_alert_rules()?
        .into_iter()
        .filter(|rule| require_alert_rule_access(&context, rule, ALERTS_READ_PERMISSION).is_ok())
        .collect();
    Ok(Json(rules))
}

pub async fn get_alert_rule(
    State(state): State<AppState>,
    Path(rule_id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Json<AlertRuleView>> {
    let context = require_alerts_read(&state, &headers).await?;
    let rule_id = validate_path_rule_id(&rule_id)?;
    let rule = state
        .alerts
        .get_alert_rule(rule_id)?
        .ok_or_else(|| AppError::Validation("alert rule not found".to_string()))?;
    require_alert_rule_access(&context, &rule, ALERTS_READ_PERMISSION)?;
    Ok(Json(rule))
}

pub async fn create_alert_rule(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<AlertRuleView>> {
    let context = require_alerts_write(&state, &headers).await?;
    let admin_user_id = context.user_id;
    let payload: CreateAlertRuleRequest = match decrypt_alert_rule_request(&state, &request) {
        Ok(payload) => payload,
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.create",
                "创建告警规则失败：无效加密请求",
                "failed",
            );
            return Err(error);
        }
    };
    let rule = match build_rule_from_create(payload) {
        Ok(rule) => rule,
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.create",
                &format!("创建告警规则失败：{}", safe_error_message(&error)),
                "failed",
            );
            return Err(error);
        }
    };
    if let Err(error) =
        require_alert_rule_server_access(&context, &rule.server_id, ALERTS_WRITE_PERMISSION)
    {
        state.settings_store.record_audit_best_effort(
            Some(admin_user_id),
            "alerts.rule.create",
            &format!("创建告警规则失败：{}", safe_error_message(&error)),
            "failed",
        );
        return Err(error);
    }
    let rule_id = rule.id.clone();
    let alerts = &state.alerts;
    match alerts.create_alert_rule(rule) {
        Ok(()) => {
            let view = alerts
                .get_alert_rule(&rule_id)?
                .ok_or_else(|| AppError::Internal("created alert rule unavailable".to_string()))?;
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.create",
                &format!("创建告警规则 {}", audit_rule_name(&view.name)),
                "success",
            );
            Ok(Json(view))
        }
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.create",
                &format!("创建告警规则失败：{}", safe_error_message(&error)),
                "failed",
            );
            Err(error)
        }
    }
}

pub async fn update_alert_rule(
    State(state): State<AppState>,
    Path(rule_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<AlertRuleView>> {
    let context = require_alerts_write(&state, &headers).await?;
    let admin_user_id = context.user_id;
    let rule_id = match validate_path_rule_id(&rule_id) {
        Ok(id) => id.to_string(),
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.update",
                &format!("更新告警规则失败：{}", safe_error_message(&error)),
                "failed",
            );
            return Err(error);
        }
    };
    let alerts = &state.alerts;
    let existing = match alerts.get_alert_rule(&rule_id)? {
        Some(rule) => rule,
        None => {
            let error = AppError::Validation("alert rule not found".to_string());
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.update",
                &format!("更新告警规则失败：{}", safe_error_message(&error)),
                "failed",
            );
            return Err(error);
        }
    };
    if let Err(error) = require_alert_rule_access(&context, &existing, ALERTS_WRITE_PERMISSION) {
        state.settings_store.record_audit_best_effort(
            Some(admin_user_id),
            "alerts.rule.update",
            &format!("更新告警规则失败：{}", safe_error_message(&error)),
            "failed",
        );
        return Err(error);
    }
    let payload: UpdateAlertRuleRequest = match decrypt_alert_rule_request(&state, &request) {
        Ok(payload) => payload,
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.update",
                "更新告警规则失败：无效加密请求",
                "failed",
            );
            return Err(error);
        }
    };
    let (rule, config_update) = match build_rule_from_update(&rule_id, payload, &existing) {
        Ok(result) => result,
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.update",
                &format!("更新告警规则失败：{}", safe_error_message(&error)),
                "failed",
            );
            return Err(error);
        }
    };
    if let Err(error) =
        require_alert_rule_server_access(&context, &rule.server_id, ALERTS_WRITE_PERMISSION)
    {
        state.settings_store.record_audit_best_effort(
            Some(admin_user_id),
            "alerts.rule.update",
            &format!("更新告警规则失败：{}", safe_error_message(&error)),
            "failed",
        );
        return Err(error);
    }
    match alerts.update_alert_rule(rule, config_update) {
        Ok(()) => {
            let view = alerts
                .get_alert_rule(&rule_id)?
                .ok_or_else(|| AppError::Internal("updated alert rule unavailable".to_string()))?;
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.update",
                &format!("更新告警规则 {}", audit_rule_name(&view.name)),
                "success",
            );
            Ok(Json(view))
        }
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.update",
                &format!("更新告警规则失败：{}", safe_error_message(&error)),
                "failed",
            );
            Err(error)
        }
    }
}

pub async fn toggle_alert_rule(
    State(state): State<AppState>,
    Path(rule_id): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<ToggleAlertRuleRequest>,
) -> AppResult<Json<()>> {
    let context = require_alerts_write(&state, &headers).await?;
    let admin_user_id = context.user_id;
    let rule_id = validate_path_rule_id(&rule_id)?.to_string();
    authorized_alert_rule(&state.alerts, &context, &rule_id, ALERTS_WRITE_PERMISSION)?;
    let alerts = &state.alerts;
    match alerts.toggle_rule(&rule_id, payload.enabled) {
        Ok(()) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.toggle",
                &format!(
                    "{}告警规则 {}",
                    if payload.enabled { "启用" } else { "禁用" },
                    rule_id
                ),
                "success",
            );
            Ok(Json(()))
        }
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.toggle",
                &format!("切换告警规则失败：{}", safe_error_message(&error)),
                "failed",
            );
            Err(error)
        }
    }
}

pub async fn set_alert_rule_silent(
    State(state): State<AppState>,
    Path(rule_id): Path<String>,
    headers: HeaderMap,
    Json(payload): Json<SilentAlertRuleRequest>,
) -> AppResult<Json<()>> {
    let context = require_alerts_write(&state, &headers).await?;
    let admin_user_id = context.user_id;
    let rule_id = validate_path_rule_id(&rule_id)?.to_string();
    authorized_alert_rule(&state.alerts, &context, &rule_id, ALERTS_WRITE_PERMISSION)?;
    let (silent, until_ms) = match parse_silent_until(&payload.until) {
        Ok(v) => v,
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.silent",
                &format!("设置告警规则静默失败：{}", safe_error_message(&error)),
                "failed",
            );
            return Err(error);
        }
    };
    let alerts = &state.alerts;
    match alerts.update_silence(&rule_id, silent, until_ms) {
        Ok(()) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.silent",
                &format!(
                    "{}告警规则 {}",
                    if silent { "静默" } else { "取消静默" },
                    rule_id
                ),
                "success",
            );
            Ok(Json(()))
        }
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.silent",
                &format!("设置告警规则静默失败：{}", safe_error_message(&error)),
                "failed",
            );
            Err(error)
        }
    }
}

pub async fn delete_alert_rule(
    State(state): State<AppState>,
    Path(rule_id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Json<()>> {
    let context = require_alerts_write(&state, &headers).await?;
    let admin_user_id = context.user_id;
    let rule_id = validate_path_rule_id(&rule_id)?.to_string();
    authorized_alert_rule(&state.alerts, &context, &rule_id, ALERTS_WRITE_PERMISSION)?;
    let alerts = &state.alerts;
    match alerts.delete_rule(&rule_id) {
        Ok(()) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.delete",
                &format!("删除告警规则 {}", rule_id),
                "success",
            );
            Ok(Json(()))
        }
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.delete",
                &format!("删除告警规则失败：{}", safe_error_message(&error)),
                "failed",
            );
            Err(error)
        }
    }
}

pub async fn list_alert_rule_history(
    State(state): State<AppState>,
    Path(rule_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<AlertDeliveriesQuery>,
) -> AppResult<Json<Vec<AlertDelivery>>> {
    let context = require_alerts_read(&state, &headers).await?;
    let rule_id = validate_path_rule_id(&rule_id)?;
    authorized_alert_rule(&state.alerts, &context, rule_id, ALERTS_READ_PERMISSION)?;
    let limit = clamp_delivery_limit(query.limit);
    let deliveries = state.alerts.list_alert_deliveries(Some(rule_id), limit)?;
    Ok(Json(deliveries))
}

pub async fn list_alert_deliveries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AlertDeliveriesQuery>,
) -> AppResult<Json<Vec<AlertDelivery>>> {
    let context = require_alerts_read(&state, &headers).await?;
    let limit = clamp_delivery_limit(query.limit);
    let alerts = &state.alerts;

    if let Some(rule_id) = query.rule_id.as_deref() {
        let rule_id = validate_path_rule_id(rule_id)?;
        authorized_alert_rule(alerts, &context, rule_id, ALERTS_READ_PERMISSION)?;
        return Ok(Json(alerts.list_alert_deliveries(Some(rule_id), limit)?));
    }

    let allowed_rule_ids = alerts
        .get_alert_rules()?
        .into_iter()
        .filter(|rule| require_alert_rule_access(&context, rule, ALERTS_READ_PERMISSION).is_ok())
        .map(|rule| rule.id)
        .collect::<Vec<_>>();
    let deliveries = alerts.list_authorized_deliveries(&allowed_rule_ids, limit)?;
    Ok(Json(deliveries))
}

pub async fn test_alert_rule(
    State(state): State<AppState>,
    Path(rule_id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Json<()>> {
    let context = require_alerts_write(&state, &headers).await?;
    let admin_user_id = context.user_id;
    let rule_id = validate_path_rule_id(&rule_id)?.to_string();
    authorized_alert_rule(&state.alerts, &context, &rule_id, ALERTS_WRITE_PERMISSION)?;
    let alerts = &state.alerts;
    match deliver_webhook(
        alerts,
        &state.client,
        &rule_id,
        DEFAULT_TEST_PAYLOAD,
        "test delivery success",
    )
    .await
    {
        Ok(()) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.test",
                &format!("测试告警规则 Webhook {}", rule_id),
                "success",
            );
            Ok(Json(()))
        }
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "alerts.rule.test",
                &format!("测试告警规则 Webhook 失败：{}", safe_error_message(&error)),
                "failed",
            );
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActivityLogStore, BlockLogStore, Config, CryptoKeys, DirectLinkCache, FileLogStore,
        PASSWORD_TASK_CONCURRENCY_LIMIT, PlaybackPathState, Semaphore, SettingsStore, StdRwLock,
        TelemetryWriteQueue, USER_PASSWORD_RESET_CONCURRENCY_LIMIT,
        alerts::{DEFAULT_OPERATOR, MAX_EVALUATION_WINDOW_SECS, MAX_SERVER_ID_LEN},
        alerts_runtime::AlertRuntime,
        build_http_client, build_proxy_http_client,
        connectivity::ConnectivityMonitor,
        db::{AdminServerGrant, AdminServerGrantInput},
        ip_location::IpLocationStore,
        playback_user_cache,
    };
    use axum::http::header;
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use rand::RngCore;
    use sha2::{Digest, Sha256};
    use std::{
        collections::HashMap,
        fs,
        path::PathBuf,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };
    use tokio::{
        io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        sync::{Mutex, RwLock},
        time::timeout,
    };
    use tokio_rustls::{
        TlsAcceptor,
        rustls::{
            self,
            pki_types::{CertificateDer, PrivatePkcs8KeyDer, pem::PemObject},
        },
    };

    fn assert_safe_delivery(
        delivery: &AlertDelivery,
        expected_status: &str,
        expected_msg_contains: &str,
        payload: &str,
    ) {
        assert_eq!(delivery.status, expected_status);
        assert!(
            delivery.message.contains(expected_msg_contains),
            "message={} expected to contain {}",
            delivery.message,
            expected_msg_contains
        );
        assert!(!delivery.message.contains(payload));
        assert!(!delivery.message.contains("http://"));
        assert!(!delivery.message.contains("https://"));
        assert!(!delivery.message.contains("@"));
        assert!(!delivery.message.contains("user:"));
        assert!(!delivery.message.contains("Authorization"));
        assert!(delivery.message.len() <= MAX_DELIVERY_MESSAGE_LEN);
    }

    fn assert_validation_error(err: &AppError, expected_msg: &str) {
        match err {
            AppError::Validation(msg) => assert_eq!(msg, expected_msg),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    fn assert_badgateway_error(err: &AppError, expected_msg_contains: &str) {
        match err {
            AppError::BadGateway(msg) => assert!(
                msg.contains(expected_msg_contains),
                "msg={msg} expected {expected_msg_contains}"
            ),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    fn update_request(
        rule_config: Option<&str>,
        clear_rule_config: bool,
    ) -> UpdateAlertRuleRequest {
        UpdateAlertRuleRequest {
            name: "updated".into(),
            enabled: true,
            rule_config: rule_config.map(str::to_string),
            clear_rule_config,
            threshold: "2".into(),
            silence_period: 1,
            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
            id: None,
            created_at: None,
        }
    }

    fn temp_db_path() -> String {
        let mut bytes = [0_u8; 8];
        rand::rng().fill_bytes(&mut bytes);
        format!("/tmp/alerts_api_test_{}.db", u64::from_le_bytes(bytes))
    }

    fn cleanup_temp_db(db_path: &str) {
        for suffix in ["", ".key", "-wal", "-shm"] {
            let _ = fs::remove_file(format!("{db_path}{suffix}"));
        }
    }

    struct AlertsAuthFixture {
        store: Option<SettingsStore>,
        state: Option<AppState>,
        db_path: PathBuf,
        read_session: String,
        write_session: String,
        read_token: String,
        write_token: String,
    }

    impl AlertsAuthFixture {
        fn new() -> Self {
            let db_path = PathBuf::from(temp_db_path());
            let store = SettingsStore::open_for_test(db_path.clone()).unwrap();

            let read_role = store
                .create_admin_role_for_test("alerts-reader", "Alerts Reader")
                .unwrap();
            store
                .replace_role_permissions(read_role, &[ALERTS_READ_PERMISSION.to_string()])
                .unwrap();
            let write_role = store
                .create_admin_role_for_test("alerts-writer", "Alerts Writer")
                .unwrap();
            store
                .replace_role_permissions(write_role, &[ALERTS_WRITE_PERMISSION.to_string()])
                .unwrap();
            let full_role = store
                .create_admin_role_for_test("alerts-full", "Alerts Full")
                .unwrap();
            store
                .replace_role_permissions(
                    full_role,
                    &[
                        ALERTS_READ_PERMISSION.to_string(),
                        ALERTS_WRITE_PERMISSION.to_string(),
                    ],
                )
                .unwrap();

            let reader_id = store
                .create_admin_with_roles("alerts-reader", "hash", None, &["alerts-reader"], None)
                .unwrap();
            let writer_id = store
                .create_admin_with_roles("alerts-writer", "hash", None, &["alerts-writer"], None)
                .unwrap();
            let token_owner_id = store
                .create_admin_with_roles("alerts-token", "hash", None, &["alerts-full"], None)
                .unwrap();
            let alert_grants = [AdminServerGrantInput {
                server_id: "srv-alerts".to_string(),
                scope: "full".to_string(),
            }];
            for admin_user_id in [reader_id, writer_id, token_owner_id] {
                store
                    .replace_admin_server_grants(
                        admin_user_id,
                        &alert_grants,
                        None,
                        test_now_seconds(),
                    )
                    .unwrap();
            }

            let read_session = "alerts-read-session".to_string();
            let write_session = "alerts-write-session".to_string();
            store
                .create_session(
                    reader_id,
                    &test_token_hash(&read_session),
                    test_now_seconds() + 3600,
                )
                .unwrap();
            store
                .create_session(
                    writer_id,
                    &test_token_hash(&write_session),
                    test_now_seconds() + 3600,
                )
                .unwrap();

            let read_token = "alerts-read-api-token".to_string();
            let write_token = "alerts-write-api-token".to_string();
            store
                .create_admin_api_token(
                    token_owner_id,
                    "read alerts",
                    "read",
                    &test_token_hash(&read_token),
                    &[ALERTS_READ_PERMISSION.to_string()],
                    None,
                    None,
                    Some(token_owner_id),
                    test_now_seconds(),
                )
                .unwrap();
            store
                .create_admin_api_token(
                    token_owner_id,
                    "write alerts",
                    "write",
                    &test_token_hash(&write_token),
                    &[ALERTS_WRITE_PERMISSION.to_string()],
                    None,
                    None,
                    Some(token_owner_id),
                    test_now_seconds(),
                )
                .unwrap();

            let config = Config {
                emby_host: "http://127.0.0.1:8096".to_string(),
                emby_api_key: "test".to_string(),
                servers: vec![],
                openlist_addr: Some("http://127.0.0.1:8097".to_string()),
                openlist_token: Some("test".to_string()),
                port: 8096,
                cache_ttl_seconds: 180,
                cache_max_capacity: 10_000,
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
            let alerts = Alerts::new(db_path.to_str().unwrap()).unwrap();
            let state = AppState {
                config: Arc::new(RwLock::new(config)),
                config_updates: Arc::new(Mutex::new(())),
                client,
                proxy_client,
                cache: Arc::new(RwLock::new(DirectLinkCache::new(true, 180, 10_000))),
                file_log: Arc::new(FileLogStore::new(&store)),
                settings_store: store.clone(),
                telemetry_writes,
                alerts,
                alert_runtime: AlertRuntime::disabled(),
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
                playback_media: crate::playback_media_cache(),
                playback_rate_hits: Arc::new(Mutex::new(HashMap::new())),
                playback_rate_recent_events: Arc::new(Mutex::new(HashMap::new())),
                playback_rate_bans: Arc::new(Mutex::new(HashMap::new())),
                playback_rate_ip_bans: Arc::new(Mutex::new(HashMap::new())),
                login_failures: Arc::new(Mutex::new(HashMap::new())),
                update_check: Arc::new(Mutex::new(None)),
            };

            Self {
                store: Some(store),
                state: Some(state),
                db_path,
                read_session,
                write_session,
                read_token,
                write_token,
            }
        }

        fn state(&self) -> &AppState {
            self.state.as_ref().expect("state already dropped")
        }

        fn store(&self) -> &SettingsStore {
            self.store.as_ref().expect("store already dropped")
        }
    }

    impl Drop for AlertsAuthFixture {
        fn drop(&mut self) {
            if let Some(state) = self.state.take() {
                let _ = state.telemetry_writes.shutdown();
                drop(state);
            }
            let _ = self.store.take();
            cleanup_temp_db(self.db_path.to_str().unwrap());
        }
    }

    fn test_now_seconds() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64
    }

    fn test_token_hash(token: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
    }

    fn bearer_headers(token: &str) -> HeaderMap {
        HeaderMap::from_iter([(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        )])
    }

    fn invalid_encrypted_request() -> EncryptedRequest {
        EncryptedRequest {
            encrypted_key: "invalid".to_string(),
            fields: HashMap::new(),
        }
    }

    fn permission_test_rule(id: &str) -> AlertRule {
        AlertRule {
            id: id.to_string(),
            name: "permission guard".to_string(),
            enabled: true,
            rule_config: String::new(),
            threshold: "1".to_string(),
            silence_period: 0,
            created_at: now_ms(),
            metric: String::new(),
            operator: DEFAULT_OPERATOR.to_string(),
            server_id: "srv-alerts".to_string(),
            evaluation_window_secs: 0,
        }
    }

    fn assert_missing_permission<T>(result: AppResult<T>, permission: &str) {
        match result {
            Err(AppError::Forbidden(message)) => {
                assert_eq!(message, format!("missing permission: {permission}"));
                use axum::response::IntoResponse;
                assert_eq!(
                    AppError::Forbidden(message).into_response().status(),
                    axum::http::StatusCode::FORBIDDEN
                );
            }
            Err(other) => panic!("expected missing permission, got {other:?}"),
            Ok(_) => panic!("expected missing permission"),
        }
    }

    fn assert_alert_rule_access_denied<T>(result: AppResult<T>) {
        match result {
            Err(AppError::Forbidden(message)) => {
                assert_eq!(message, ALERT_RULE_ACCESS_DENIED);
                use axum::response::IntoResponse;
                assert_eq!(
                    AppError::Forbidden(message).into_response().status(),
                    axum::http::StatusCode::FORBIDDEN
                );
            }
            Err(other) => panic!("expected alert rule access denial, got {other:?}"),
            Ok(_) => panic!("expected alert rule access denial"),
        }
    }

    fn synthetic_alert_context(
        kind: auth::AuthKind,
        super_admin: bool,
        grants: &[&str],
        token_servers: Option<&[&str]>,
    ) -> auth::AuthContext {
        auth::AuthContext {
            user_id: 1,
            kind,
            scopes: Some(vec![
                ALERTS_READ_PERMISSION.to_string(),
                ALERTS_WRITE_PERMISSION.to_string(),
            ]),
            server_ids: token_servers.map(|ids| ids.iter().map(|id| (*id).to_string()).collect()),
            role_keys: if super_admin {
                vec!["super_admin".to_string()]
            } else {
                vec!["restricted_admin".to_string()]
            },
            permission_keys: vec![
                ALERTS_READ_PERMISSION.to_string(),
                ALERTS_WRITE_PERMISSION.to_string(),
            ],
            server_grants: grants
                .iter()
                .map(|server_id| AdminServerGrant {
                    server_id: (*server_id).to_string(),
                    scope: "full".to_string(),
                    granted_at: 0,
                })
                .collect(),
        }
    }

    async fn read_http_request<S>(stream: &mut S, expected_body: &str) -> tokio::io::Result<()>
    where
        S: AsyncRead + Unpin,
    {
        let res = timeout(Duration::from_secs(5), async {
            let mut header = Vec::new();
            let (header_end, body_prefix) = loop {
                let mut b = [0u8; 1024];
                let n = stream.read(&mut b).await?;
                if n == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "eof",
                    ));
                }
                header.extend_from_slice(&b[..n]);
                if let Some(pos) = header.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header_end = pos + 4;
                    if header_end > 16 * 1024 {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "header too large",
                        ));
                    }
                    let body_prefix = header[header_end..].to_vec();
                    break (header_end, body_prefix);
                }
                if header.len() > 16 * 1024 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "header too large",
                    ));
                }
            };
            let header_str = std::str::from_utf8(&header[..header_end]).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid utf8 header")
            })?;
            // Ensure no Authorization header is sent by test delivery.
            for line in header_str.lines() {
                if line.to_ascii_lowercase().starts_with("authorization:") {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "unexpected authorization header",
                    ));
                }
            }
            let mut cl_value = None;
            for line in header_str.lines() {
                let Some((name, value)) = line.split_once(':') else {
                    continue;
                };
                if name.eq_ignore_ascii_case("content-length") {
                    cl_value = Some(value.trim().to_string());
                }
            }
            let cl_value = cl_value.ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "no content-length")
            })?;
            let body_len = cl_value.parse::<usize>().map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "bad content-length")
            })?;
            if body_len != expected_body.as_bytes().len() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "body length mismatch",
                ));
            }
            let mut body = vec![0u8; body_len];
            let mut filled = 0usize;
            if !body_prefix.is_empty() {
                let copy = body_prefix.len().min(body_len);
                body[..copy].copy_from_slice(&body_prefix[..copy]);
                filled = copy;
            }
            while filled < body_len {
                let n = stream.read(&mut body[filled..]).await?;
                if n == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "body eof",
                    ));
                }
                filled += n;
            }
            if &body[..] != expected_body.as_bytes() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "body mismatch",
                ));
            }
            Ok(())
        })
        .await;
        match res {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "read timeout",
            )),
        }
    }

    #[tokio::test]
    async fn alert_handlers_keep_missing_and_invalid_authentication_unauthorized() {
        use axum::{http::StatusCode, response::IntoResponse};
        let fixture = AlertsAuthFixture::new();
        for headers in [HeaderMap::new(), bearer_headers("invalid-alert-credential")] {
            let error = list_alert_rules(State(fixture.state().clone()), headers)
                .await
                .unwrap_err();
            assert!(matches!(&error, AppError::Unauthorized(_)));
            assert_eq!(error.into_response().status(), StatusCode::UNAUTHORIZED);
        }
    }

    #[tokio::test]
    async fn alert_read_handlers_require_alerts_read_for_sessions_and_tokens() {
        let fixture = AlertsAuthFixture::new();
        let state = fixture.state().clone();
        state
            .alerts
            .create_alert_rule(permission_test_rule("read-rule"))
            .unwrap();

        let Json(rules) =
            list_alert_rules(State(state.clone()), bearer_headers(&fixture.read_session))
                .await
                .unwrap();
        assert_eq!(rules.len(), 1);
        assert_missing_permission(
            list_alert_rules(State(state.clone()), bearer_headers(&fixture.write_session)).await,
            ALERTS_READ_PERMISSION,
        );

        let Json(rule) = get_alert_rule(
            State(state.clone()),
            Path("read-rule".to_string()),
            bearer_headers(&fixture.read_session),
        )
        .await
        .unwrap();
        assert_eq!(rule.id, "read-rule");
        assert!(!rule.rule_configured);
        assert_missing_permission(
            get_alert_rule(
                State(state.clone()),
                Path("read-rule".to_string()),
                bearer_headers(&fixture.write_session),
            )
            .await,
            ALERTS_READ_PERMISSION,
        );
        let error = get_alert_rule(
            State(state.clone()),
            Path("missing-rule".to_string()),
            bearer_headers(&fixture.read_token),
        )
        .await
        .unwrap_err();
        assert_validation_error(&error, "alert rule not found");

        let Json(history) = list_alert_rule_history(
            State(state.clone()),
            Path("read-rule".to_string()),
            bearer_headers(&fixture.read_token),
            Query(AlertDeliveriesQuery {
                rule_id: None,
                limit: Some(10),
            }),
        )
        .await
        .unwrap();
        assert!(history.is_empty());
        assert_missing_permission(
            list_alert_rule_history(
                State(state.clone()),
                Path("read-rule".to_string()),
                bearer_headers(&fixture.write_token),
                Query(AlertDeliveriesQuery {
                    rule_id: None,
                    limit: Some(10),
                }),
            )
            .await,
            ALERTS_READ_PERMISSION,
        );

        let Json(deliveries) = list_alert_deliveries(
            State(state.clone()),
            bearer_headers(&fixture.read_token),
            Query(AlertDeliveriesQuery {
                rule_id: Some("read-rule".to_string()),
                limit: Some(10),
            }),
        )
        .await
        .unwrap();
        assert!(deliveries.is_empty());
        assert_missing_permission(
            list_alert_deliveries(
                State(state),
                bearer_headers(&fixture.write_token),
                Query(AlertDeliveriesQuery {
                    rule_id: Some("read-rule".to_string()),
                    limit: Some(10),
                }),
            )
            .await,
            ALERTS_READ_PERMISSION,
        );
    }

    #[tokio::test]
    async fn alert_write_handlers_deny_before_decryption_queries_audit_and_side_effects() {
        let fixture = AlertsAuthFixture::new();
        let state = fixture.state().clone();
        state
            .alerts
            .create_alert_rule(permission_test_rule("guarded-rule"))
            .unwrap();
        let audit_count = fixture
            .store()
            .list_audit_logs(None, None, 1000)
            .unwrap()
            .len();

        assert_missing_permission(
            create_alert_rule(
                State(state.clone()),
                bearer_headers(&fixture.read_token),
                Json(invalid_encrypted_request()),
            )
            .await,
            ALERTS_WRITE_PERMISSION,
        );
        assert_missing_permission(
            update_alert_rule(
                State(state.clone()),
                Path("guarded-rule".to_string()),
                bearer_headers(&fixture.read_session),
                Json(invalid_encrypted_request()),
            )
            .await,
            ALERTS_WRITE_PERMISSION,
        );
        assert_missing_permission(
            toggle_alert_rule(
                State(state.clone()),
                Path("guarded-rule".to_string()),
                bearer_headers(&fixture.read_session),
                Json(ToggleAlertRuleRequest { enabled: false }),
            )
            .await,
            ALERTS_WRITE_PERMISSION,
        );
        assert_missing_permission(
            set_alert_rule_silent(
                State(state.clone()),
                Path("guarded-rule".to_string()),
                bearer_headers(&fixture.read_token),
                Json(SilentAlertRuleRequest {
                    until: (now_ms() + 60_000).to_string(),
                }),
            )
            .await,
            ALERTS_WRITE_PERMISSION,
        );
        assert_missing_permission(
            delete_alert_rule(
                State(state.clone()),
                Path("guarded-rule".to_string()),
                bearer_headers(&fixture.read_token),
            )
            .await,
            ALERTS_WRITE_PERMISSION,
        );
        assert_missing_permission(
            test_alert_rule(
                State(state.clone()),
                Path("guarded-rule".to_string()),
                bearer_headers(&fixture.read_token),
            )
            .await,
            ALERTS_WRITE_PERMISSION,
        );

        let rule = state
            .alerts
            .get_alert_rule("guarded-rule")
            .unwrap()
            .unwrap();
        assert_eq!(rule.name, "permission guard");
        assert!(rule.enabled);
        assert!(!state.alerts.is_rule_silenced("guarded-rule").unwrap());
        assert!(
            state
                .alerts
                .list_alert_deliveries(Some("guarded-rule"), 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            fixture
                .store()
                .list_audit_logs(None, None, 1000)
                .unwrap()
                .len(),
            audit_count
        );
    }

    #[tokio::test]
    async fn alert_write_handlers_accept_alerts_write_session_and_token() {
        let fixture = AlertsAuthFixture::new();
        let state = fixture.state().clone();
        state
            .alerts
            .create_alert_rule(permission_test_rule("write-rule"))
            .unwrap();

        let error = create_alert_rule(
            State(state.clone()),
            bearer_headers(&fixture.write_token),
            Json(invalid_encrypted_request()),
        )
        .await
        .unwrap_err();
        assert_validation_error(&error, "invalid encrypted alert rule request");

        let error = update_alert_rule(
            State(state.clone()),
            Path(String::new()),
            bearer_headers(&fixture.write_session),
            Json(invalid_encrypted_request()),
        )
        .await
        .unwrap_err();
        assert_validation_error(&error, "invalid rule id");

        let _ = toggle_alert_rule(
            State(state.clone()),
            Path("write-rule".to_string()),
            bearer_headers(&fixture.write_session),
            Json(ToggleAlertRuleRequest { enabled: false }),
        )
        .await
        .unwrap();
        assert!(
            !state
                .alerts
                .get_alert_rule("write-rule")
                .unwrap()
                .unwrap()
                .enabled
        );

        let _ = set_alert_rule_silent(
            State(state.clone()),
            Path("write-rule".to_string()),
            bearer_headers(&fixture.write_token),
            Json(SilentAlertRuleRequest {
                until: (now_ms() + 60_000).to_string(),
            }),
        )
        .await
        .unwrap();
        assert!(state.alerts.is_rule_silenced("write-rule").unwrap());

        let error = test_alert_rule(
            State(state.clone()),
            Path("write-rule".to_string()),
            bearer_headers(&fixture.write_token),
        )
        .await
        .unwrap_err();
        assert_validation_error(&error, "no webhook URL configured");
        assert_eq!(
            state
                .alerts
                .list_alert_deliveries(Some("write-rule"), 10)
                .unwrap()
                .len(),
            1
        );

        let _ = delete_alert_rule(
            State(state.clone()),
            Path("write-rule".to_string()),
            bearer_headers(&fixture.write_token),
        )
        .await
        .unwrap();
        assert!(state.alerts.get_alert_rule("write-rule").unwrap().is_none());
    }

    #[test]
    fn alert_rule_authorization_matrix_is_fail_closed() {
        let global_super_session =
            synthetic_alert_context(auth::AuthKind::Session, true, &[], None);
        assert!(has_alert_rule_server_access(&global_super_session, ""));
        assert!(
            require_alert_rule_server_access(&global_super_session, "", ALERTS_READ_PERMISSION,)
                .is_ok()
        );
        assert!(
            require_alert_rule_server_access(&global_super_session, "", ALERTS_WRITE_PERMISSION,)
                .is_ok()
        );

        for ordinary in [
            synthetic_alert_context(auth::AuthKind::Session, false, &[], None),
            synthetic_alert_context(auth::AuthKind::Session, false, &["srv-alerts"], None),
        ] {
            assert!(!has_alert_rule_server_access(&ordinary, ""));
            assert_alert_rule_access_denied(require_alert_rule_server_access(
                &ordinary,
                "",
                ALERTS_READ_PERMISSION,
            ));
        }

        let restricted_token = synthetic_alert_context(
            auth::AuthKind::ApiToken,
            true,
            &["srv-alerts"],
            Some(&["srv-alerts"]),
        );
        assert!(!has_alert_rule_server_access(&restricted_token, ""));
        assert_alert_rule_access_denied(require_alert_rule_server_access(
            &restricted_token,
            "",
            ALERTS_READ_PERMISSION,
        ));

        let scoped_session =
            synthetic_alert_context(auth::AuthKind::Session, false, &["srv-alerts"], None);
        assert!(has_alert_rule_server_access(&scoped_session, "srv-alerts"));
        assert!(!has_alert_rule_server_access(&scoped_session, "srv-other"));
        assert_alert_rule_access_denied(require_alert_rule_server_access(
            &scoped_session,
            "srv-other",
            ALERTS_WRITE_PERMISSION,
        ));

        let new_rule = build_rule_from_create(CreateAlertRuleRequest {
            name: "new scoped rule".to_string(),
            enabled: true,
            rule_config: String::new(),
            threshold: "1".to_string(),
            silence_period: 0,
            metric: String::new(),
            operator: DEFAULT_OPERATOR.to_string(),
            server_id: "srv-other".to_string(),
            evaluation_window_secs: 0,
            id: None,
            created_at: None,
        })
        .unwrap();
        assert_alert_rule_access_denied(require_alert_rule_server_access(
            &scoped_session,
            &new_rule.server_id,
            ALERTS_WRITE_PERMISSION,
        ));

        let existing_rule = AlertRuleView {
            id: "existing-rule".to_string(),
            name: "existing scoped rule".to_string(),
            enabled: true,
            rule_configured: false,
            threshold: "1".to_string(),
            silence_period: 0,
            created_at: now_ms(),
            metric: String::new(),
            operator: DEFAULT_OPERATOR.to_string(),
            server_id: "srv-alerts".to_string(),
            evaluation_window_secs: 0,
        };
        assert!(
            require_alert_rule_access(&scoped_session, &existing_rule, ALERTS_WRITE_PERMISSION,)
                .is_ok()
        );
        let (migrated_rule, _) = build_rule_from_update(
            &existing_rule.id,
            UpdateAlertRuleRequest {
                name: existing_rule.name.clone(),
                enabled: existing_rule.enabled,
                rule_config: None,
                clear_rule_config: false,
                threshold: existing_rule.threshold.clone(),
                silence_period: existing_rule.silence_period,
                metric: existing_rule.metric.clone(),
                operator: existing_rule.operator.clone(),
                server_id: "srv-other".to_string(),
                evaluation_window_secs: existing_rule.evaluation_window_secs,
                id: None,
                created_at: None,
            },
            &existing_rule,
        )
        .unwrap();
        assert_alert_rule_access_denied(require_alert_rule_server_access(
            &scoped_session,
            &migrated_rule.server_id,
            ALERTS_WRITE_PERMISSION,
        ));

        let intersected_token = synthetic_alert_context(
            auth::AuthKind::ApiToken,
            false,
            &["srv-alerts", "srv-account-only"],
            Some(&["srv-alerts", "srv-token-only"]),
        );
        assert!(has_alert_rule_server_access(
            &intersected_token,
            "srv-alerts"
        ));
        assert!(!has_alert_rule_server_access(
            &intersected_token,
            "srv-account-only"
        ));
        assert!(!has_alert_rule_server_access(
            &intersected_token,
            "srv-token-only"
        ));

        let inherited_token =
            synthetic_alert_context(auth::AuthKind::ApiToken, false, &["srv-account-only"], None);
        assert!(has_alert_rule_server_access(
            &inherited_token,
            "srv-account-only"
        ));
        let empty_scope_token = synthetic_alert_context(
            auth::AuthKind::ApiToken,
            false,
            &["srv-account-only"],
            Some(&[]),
        );
        assert!(!has_alert_rule_server_access(
            &empty_scope_token,
            "srv-account-only"
        ));
    }

    #[tokio::test]
    async fn alert_rule_server_grants_filter_and_guard_every_rule_path() {
        let fixture = AlertsAuthFixture::new();
        let state = fixture.state().clone();
        state
            .alerts
            .create_alert_rule(permission_test_rule("granted-rule"))
            .unwrap();
        let mut foreign_rule = permission_test_rule("foreign-rule");
        foreign_rule.server_id = "srv-foreign".to_string();
        state.alerts.create_alert_rule(foreign_rule).unwrap();
        state
            .alerts
            .record_test_delivery("granted-rule", "success", "visible delivery")
            .unwrap();
        state
            .alerts
            .record_test_delivery("foreign-rule", "success", "hidden delivery")
            .unwrap();

        let Json(rules) =
            list_alert_rules(State(state.clone()), bearer_headers(&fixture.read_session))
                .await
                .unwrap();
        assert_eq!(
            rules
                .iter()
                .map(|rule| rule.id.as_str())
                .collect::<Vec<_>>(),
            ["granted-rule"]
        );
        let Json(deliveries) = list_alert_deliveries(
            State(state.clone()),
            bearer_headers(&fixture.read_session),
            Query(AlertDeliveriesQuery {
                rule_id: None,
                limit: Some(10),
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            deliveries
                .iter()
                .map(|delivery| delivery.rule_id.as_str())
                .collect::<Vec<_>>(),
            ["granted-rule"]
        );
        assert!(
            deliveries
                .iter()
                .all(|delivery| !delivery.message.contains("hidden delivery"))
        );

        assert_alert_rule_access_denied(
            get_alert_rule(
                State(state.clone()),
                Path("foreign-rule".to_string()),
                bearer_headers(&fixture.read_session),
            )
            .await,
        );
        assert_alert_rule_access_denied(
            list_alert_rule_history(
                State(state.clone()),
                Path("foreign-rule".to_string()),
                bearer_headers(&fixture.read_session),
                Query(AlertDeliveriesQuery {
                    rule_id: None,
                    limit: Some(10),
                }),
            )
            .await,
        );
        assert_alert_rule_access_denied(
            list_alert_deliveries(
                State(state.clone()),
                bearer_headers(&fixture.read_session),
                Query(AlertDeliveriesQuery {
                    rule_id: Some("foreign-rule".to_string()),
                    limit: Some(10),
                }),
            )
            .await,
        );

        assert_alert_rule_access_denied(
            update_alert_rule(
                State(state.clone()),
                Path("foreign-rule".to_string()),
                bearer_headers(&fixture.write_session),
                Json(invalid_encrypted_request()),
            )
            .await,
        );
        assert_alert_rule_access_denied(
            toggle_alert_rule(
                State(state.clone()),
                Path("foreign-rule".to_string()),
                bearer_headers(&fixture.write_session),
                Json(ToggleAlertRuleRequest { enabled: false }),
            )
            .await,
        );
        assert_alert_rule_access_denied(
            set_alert_rule_silent(
                State(state.clone()),
                Path("foreign-rule".to_string()),
                bearer_headers(&fixture.write_session),
                Json(SilentAlertRuleRequest {
                    until: (now_ms() + 60_000).to_string(),
                }),
            )
            .await,
        );
        assert_alert_rule_access_denied(
            delete_alert_rule(
                State(state.clone()),
                Path("foreign-rule".to_string()),
                bearer_headers(&fixture.write_session),
            )
            .await,
        );
        assert_alert_rule_access_denied(
            test_alert_rule(
                State(state.clone()),
                Path("foreign-rule".to_string()),
                bearer_headers(&fixture.write_session),
            )
            .await,
        );

        let foreign = state
            .alerts
            .get_alert_rule("foreign-rule")
            .unwrap()
            .unwrap();
        assert!(foreign.enabled);
        let foreign_deliveries = state
            .alerts
            .list_alert_deliveries(Some("foreign-rule"), 10)
            .unwrap();
        assert_eq!(foreign_deliveries.len(), 1);
        assert_eq!(foreign_deliveries[0].message, "hidden delivery");
    }

    #[test]
    fn parse_silent_until_compat_and_bounds() {
        for clear in ["", " 0 ", "NULL", " none "] {
            let (silent, until) = parse_silent_until(clear).unwrap();
            assert!(!silent);
            assert!(until.is_none());
        }

        assert_validation_error(
            &parse_silent_until("1").unwrap_err(),
            "silent until timestamp must be in the future",
        );

        let future = now_ms() + 60_000;
        let future_string = future.to_string();
        let (silent, until) = parse_silent_until(&future_string).unwrap();
        assert!(silent);
        assert_eq!(until, Some(future));

        assert_validation_error(
            &parse_silent_until("not-a-number").unwrap_err(),
            "invalid silent until timestamp",
        );
        assert_validation_error(
            &parse_silent_until(&"9".repeat(40)).unwrap_err(),
            "invalid silent until timestamp",
        );
        assert_validation_error(
            &parse_silent_until(&(i64::MAX as u128 + 1).to_string()).unwrap_err(),
            "silent until timestamp out of range",
        );
    }

    #[tokio::test]
    async fn set_alert_rule_silent_rejects_stale_timestamp_without_clearing_or_success_audit() {
        let fixture = AlertsAuthFixture::new();
        let state = fixture.state().clone();
        state
            .alerts
            .create_alert_rule(permission_test_rule("stale-silence-rule"))
            .unwrap();

        let future = now_ms() + 60_000;
        let _ = set_alert_rule_silent(
            State(state.clone()),
            Path("stale-silence-rule".to_string()),
            bearer_headers(&fixture.write_token),
            Json(SilentAlertRuleRequest {
                until: future.to_string(),
            }),
        )
        .await
        .unwrap();
        let before = state.alerts.get_alert_state("stale-silence-rule").unwrap();
        let success_audits_before = fixture
            .store()
            .list_audit_logs(Some("alerts.rule.silent"), None, 1000)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.result == "success")
            .count();

        let error = set_alert_rule_silent(
            State(state.clone()),
            Path("stale-silence-rule".to_string()),
            bearer_headers(&fixture.write_token),
            Json(SilentAlertRuleRequest {
                until: "1".to_string(),
            }),
        )
        .await
        .unwrap_err();
        assert_validation_error(&error, "silent until timestamp must be in the future");

        let after = state.alerts.get_alert_state("stale-silence-rule").unwrap();
        assert_eq!(after.is_silent, before.is_silent);
        assert_eq!(after.silent_until_ms, before.silent_until_ms);
        let success_audits_after = fixture
            .store()
            .list_audit_logs(Some("alerts.rule.silent"), None, 1000)
            .unwrap()
            .into_iter()
            .filter(|entry| entry.result == "success")
            .count();
        assert_eq!(success_audits_after, success_audits_before);
    }

    #[test]
    fn create_request_ignores_client_id_and_created_at() {
        let req = CreateAlertRuleRequest {
            name: "  demo  ".into(),
            enabled: true,
            rule_config: "http://127.0.0.1:9/h".into(),
            threshold: " 5 ".into(),
            silence_period: 10,
            id: Some("client-forged-id".into()),
            created_at: Some(123),

            metric: String::new(),
            operator: String::new(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        };
        let rule = build_rule_from_create(req).unwrap();
        assert_ne!(rule.id, "client-forged-id");
        assert!(rule.id.starts_with("alert-"));
        assert_ne!(rule.created_at, 123);
        assert_eq!(rule.name, "demo");
        assert_eq!(rule.threshold, "5");
    }

    #[tokio::test]
    async fn test_real_alert_delivery_success_loopback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = r#"{"ok":true}"#;
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_http_request(&mut stream, payload).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .unwrap();
        });

        let db_path = temp_db_path();
        let db = Alerts::new(&db_path).unwrap();
        let rule = AlertRule {
            id: "ok-rule".into(),
            name: "ok".into(),
            enabled: true,
            rule_config: format!("http://{addr}/hook"),
            threshold: "1".into(),
            silence_period: 0,
            created_at: now_ms(),

            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        };
        db.create_alert_rule(rule).unwrap();
        let api = AlertsApi::new(db);
        api.test_alert_delivery("ok-rule", payload).await.unwrap();
        let deliveries = api.list_alert_deliveries(Some("ok-rule")).await.unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_safe_delivery(&deliveries[0], "success", "test delivery success", payload);
        assert!(deliveries[0].retry_count <= MAX_RETRY_COUNT);
        server.await.unwrap();
        cleanup_temp_db(&db_path);
    }

    /// Use only a loopback TLS mock and an embedded test certificate. The
    /// explicit-root client must deliver, while the default client must reject
    /// the same self-signed certificate without ever falling back to HTTP.
    #[tokio::test]
    async fn test_alert_delivery_https_explicit_trust_and_untrusted_failure() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let cert_pem = include_bytes!("test-fixtures/alerts_tls_cert.pem");
        let key_pem = include_bytes!("test-fixtures/alerts_tls_key.pem");
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_slice(cert_pem).expect("parse test certificate")],
                PrivatePkcs8KeyDer::from_pem_slice(key_pem)
                    .expect("parse test private key")
                    .into(),
            )
            .expect("build local TLS server");
        let tls_acceptor = TlsAcceptor::from(Arc::new(server_config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        assert!(addr.ip().is_loopback());
        let expected_payload = r#"{"kind":"tls-evidence","secret":"must-not-be-recorded"}"#;
        let server = tokio::spawn(async move {
            let mut successful_requests = 0usize;
            let mut handshake_failures = 0usize;
            let mut plaintext_http_requests = 0usize;
            // One trusted request plus four failed default-verification retries.
            for _ in 0..=(MAX_RETRY_COUNT as usize + 1) {
                let (stream, _) = timeout(Duration::from_secs(5), listener.accept())
                    .await
                    .expect("local TLS mock accept timeout")
                    .expect("local TLS mock accept");
                let mut prefix = [0u8; 4];
                let prefix_len = timeout(Duration::from_secs(5), stream.peek(&mut prefix))
                    .await
                    .expect("local TLS mock read timeout")
                    .expect("local TLS mock read");
                if prefix[..prefix_len].starts_with(b"GET")
                    || prefix[..prefix_len].starts_with(b"POST")
                    || prefix[..prefix_len].starts_with(b"PUT")
                    || prefix[..prefix_len].starts_with(b"HEAD")
                {
                    plaintext_http_requests += 1;
                }
                match tls_acceptor.accept(stream).await {
                    Ok(mut stream) => {
                        read_http_request(&mut stream, expected_payload)
                            .await
                            .expect("trusted HTTPS request");
                        stream
                            .write_all(
                                b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            )
                            .await
                            .expect("write HTTPS response");
                        stream.shutdown().await.expect("close HTTPS response");
                        successful_requests += 1;
                    }
                    Err(_) => handshake_failures += 1,
                }
            }
            (
                successful_requests,
                handshake_failures,
                plaintext_http_requests,
            )
        });

        let trusted_cert = reqwest::Certificate::from_pem(cert_pem).unwrap();
        let trusted_client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .add_root_certificate(trusted_cert)
            .build()
            .expect("build explicitly trusted client");
        let untrusted_client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(5))
            .build()
            .expect("build default-verification client");

        let mut temp_suffix = [0_u8; 8];
        rand::rng().fill_bytes(&mut temp_suffix);
        let temp_dir = std::env::temp_dir().join(format!(
            "embypanel_alert_tls_{}",
            u64::from_le_bytes(temp_suffix)
        ));
        fs::create_dir(&temp_dir).expect("create TLS test temp directory");
        let db_path = temp_dir.join("alerts.db").to_string_lossy().into_owned();
        let db = Alerts::new(&db_path).unwrap();
        let url = format!("https://127.0.0.1:{}/hook", addr.port());
        db.create_alert_rule(AlertRule {
            id: "https-rule".into(),
            name: "https".into(),
            enabled: true,
            rule_config: url.clone(),
            threshold: "1".into(),
            silence_period: 0,
            created_at: now_ms(),
            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        })
        .unwrap();

        deliver_webhook(
            &db,
            &trusted_client,
            "https-rule",
            expected_payload,
            "trusted TLS delivery success",
        )
        .await
        .expect("explicitly trusted TLS delivery");
        let untrusted = deliver_webhook(
            &db,
            &untrusted_client,
            "https-rule",
            expected_payload,
            "untrusted TLS delivery success",
        )
        .await
        .expect_err("default verification must reject self-signed certificate");
        assert_badgateway_error(&untrusted, "network delivery failed");
        let untrusted_debug = format!("{untrusted:?}");
        assert!(!untrusted_debug.contains(&url));
        assert!(!untrusted_debug.contains("must-not-be-recorded"));

        let deliveries = db.list_alert_deliveries(Some("https-rule"), 10).unwrap();
        assert_eq!(deliveries.len(), 2);
        let trusted = deliveries
            .iter()
            .find(|delivery| delivery.status == "success")
            .expect("trusted TLS delivery history");
        assert_safe_delivery(
            trusted,
            "success",
            "trusted TLS delivery success",
            expected_payload,
        );
        let failed = deliveries
            .iter()
            .find(|delivery| delivery.status == "failed")
            .expect("untrusted TLS delivery history");
        assert_eq!(failed.retry_count, MAX_RETRY_COUNT);
        assert_safe_delivery(
            failed,
            "failed",
            "network delivery failed",
            expected_payload,
        );

        let (successful_requests, handshake_failures, plaintext_http_requests) =
            server.await.unwrap();
        assert_eq!(successful_requests, 1);
        assert_eq!(handshake_failures, MAX_RETRY_COUNT as usize + 1);
        assert_eq!(
            plaintext_http_requests, 0,
            "HTTPS delivery must not downgrade to HTTP"
        );
        cleanup_temp_db(&db_path);
        fs::remove_dir(&temp_dir).expect("remove TLS test temp directory");
    }

    #[tokio::test]
    async fn test_real_alert_delivery_non_2xx() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let payload = r#"{"ok":false}"#;
        let server = tokio::spawn(async move {
            // Keep accepting across retries so last_error stays the HTTP 500
            // summary, not a later connection-refused network failure.
            loop {
                let Ok(Ok((mut stream, _))) =
                    timeout(Duration::from_secs(10), listener.accept()).await
                else {
                    break;
                };
                match timeout(
                    Duration::from_secs(5),
                    read_http_request(&mut stream, payload),
                )
                .await
                {
                    Ok(Ok(())) => {
                        let _ = stream
                            .write_all(
                                b"HTTP/1.1 500 Internal Server Error
Content-Length: 0
Connection: close

",
                            )
                            .await;
                    }
                    _ => break,
                }
            }
        });

        let db_path = temp_db_path();
        let db = Alerts::new(&db_path).unwrap();
        let rule = AlertRule {
            id: "non2xx-rule".into(),
            name: "non2xx".into(),
            enabled: true,
            rule_config: format!("http://{addr}/hook"),
            threshold: "1".into(),
            silence_period: 0,
            created_at: now_ms(),

            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        };
        db.create_alert_rule(rule).unwrap();
        let api = AlertsApi::new(db);
        let res = api.test_alert_delivery("non2xx-rule", payload).await;
        assert_badgateway_error(&res.unwrap_err(), "webhook returned HTTP");
        let deliveries = api
            .list_alert_deliveries(Some("non2xx-rule"))
            .await
            .unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_safe_delivery(&deliveries[0], "failed", "webhook returned HTTP", payload);
        assert!(deliveries[0].message.contains("500") || deliveries[0].message.contains("HTTP"));
        let _ = server.await;
        cleanup_temp_db(&db_path);
    }

    #[tokio::test]
    async fn test_real_alert_delivery_userinfo_rejected() {
        let db_path = temp_db_path();
        let db = Alerts::new(&db_path).unwrap();
        let rule = AlertRule {
            id: "userinfo-rule".into(),
            name: "bad".into(),
            enabled: true,
            rule_config: "http://user:pass@example.com/hook".into(),
            threshold: "1".into(),
            silence_period: 0,
            created_at: now_ms(),

            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        };
        // create path validates URL and should reject
        let err = db.create_alert_rule(rule.clone()).expect_err("userinfo");
        match err {
            AppError::Validation(msg) => assert!(msg.contains("userinfo")),
            other => panic!("unexpected {other:?}"),
        }

        // Bypass create validation by writing raw config via temporary open path:
        // use a valid rule then force invalid config through update after loosening?
        // Instead insert via SQL-free path: create valid then update_alert_rule with invalid.
        let mut good = rule.clone();
        good.rule_config = "http://127.0.0.1:1/h".into();
        good.id = "userinfo-rule".into();
        db.create_alert_rule(good).unwrap();
        // Directly record invalid config by re-opening and updating storage isn't exposed;
        // exercise deliver path via parse on get: overwrite using update with invalid should fail.
        let bad = AlertRule {
            id: "userinfo-rule".into(),
            name: "bad".into(),
            enabled: true,
            rule_config: "http://user:pass@example.com/hook".into(),
            threshold: "1".into(),
            silence_period: 0,
            created_at: now_ms(),

            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        };
        let replacement = RuleConfigUpdate::Replace(bad.rule_config.clone());
        assert!(db.update_alert_rule(bad.clone(), replacement).is_err());

        // Simulate stored invalid config by creating with empty then manually? Keep unit on parse.
        let _ = bad;
        let api = AlertsApi::new(db);
        // existing good rule should still deliver network failure, not leak userinfo
        let res = api.test_alert_delivery("userinfo-rule", "unused").await;
        assert!(res.is_err());
        let deliveries = api
            .list_alert_deliveries(Some("userinfo-rule"))
            .await
            .unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_safe_delivery(
            &deliveries[0],
            "failed",
            "network delivery failed",
            "unused",
        );
        cleanup_temp_db(&db_path);
    }

    #[tokio::test]
    async fn test_real_alert_delivery_network_failure_bounded_retries() {
        let db_path = temp_db_path();
        let db = Alerts::new(&db_path).unwrap();
        let rule = AlertRule {
            id: "fail-rule".into(),
            name: "fail".into(),
            enabled: true,
            rule_config: "http://127.0.0.1:1".into(),
            threshold: "1".into(),
            silence_period: 0,
            created_at: now_ms(),

            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        };
        db.create_alert_rule(rule).unwrap();
        let api = AlertsApi::new(db);
        let started = std::time::Instant::now();
        let res = api.test_alert_delivery("fail-rule", "unused").await;
        assert_badgateway_error(&res.unwrap_err(), "network delivery failed");
        // Bounded retries should finish reasonably quickly (timeouts + small backoff).
        assert!(started.elapsed() < Duration::from_secs(40));

        let deliveries = api.list_alert_deliveries(Some("fail-rule")).await.unwrap();
        assert_eq!(deliveries.len(), 1);
        let d = &deliveries[0];
        assert_safe_delivery(d, "failed", "network delivery failed", "unused");
        assert_eq!(d.retry_count, MAX_RETRY_COUNT);
        cleanup_temp_db(&db_path);
    }

    #[tokio::test]
    async fn test_real_alert_delivery_invalid_config() {
        let db_path = temp_db_path();
        let db = Alerts::new(&db_path).unwrap();
        let rule = AlertRule {
            id: "bad-rule".into(),
            name: "bad".into(),
            enabled: true,
            rule_config: "".into(),
            threshold: "1".into(),
            silence_period: 0,
            created_at: now_ms(),

            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        };
        db.create_alert_rule(rule).unwrap();
        // Defense-in-depth: corrupt stored config outside API validation.
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute(
                "UPDATE alert_rules SET rule_config = ?1 WHERE id = ?2",
                rusqlite::params!["not a json or bad url", "bad-rule"],
            )
            .unwrap();
        }
        let listed = db.get_alert_rules().unwrap();
        let detail = db.get_alert_rule("bad-rule").unwrap().unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].rule_configured);
        assert!(detail.rule_configured);
        let api = AlertsApi::new(db);
        let res = api.test_alert_delivery("bad-rule", "unused").await;
        match res.unwrap_err() {
            AppError::Internal(msg) => {
                assert_eq!(msg, "alert rule configuration storage is unavailable")
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
        let deliveries = api.list_alert_deliveries(Some("bad-rule")).await.unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_safe_delivery(
            &deliveries[0],
            "failed",
            "webhook configuration unavailable",
            "unused",
        );
        cleanup_temp_db(&db_path);
    }

    #[tokio::test]
    async fn test_real_alert_delivery_missing_rule() {
        let db_path = temp_db_path();
        let db = Alerts::new(&db_path).unwrap();
        let api = AlertsApi::new(db);
        let res = api.test_alert_delivery("missing-rule", "unused").await;
        assert_validation_error(&res.unwrap_err(), "alert rule not found");
        let deliveries = api
            .list_alert_deliveries(Some("missing-rule"))
            .await
            .unwrap();
        assert_eq!(deliveries.len(), 0);
        cleanup_temp_db(&db_path);
    }

    #[tokio::test]
    async fn test_real_alert_delivery_empty_url() {
        let db_path = temp_db_path();
        let db = Alerts::new(&db_path).unwrap();
        let rule = AlertRule {
            id: "empty-rule".into(),
            name: "empty".into(),
            enabled: true,
            rule_config: "".into(),
            threshold: "1".into(),
            silence_period: 0,
            created_at: now_ms(),

            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        };
        db.create_alert_rule(rule).unwrap();
        let api = AlertsApi::new(db);
        let res = api.test_alert_delivery("empty-rule", "unused").await;
        assert_validation_error(&res.unwrap_err(), "no webhook URL configured");
        let deliveries = api.list_alert_deliveries(Some("empty-rule")).await.unwrap();
        assert_eq!(deliveries.len(), 1);
        assert_safe_delivery(
            &deliveries[0],
            "failed",
            "no webhook URL configured",
            "unused",
        );
        cleanup_temp_db(&db_path);
    }

    #[tokio::test]
    async fn test_silent_and_recovery_flow() {
        let db_path = temp_db_path();
        let db = Alerts::new(&db_path).unwrap();
        let rule = AlertRule {
            id: "silent-rule".into(),
            name: "silent".into(),
            enabled: true,
            rule_config: "http://127.0.0.1:1".into(),
            threshold: "1".into(),
            silence_period: 0,
            created_at: now_ms(),

            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        };
        db.create_alert_rule(rule).unwrap();
        // First trigger notifies once.
        assert!(db.trigger_alert("silent-rule").unwrap());
        let future = now_ms() + 3_600_000;
        db.update_silence("silent-rule", true, Some(future))
            .unwrap();
        assert!(db.is_rule_silenced("silent-rule").unwrap());
        // Sustained breach while manually silenced: no re-notify.
        assert!(!db.trigger_alert("silent-rule").unwrap());
        // Clear under manual silence: state returns to normal, recovered
        // notification suppressed, and silence is preserved (not auto-cleared).
        assert!(!db.mark_recovered("silent-rule").unwrap());
        assert!(db.is_rule_silenced("silent-rule").unwrap());
        let state = db.get_alert_state("silent-rule").unwrap();
        assert_eq!(state.status, crate::alerts::ALERT_STATUS_NORMAL);
        assert!(!state.notification_active);
        let deliveries = db.list_alert_deliveries(Some("silent-rule"), 10).unwrap();
        assert!(
            deliveries
                .iter()
                .any(|d| d.status == "success" && d.message == "alert triggered")
        );
        assert!(!deliveries.iter().any(|d| d.message == "alert recovered"));
        for d in &deliveries {
            assert!(!d.message.contains("http"));
        }
        cleanup_temp_db(&db_path);
    }

    #[test]
    fn dto_threshold_string_validated_for_length() {
        let req = CreateAlertRuleRequest {
            name: "n".into(),
            enabled: true,
            rule_config: String::new(),
            threshold: "x".repeat(MAX_THRESHOLD_LEN + 1),
            silence_period: 0,
            metric: String::new(),
            operator: String::new(),
            server_id: String::new(),
            evaluation_window_secs: 0,
            id: None,
            created_at: None,
        };
        assert!(build_rule_from_create(req).is_err());
    }

    #[test]
    fn dto_accepts_legacy_payload_defaults_and_metric_rule() {
        // Old client: no metric fields in JSON.
        let legacy: CreateAlertRuleRequest = serde_json::from_str(
            r#"{"name":"legacy","rule_config":"http://127.0.0.1:9/h","threshold":"free text","silence_period":10}"#,
        )
        .unwrap();
        let rule = build_rule_from_create(legacy).unwrap();
        assert_eq!(rule.metric, "");
        assert_eq!(rule.operator, DEFAULT_OPERATOR);
        assert_eq!(rule.server_id, "");
        assert_eq!(rule.evaluation_window_secs, 0);
        assert_eq!(rule.threshold, "free text");
        assert_eq!(rule.silence_period, 10);
        assert!(rule.id.starts_with("alert-"));
        assert!(rule.created_at > 0);

        // Metric rule requires numeric threshold and whitelist metric.
        let metric_req = CreateAlertRuleRequest {
            name: "m".into(),
            enabled: true,
            rule_config: "http://127.0.0.1:9/h".into(),
            threshold: "42.50".into(),
            silence_period: 5,
            metric: "proxy.errors".into(),
            operator: "gt".into(),
            server_id: " srv-a ".into(),
            evaluation_window_secs: 0,
            id: Some("client-id-ignored".into()),
            created_at: Some(1),
        };
        let rule = build_rule_from_create(metric_req).unwrap();
        assert_eq!(rule.metric, "proxy.errors");
        assert_eq!(rule.operator, "gt");
        assert_eq!(rule.server_id, "srv-a");
        assert_eq!(rule.threshold, "42.5");
        assert_ne!(rule.id, "client-id-ignored");
        assert_ne!(rule.created_at, 1);
    }

    #[test]
    fn dto_rejects_unknown_metric_operator_nan_window() {
        let base = || CreateAlertRuleRequest {
            name: "m".into(),
            enabled: true,
            rule_config: String::new(),
            threshold: "1".into(),
            silence_period: 0,
            metric: "proxy.errors".into(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
            id: None,
            created_at: None,
        };

        let mut bad = base();
        bad.metric = "cpu.p95".into();
        assert!(build_rule_from_create(bad).is_err());

        let mut bad = base();
        bad.operator = "neq".into();
        assert!(build_rule_from_create(bad).is_err());

        let mut bad = base();
        bad.threshold = "NaN".into();
        assert!(build_rule_from_create(bad).is_err());

        let mut bad = base();
        bad.threshold = "inf".into();
        assert!(build_rule_from_create(bad).is_err());

        let mut bad = base();
        bad.evaluation_window_secs = MAX_EVALUATION_WINDOW_SECS + 1;
        assert!(build_rule_from_create(bad).is_err());

        let mut bad = base();
        bad.server_id = "x".repeat(MAX_SERVER_ID_LEN + 1);
        assert!(build_rule_from_create(bad).is_err());
    }

    #[test]
    fn update_dto_path_id_wins_and_preserves_created_at() {
        let existing = AlertRuleView {
            id: "path-id".into(),
            name: "old".into(),
            enabled: true,
            rule_configured: true,
            threshold: "1".into(),
            silence_period: 0,
            created_at: 1_234_567_890,
            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        };
        let req = UpdateAlertRuleRequest {
            name: "new".into(),
            enabled: false,
            rule_config: Some("http://127.0.0.1:2".into()),
            clear_rule_config: false,
            threshold: "9".into(),
            silence_period: 3,
            metric: "proxy.blocks".into(),
            operator: "lt".into(),
            server_id: "s1".into(),
            evaluation_window_secs: 0,
            id: Some("body-id".into()),
            created_at: Some(999),
        };
        let (rule, config_update) = build_rule_from_update("path-id", req, &existing).unwrap();
        assert_eq!(rule.id, "path-id");
        assert_eq!(rule.created_at, 1_234_567_890);
        assert_eq!(rule.name, "new");
        assert!(!rule.enabled);
        assert_eq!(rule.metric, "proxy.blocks");
        assert_eq!(rule.operator, "lt");
        assert_eq!(rule.server_id, "s1");
        assert_eq!(rule.threshold, "9");
        assert!(rule.rule_config.is_empty());
        assert!(
            matches!(config_update, RuleConfigUpdate::Replace(value) if value == "http://127.0.0.1:2")
        );
    }

    #[test]
    fn update_rule_config_three_state_mapping_and_conflict() {
        assert!(matches!(
            map_rule_config_update(None, false).unwrap(),
            RuleConfigUpdate::Preserve
        ));
        assert!(matches!(
            map_rule_config_update(None, true).unwrap(),
            RuleConfigUpdate::Clear
        ));
        assert!(matches!(
            map_rule_config_update(Some(String::new()), false).unwrap(),
            RuleConfigUpdate::Replace(value) if value.is_empty()
        ));
        assert!(matches!(
            map_rule_config_update(Some("http://127.0.0.1:9/h".into()), false).unwrap(),
            RuleConfigUpdate::Replace(value) if value == "http://127.0.0.1:9/h"
        ));
        assert_validation_error(
            &map_rule_config_update(Some("secret".into()), true).unwrap_err(),
            "rule_config and clear_rule_config cannot be used together",
        );
    }

    #[test]
    fn update_rule_config_preserve_replace_clear_and_conflict_are_atomic() {
        let db_path = temp_db_path();
        let alerts = Alerts::new(&db_path).unwrap();
        let original = AlertRule {
            id: "three-state".into(),
            name: "original".into(),
            enabled: true,
            rule_config: "http://127.0.0.1:9/old-token".into(),
            threshold: "1".into(),
            silence_period: 0,
            created_at: now_ms(),
            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        };
        alerts.create_alert_rule(original.clone()).unwrap();

        let view = alerts.get_alert_rule("three-state").unwrap().unwrap();
        let (rule, update) =
            build_rule_from_update("three-state", update_request(None, false), &view).unwrap();
        alerts.update_alert_rule(rule, update).unwrap();
        assert_eq!(
            alerts
                .get_rule_webhook_url("three-state")
                .unwrap()
                .as_deref(),
            Some("http://127.0.0.1:9/old-token")
        );

        let view = alerts.get_alert_rule("three-state").unwrap().unwrap();
        let (rule, update) = build_rule_from_update(
            "three-state",
            update_request(Some("http://127.0.0.1:9/new-token"), false),
            &view,
        )
        .unwrap();
        alerts.update_alert_rule(rule, update).unwrap();
        assert_eq!(
            alerts
                .get_rule_webhook_url("three-state")
                .unwrap()
                .as_deref(),
            Some("http://127.0.0.1:9/new-token")
        );

        let before_conflict = alerts.get_alert_rule("three-state").unwrap().unwrap();
        let conflict = build_rule_from_update(
            "three-state",
            update_request(Some("http://127.0.0.1:9/rejected"), true),
            &before_conflict,
        );
        assert_validation_error(
            &conflict.unwrap_err(),
            "rule_config and clear_rule_config cannot be used together",
        );
        assert_eq!(
            alerts
                .get_rule_webhook_url("three-state")
                .unwrap()
                .as_deref(),
            Some("http://127.0.0.1:9/new-token")
        );
        assert_eq!(
            alerts.get_alert_rule("three-state").unwrap().unwrap(),
            before_conflict
        );

        let view = alerts.get_alert_rule("three-state").unwrap().unwrap();
        let (rule, update) =
            build_rule_from_update("three-state", update_request(Some(""), false), &view).unwrap();
        assert!(matches!(update, RuleConfigUpdate::Replace(ref value) if value.is_empty()));
        alerts.update_alert_rule(rule, update).unwrap();
        assert!(
            alerts
                .get_rule_webhook_url("three-state")
                .unwrap()
                .is_none()
        );
        assert!(
            !alerts
                .get_alert_rule("three-state")
                .unwrap()
                .unwrap()
                .rule_configured
        );

        let replacement = AlertRule {
            rule_config: "http://127.0.0.1:9/restored".into(),
            ..original
        };
        alerts
            .update_alert_rule(
                replacement,
                RuleConfigUpdate::Replace("http://127.0.0.1:9/restored".into()),
            )
            .unwrap();
        let view = alerts.get_alert_rule("three-state").unwrap().unwrap();
        let (rule, update) =
            build_rule_from_update("three-state", update_request(None, true), &view).unwrap();
        alerts.update_alert_rule(rule, update).unwrap();
        assert!(
            alerts
                .get_rule_webhook_url("three-state")
                .unwrap()
                .is_none()
        );
        cleanup_temp_db(&db_path);
    }

    #[test]
    fn public_rule_views_never_serialize_secret_or_storage_envelope() {
        let db_path = temp_db_path();
        let alerts = Alerts::new(&db_path).unwrap();
        let rule = AlertRule {
            id: "public".into(),
            name: "public".into(),
            enabled: true,
            rule_config: "http://example.com/token-123".into(),
            threshold: "1".into(),
            silence_period: 0,
            created_at: now_ms(),
            metric: String::new(),
            operator: DEFAULT_OPERATOR.into(),
            server_id: String::new(),
            evaluation_window_secs: 0,
        };
        alerts.create_alert_rule(rule.clone()).unwrap();

        let detail = alerts.get_alert_rule("public").unwrap().unwrap();
        let list = alerts.get_alert_rules().unwrap();
        let created = detail.clone();

        let mut updated_rule = rule;
        updated_rule.name = "updated".into();
        alerts
            .update_alert_rule(
                updated_rule,
                RuleConfigUpdate::Replace("http://example.com/token-456".into()),
            )
            .unwrap();
        let updated = alerts.get_alert_rule("public").unwrap().unwrap();

        for serialized in [
            serde_json::to_string(&detail).unwrap(),
            serde_json::to_string(&list).unwrap(),
            serde_json::to_string(&created).unwrap(),
            serde_json::to_string(&updated).unwrap(),
        ] {
            assert!(serialized.contains("rule_configured"));
            assert!(!serialized.contains("rule_config\""));
            assert!(!serialized.contains("http://"));
            assert!(!serialized.contains("token-123"));
            assert!(!serialized.contains("token-456"));
            assert!(!serialized.contains("embypanel:alert-rule-config"));
        }
        cleanup_temp_db(&db_path);
    }

    #[test]
    fn plaintext_wire_payload_does_not_match_encrypted_request_contract() {
        let plain = serde_json::from_str::<EncryptedRequest>(
            r#"{"name":"plain","rule_config":"http://127.0.0.1:9/token"}"#,
        );
        assert!(plain.is_err());
    }
    #[tokio::test]
    async fn authorized_history_is_filtered_before_limit() {
        let fixture = AlertsAuthFixture::new();
        let state = fixture.state().clone();
        let a = "review-history-a";
        let b = "review-history-b";
        state
            .alerts
            .create_alert_rule(permission_test_rule(a))
            .unwrap();
        let mut foreign = permission_test_rule(b);
        foreign.server_id = "srv-foreign".to_string();
        state.alerts.create_alert_rule(foreign).unwrap();
        state
            .alerts
            .record_test_delivery(a, "failed", "visible A failure")
            .unwrap();
        // INSERT order also breaks timestamp ties via id DESC; no sleeps required.
        for _ in 0..100 {
            state
                .alerts
                .record_test_delivery(b, "success", "foreign B delivery")
                .unwrap();
        }
        let raw = state.alerts.list_alert_deliveries(None, 100).unwrap();
        assert_eq!(raw.len(), 100);
        assert!(raw.iter().all(|delivery| delivery.rule_id == b));
        let Json(rules) =
            list_alert_rules(State(state.clone()), bearer_headers(&fixture.read_session))
                .await
                .unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].id, a);
        let Json(per_rule) = list_alert_rule_history(
            State(state.clone()),
            Path(a.to_string()),
            bearer_headers(&fixture.read_session),
            Query(AlertDeliveriesQuery {
                rule_id: None,
                limit: Some(10),
            }),
        )
        .await
        .unwrap();
        assert_eq!(per_rule.len(), 1);
        assert_eq!(per_rule[0].rule_id, a);
        assert_eq!(per_rule[0].status, "failed");
        let Json(combined) = list_alert_deliveries(
            State(state.clone()),
            bearer_headers(&fixture.read_session),
            Query(AlertDeliveriesQuery {
                rule_id: None,
                limit: Some(10),
            }),
        )
        .await
        .unwrap();
        assert_eq!(combined.len(), 1);
        assert_eq!(combined[0].rule_id, a);
        assert_eq!(combined[0].status, "failed");
        assert!(
            state
                .alerts
                .list_authorized_deliveries(&[], 100)
                .unwrap()
                .is_empty()
        );
        // state drops before fixture, whose Drop shuts down telemetry and removes
        // only this fixture's random temporary DB, key, WAL and SHM.
    }
}
