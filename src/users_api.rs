use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::task::JoinSet;

use crate::{
    AppState, auth,
    config::Config,
    crypto_api::EncryptedRequest,
    emby,
    error::{AppError, AppResult, safe_error_message},
};

pub const USER_POLICIES_SETTING_KEY: &str = "user_policies";
pub const USER_TEMPLATES_SETTING_KEY: &str = "user_templates";
const USER_QUERY_CONCURRENCY_LIMIT: usize = 4;
const MAX_POLICY_LIST_ITEMS: usize = 256;
const MAX_USER_POLICY_LIMIT: u64 = 64;
const MAX_USER_TEMPLATES: usize = 512;
const MAX_TEMPLATE_NAME: usize = 128;
const MAX_ACCESS_SCHEDULES: usize = 64;
const MAX_PARENTAL_RATING: i64 = 10_000;
const MAX_REMOTE_QUALITY: i64 = 1_000_000_000;
const MAX_REMOTE_BITRATE: u64 = u32::MAX as u64;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserAccessSchedule {
    #[serde(alias = "DayOfWeek")]
    pub day_of_week: String,
    #[serde(alias = "StartHour")]
    pub start_hour: f64,
    #[serde(alias = "EndHour")]
    pub end_hour: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserPolicyRecord {
    #[serde(default)]
    pub server_id: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub rate_limit_enabled: bool,
    #[serde(default = "default_rate_window")]
    pub rate_limit_window_seconds: u64,
    #[serde(default = "default_rate_max")]
    pub rate_limit_max_requests: u64,
    #[serde(default = "default_rate_block")]
    pub rate_limit_block_seconds: u64,
    #[serde(default = "default_rate_action")]
    pub rate_limit_action: String,
    #[serde(default)]
    pub concurrent_playback_limit_enabled: bool,
    #[serde(default = "default_concurrent_max")]
    pub concurrent_playback_limit_max: u64,
}

impl Default for UserPolicyRecord {
    fn default() -> Self {
        Self {
            server_id: String::new(),
            user_id: String::new(),
            user_name: String::new(),
            rate_limit_enabled: false,
            rate_limit_window_seconds: default_rate_window(),
            rate_limit_max_requests: default_rate_max(),
            rate_limit_block_seconds: default_rate_block(),
            rate_limit_action: default_rate_action(),
            concurrent_playback_limit_enabled: false,
            concurrent_playback_limit_max: default_concurrent_max(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct UserPoliciesDocument {
    #[serde(default)]
    pub policies: Vec<UserPolicyRecord>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct UserTemplatesDocument {
    #[serde(default)]
    pub templates: Vec<UserTemplate>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserTemplate {
    pub id: String,
    pub server_id: String,
    pub name: String,
    pub policy: UserPolicyUpdate,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserAccessOption {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct UserSummary {
    pub server_id: String,
    pub server_name: String,
    pub user_id: String,
    pub name: String,
    pub is_administrator: bool,
    pub is_disabled: bool,
    pub enable_remote_access: bool,
    pub enable_media_playback: bool,
    pub enable_all_folders: bool,
    pub enabled_folders: Vec<String>,
    pub available_folders: Vec<UserAccessOption>,
    pub enable_all_devices: bool,
    pub enabled_devices: Vec<String>,
    pub available_devices: Vec<UserAccessOption>,
    pub simultaneous_stream_limit: Option<u64>,
    pub last_activity: Option<String>,
    pub active_sessions: u64,
    pub devices: Vec<String>,
    pub policy: UserPolicyUpdate,
    pub user_policy: UserPolicyRecord,
}

#[derive(Debug, Serialize)]
pub(crate) struct UserServerError {
    server_id: String,
    server_name: String,
    error: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct UserServerOption {
    id: String,
    name: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct UsersResponse {
    users: Vec<UserSummary>,
    servers: Vec<UserServerOption>,
    server_errors: Vec<UserServerError>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UserListQuery {
    pub search: Option<String>,
    pub server_id: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserPolicyUpdate {
    pub is_administrator: Option<bool>,
    pub is_hidden: Option<bool>,
    pub is_hidden_remotely: Option<bool>,
    pub is_hidden_from_unused_devices: Option<bool>,
    pub is_disabled: Option<bool>,
    pub max_parental_rating_enabled: Option<bool>,
    pub max_parental_rating: Option<i64>,
    pub allow_tag_or_rating: Option<bool>,
    pub blocked_tags: Option<Vec<String>>,
    pub is_tag_blocking_mode_inclusive: Option<bool>,
    pub include_tags: Option<Vec<String>>,
    pub enable_user_preference_access: Option<bool>,
    pub access_schedules: Option<Vec<UserAccessSchedule>>,
    pub block_unrated_items: Option<Vec<String>>,
    pub enable_remote_control_of_other_users: Option<bool>,
    pub enable_shared_device_control: Option<bool>,
    pub enable_remote_access: Option<bool>,
    pub enable_live_tv_management: Option<bool>,
    pub enable_live_tv_access: Option<bool>,
    pub enable_media_playback: Option<bool>,
    pub enable_audio_playback_transcoding: Option<bool>,
    pub enable_video_playback_transcoding: Option<bool>,
    pub auto_remote_quality: Option<i64>,
    pub enable_playback_remuxing: Option<bool>,
    pub enable_content_deletion: Option<bool>,
    pub restricted_features: Option<Vec<String>>,
    pub enable_content_deletion_from_folders: Option<Vec<String>>,
    pub enable_content_downloading: Option<bool>,
    pub enable_subtitle_downloading: Option<bool>,
    pub enable_subtitle_management: Option<bool>,
    pub enable_sync_transcoding: Option<bool>,
    pub enable_media_conversion: Option<bool>,
    pub enabled_channels: Option<Vec<String>>,
    pub enable_all_channels: Option<bool>,
    pub enable_all_folders: Option<bool>,
    pub enabled_folders: Option<Vec<String>>,
    pub enable_public_sharing: Option<bool>,
    pub remote_client_bitrate_limit: Option<u64>,
    pub excluded_sub_folders: Option<Vec<String>>,
    pub enable_all_devices: Option<bool>,
    pub enabled_devices: Option<Vec<String>>,
    pub simultaneous_stream_limit: Option<u64>,
    pub allow_camera_upload: Option<bool>,
    pub allow_sharing_personal_items: Option<bool>,
    pub rate_limit_enabled: Option<bool>,
    pub rate_limit_window_seconds: Option<u64>,
    pub rate_limit_max_requests: Option<u64>,
    pub rate_limit_block_seconds: Option<u64>,
    pub rate_limit_action: Option<String>,
    pub concurrent_playback_limit_enabled: Option<bool>,
    pub concurrent_playback_limit_max: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct UserPasswordReset {
    pub new_password: String,
}

#[derive(Debug, Deserialize)]
pub struct UserCreateRequest {
    pub name: String,
    pub new_password: String,
    #[serde(default)]
    pub template_id: Option<String>,
    #[serde(default)]
    pub policy: Option<UserPolicyUpdate>,
}

#[derive(Debug, Deserialize)]
pub struct UserDeleteRequest {
    pub confirm_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UserTemplateRequest {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub policy: UserPolicyUpdate,
}

#[derive(Debug, Deserialize)]
pub struct UserTemplateDeleteRequest {
    pub confirm: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct UserTemplateListQuery {
    pub server_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UserTemplatesResponse {
    pub templates: Vec<UserTemplate>,
}

pub async fn list_users(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UserListQuery>,
) -> AppResult<Json<UsersResponse>> {
    auth::require_auth(&state, &headers).await?;
    let status = query.status.as_deref().unwrap_or("all").trim();
    if !matches!(status, "" | "all" | "enabled" | "disabled") {
        return Err(AppError::Validation(
            "status must be all, enabled, or disabled".to_string(),
        ));
    }
    let root_config = state.config.read().await.clone();
    let mut configs = managed_server_configs(&root_config);
    if let Some(server_id) = query
        .server_id
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        configs.retain(|config| config.server_label().0 == server_id);
    }
    let mut server_options: Vec<UserServerOption> = configs
        .iter()
        .map(|config| {
            let (id, name) = config.server_label();
            UserServerOption { id, name }
        })
        .collect();
    server_options.sort_by(|left, right| left.name.cmp(&right.name));
    let policies = load_user_policies(&state)?;
    let mut pending = configs.into_iter();
    let mut tasks = JoinSet::new();
    for _ in 0..USER_QUERY_CONCURRENCY_LIMIT {
        spawn_next_user_query(
            &mut tasks,
            &mut pending,
            state.client.clone(),
            policies.clone(),
        );
    }

    let mut users = Vec::new();
    let mut server_errors = Vec::new();
    while let Some(result) = tasks.join_next().await {
        spawn_next_user_query(
            &mut tasks,
            &mut pending,
            state.client.clone(),
            policies.clone(),
        );
        match result {
            Ok((config, Ok((server_users, warnings)))) => {
                users.extend(server_users);
                if !warnings.is_empty() {
                    let (server_id, server_name) = config.server_label();
                    server_errors.push(UserServerError {
                        server_id,
                        server_name,
                        error: warnings.join("; "),
                    });
                }
            }
            Ok((config, Err(error))) => {
                let (server_id, server_name) = config.server_label();
                server_errors.push(UserServerError {
                    server_id,
                    server_name,
                    error: safe_error_message(&error),
                });
            }
            Err(error) => tracing::warn!(error = %error, "user query task failed"),
        }
    }

    let search = query.search.as_deref().unwrap_or("").trim().to_lowercase();
    users.retain(|user| {
        let matches_search = search.is_empty()
            || user.name.to_lowercase().contains(&search)
            || user.server_name.to_lowercase().contains(&search);
        let matches_status = match status {
            "disabled" => user.is_disabled,
            "enabled" => !user.is_disabled,
            _ => true,
        };
        matches_search && matches_status
    });
    users.sort_by(|left, right| {
        left.server_name
            .cmp(&right.server_name)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(Json(UsersResponse {
        users,
        servers: server_options,
        server_errors,
    }))
}

pub async fn get_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server_id, user_id)): Path<(String, String)>,
) -> AppResult<Json<UserSummary>> {
    auth::require_auth(&state, &headers).await?;
    let config = server_config(&state, &server_id).await?;
    let policies = load_user_policies(&state)?;
    let (profile, sessions, folders) = tokio::join!(
        emby::get_user_profile_value(&state.client, &config, &user_id),
        emby::list_sessions_value(&state.client, &config),
        emby::list_virtual_folders_value(&state.client, &config),
    );
    if let Err(error) = &sessions {
        tracing::warn!(
            server_id = %server_id,
            user_id = %user_id,
            error = %safe_error_message(error),
            "failed to load Emby sessions for user detail"
        );
    }
    if let Err(error) = &folders {
        tracing::warn!(
            server_id = %server_id,
            user_id = %user_id,
            error = %safe_error_message(error),
            "failed to load Emby folders for user detail"
        );
    }
    Ok(Json(user_summary(
        &config,
        &profile?,
        &sessions.unwrap_or_default(),
        &folders.unwrap_or_default(),
        &policies,
    )))
}

pub async fn list_user_templates(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<UserTemplateListQuery>,
) -> AppResult<Json<UserTemplatesResponse>> {
    auth::require_auth(&state, &headers).await?;
    let document = load_user_templates(&state)?;
    let server_id = query
        .server_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    Ok(Json(UserTemplatesResponse {
        templates: document
            .templates
            .into_iter()
            .filter(|template| server_id.is_none_or(|id| template.server_id == id))
            .collect(),
    }))
}

pub async fn save_user_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server_id): Path<String>,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<UserTemplate>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: UserTemplateRequest =
        match state.crypto_keys.decrypt_named(&request, "user_template") {
            Ok(payload) => payload,
            Err(error) => {
                record_user_audit(
                    &state,
                    admin_user_id,
                    "user.template.save",
                    &format!("保存用户权限模板失败：{server_id}"),
                    &format!("error: {}", safe_error_message(&error)),
                );
                return Err(error);
            }
        };
    let result = async {
        validate_template_name(&payload.name)?;
        validate_policy_update(&payload.policy)?;
        let _settings_update = state.config_updates.lock().await;
        let _ = server_config(&state, &server_id).await?;
        let mut document = load_user_templates(&state)?;
        let id = payload
            .id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(new_template_id);
        validate_template_id(&id)?;
        if document
            .templates
            .iter()
            .any(|template| template.id == id && template.server_id != server_id)
        {
            return Err(AppError::Validation(
                "template id belongs to another server".to_string(),
            ));
        }
        let template = UserTemplate {
            id,
            server_id: server_id.clone(),
            name: payload
                .name
                .trim()
                .chars()
                .take(MAX_TEMPLATE_NAME)
                .collect(),
            policy: payload.policy,
        };
        if let Some(existing) = document
            .templates
            .iter_mut()
            .find(|item| item.server_id == server_id && item.id == template.id)
        {
            *existing = template.clone();
        } else {
            document.templates.push(template.clone());
        }
        normalize_user_templates(&mut document);
        let saved_template = document
            .templates
            .iter()
            .find(|item| item.server_id == server_id && item.id == template.id)
            .cloned()
            .ok_or_else(|| {
                AppError::Internal("saved user template was not retained".to_string())
            })?;
        state
            .settings_store
            .save_setting_json(USER_TEMPLATES_SETTING_KEY, &document)?;
        Ok::<_, AppError>(saved_template)
    }
    .await;
    match result {
        Ok(template) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.template.save",
                &format!("保存用户权限模板：{} / {}", server_id, template.name),
                "success",
            );
            Ok(Json(template))
        }
        Err(error) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.template.save",
                &format!("保存用户权限模板失败：{server_id}"),
                &format!("error: {}", safe_error_message(&error)),
            );
            Err(error)
        }
    }
}

pub async fn delete_user_template(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server_id, template_id)): Path<(String, String)>,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<Value>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: UserTemplateDeleteRequest = match state
        .crypto_keys
        .decrypt_named(&request, "user_template_delete")
    {
        Ok(payload) => payload,
        Err(error) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.template.delete",
                &format!("删除用户权限模板失败：{server_id} / {template_id}"),
                &format!("error: {}", safe_error_message(&error)),
            );
            return Err(error);
        }
    };
    if !payload.confirm {
        let error = AppError::Validation("template deletion was not confirmed".to_string());
        record_user_audit(
            &state,
            admin_user_id,
            "user.template.delete",
            &format!("删除用户权限模板失败：{server_id} / {template_id}"),
            &format!("error: {}", safe_error_message(&error)),
        );
        return Err(error);
    }
    if let Err(error) = validate_template_id(&template_id) {
        record_user_audit(
            &state,
            admin_user_id,
            "user.template.delete",
            &format!("删除用户权限模板失败：{server_id} / {template_id}"),
            &format!("error: {}", safe_error_message(&error)),
        );
        return Err(error);
    }
    let result = async {
        let _settings_update = state.config_updates.lock().await;
        let _ = server_config(&state, &server_id).await?;
        let mut document = load_user_templates(&state)?;
        let before = document.templates.len();
        document
            .templates
            .retain(|template| !(template.server_id == server_id && template.id == template_id));
        if document.templates.len() == before {
            return Err(AppError::Validation("user template not found".to_string()));
        }
        state
            .settings_store
            .save_setting_json(USER_TEMPLATES_SETTING_KEY, &document)?;
        Ok::<_, AppError>(())
    }
    .await;
    match result {
        Ok(()) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.template.delete",
                &format!("删除用户权限模板：{server_id} / {template_id}"),
                "success",
            );
            Ok(Json(json!({ "success": true })))
        }
        Err(error) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.template.delete",
                &format!("删除用户权限模板失败：{server_id} / {template_id}"),
                &format!("error: {}", safe_error_message(&error)),
            );
            Err(error)
        }
    }
}

pub async fn create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(server_id): Path<String>,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<UserSummary>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: UserCreateRequest = match state.crypto_keys.decrypt_named(&request, "user_create")
    {
        Ok(payload) => payload,
        Err(error) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.create",
                &format!("创建 Emby 用户失败：{server_id}"),
                &format!("error: {}", safe_error_message(&error)),
            );
            return Err(error);
        }
    };
    let result = async {
        validate_user_name(&payload.name)?;
        validate_password(&payload.new_password)?;
        let _settings_update = state.config_updates.lock().await;
        let config = server_config(&state, &server_id).await?;
        let templates = load_user_templates(&state)?;
        let mut policy = payload.policy.unwrap_or_default();
        if let Some(template_id) = payload
            .template_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            let template = templates
                .templates
                .iter()
                .find(|template| template.server_id == server_id && template.id == template_id)
                .ok_or_else(|| AppError::Validation("user template not found".to_string()))?;
            merge_policy_defaults(&mut policy, &template.policy);
        }
        validate_policy_update(&policy)?;

        let created = emby::create_user(&state.client, &config, payload.name.trim()).await?;
        let user_id = string_field(&created, &["Id", "id"]).ok_or_else(|| {
            AppError::BadGateway("Emby did not return the new user id".to_string())
        })?;
        if let Err(error) =
            emby::set_user_password(&state.client, &config, &user_id, "", &payload.new_password)
                .await
        {
            if let Err(cleanup_error) = emby::delete_user(&state.client, &config, &user_id).await {
                tracing::error!(
                    user_id = %user_id,
                    error = %cleanup_error.safe_log_message(),
                    "failed to remove partially created Emby user"
                );
            }
            return Err(error);
        }

        let mut profile = emby::get_user_profile_value(&state.client, &config, &user_id)
            .await
            .unwrap_or(created);
        if policy_has_emby_fields(&policy) {
            let policy_value = {
                let emby_policy = policy_object_mut(&mut profile);
                apply_policy_update(emby_policy, &policy);
                Value::Object(emby_policy.clone())
            };
            if let Err(error) =
                emby::update_user_policy_value(&state.client, &config, &user_id, &policy_value)
                    .await
            {
                if let Err(cleanup_error) =
                    emby::delete_user(&state.client, &config, &user_id).await
                {
                    tracing::error!(
                        user_id = %user_id,
                        error = %cleanup_error.safe_log_message(),
                        "failed to remove Emby user after policy failure"
                    );
                }
                return Err(error);
            }
        }

        let mut policies = load_user_policies(&state)?;
        update_saved_user_policy(&mut policies, &server_id, &user_id, &profile, &policy);
        if policy_has_panel_fields(&policy) {
            if let Err(error) = state
                .settings_store
                .save_setting_json(USER_POLICIES_SETTING_KEY, &policies)
            {
                if let Err(cleanup_error) =
                    emby::delete_user(&state.client, &config, &user_id).await
                {
                    tracing::error!(
                        user_id = %user_id,
                        error = %cleanup_error.safe_log_message(),
                        "failed to remove Emby user after local policy save failure"
                    );
                }
                return Err(error);
            }
        }
        let (sessions, folders) = tokio::join!(
            emby::list_sessions_value(&state.client, &config),
            emby::list_virtual_folders_value(&state.client, &config),
        );
        Ok::<_, AppError>(user_summary(
            &config,
            &profile,
            &sessions.unwrap_or_default(),
            &folders.unwrap_or_default(),
            &policies,
        ))
    }
    .await;
    match result {
        Ok(summary) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.create",
                &format!("创建 Emby 用户：{} / {}", summary.server_id, summary.name),
                "success",
            );
            Ok(Json(summary))
        }
        Err(error) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.create",
                &format!("创建 Emby 用户失败：{server_id}"),
                &format!("error: {}", safe_error_message(&error)),
            );
            Err(error)
        }
    }
}

pub async fn delete_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server_id, user_id)): Path<(String, String)>,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<Value>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: UserDeleteRequest = match state.crypto_keys.decrypt_named(&request, "user_delete")
    {
        Ok(payload) => payload,
        Err(error) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.delete",
                &format!("删除 Emby 用户失败：{} / {}", server_id, user_id),
                &format!("error: {}", safe_error_message(&error)),
            );
            return Err(error);
        }
    };
    let result = async {
        let _settings_update = state.config_updates.lock().await;
        let config = server_config(&state, &server_id).await?;
        let profile = emby::get_user_profile_value(&state.client, &config, &user_id).await?;
        let name = string_field(&profile, &["Name", "name"]).unwrap_or_else(|| user_id.clone());
        if payload.confirm_name.trim() != name {
            return Err(AppError::Validation(
                "user name confirmation does not match".to_string(),
            ));
        }
        let policies = load_user_policies(&state)?;
        let previous_policies = policies.clone();
        let mut policies = policies;
        policies
            .policies
            .retain(|policy| !(policy.server_id == server_id && policy.user_id == user_id));
        state
            .settings_store
            .save_setting_json(USER_POLICIES_SETTING_KEY, &policies)?;
        if let Err(error) = emby::delete_user(&state.client, &config, &user_id).await {
            if let Err(rollback_error) = state
                .settings_store
                .save_setting_json(USER_POLICIES_SETTING_KEY, &previous_policies)
            {
                tracing::error!(
                    server_id = %server_id,
                    user_id = %user_id,
                    error = %error.safe_log_message(),
                    rollback_error = %rollback_error.safe_log_message(),
                    "failed to rollback local policy after Emby user deletion failure"
                );
                return Err(AppError::Internal(
                    "Emby user deletion failed and local policy rollback also failed".to_string(),
                ));
            }
            return Err(error);
        }
        Ok::<_, AppError>(name)
    }
    .await;
    match result {
        Ok(name) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.delete",
                &format!("删除 Emby 用户：{} / {}", server_id, name),
                "success",
            );
            Ok(Json(json!({ "success": true })))
        }
        Err(error) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.delete",
                &format!("删除 Emby 用户失败：{} / {}", server_id, user_id),
                &format!("error: {}", safe_error_message(&error)),
            );
            Err(error)
        }
    }
}

pub async fn update_user_policy(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server_id, user_id)): Path<(String, String)>,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<UserSummary>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: UserPolicyUpdate = match state.crypto_keys.decrypt_named(&request, "user_policy") {
        Ok(payload) => payload,
        Err(error) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.policy.update",
                &format!("更新 Emby 用户策略失败：{} / {}", server_id, user_id),
                &format!("error: {}", safe_error_message(&error)),
            );
            return Err(error);
        }
    };
    if let Err(error) = validate_policy_update(&payload) {
        record_user_audit(
            &state,
            admin_user_id,
            "user.policy.update",
            &format!("更新 Emby 用户策略失败：{} / {}", server_id, user_id),
            &format!("error: {}", safe_error_message(&error)),
        );
        return Err(error);
    }
    let _settings_update = state.config_updates.lock().await;
    let config = match server_config(&state, &server_id).await {
        Ok(config) => config,
        Err(error) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.policy.update",
                &format!("更新 Emby 用户策略失败：{} / {}", server_id, user_id),
                &format!("error: {}", safe_error_message(&error)),
            );
            return Err(error);
        }
    };
    let result = async {
        let mut profile = emby::get_user_profile_value(&state.client, &config, &user_id).await?;
        let previous_policy = object_field(&profile, &["Policy", "policy"])
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
        let mut policies = load_user_policies(&state)?;
        update_saved_user_policy(&mut policies, &server_id, &user_id, &profile, &payload);
        let policy = policy_object_mut(&mut profile);
        apply_policy_update(policy, &payload);
        emby::update_user_policy_value(
            &state.client,
            &config,
            &user_id,
            &Value::Object(policy.clone()),
        )
        .await?;

        if let Err(error) = state
            .settings_store
            .save_setting_json(USER_POLICIES_SETTING_KEY, &policies)
        {
            if let Err(rollback_error) =
                emby::update_user_policy_value(&state.client, &config, &user_id, &previous_policy)
                    .await
            {
                tracing::error!(
                    server_id = %server_id,
                    user_id = %user_id,
                    save_error = %error.safe_log_message(),
                    rollback_error = %rollback_error.safe_log_message(),
                    "failed to rollback Emby user policy after local save failure"
                );
                return Err(AppError::Internal(
                    "user policy save failed and the Emby policy rollback also failed".to_string(),
                ));
            }
            return Err(error);
        }
        let (sessions, folders) = tokio::join!(
            emby::list_sessions_value(&state.client, &config),
            emby::list_virtual_folders_value(&state.client, &config),
        );
        if let Err(error) = &sessions {
            tracing::warn!(
                server_id = %server_id,
                user_id = %user_id,
                error = %safe_error_message(error),
                "failed to refresh Emby sessions after user policy update"
            );
        }
        if let Err(error) = &folders {
            tracing::warn!(
                server_id = %server_id,
                user_id = %user_id,
                error = %safe_error_message(error),
                "failed to refresh Emby folders after user policy update"
            );
        }
        Ok::<_, AppError>(user_summary(
            &config,
            &profile,
            &sessions.unwrap_or_default(),
            &folders.unwrap_or_default(),
            &policies,
        ))
    }
    .await;

    match result {
        Ok(summary) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.policy.update",
                &format!(
                    "更新 Emby 用户策略：{} / {}",
                    summary.server_id, summary.name
                ),
                "success",
            );
            Ok(Json(summary))
        }
        Err(error) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.policy.update",
                &format!("更新 Emby 用户策略失败：{} / {}", server_id, user_id),
                &format!("error: {}", safe_error_message(&error)),
            );
            Err(error)
        }
    }
}

pub async fn reset_user_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((server_id, user_id)): Path<(String, String)>,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<Value>> {
    let admin_user_id = auth::require_auth_user_id(&state, &headers).await?;
    let payload: UserPasswordReset =
        match state.crypto_keys.decrypt_named(&request, "user_password") {
            Ok(payload) => payload,
            Err(error) => {
                record_user_audit(
                    &state,
                    admin_user_id,
                    "user.password.reset",
                    &format!("重置 Emby 用户密码失败：{} / {}", server_id, user_id),
                    &format!("error: {}", safe_error_message(&error)),
                );
                return Err(error);
            }
        };
    let new_password = payload.new_password;
    if let Err(error) = validate_password(&new_password) {
        record_user_audit(
            &state,
            admin_user_id,
            "user.password.reset",
            &format!("重置 Emby 用户密码失败：{} / {}", server_id, user_id),
            &format!("error: {}", safe_error_message(&error)),
        );
        return Err(error);
    }
    let _password_task = state
        .user_password_tasks
        .clone()
        .try_acquire_owned()
        .map_err(|_| AppError::RateLimited("user password reset is busy; try again".to_string()))
        .map_err(|error| {
            record_user_audit(
                &state,
                admin_user_id,
                "user.password.reset",
                &format!("重置 Emby 用户密码失败：{} / {}", server_id, user_id),
                &format!("error: {}", safe_error_message(&error)),
            );
            error
        })?;
    let config = match server_config(&state, &server_id).await {
        Ok(config) => config,
        Err(error) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.password.reset",
                &format!("重置 Emby 用户密码失败：{} / {}", server_id, user_id),
                &format!("error: {}", safe_error_message(&error)),
            );
            return Err(error);
        }
    };
    let result = emby::reset_user_password(&state.client, &config, &user_id, &new_password).await;
    match result {
        Ok(()) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.password.reset",
                &format!("重置 Emby 用户密码：{} / {}", server_id, user_id),
                "success",
            );
            Ok(Json(json!({ "success": true })))
        }
        Err(error) => {
            record_user_audit(
                &state,
                admin_user_id,
                "user.password.reset",
                &format!("重置 Emby 用户密码失败：{} / {}", server_id, user_id),
                &format!("error: {}", safe_error_message(&error)),
            );
            Err(error)
        }
    }
}

fn record_user_audit(
    state: &AppState,
    admin_user_id: i64,
    action: &str,
    summary: &str,
    result: &str,
) {
    state
        .settings_store
        .record_audit_best_effort(Some(admin_user_id), action, summary, result);
}

pub(crate) fn load_user_templates(state: &AppState) -> AppResult<UserTemplatesDocument> {
    let mut document = state
        .settings_store
        .load_setting_json(USER_TEMPLATES_SETTING_KEY)?
        .unwrap_or_default();
    normalize_user_templates(&mut document);
    Ok(document)
}

pub(crate) fn normalize_user_templates(document: &mut UserTemplatesDocument) {
    for template in &mut document.templates {
        template.id = template.id.trim().to_string();
        template.server_id = template.server_id.trim().to_string();
        template.name = template
            .name
            .trim()
            .chars()
            .take(MAX_TEMPLATE_NAME)
            .collect();
        normalize_policy_update(&mut template.policy);
    }
    document.templates.retain(|template| {
        validate_template_id(&template.id).is_ok()
            && !template.server_id.is_empty()
            && template.server_id.len() <= 128
            && !template.name.is_empty()
    });
    let mut seen = std::collections::HashSet::new();
    document
        .templates
        .retain(|template| seen.insert((template.server_id.clone(), template.id.clone())));
    document.templates.truncate(MAX_USER_TEMPLATES);
}

fn validate_template_name(name: &str) -> AppResult<()> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > MAX_TEMPLATE_NAME
        || name.chars().any(char::is_control)
    {
        return Err(AppError::Validation(
            "template name must be between 1 and 128 characters".to_string(),
        ));
    }
    Ok(())
}

fn validate_template_id(id: &str) -> AppResult<()> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AppError::Validation("invalid user template id".to_string()));
    }
    Ok(())
}

fn new_template_id() -> String {
    let mut bytes = [0_u8; 4];
    rand::rng().fill_bytes(&mut bytes);
    format!(
        "template-{}-{:02x}{:02x}{:02x}{:02x}",
        now_seconds(),
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3]
    )
}

fn now_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn validate_user_name(name: &str) -> AppResult<()> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 128 || name.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "user name must be between 1 and 128 characters".to_string(),
        ));
    }
    Ok(())
}

fn validate_password(password: &str) -> AppResult<()> {
    let length = password.chars().count();
    if !(4..=256).contains(&length) || password.chars().any(char::is_control) {
        return Err(AppError::Validation(
            "password must be between 4 and 256 characters".to_string(),
        ));
    }
    Ok(())
}

fn merge_policy_defaults(target: &mut UserPolicyUpdate, defaults: &UserPolicyUpdate) {
    macro_rules! merge {
        ($field:ident) => {
            if target.$field.is_none() {
                target.$field = defaults.$field.clone();
            }
        };
    }
    merge!(is_administrator);
    merge!(is_hidden);
    merge!(is_hidden_remotely);
    merge!(is_hidden_from_unused_devices);
    merge!(is_disabled);
    merge!(max_parental_rating_enabled);
    merge!(max_parental_rating);
    merge!(allow_tag_or_rating);
    merge!(blocked_tags);
    merge!(is_tag_blocking_mode_inclusive);
    merge!(include_tags);
    merge!(enable_user_preference_access);
    merge!(access_schedules);
    merge!(block_unrated_items);
    merge!(enable_remote_control_of_other_users);
    merge!(enable_shared_device_control);
    merge!(enable_remote_access);
    merge!(enable_live_tv_management);
    merge!(enable_live_tv_access);
    merge!(enable_media_playback);
    merge!(enable_audio_playback_transcoding);
    merge!(enable_video_playback_transcoding);
    merge!(auto_remote_quality);
    merge!(enable_playback_remuxing);
    merge!(enable_content_deletion);
    merge!(restricted_features);
    merge!(enable_content_deletion_from_folders);
    merge!(enable_content_downloading);
    merge!(enable_subtitle_downloading);
    merge!(enable_subtitle_management);
    merge!(enable_sync_transcoding);
    merge!(enable_media_conversion);
    merge!(enabled_channels);
    merge!(enable_all_channels);
    merge!(enable_all_folders);
    merge!(enabled_folders);
    merge!(enable_public_sharing);
    merge!(remote_client_bitrate_limit);
    merge!(excluded_sub_folders);
    merge!(enable_all_devices);
    merge!(enabled_devices);
    merge!(simultaneous_stream_limit);
    merge!(allow_camera_upload);
    merge!(allow_sharing_personal_items);
    merge!(rate_limit_enabled);
    merge!(rate_limit_window_seconds);
    merge!(rate_limit_max_requests);
    merge!(rate_limit_block_seconds);
    merge!(rate_limit_action);
    merge!(concurrent_playback_limit_enabled);
    merge!(concurrent_playback_limit_max);
}

fn policy_has_emby_fields(payload: &UserPolicyUpdate) -> bool {
    macro_rules! any_some {
        ($($field:ident),+ $(,)?) => {
            false $(|| payload.$field.is_some())+
        };
    }
    any_some!(
        is_administrator,
        is_hidden,
        is_hidden_remotely,
        is_hidden_from_unused_devices,
        is_disabled,
        max_parental_rating_enabled,
        max_parental_rating,
        allow_tag_or_rating,
        blocked_tags,
        is_tag_blocking_mode_inclusive,
        include_tags,
        enable_user_preference_access,
        access_schedules,
        block_unrated_items,
        enable_remote_control_of_other_users,
        enable_shared_device_control,
        enable_remote_access,
        enable_live_tv_management,
        enable_live_tv_access,
        enable_media_playback,
        enable_audio_playback_transcoding,
        enable_video_playback_transcoding,
        auto_remote_quality,
        enable_playback_remuxing,
        enable_content_deletion,
        restricted_features,
        enable_content_deletion_from_folders,
        enable_content_downloading,
        enable_subtitle_downloading,
        enable_subtitle_management,
        enable_sync_transcoding,
        enable_media_conversion,
        enabled_channels,
        enable_all_channels,
        enable_all_folders,
        enabled_folders,
        enable_public_sharing,
        remote_client_bitrate_limit,
        excluded_sub_folders,
        enable_all_devices,
        enabled_devices,
        simultaneous_stream_limit,
        allow_camera_upload,
        allow_sharing_personal_items,
    )
}

fn policy_has_panel_fields(payload: &UserPolicyUpdate) -> bool {
    payload.rate_limit_enabled.is_some()
        || payload.rate_limit_window_seconds.is_some()
        || payload.rate_limit_max_requests.is_some()
        || payload.rate_limit_block_seconds.is_some()
        || payload.rate_limit_action.is_some()
        || payload.concurrent_playback_limit_enabled.is_some()
        || payload.concurrent_playback_limit_max.is_some()
}

fn normalize_policy_update(payload: &mut UserPolicyUpdate) {
    for value in [
        payload.blocked_tags.as_mut(),
        payload.include_tags.as_mut(),
        payload.block_unrated_items.as_mut(),
        payload.restricted_features.as_mut(),
        payload.enable_content_deletion_from_folders.as_mut(),
        payload.enabled_channels.as_mut(),
        payload.enabled_folders.as_mut(),
        payload.excluded_sub_folders.as_mut(),
        payload.enabled_devices.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        *value = normalized_policy_list(value);
    }
    if let Some(value) = payload.block_unrated_items.as_mut() {
        value.retain(|item| is_valid_unrated_item(item));
    }
    if let Some(value) = payload.access_schedules.as_mut() {
        for schedule in value.iter_mut() {
            schedule.day_of_week = schedule.day_of_week.trim().to_string();
        }
        value.retain(is_valid_access_schedule);
        value.truncate(MAX_ACCESS_SCHEDULES);
    }
    if let Some(value) = payload.max_parental_rating.as_mut() {
        *value = (*value).clamp(0, MAX_PARENTAL_RATING);
    }
    if let Some(value) = payload.auto_remote_quality.as_mut() {
        *value = (*value).clamp(0, MAX_REMOTE_QUALITY);
    }
    if let Some(value) = payload.remote_client_bitrate_limit.as_mut() {
        *value = (*value).min(MAX_REMOTE_BITRATE);
    }
    if let Some(value) = payload.simultaneous_stream_limit.as_mut() {
        *value = (*value).min(MAX_USER_POLICY_LIMIT);
    }
    if let Some(value) = payload.concurrent_playback_limit_max.as_mut() {
        *value = (*value).clamp(1, MAX_USER_POLICY_LIMIT);
    }
    if let Some(value) = payload.rate_limit_window_seconds.as_mut() {
        *value = (*value).clamp(1, 86_400);
    }
    if let Some(value) = payload.rate_limit_max_requests.as_mut() {
        *value = (*value).clamp(1, 10_000);
    }
    if let Some(value) = payload.rate_limit_block_seconds.as_mut() {
        *value = (*value).clamp(1, 86_400);
    }
    if let Some(value) = payload.rate_limit_action.as_mut() {
        let normalized = normalize_rate_action(value);
        *value = normalized;
    }
}

pub(crate) fn load_user_policies(state: &AppState) -> AppResult<UserPoliciesDocument> {
    let revision = state.settings_store.settings_revision();
    if let Some(document) = state
        .user_policies_cache
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .as_ref()
        .filter(|(cached_revision, _)| *cached_revision == revision)
        .map(|(_, document)| document.clone())
    {
        return Ok(document);
    }
    let mut document = state
        .settings_store
        .load_setting_json(USER_POLICIES_SETTING_KEY)?
        .unwrap_or_default();
    normalize_user_policies(&mut document);
    *state
        .user_policies_cache
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((revision, document.clone()));
    Ok(document)
}

pub(crate) fn user_policy_for(
    state: &AppState,
    server_id: &str,
    user_name: &str,
) -> AppResult<Option<UserPolicyRecord>> {
    let policies = load_user_policies(state)?;
    Ok(policies.policies.into_iter().find(|policy| {
        policy.server_id == server_id && policy.user_name.eq_ignore_ascii_case(user_name)
    }))
}

fn spawn_next_user_query(
    tasks: &mut JoinSet<(Config, AppResult<(Vec<UserSummary>, Vec<String>)>)>,
    pending: &mut impl Iterator<Item = Config>,
    client: reqwest::Client,
    policies: UserPoliciesDocument,
) {
    let Some(config) = pending.next() else { return };
    tasks.spawn(async move {
        let result = list_server_users(&client, &config, &policies).await;
        (config, result)
    });
}

async fn list_server_users(
    client: &reqwest::Client,
    config: &Config,
    policies: &UserPoliciesDocument,
) -> AppResult<(Vec<UserSummary>, Vec<String>)> {
    let (users, sessions, folders) = tokio::join!(
        emby::list_users(client, config),
        emby::list_sessions_value(client, config),
        emby::list_virtual_folders_value(client, config),
    );
    let users = users?;
    let mut warnings = Vec::new();
    let sessions = match sessions {
        Ok(value) => value,
        Err(error) => {
            warnings.push(format!("sessions: {}", safe_error_message(&error)));
            Vec::new()
        }
    };
    let folders = match folders {
        Ok(value) => value,
        Err(error) => {
            warnings.push(format!("folders: {}", safe_error_message(&error)));
            Vec::new()
        }
    };
    Ok((
        users
            .iter()
            .map(|user| user_summary(config, user, &sessions, &folders, policies))
            .collect(),
        warnings,
    ))
}

fn managed_server_configs(config: &Config) -> Vec<Config> {
    let configs = config.proxy_configs();
    if configs.is_empty() && config.servers.is_empty() {
        vec![config.clone()]
    } else {
        configs
    }
}

async fn server_config(state: &AppState, server_id: &str) -> AppResult<Config> {
    let root = state.config.read().await;
    if root.servers.is_empty() && server_id == "default" {
        return Ok(root.clone());
    }
    root.servers
        .iter()
        .find(|server| server.id == server_id && server.enabled)
        .map(|server| root.for_server_for_validation(server))
        .ok_or_else(|| AppError::Validation("server not found or disabled".to_string()))
}

fn user_summary(
    config: &Config,
    user: &Value,
    sessions: &[Value],
    folders: &[Value],
    policies: &UserPoliciesDocument,
) -> UserSummary {
    let (server_id, server_name) = config.server_label();
    let user_id = string_field(user, &["Id", "id"]).unwrap_or_default();
    let name = string_field(user, &["Name", "name"]).unwrap_or_else(|| user_id.clone());
    let policy = object_field(user, &["Policy", "policy"]).unwrap_or(&Value::Null);
    let activity = activity_for_user(sessions, &user_id, &name);
    let available_devices = device_options_for_user(sessions, &user_id, &name);
    let user_policy = policies
        .policies
        .iter()
        .find(|entry| entry.server_id == server_id && entry.user_id == user_id)
        .cloned()
        .unwrap_or_else(|| default_user_policy(&server_id, &user_id, &name));
    let mut editable_policy = policy_update_from_value(policy);
    editable_policy.rate_limit_enabled = Some(user_policy.rate_limit_enabled);
    editable_policy.rate_limit_window_seconds = Some(user_policy.rate_limit_window_seconds);
    editable_policy.rate_limit_max_requests = Some(user_policy.rate_limit_max_requests);
    editable_policy.rate_limit_block_seconds = Some(user_policy.rate_limit_block_seconds);
    editable_policy.rate_limit_action = Some(user_policy.rate_limit_action.clone());
    editable_policy.concurrent_playback_limit_enabled =
        Some(user_policy.concurrent_playback_limit_enabled);
    editable_policy.concurrent_playback_limit_max = Some(user_policy.concurrent_playback_limit_max);
    UserSummary {
        server_id,
        server_name,
        user_id,
        name,
        is_administrator: bool_field(policy, &["IsAdministrator", "is_administrator"])
            .unwrap_or(false),
        is_disabled: bool_field(policy, &["IsDisabled", "is_disabled"]).unwrap_or(false),
        enable_remote_access: bool_field(policy, &["EnableRemoteAccess", "enable_remote_access"])
            .unwrap_or(true),
        enable_media_playback: bool_field(
            policy,
            &["EnableMediaPlayback", "enable_media_playback"],
        )
        .unwrap_or(true),
        enable_all_folders: bool_field(policy, &["EnableAllFolders", "enable_all_folders"])
            .unwrap_or(true),
        enabled_folders: string_array_field(policy, &["EnabledFolders", "enabled_folders"]),
        available_folders: folder_options(folders),
        enable_all_devices: bool_field(policy, &["EnableAllDevices", "enable_all_devices"])
            .unwrap_or(true),
        enabled_devices: string_array_field(policy, &["EnabledDevices", "enabled_devices"]),
        available_devices,
        simultaneous_stream_limit: number_field(
            policy,
            &["SimultaneousStreamLimit", "simultaneous_stream_limit"],
        ),
        last_activity: activity.0.or_else(|| {
            string_field(
                user,
                &["LastActivityDate", "LastLoginDate", "last_activity_date"],
            )
        }),
        active_sessions: activity.1,
        devices: activity.2,
        policy: editable_policy,
        user_policy,
    }
}

fn policy_update_from_value(policy: &Value) -> UserPolicyUpdate {
    UserPolicyUpdate {
        is_administrator: bool_field(policy, &["IsAdministrator", "is_administrator"]),
        is_hidden: bool_field(policy, &["IsHidden", "is_hidden"]),
        is_hidden_remotely: bool_field(policy, &["IsHiddenRemotely", "is_hidden_remotely"]),
        is_hidden_from_unused_devices: bool_field(
            policy,
            &["IsHiddenFromUnusedDevices", "is_hidden_from_unused_devices"],
        ),
        is_disabled: bool_field(policy, &["IsDisabled", "is_disabled"]),
        max_parental_rating_enabled: policy_field_exists(
            policy,
            &["MaxParentalRating", "max_parental_rating"],
        )
        .map(|value| !value.is_null()),
        max_parental_rating: i64_field(policy, &["MaxParentalRating", "max_parental_rating"]),
        allow_tag_or_rating: bool_field(policy, &["AllowTagOrRating", "allow_tag_or_rating"]),
        blocked_tags: optional_string_array_field(policy, &["BlockedTags", "blocked_tags"]),
        is_tag_blocking_mode_inclusive: bool_field(
            policy,
            &[
                "IsTagBlockingModeInclusive",
                "is_tag_blocking_mode_inclusive",
            ],
        ),
        include_tags: optional_string_array_field(policy, &["IncludeTags", "include_tags"]),
        enable_user_preference_access: bool_field(
            policy,
            &[
                "EnableUserPreferenceAccess",
                "enable_user_preference_access",
            ],
        ),
        access_schedules: access_schedules_field(policy, &["AccessSchedules", "access_schedules"]),
        block_unrated_items: optional_string_array_field(
            policy,
            &["BlockUnratedItems", "block_unrated_items"],
        ),
        enable_remote_control_of_other_users: bool_field(
            policy,
            &[
                "EnableRemoteControlOfOtherUsers",
                "enable_remote_control_of_other_users",
            ],
        ),
        enable_shared_device_control: bool_field(
            policy,
            &["EnableSharedDeviceControl", "enable_shared_device_control"],
        ),
        enable_remote_access: bool_field(policy, &["EnableRemoteAccess", "enable_remote_access"]),
        enable_live_tv_management: bool_field(
            policy,
            &["EnableLiveTvManagement", "enable_live_tv_management"],
        ),
        enable_live_tv_access: bool_field(policy, &["EnableLiveTvAccess", "enable_live_tv_access"]),
        enable_media_playback: bool_field(
            policy,
            &["EnableMediaPlayback", "enable_media_playback"],
        ),
        enable_audio_playback_transcoding: bool_field(
            policy,
            &[
                "EnableAudioPlaybackTranscoding",
                "enable_audio_playback_transcoding",
            ],
        ),
        enable_video_playback_transcoding: bool_field(
            policy,
            &[
                "EnableVideoPlaybackTranscoding",
                "enable_video_playback_transcoding",
            ],
        ),
        auto_remote_quality: i64_field(policy, &["AutoRemoteQuality", "auto_remote_quality"]),
        enable_playback_remuxing: bool_field(
            policy,
            &["EnablePlaybackRemuxing", "enable_playback_remuxing"],
        ),
        enable_content_deletion: bool_field(
            policy,
            &["EnableContentDeletion", "enable_content_deletion"],
        ),
        restricted_features: optional_string_array_field(
            policy,
            &["RestrictedFeatures", "restricted_features"],
        ),
        enable_content_deletion_from_folders: optional_string_array_field(
            policy,
            &[
                "EnableContentDeletionFromFolders",
                "enable_content_deletion_from_folders",
            ],
        ),
        enable_content_downloading: bool_field(
            policy,
            &["EnableContentDownloading", "enable_content_downloading"],
        ),
        enable_subtitle_downloading: bool_field(
            policy,
            &["EnableSubtitleDownloading", "enable_subtitle_downloading"],
        ),
        enable_subtitle_management: bool_field(
            policy,
            &["EnableSubtitleManagement", "enable_subtitle_management"],
        ),
        enable_sync_transcoding: bool_field(
            policy,
            &["EnableSyncTranscoding", "enable_sync_transcoding"],
        ),
        enable_media_conversion: bool_field(
            policy,
            &["EnableMediaConversion", "enable_media_conversion"],
        ),
        enabled_channels: optional_string_array_field(
            policy,
            &["EnabledChannels", "enabled_channels"],
        ),
        enable_all_channels: bool_field(policy, &["EnableAllChannels", "enable_all_channels"]),
        enable_all_folders: bool_field(policy, &["EnableAllFolders", "enable_all_folders"]),
        enabled_folders: optional_string_array_field(
            policy,
            &["EnabledFolders", "enabled_folders"],
        ),
        enable_public_sharing: bool_field(
            policy,
            &["EnablePublicSharing", "enable_public_sharing"],
        ),
        remote_client_bitrate_limit: number_field(
            policy,
            &["RemoteClientBitrateLimit", "remote_client_bitrate_limit"],
        ),
        excluded_sub_folders: optional_string_array_field(
            policy,
            &["ExcludedSubFolders", "excluded_sub_folders"],
        ),
        enable_all_devices: bool_field(policy, &["EnableAllDevices", "enable_all_devices"]),
        enabled_devices: optional_string_array_field(
            policy,
            &["EnabledDevices", "enabled_devices"],
        ),
        simultaneous_stream_limit: number_field(
            policy,
            &["SimultaneousStreamLimit", "simultaneous_stream_limit"],
        ),
        allow_camera_upload: bool_field(policy, &["AllowCameraUpload", "allow_camera_upload"]),
        allow_sharing_personal_items: bool_field(
            policy,
            &["AllowSharingPersonalItems", "allow_sharing_personal_items"],
        ),
        ..UserPolicyUpdate::default()
    }
}

fn folder_options(folders: &[Value]) -> Vec<UserAccessOption> {
    access_options(folders, &["ItemId", "Id", "id"], &["Name", "name"])
}

fn device_options_for_user(
    sessions: &[Value],
    user_id: &str,
    user_name: &str,
) -> Vec<UserAccessOption> {
    let matching_sessions: Vec<Value> = sessions
        .iter()
        .filter(|session| session_matches_user(session, user_id, user_name))
        .cloned()
        .collect();
    access_options(
        &matching_sessions,
        &[
            "DeviceId",
            "device_id",
            "DeviceName",
            "device_name",
            "Client",
        ],
        &["DeviceName", "Client", "device_name"],
    )
}

fn access_options(
    values: &[Value],
    id_fields: &[&str],
    name_fields: &[&str],
) -> Vec<UserAccessOption> {
    let mut options = Vec::new();
    for value in values {
        let Some(id) = string_field(value, id_fields) else {
            continue;
        };
        if options
            .iter()
            .any(|option: &UserAccessOption| option.id == id)
        {
            continue;
        }
        options.push(UserAccessOption {
            name: string_field(value, name_fields).unwrap_or_else(|| id.clone()),
            id,
        });
    }
    options.sort_by(|left, right| left.name.to_lowercase().cmp(&right.name.to_lowercase()));
    options
}

fn activity_for_user(
    sessions: &[Value],
    user_id: &str,
    user_name: &str,
) -> (Option<String>, u64, Vec<String>) {
    let mut latest = None;
    let mut active_sessions = 0;
    let mut devices = Vec::new();
    for session in sessions {
        if !session_matches_user(session, user_id, user_name) {
            continue;
        }
        if session
            .get("NowPlayingItem")
            .or_else(|| session.get("now_playing_item"))
            .is_some_and(|item| !item.is_null() && item.is_object())
        {
            active_sessions += 1;
        }
        if let Some(value) = string_field(
            session,
            &[
                "LastActivityDate",
                "LastPlaybackCheckIn",
                "LastPlaybackCheckInDate",
            ],
        ) && latest
            .as_ref()
            .is_none_or(|current: &String| value > *current)
        {
            latest = Some(value);
        }
        let device = string_field(session, &["DeviceName", "DeviceId", "Client"]);
        if let Some(device) = device.filter(|value| !value.is_empty())
            && !devices.iter().any(|existing| existing == &device)
        {
            devices.push(device);
        }
    }
    (latest, active_sessions, devices)
}

fn session_matches_user(session: &Value, user_id: &str, user_name: &str) -> bool {
    if let Some(session_user_id) = string_field(session, &["UserId", "user_id"]) {
        return !user_id.is_empty() && session_user_id == user_id;
    }
    string_field(session, &["UserName", "user_name"])
        .is_some_and(|session_user_name| session_user_name.eq_ignore_ascii_case(user_name))
}

fn policy_object_mut(profile: &mut Value) -> &mut Map<String, Value> {
    if !profile.get("Policy").is_some_and(Value::is_object) {
        profile["Policy"] = profile
            .get("policy")
            .filter(|value| value.is_object())
            .cloned()
            .unwrap_or_else(|| json!({}));
    }
    profile["Policy"]
        .as_object_mut()
        .expect("policy object initialized")
}

fn apply_policy_update(policy: &mut Map<String, Value>, payload: &UserPolicyUpdate) {
    macro_rules! insert_copy {
        ($field:ident, $name:literal) => {
            if let Some(value) = payload.$field {
                policy.insert($name.to_string(), json!(value));
            }
        };
    }
    macro_rules! insert_list {
        ($field:ident, $name:literal) => {
            if let Some(value) = payload.$field.as_ref() {
                policy.insert($name.to_string(), json!(normalized_policy_list(value)));
            }
        };
    }

    insert_copy!(is_administrator, "IsAdministrator");
    insert_copy!(is_hidden, "IsHidden");
    insert_copy!(is_hidden_remotely, "IsHiddenRemotely");
    insert_copy!(is_hidden_from_unused_devices, "IsHiddenFromUnusedDevices");
    insert_copy!(is_disabled, "IsDisabled");
    if payload.max_parental_rating_enabled == Some(false) {
        policy.insert("MaxParentalRating".to_string(), Value::Null);
    } else if let Some(value) = payload.max_parental_rating {
        policy.insert("MaxParentalRating".to_string(), json!(value));
    }
    insert_copy!(allow_tag_or_rating, "AllowTagOrRating");
    insert_list!(blocked_tags, "BlockedTags");
    insert_copy!(is_tag_blocking_mode_inclusive, "IsTagBlockingModeInclusive");
    insert_list!(include_tags, "IncludeTags");
    insert_copy!(enable_user_preference_access, "EnableUserPreferenceAccess");
    if let Some(schedules) = payload.access_schedules.as_ref() {
        policy.insert(
            "AccessSchedules".to_string(),
            Value::Array(
                schedules
                    .iter()
                    .map(|schedule| {
                        json!({
                            "DayOfWeek": schedule.day_of_week,
                            "StartHour": schedule.start_hour,
                            "EndHour": schedule.end_hour,
                        })
                    })
                    .collect(),
            ),
        );
    }
    insert_list!(block_unrated_items, "BlockUnratedItems");
    insert_copy!(
        enable_remote_control_of_other_users,
        "EnableRemoteControlOfOtherUsers"
    );
    insert_copy!(enable_shared_device_control, "EnableSharedDeviceControl");
    insert_copy!(enable_remote_access, "EnableRemoteAccess");
    insert_copy!(enable_live_tv_management, "EnableLiveTvManagement");
    insert_copy!(enable_live_tv_access, "EnableLiveTvAccess");
    insert_copy!(enable_media_playback, "EnableMediaPlayback");
    insert_copy!(
        enable_audio_playback_transcoding,
        "EnableAudioPlaybackTranscoding"
    );
    insert_copy!(
        enable_video_playback_transcoding,
        "EnableVideoPlaybackTranscoding"
    );
    insert_copy!(auto_remote_quality, "AutoRemoteQuality");
    insert_copy!(enable_playback_remuxing, "EnablePlaybackRemuxing");
    insert_copy!(enable_content_deletion, "EnableContentDeletion");
    insert_list!(restricted_features, "RestrictedFeatures");
    insert_list!(
        enable_content_deletion_from_folders,
        "EnableContentDeletionFromFolders"
    );
    insert_copy!(enable_content_downloading, "EnableContentDownloading");
    insert_copy!(enable_subtitle_downloading, "EnableSubtitleDownloading");
    insert_copy!(enable_subtitle_management, "EnableSubtitleManagement");
    insert_copy!(enable_sync_transcoding, "EnableSyncTranscoding");
    insert_copy!(enable_media_conversion, "EnableMediaConversion");
    insert_list!(enabled_channels, "EnabledChannels");
    insert_copy!(enable_all_channels, "EnableAllChannels");
    insert_list!(enabled_folders, "EnabledFolders");
    insert_copy!(enable_all_folders, "EnableAllFolders");
    insert_copy!(enable_public_sharing, "EnablePublicSharing");
    insert_copy!(remote_client_bitrate_limit, "RemoteClientBitrateLimit");
    insert_list!(excluded_sub_folders, "ExcludedSubFolders");
    insert_list!(enabled_devices, "EnabledDevices");
    insert_copy!(enable_all_devices, "EnableAllDevices");
    insert_copy!(simultaneous_stream_limit, "SimultaneousStreamLimit");
    insert_copy!(allow_camera_upload, "AllowCameraUpload");
    insert_copy!(allow_sharing_personal_items, "AllowSharingPersonalItems");
}

fn update_saved_user_policy(
    policies: &mut UserPoliciesDocument,
    server_id: &str,
    user_id: &str,
    profile: &Value,
    payload: &UserPolicyUpdate,
) {
    let name = string_field(profile, &["Name", "name"]).unwrap_or_else(|| user_id.to_string());
    let has_panel_update = payload.rate_limit_enabled.is_some()
        || payload.rate_limit_window_seconds.is_some()
        || payload.rate_limit_max_requests.is_some()
        || payload.rate_limit_block_seconds.is_some()
        || payload.rate_limit_action.is_some()
        || payload.concurrent_playback_limit_enabled.is_some()
        || payload.concurrent_playback_limit_max.is_some();
    if !has_panel_update
        && !policies
            .policies
            .iter()
            .any(|entry| entry.server_id == server_id && entry.user_id == user_id)
    {
        return;
    }
    let index = policies
        .policies
        .iter()
        .position(|entry| entry.server_id == server_id && entry.user_id == user_id);
    let index = match index {
        Some(index) => index,
        None => {
            policies
                .policies
                .push(default_user_policy(server_id, user_id, &name));
            policies.policies.len() - 1
        }
    };
    let entry = &mut policies.policies[index];
    entry.user_name = name;
    if let Some(value) = payload.rate_limit_enabled {
        entry.rate_limit_enabled = value;
    }
    if let Some(value) = payload.rate_limit_window_seconds {
        entry.rate_limit_window_seconds = value;
    }
    if let Some(value) = payload.rate_limit_max_requests {
        entry.rate_limit_max_requests = value;
    }
    if let Some(value) = payload.rate_limit_block_seconds {
        entry.rate_limit_block_seconds = value;
    }
    if let Some(value) = payload.rate_limit_action.as_deref() {
        entry.rate_limit_action = normalize_rate_action(value);
    }
    if let Some(value) = payload.concurrent_playback_limit_enabled {
        entry.concurrent_playback_limit_enabled = value;
    }
    if let Some(value) = payload.concurrent_playback_limit_max {
        entry.concurrent_playback_limit_max = value;
    }
}

fn validate_policy_update(payload: &UserPolicyUpdate) -> AppResult<()> {
    for list in [
        payload.blocked_tags.as_ref(),
        payload.include_tags.as_ref(),
        payload.block_unrated_items.as_ref(),
        payload.restricted_features.as_ref(),
        payload.enable_content_deletion_from_folders.as_ref(),
        payload.enabled_channels.as_ref(),
        payload.enabled_folders.as_ref(),
        payload.excluded_sub_folders.as_ref(),
        payload.enabled_devices.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if list.len() > MAX_POLICY_LIST_ITEMS
            || list
                .iter()
                .any(|value| value.trim().is_empty() || value.len() > 128)
        {
            return Err(AppError::Validation(
                "policy list contains an invalid entry".to_string(),
            ));
        }
    }
    if payload
        .block_unrated_items
        .as_ref()
        .is_some_and(|items| items.iter().any(|item| !is_valid_unrated_item(item)))
    {
        return Err(AppError::Validation(
            "block_unrated_items contains an unsupported item type".to_string(),
        ));
    }
    if payload.access_schedules.as_ref().is_some_and(|schedules| {
        schedules.len() > MAX_ACCESS_SCHEDULES
            || schedules.iter().any(|item| !is_valid_access_schedule(item))
    }) {
        return Err(AppError::Validation(
            "access schedule must use a supported day and a time range between 0 and 24"
                .to_string(),
        ));
    }
    if payload
        .max_parental_rating
        .is_some_and(|value| !(0..=MAX_PARENTAL_RATING).contains(&value))
    {
        return Err(AppError::Validation(
            "max parental rating is out of range".to_string(),
        ));
    }
    if payload
        .auto_remote_quality
        .is_some_and(|value| !(0..=MAX_REMOTE_QUALITY).contains(&value))
        || payload
            .remote_client_bitrate_limit
            .is_some_and(|value| value > MAX_REMOTE_BITRATE)
    {
        return Err(AppError::Validation(
            "remote playback quality limit is out of range".to_string(),
        ));
    }
    if payload
        .simultaneous_stream_limit
        .is_some_and(|value| value > MAX_USER_POLICY_LIMIT)
        || payload
            .concurrent_playback_limit_max
            .is_some_and(|value| value > MAX_USER_POLICY_LIMIT)
    {
        return Err(AppError::Validation(
            "simultaneous stream limit must be between 0 and 64".to_string(),
        ));
    }
    if payload
        .rate_limit_window_seconds
        .is_some_and(|value| !(1..=86_400).contains(&value))
        || payload
            .rate_limit_max_requests
            .is_some_and(|value| !(1..=10_000).contains(&value))
        || payload
            .rate_limit_block_seconds
            .is_some_and(|value| !(1..=86_400).contains(&value))
    {
        return Err(AppError::Validation(
            "user rate limit values are out of range".to_string(),
        ));
    }
    if payload.rate_limit_action.as_deref().is_some_and(|value| {
        !matches!(
            value.trim(),
            "block_ip" | "block_user" | "disable_user" | "mixed"
        )
    }) {
        return Err(AppError::Validation(
            "invalid user rate limit action".to_string(),
        ));
    }
    Ok(())
}

fn is_valid_access_schedule(schedule: &UserAccessSchedule) -> bool {
    matches!(
        schedule.day_of_week.trim(),
        "Sunday"
            | "Monday"
            | "Tuesday"
            | "Wednesday"
            | "Thursday"
            | "Friday"
            | "Saturday"
            | "Everyday"
            | "Weekday"
            | "Weekend"
    ) && schedule.start_hour.is_finite()
        && schedule.end_hour.is_finite()
        && (0.0..24.0).contains(&schedule.start_hour)
        && (0.0..=24.0).contains(&schedule.end_hour)
        && schedule.start_hour < schedule.end_hour
}

fn is_valid_unrated_item(value: &str) -> bool {
    matches!(
        value.trim(),
        "Movie"
            | "Trailer"
            | "Series"
            | "Music"
            | "Game"
            | "Book"
            | "LiveTvChannel"
            | "LiveTvProgram"
            | "ChannelContent"
            | "Other"
    )
}

pub(crate) fn normalize_user_policies(document: &mut UserPoliciesDocument) {
    for policy in &mut document.policies {
        policy.server_id = policy.server_id.trim().to_string();
        policy.user_id = policy.user_id.trim().to_string();
        policy.user_name = policy.user_name.trim().chars().take(256).collect();
        policy.rate_limit_window_seconds = policy.rate_limit_window_seconds.clamp(1, 86_400);
        policy.rate_limit_max_requests = policy.rate_limit_max_requests.clamp(1, 10_000);
        policy.rate_limit_block_seconds = policy.rate_limit_block_seconds.clamp(1, 86_400);
        policy.rate_limit_action = normalize_rate_action(&policy.rate_limit_action);
        policy.concurrent_playback_limit_max = policy.concurrent_playback_limit_max.clamp(1, 64);
    }
    document.policies.retain(|policy| {
        !policy.server_id.is_empty()
            && !policy.user_id.is_empty()
            && policy.server_id.len() <= 128
            && policy.user_id.len() <= 128
    });
    let mut seen = std::collections::HashSet::new();
    document
        .policies
        .retain(|policy| seen.insert((policy.server_id.clone(), policy.user_id.clone())));
    document.policies.truncate(4096);
}

fn normalized_policy_list(values: &[String]) -> Vec<String> {
    values
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .take(MAX_POLICY_LIST_ITEMS)
        .collect()
}

fn default_user_policy(server_id: &str, user_id: &str, user_name: &str) -> UserPolicyRecord {
    UserPolicyRecord {
        server_id: server_id.to_string(),
        user_id: user_id.to_string(),
        user_name: user_name.to_string(),
        rate_limit_action: default_rate_action(),
        ..UserPolicyRecord::default()
    }
}

fn normalize_rate_action(value: &str) -> String {
    match value.trim() {
        "block_user" | "disable_user" | "mixed" => value.trim().to_string(),
        _ => "block_ip".to_string(),
    }
}

fn string_field(value: &Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_str))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn object_field<'a>(value: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|name| value.get(*name))
}

fn bool_field(value: &Value, names: &[&str]) -> Option<bool> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_bool))
}

fn number_field(value: &Value, names: &[&str]) -> Option<u64> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_u64))
}

fn i64_field(value: &Value, names: &[&str]) -> Option<i64> {
    names
        .iter()
        .find_map(|name| value.get(*name).and_then(Value::as_i64))
}

fn policy_field_exists<'a>(value: &'a Value, names: &[&str]) -> Option<&'a Value> {
    names.iter().find_map(|name| value.get(*name))
}

fn optional_string_array_field(value: &Value, names: &[&str]) -> Option<Vec<String>> {
    names.iter().find_map(|name| {
        value.get(*name).and_then(Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
    })
}

fn access_schedules_field(value: &Value, names: &[&str]) -> Option<Vec<UserAccessSchedule>> {
    names.iter().find_map(|name| {
        value
            .get(*name)
            .and_then(|field| serde_json::from_value(field.clone()).ok())
    })
}

fn string_array_field(value: &Value, names: &[&str]) -> Vec<String> {
    optional_string_array_field(value, names).unwrap_or_default()
}

fn default_rate_window() -> u64 {
    60
}
fn default_rate_max() -> u64 {
    20
}
fn default_rate_block() -> u64 {
    1800
}
fn default_rate_action() -> String {
    "block_ip".to_string()
}
fn default_concurrent_max() -> u64 {
    3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_update_preserves_unknown_fields() {
        for mut profile in [
            json!({"Name":"alice","Policy":{"Unknown":true,"IsDisabled":false}}),
            json!({"Name":"alice","policy":{"Unknown":true,"IsDisabled":false}}),
        ] {
            let payload = UserPolicyUpdate {
                is_administrator: Some(true),
                is_disabled: Some(true),
                ..Default::default()
            };
            let policy = policy_object_mut(&mut profile);
            apply_policy_update(policy, &payload);
            assert_eq!(profile["Policy"]["Unknown"], true);
            assert_eq!(profile["Policy"]["IsDisabled"], true);
            assert_eq!(profile["Policy"]["IsAdministrator"], true);
        }
    }

    #[test]
    fn policy_update_initializes_missing_or_invalid_policy() {
        for mut profile in [json!({"Name":"alice"}), json!({"Policy": []})] {
            let payload = UserPolicyUpdate {
                enable_remote_access: Some(false),
                ..Default::default()
            };
            let policy = policy_object_mut(&mut profile);
            apply_policy_update(policy, &payload);
            assert_eq!(profile["Policy"]["EnableRemoteAccess"], false);
        }
    }

    #[test]
    fn extended_policy_fields_round_trip_without_dropping_unknown_values() {
        let mut profile = json!({"Policy": {"UnknownFuturePermission": true}});
        let payload = UserPolicyUpdate {
            is_hidden: Some(true),
            is_hidden_remotely: Some(true),
            is_hidden_from_unused_devices: Some(true),
            enable_user_preference_access: Some(true),
            enable_content_downloading: Some(true),
            enable_audio_playback_transcoding: Some(false),
            max_parental_rating_enabled: Some(true),
            max_parental_rating: Some(12),
            blocked_tags: Some(vec!["adult".to_string()]),
            block_unrated_items: Some(vec!["Movie".to_string(), "Series".to_string()]),
            access_schedules: Some(vec![UserAccessSchedule {
                day_of_week: "Weekend".to_string(),
                start_hour: 8.0,
                end_hour: 22.5,
            }]),
            ..Default::default()
        };
        let policy = policy_object_mut(&mut profile);
        apply_policy_update(policy, &payload);

        assert_eq!(profile["Policy"]["UnknownFuturePermission"], true);
        assert_eq!(profile["Policy"]["IsHidden"], true);
        assert_eq!(profile["Policy"]["EnableContentDownloading"], true);
        assert_eq!(profile["Policy"]["MaxParentalRating"], 12);
        assert_eq!(
            profile["Policy"]["AccessSchedules"][0]["DayOfWeek"],
            "Weekend"
        );

        let projected = policy_update_from_value(&profile["Policy"]);
        assert_eq!(projected.is_hidden, Some(true));
        assert_eq!(projected.enable_content_downloading, Some(true));
        assert_eq!(projected.max_parental_rating_enabled, Some(true));
        assert_eq!(projected.max_parental_rating, Some(12));
        assert_eq!(projected.access_schedules.unwrap()[0].end_hour, 22.5);
    }

    #[test]
    fn disabling_parental_rating_writes_null_and_invalid_schedules_are_rejected() {
        let mut profile = json!({"Policy": {"MaxParentalRating": 12}});
        let payload = UserPolicyUpdate {
            max_parental_rating_enabled: Some(false),
            access_schedules: Some(vec![UserAccessSchedule {
                day_of_week: "Everyday".to_string(),
                start_hour: 22.0,
                end_hour: 8.0,
            }]),
            ..Default::default()
        };
        apply_policy_update(policy_object_mut(&mut profile), &payload);
        assert!(profile["Policy"]["MaxParentalRating"].is_null());
        assert!(validate_policy_update(&payload).is_err());
    }

    #[test]
    fn activity_matches_user_id_and_deduplicates_devices() {
        let sessions = vec![
            json!({"UserId":"u1","DeviceName":"TV","NowPlayingItem":{"Id":"i1"},"LastActivityDate":"2026-01-02"}),
            json!({"UserName":"Alice","DeviceName":"TV","NowPlayingItem":{"Id":"i2"},"LastActivityDate":"2026-01-03"}),
            json!({"UserId":"u2","UserName":"Alice","DeviceName":"Other","NowPlayingItem":{"Id":"i3"}}),
            json!({"UserId":"u1","DeviceName":"Phone","LastActivityDate":"2026-01-04"}),
        ];
        let (last, count, devices) = activity_for_user(&sessions, "u1", "Alice");
        assert_eq!(last.as_deref(), Some("2026-01-04"));
        assert_eq!(count, 2);
        assert_eq!(devices, vec!["TV", "Phone"]);
        assert_eq!(device_options_for_user(&sessions, "u1", "Alice").len(), 2);
        assert!(!session_matches_user(&sessions[2], "u1", "Alice"));
    }

    #[test]
    fn normalize_user_policies_deduplicates_and_clamps_records() {
        let mut document = UserPoliciesDocument {
            policies: vec![
                UserPolicyRecord {
                    server_id: " server ".to_string(),
                    user_id: " user ".to_string(),
                    rate_limit_window_seconds: 0,
                    concurrent_playback_limit_max: u64::MAX,
                    ..Default::default()
                },
                UserPolicyRecord {
                    server_id: "server".to_string(),
                    user_id: "user".to_string(),
                    ..Default::default()
                },
            ],
        };
        normalize_user_policies(&mut document);
        assert_eq!(document.policies.len(), 1);
        assert_eq!(document.policies[0].rate_limit_window_seconds, 1);
        assert_eq!(document.policies[0].concurrent_playback_limit_max, 64);
    }

    #[test]
    fn normalize_user_templates_isolated_and_safe() {
        let mut document = UserTemplatesDocument {
            templates: vec![
                UserTemplate {
                    id: " template-a ".to_string(),
                    server_id: "server-a".to_string(),
                    name: " Home ".to_string(),
                    policy: UserPolicyUpdate {
                        rate_limit_window_seconds: Some(0),
                        concurrent_playback_limit_max: Some(u64::MAX),
                        rate_limit_action: Some("invalid".to_string()),
                        ..Default::default()
                    },
                },
                UserTemplate {
                    id: "template-a".to_string(),
                    server_id: "server-a".to_string(),
                    name: "Duplicate".to_string(),
                    policy: UserPolicyUpdate::default(),
                },
                UserTemplate {
                    id: "bad id".to_string(),
                    server_id: "server-b".to_string(),
                    name: "Invalid".to_string(),
                    policy: UserPolicyUpdate::default(),
                },
            ],
        };
        normalize_user_templates(&mut document);
        assert_eq!(document.templates.len(), 1);
        assert_eq!(document.templates[0].name, "Home");
        assert_eq!(
            document.templates[0].policy.rate_limit_window_seconds,
            Some(1)
        );
        assert_eq!(
            document.templates[0].policy.concurrent_playback_limit_max,
            Some(64)
        );
        assert_eq!(
            document.templates[0].policy.rate_limit_action.as_deref(),
            Some("block_ip")
        );
    }
}
