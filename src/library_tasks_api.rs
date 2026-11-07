use std::{
    future::Future,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::task::JoinSet;

use crate::{
    AppState, auth,
    crypto_api::EncryptedRequest,
    db::{ScheduledTaskFailureHistory, ScheduledTaskFailureInput, TaskFailureListParams},
    emby,
    error::{AppError, AppResult, safe_error_message},
};

const SERVER_QUERY_CONCURRENCY_LIMIT: usize = 4;

#[derive(Debug, Deserialize)]
pub struct LibraryQuery {
    #[serde(default)]
    pub server_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskIdRequest {
    server_id: String,
}

#[derive(Debug, Deserialize)]
struct LibraryPathValidationRequest {
    server_id: String,
    paths: Vec<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct FieldCapability {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<&'static str>,
}

impl FieldCapability {
    fn available() -> Self {
        Self {
            available: true,
            unavailable_reason: None,
        }
    }

    fn emby_not_provided() -> Self {
        Self {
            available: false,
            unavailable_reason: Some("emby_not_provided"),
        }
    }

    fn unsupported() -> Self {
        Self {
            available: false,
            unavailable_reason: Some("unsupported"),
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LibrarySummaryCapabilities {
    pub path: FieldCapability,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LibraryLocationStatusResponse {
    pub path: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LibraryPathValidationResponse {
    pub items: Vec<LibraryLocationStatusResponse>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LibrarySummaryResponse {
    pub server_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    pub name: String,
    pub path: Option<String>,
    pub locations: Vec<String>,
    pub location_statuses: Vec<LibraryLocationStatusResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection_type: Option<String>,
    pub capabilities: LibrarySummaryCapabilities,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LibraryTaskCapabilities {
    pub path: FieldCapability,
    pub progress: FieldCapability,
    pub last_run: FieldCapability,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct LibraryTaskResponse {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub task_type: Option<String>,
    pub state: String,
    pub path: Option<String>,
    pub progress: Option<f64>,
    pub last_run: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub server_id: String,
    pub capabilities: LibraryTaskCapabilities,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ServerQueryFailureResponse {
    pub server_id: String,
    pub error: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct LibraryCollectionResponse<T> {
    pub items: Vec<T>,
    pub failures: Vec<ServerQueryFailureResponse>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct TaskFailureResponse {
    pub summary: Option<String>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct TaskFailureDetailResponse {
    #[serde(flatten)]
    compatibility: TaskFailureResponse,
    history: Vec<ScheduledTaskFailureHistory>,
}

pub async fn list_library_libraries(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LibraryQuery>,
) -> AppResult<Json<LibraryCollectionResponse<LibrarySummaryResponse>>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "library.tasks.read")?;
    let server_ids =
        match resolve_server_ids(&state, &auth_context, query.server_id.as_deref()).await {
            Ok(server_ids) => server_ids,
            Err(error) => {
                state.settings_store.record_audit_best_effort(
                    Some(auth_context.user_id),
                    "library_tasks.list",
                    "列出库数据失败",
                    "error",
                );
                return Err(error);
            }
        };
    let client = state.client.clone();
    let query_state = state.clone();
    let queries = server_ids.into_iter().map(move |server_id| {
        let client = client.clone();
        let state = query_state.clone();
        async move {
            let result = async {
                let config = load_server_config(&state, &server_id).await?;
                load_library_summaries_without_validation(&client, &config, &server_id).await
            }
            .await;
            (server_id, result)
        }
    });
    let libraries = match flatten_server_results(collect_server_queries(queries).await) {
        Ok(libraries) => libraries,
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(auth_context.user_id),
                "library_tasks.list",
                "列出虚拟文件夹失败",
                "error",
            );
            return Err(error);
        }
    };
    state.settings_store.record_audit_best_effort(
        Some(auth_context.user_id),
        "library_tasks.list",
        "列出虚拟文件夹",
        if libraries.failures.is_empty() {
            "success"
        } else {
            "partial"
        },
    );
    Ok(Json(LibraryCollectionResponse {
        items: libraries.items,
        failures: libraries.failures,
    }))
}

pub async fn validate_library_paths(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<LibraryPathValidationResponse>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    if let Err(error) = auth::require_permission(&auth_context, "library.tasks.execute") {
        record_path_validation_audit(&state, auth_context.user_id, "error");
        return Err(error);
    }
    let payload: LibraryPathValidationRequest = match state
        .crypto_keys
        .decrypt_named(&request, "validate_library_paths")
    {
        Ok(payload) => payload,
        Err(error) => {
            record_path_validation_audit(&state, auth_context.user_id, "error");
            return Err(error);
        }
    };
    let server_id = match normalize_required_server_id(&payload.server_id) {
        Ok(server_id) => server_id,
        Err(error) => {
            record_path_validation_audit(&state, auth_context.user_id, "error");
            return Err(error);
        }
    };
    if let Err(error) = auth::require_server_access(&auth_context, server_id) {
        record_path_validation_audit(&state, auth_context.user_id, "error");
        return Err(error);
    }
    let paths = match normalize_library_validation_paths(payload.paths) {
        Ok(paths) => paths,
        Err(error) => {
            record_path_validation_audit(&state, auth_context.user_id, "error");
            return Err(error);
        }
    };
    let config = match load_server_config(&state, server_id).await {
        Ok(config) => config,
        Err(error) => {
            record_path_validation_audit(&state, auth_context.user_id, "error");
            return Err(error);
        }
    };
    let items = validate_library_paths_for_server(&state.client, &config, paths).await;
    let valid_count = items.iter().filter(|item| item.status == "valid").count();
    let outcome = if valid_count == items.len() {
        "success"
    } else if valid_count == 0 {
        "validation_failed"
    } else {
        "partial"
    };
    record_path_validation_audit(&state, auth_context.user_id, outcome);
    Ok(Json(LibraryPathValidationResponse { items }))
}

fn record_path_validation_audit(state: &AppState, user_id: i64, outcome: &str) {
    state.settings_store.record_audit_best_effort(
        Some(user_id),
        "library_tasks.validate_paths",
        "验证媒体库路径",
        outcome,
    );
}

pub async fn list_library_tasks(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LibraryQuery>,
) -> AppResult<Json<LibraryCollectionResponse<LibraryTaskResponse>>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "library.tasks.read")?;
    let server_ids =
        match resolve_server_ids(&state, &auth_context, query.server_id.as_deref()).await {
            Ok(server_ids) => server_ids,
            Err(error) => {
                state.settings_store.record_audit_best_effort(
                    Some(auth_context.user_id),
                    "library_tasks.list",
                    "列出库数据失败",
                    "error",
                );
                return Err(error);
            }
        };
    let client = state.client.clone();
    let query_state = state.clone();
    let queries = server_ids.into_iter().map(move |server_id| {
        let client = client.clone();
        let state = query_state.clone();
        async move {
            let result = async {
                let config = load_server_config(&state, &server_id).await?;
                let tasks = emby::list_scheduled_tasks(&client, &config).await?;
                let observed_at = current_timestamp_ms();
                let history_store = state.settings_store.task_failure_history_store();
                let tasks = tasks
                    .into_iter()
                    .map(|task| {
                        observe_task_failure(&history_store, &server_id, &task, observed_at);
                        map_library_task(&server_id, task)
                    })
                    .collect::<Vec<_>>();
                Ok(tasks)
            }
            .await;
            (server_id, result)
        }
    });
    let tasks = match flatten_server_results(collect_server_queries(queries).await) {
        Ok(tasks) => tasks,
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(auth_context.user_id),
                "library_tasks.list",
                "列出库扫描任务失败",
                "error",
            );
            return Err(error);
        }
    };
    state.settings_store.record_audit_best_effort(
        Some(auth_context.user_id),
        "library_tasks.list",
        "列出库扫描任务",
        if tasks.failures.is_empty() {
            "success"
        } else {
            "partial"
        },
    );
    Ok(Json(LibraryCollectionResponse {
        items: tasks.items,
        failures: tasks.failures,
    }))
}

pub async fn get_library_task_failure(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
    Query(query): Query<LibraryQuery>,
) -> AppResult<Json<TaskFailureDetailResponse>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    auth::require_permission(&auth_context, "library.tasks.read")?;
    let result = async {
        let server_id = resolve_single_server_id(query.server_id.as_deref())?;
        auth::require_server_access(&auth_context, server_id)?;
        let mut history = state
            .settings_store
            .task_failure_history_store()
            .list_failures(&TaskFailureListParams {
                server_id: Some(server_id.to_string()),
                task_id: Some(task_id.clone()),
                ..Default::default()
            })?;
        for entry in &mut history {
            entry.safe_error_summary = entry
                .safe_error_summary
                .as_deref()
                .map(emby::sanitize_task_error_summary);
        }
        let config = load_server_config(&state, server_id).await?;
        let summary =
            match emby::get_task_failure_summary(&state.client, &config, &task_id).await {
                Ok(summary) => summary,
                Err(error) if !history.is_empty() => {
                    tracing::warn!(
                        server_id,
                        error = %safe_error_message(&error),
                        "failed to refresh scheduled task failure summary; using persisted history"
                    );
                    None
                }
                Err(error) => return Err(error),
            }
            .or_else(|| {
                history
                    .first()
                    .and_then(|entry| entry.safe_error_summary.clone())
            });
        Ok((summary, history))
    }
    .await;
    match result {
        Ok((summary, history)) => {
            state.settings_store.record_audit_best_effort(
                Some(auth_context.user_id),
                "library_tasks.failure",
                "获取任务失败摘要",
                "success",
            );
            Ok(Json(TaskFailureDetailResponse {
                compatibility: TaskFailureResponse { summary },
                history,
            }))
        }
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(auth_context.user_id),
                "library_tasks.failure",
                "获取任务失败摘要失败",
                "error",
            );
            Err(error)
        }
    }
}

pub async fn start_library_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<()>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    if let Err(error) = auth::require_permission(&auth_context, "library.tasks.execute") {
        state.settings_store.record_audit_best_effort(
            Some(auth_context.user_id),
            "library_tasks.execute",
            "执行库扫描任务失败",
            "error",
        );
        return Err(error);
    }
    let payload: TaskIdRequest = match state
        .crypto_keys
        .decrypt_named(&request, "start_library_task")
    {
        Ok(payload) => payload,
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(auth_context.user_id),
                "library_tasks.start",
                &format!("启动库扫描任务失败：{}", safe_error_message(&error)),
                "error",
            );
            return Err(error);
        }
    };
    let action_result = async {
        let server_id = normalize_required_server_id(&payload.server_id)?;
        auth::require_server_access(&auth_context, server_id)?;
        let config = load_server_config(&state, server_id).await?;
        emby::start_scheduled_task(&state.client, &config, &task_id).await
    }
    .await;
    match action_result {
        Ok(()) => {
            state.settings_store.record_audit_best_effort(
                Some(auth_context.user_id),
                "library_tasks.start",
                "启动库扫描任务",
                "success",
            );
            Ok(Json(()))
        }
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(auth_context.user_id),
                "library_tasks.start",
                "启动库扫描任务失败",
                "error",
            );
            Err(error)
        }
    }
}

pub async fn cancel_library_task(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<()>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    if let Err(error) = auth::require_permission(&auth_context, "library.tasks.execute") {
        state.settings_store.record_audit_best_effort(
            Some(auth_context.user_id),
            "library_tasks.execute",
            "执行库扫描任务失败",
            "error",
        );
        return Err(error);
    }
    let payload: TaskIdRequest = match state
        .crypto_keys
        .decrypt_named(&request, "cancel_library_task")
    {
        Ok(payload) => payload,
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(auth_context.user_id),
                "library_tasks.cancel",
                &format!("取消库扫描任务失败：{}", safe_error_message(&error)),
                "error",
            );
            return Err(error);
        }
    };
    let action_result = async {
        let server_id = normalize_required_server_id(&payload.server_id)?;
        auth::require_server_access(&auth_context, server_id)?;
        let config = load_server_config(&state, server_id).await?;
        emby::cancel_scheduled_task(&state.client, &config, &task_id).await
    }
    .await;
    match action_result {
        Ok(()) => {
            state.settings_store.record_audit_best_effort(
                Some(auth_context.user_id),
                "library_tasks.cancel",
                "取消库扫描任务",
                "success",
            );
            Ok(Json(()))
        }
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(auth_context.user_id),
                "library_tasks.cancel",
                "取消库扫描任务失败",
                "error",
            );
            Err(error)
        }
    }
}

async fn resolve_server_ids(
    state: &AppState,
    auth_context: &auth::AuthContext,
    requested: Option<&str>,
) -> AppResult<Vec<String>> {
    if let Some(server_id) = requested {
        let server_id = server_id.trim();
        if !server_id.is_empty() && server_id != "all" {
            let server_id = normalize_required_server_id(server_id)?;
            auth::require_server_access(auth_context, server_id)?;
            return Ok(vec![server_id.to_string()]);
        }
    }

    let config = state.config.read().await;
    let candidates = if config.servers.is_empty() {
        vec!["default".to_string()]
    } else {
        config
            .servers
            .iter()
            .filter(|server| server.enabled)
            .map(|server| server.id.clone())
            .collect()
    };
    let accessible = candidates
        .into_iter()
        .filter(|server_id| auth::has_server_access(auth_context, server_id))
        .collect::<Vec<_>>();
    Ok(accessible)
}

fn resolve_single_server_id(server_id: Option<&str>) -> AppResult<&str> {
    let server_id =
        server_id.ok_or_else(|| AppError::Validation("server_id is required".into()))?;
    if server_id.trim() == "all" {
        return Err(AppError::Validation(
            "a single server_id is required".to_string(),
        ));
    }
    normalize_required_server_id(server_id)
}

async fn load_server_config(state: &AppState, server_id: &str) -> AppResult<crate::config::Config> {
    match emby::get_config_for_server(state, server_id).await? {
        Some(config) => Ok(config),
        None => Err(AppError::Validation("unknown server".to_string())),
    }
}

fn normalize_required_server_id(server_id: &str) -> AppResult<&str> {
    let server_id = server_id.trim();
    if server_id.is_empty() {
        return Err(AppError::Validation("server_id is required".to_string()));
    }
    if server_id.len() > 128 || server_id.chars().any(|ch| ch.is_control()) {
        return Err(AppError::Validation("invalid server_id".to_string()));
    }
    Ok(server_id)
}

async fn collect_server_queries<I, F, T>(futures: I) -> Vec<T>
where
    I: IntoIterator<Item = F>,
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let mut pending = futures.into_iter().enumerate();
    let mut tasks = JoinSet::new();
    for _ in 0..SERVER_QUERY_CONCURRENCY_LIMIT {
        let Some((index, future)) = pending.next() else {
            break;
        };
        tasks.spawn(async move { (index, future.await) });
    }

    let mut results = Vec::new();
    while let Some(result) = tasks.join_next().await {
        if let Some((index, future)) = pending.next() {
            tasks.spawn(async move { (index, future.await) });
        }
        match result {
            Ok(result) => results.push(result),
            Err(error) => tracing::error!(error = %error, "library task server query failed"),
        }
    }
    results.sort_unstable_by_key(|(index, _)| *index);
    results.into_iter().map(|(_, result)| result).collect()
}

#[derive(Debug)]
struct FlattenedServerResults<T> {
    items: Vec<T>,
    failures: Vec<ServerQueryFailureResponse>,
}

fn flatten_server_results<T>(
    results: Vec<(String, AppResult<Vec<T>>)>,
) -> AppResult<FlattenedServerResults<T>> {
    let mut items = Vec::new();
    let mut failures = Vec::new();
    let mut successful_servers = 0;
    let mut first_error = None;
    for (server_id, result) in results {
        match result {
            Ok(mut server_items) => {
                successful_servers += 1;
                items.append(&mut server_items);
            }
            Err(error) => {
                tracing::warn!(
                    server_id,
                    error = %safe_error_message(&error),
                    "failed to query library task data"
                );
                failures.push(ServerQueryFailureResponse {
                    server_id,
                    error: safe_error_message(&error),
                });
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    if successful_servers == 0 {
        if let Some(error) = first_error {
            return Err(error);
        }
    }
    Ok(FlattenedServerResults { items, failures })
}

fn current_timestamp_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

fn parse_task_timestamp(value: Option<&str>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value?.trim())
        .ok()
        .map(|timestamp| timestamp.timestamp_millis())
}

fn task_failure_run_identity(task: &emby::ScheduledTask) -> Option<String> {
    let result = task.last_execution_result.as_ref()?;
    let status = result.status.as_deref()?.trim();
    let normalized_status = status.to_ascii_lowercase();
    if !matches!(
        normalized_status.as_str(),
        "failed" | "cancelled" | "aborted"
    ) {
        return None;
    }

    let execution_id = result
        .id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let started_at = result
        .start_time_utc
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let ended_at = result
        .end_time_utc
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    // Emby does not consistently provide an execution id. Prefer it over timestamps, then use
    // stable start/end metadata. If all execution metadata is absent, use only stable task fields;
    // never include upstream error text in identity material.
    let material = if let Some(execution_id) = execution_id {
        format!("id\0{execution_id}")
    } else if started_at.is_some() || ended_at.is_some() {
        format!(
            "time\0{}\0{}",
            started_at.unwrap_or_default(),
            ended_at.unwrap_or_default()
        )
    } else {
        format!(
            "fallback\0{}\0{}\0{}\0{}\0{}\0{}",
            task.id.trim(),
            normalized_status,
            task.name.trim(),
            task.key.as_deref().unwrap_or_default().trim(),
            result.name.as_deref().unwrap_or_default().trim(),
            result.key.as_deref().unwrap_or_default().trim(),
        )
    };
    let digest = Sha256::digest(material.as_bytes());
    Some(format!("sha256:{digest:x}"))
}

fn failure_observation_input(
    server_id: &str,
    task: &emby::ScheduledTask,
    observed_at: i64,
) -> Option<ScheduledTaskFailureInput> {
    let safe_error_summary = emby::task_failure_summary(task)?;
    let result = task.last_execution_result.as_ref()?;
    Some(ScheduledTaskFailureInput {
        server_id: server_id.to_string(),
        task_id: task.id.clone(),
        run_identity: task_failure_run_identity(task)?,
        observed_at,
        status: result.status.as_deref()?.trim().to_ascii_lowercase(),
        error_summary: Some(safe_error_summary),
        started_at: parse_task_timestamp(result.start_time_utc.as_deref()),
        ended_at: parse_task_timestamp(result.end_time_utc.as_deref()),
    })
}

fn observe_task_failure(
    history_store: &crate::db::TaskFailureHistoryStore,
    server_id: &str,
    task: &emby::ScheduledTask,
    observed_at: i64,
) {
    let Some(input) = failure_observation_input(server_id, task, observed_at) else {
        return;
    };
    if let Err(error) = history_store.record_failure(input) {
        tracing::warn!(
            server_id,
            error = %safe_error_message(&error),
            "failed to persist scheduled task failure history"
        );
    }
}

async fn load_library_summaries_without_validation(
    client: &reqwest::Client,
    config: &crate::config::Config,
    server_id: &str,
) -> AppResult<Vec<LibrarySummaryResponse>> {
    let folders = emby::list_virtual_folders(client, config).await?;
    Ok(map_library_summaries_without_validation(server_id, folders))
}

fn map_library_summaries_without_validation(
    server_id: &str,
    folders: Vec<emby::VirtualFolder>,
) -> Vec<LibrarySummaryResponse> {
    folders
        .into_iter()
        .map(|folder| {
            let location_statuses = library_location_validation_paths(&folder.locations)
                .into_iter()
                .map(map_unvalidated_library_location_status)
                .collect();
            map_library_summary(server_id, folder, location_statuses)
        })
        .collect()
}

fn normalize_library_validation_paths(paths: Vec<String>) -> AppResult<Vec<String>> {
    if paths.is_empty() {
        return Err(AppError::Validation(
            "at least one library path is required".to_string(),
        ));
    }
    if paths.len() > 64 {
        return Err(AppError::Validation("too many library paths".to_string()));
    }
    paths
        .into_iter()
        .map(|path| {
            let path = path.trim();
            if path.is_empty() {
                return Err(AppError::Validation("library path is required".to_string()));
            }
            if path.len() > 4096 || path.chars().any(char::is_control) {
                return Err(AppError::Validation("invalid library path".to_string()));
            }
            Ok(path.to_string())
        })
        .collect()
}

async fn validate_library_paths_for_server(
    client: &reqwest::Client,
    config: &crate::config::Config,
    paths: Vec<String>,
) -> Vec<LibraryLocationStatusResponse> {
    let validations = paths.into_iter().map(|path| {
        let client = client.clone();
        let config = config.clone();
        async move {
            let result =
                emby::validate_server_path(&client, &config, &path, false, Some(false), None, None)
                    .await;
            map_library_location_status(path, result)
        }
    });
    collect_server_queries(validations).await
}

fn map_unvalidated_library_location_status(path: String) -> LibraryLocationStatusResponse {
    LibraryLocationStatusResponse {
        path,
        status: "unvalidated".to_string(),
        error: None,
    }
}

fn library_location_validation_paths(locations: &[String]) -> Vec<String> {
    locations
        .iter()
        .filter(|path| !path.trim().is_empty())
        .cloned()
        .collect()
}

fn map_library_location_status(
    path: String,
    result: AppResult<()>,
) -> LibraryLocationStatusResponse {
    match result {
        Ok(()) => LibraryLocationStatusResponse {
            path,
            status: "valid".to_string(),
            error: None,
        },
        Err(error) => LibraryLocationStatusResponse {
            path,
            status: "validation_failed".to_string(),
            error: Some(safe_error_message(&error)),
        },
    }
}

fn map_library_summary(
    server_id: &str,
    folder: emby::VirtualFolder,
    location_statuses: Vec<LibraryLocationStatusResponse>,
) -> LibrarySummaryResponse {
    let locations = folder.locations;
    let path = locations
        .iter()
        .map(|path| path.trim().to_string())
        .find(|path| !path.is_empty());
    let path_available = path.is_some();
    LibrarySummaryResponse {
        server_id: server_id.to_string(),
        id: folder.id,
        item_id: folder.item_id,
        name: folder.name.unwrap_or_default(),
        path,
        locations,
        location_statuses,
        collection_type: folder.collection_type,
        capabilities: LibrarySummaryCapabilities {
            path: if path_available {
                FieldCapability::available()
            } else {
                FieldCapability::emby_not_provided()
            },
        },
    }
}

fn map_library_task(server_id: &str, task: emby::ScheduledTask) -> LibraryTaskResponse {
    let error = emby::task_failure_summary(&task);
    let progress = task
        .current_progress_percentage
        .filter(|value| value.is_finite());
    let last_run = task.last_execution_result.as_ref().and_then(|result| {
        result
            .start_time_utc
            .as_deref()
            .or(result.end_time_utc.as_deref())
            .map(str::to_string)
    });
    let has_last_run = last_run.is_some();
    LibraryTaskResponse {
        id: task.id,
        name: task.name,
        task_type: task
            .description
            .and_then(|description| (!description.trim().is_empty()).then_some(description)),
        state: task.state,
        path: None,
        progress,
        last_run,
        error,
        server_id: server_id.to_string(),
        capabilities: LibraryTaskCapabilities {
            path: FieldCapability::unsupported(),
            progress: if progress.is_some() {
                FieldCapability::available()
            } else {
                FieldCapability::emby_not_provided()
            },
            last_run: if has_last_run {
                FieldCapability::available()
            } else {
                FieldCapability::emby_not_provided()
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        FieldCapability, LibraryLocationStatusResponse, LibraryPathValidationRequest, LibraryQuery,
        LibrarySummaryCapabilities, LibrarySummaryResponse, LibraryTaskCapabilities,
        ServerQueryFailureResponse, TaskFailureDetailResponse, TaskFailureResponse, TaskIdRequest,
        collect_server_queries, failure_observation_input, flatten_server_results,
        library_location_validation_paths, load_library_summaries_without_validation,
        map_library_location_status, map_library_summaries_without_validation, map_library_summary,
        map_library_task, map_unvalidated_library_location_status,
        normalize_library_validation_paths, normalize_required_server_id, resolve_single_server_id,
        task_failure_run_identity, validate_library_paths_for_server,
    };
    use crate::config::Config;
    use crate::db::{SettingsStore, TaskFailureListParams};
    use crate::emby::{ScheduledTask, TaskExecutionResult, VirtualFolder};
    use crate::error::AppError;
    use std::{
        fs,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
        time::Duration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        task::JoinSet,
        time::{sleep, timeout},
    };

    static TEST_DATABASE_COUNTER: AtomicU64 = AtomicU64::new(0);

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            let count = stream.read(&mut buffer).await.expect("read request");
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(headers_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers_end = headers_end + 4;
                let headers = String::from_utf8_lossy(&bytes[..headers_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if bytes.len() >= headers_end + content_length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    async fn write_http_response(
        stream: &mut tokio::net::TcpStream,
        status_line: &str,
        body: &str,
    ) {
        let response = format!(
            "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("write response");
    }

    async fn serve_virtual_folders_and_detect_extra_requests(
        body: &str,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let body = body.to_string();
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept list request");
            let mut requests = vec![read_http_request(&mut stream).await];
            write_http_response(&mut stream, "HTTP/1.1 200 OK", &body).await;
            if let Ok(Ok((mut stream, _))) =
                timeout(Duration::from_millis(200), listener.accept()).await
            {
                requests.push(read_http_request(&mut stream).await);
                write_http_response(&mut stream, "HTTP/1.1 200 OK", "{}").await;
            }
            requests
        });
        (format!("http://{addr}"), handle)
    }

    async fn serve_path_validation_responses(
        request_count: usize,
    ) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let handle = tokio::spawn(async move {
            let mut handlers = JoinSet::new();
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().await.expect("accept validation request");
                handlers.spawn(async move {
                    let request = read_http_request(&mut stream).await;
                    let (delay_ms, status, body) = if request.contains("Path=%2Ffirst") {
                        (60, "HTTP/1.1 200 OK", "{}")
                    } else if request.contains("Path=%2Fsecond") {
                        (0, "HTTP/1.1 400 Bad Request", "token=do-not-leak")
                    } else {
                        (20, "HTTP/1.1 204 No Content", "")
                    };
                    sleep(Duration::from_millis(delay_ms)).await;
                    write_http_response(&mut stream, status, body).await;
                    request
                });
            }
            let mut requests = Vec::new();
            while let Some(request) = handlers.join_next().await {
                requests.push(request.expect("validation server task"));
            }
            requests
        });
        (format!("http://{addr}"), handle)
    }

    fn failed_task(task_id: &str, execution_id: Option<&str>) -> ScheduledTask {
        ScheduledTask {
            id: task_id.to_string(),
            name: "Scan".to_string(),
            description: Some("LibraryScan".to_string()),
            category: None,
            key: None,
            state: "Idle".to_string(),
            is_hidden: None,
            is_enabled: None,
            triggers: Vec::new(),
            current_progress_percentage: None,
            last_execution_result: Some(TaskExecutionResult {
                status: Some("Failed".to_string()),
                name: None,
                key: None,
                id: execution_id.map(str::to_string),
                error_message: Some("disk full".to_string()),
                long_error_message: None,
                start_time_utc: Some("2026-08-15T01:02:03Z".to_string()),
                end_time_utc: Some("2026-08-15T01:03:03Z".to_string()),
            }),
        }
    }

    fn test_settings_store(name: &str) -> (SettingsStore, PathBuf) {
        let id = TEST_DATABASE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "embypanel-library-tasks-{name}-{}-{id}.sqlite",
            std::process::id()
        ));
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        (store, path)
    }

    fn remove_test_database(path: &PathBuf) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(format!("{}-wal", path.display()));
        let _ = fs::remove_file(format!("{}-shm", path.display()));
    }

    #[test]
    fn library_query_allows_missing_and_all_server_id() {
        let query: LibraryQuery = serde_json::from_str("{}").unwrap();
        assert!(query.server_id.is_none());
        let query: LibraryQuery = serde_json::from_str(r#"{"server_id":"all"}"#).unwrap();
        assert_eq!(query.server_id.as_deref(), Some("all"));
    }

    #[test]
    fn normalize_server_id_rejects_blank_and_control() {
        assert!(normalize_required_server_id("").is_err());
        assert!(normalize_required_server_id("   ").is_err());
        assert!(normalize_required_server_id("bad\nid").is_err());
        assert_eq!(normalize_required_server_id(" main ").unwrap(), "main");
        assert!(resolve_single_server_id(Some("all")).is_err());
    }

    #[test]
    fn map_library_summary_uses_first_non_empty_location_and_capability() {
        let response = map_library_summary(
            "main",
            VirtualFolder {
                id: Some("lib1".to_string()),
                item_id: None,
                name: Some("Movies".to_string()),
                locations: vec!["  ".to_string(), "/data/movies".to_string()],
                collection_type: None,
            },
            vec![LibraryLocationStatusResponse {
                path: "/data/movies".to_string(),
                status: "valid".to_string(),
                error: None,
            }],
        );
        assert_eq!(
            response,
            LibrarySummaryResponse {
                server_id: "main".to_string(),
                id: Some("lib1".to_string()),
                item_id: None,
                name: "Movies".to_string(),
                path: Some("/data/movies".to_string()),
                locations: vec!["  ".to_string(), "/data/movies".to_string()],
                location_statuses: vec![LibraryLocationStatusResponse {
                    path: "/data/movies".to_string(),
                    status: "valid".to_string(),
                    error: None,
                }],
                collection_type: None,
                capabilities: LibrarySummaryCapabilities {
                    path: FieldCapability::available(),
                },
            }
        );

        let response = map_library_summary(
            "main",
            VirtualFolder {
                id: Some("lib2".to_string()),
                item_id: None,
                name: Some("Unknown".to_string()),
                locations: Vec::new(),
                collection_type: None,
            },
            Vec::new(),
        );
        assert_eq!(response.path, None);
        assert_eq!(
            response.capabilities.path,
            FieldCapability::emby_not_provided()
        );
    }

    #[tokio::test]
    async fn library_list_fetch_never_calls_path_validation() {
        let body = r#"{"Items":[{"Id":"lib1","Name":"Movies","Locations":["/first","/second"]}]}"#;
        let (base, handle) = serve_virtual_folders_and_detect_extra_requests(body).await;
        let mut config = Config::default_runtime();
        config.emby_host = base;
        config.emby_api_key = "test-key".to_string();

        let libraries =
            load_library_summaries_without_validation(&reqwest::Client::new(), &config, "main")
                .await
                .expect("list libraries");
        assert_eq!(libraries.len(), 1);
        assert_eq!(
            libraries[0]
                .location_statuses
                .iter()
                .map(|status| status.status.as_str())
                .collect::<Vec<_>>(),
            vec!["unvalidated", "unvalidated"]
        );

        let requests = handle.await.expect("server task");
        assert_eq!(requests.len(), 1);
        assert!(requests[0].starts_with("GET /Library/VirtualFolders/Query?"));
        assert!(!requests[0].contains("Environment/ValidatePath"));
    }

    #[tokio::test]
    async fn explicit_path_validation_preserves_order_and_isolates_safe_failures() {
        let (base, handle) = serve_path_validation_responses(3).await;
        let mut config = Config::default_runtime();
        config.emby_host = base;
        config.emby_api_key = "test-key".to_string();

        let statuses = validate_library_paths_for_server(
            &reqwest::Client::new(),
            &config,
            vec![
                "/first".to_string(),
                "/second".to_string(),
                "/third".to_string(),
            ],
        )
        .await;
        assert_eq!(
            statuses
                .iter()
                .map(|status| (status.path.as_str(), status.status.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("/first", "valid"),
                ("/second", "validation_failed"),
                ("/third", "valid"),
            ]
        );
        assert_eq!(
            statuses[1].error.as_deref(),
            Some("Emby server-side path validation failed with status 400 Bad Request")
        );
        assert!(
            !statuses[1]
                .error
                .as_deref()
                .unwrap()
                .contains("do-not-leak")
        );
        assert!(statuses[0].error.is_none());
        assert!(statuses[2].error.is_none());

        let requests = handle.await.expect("server task");
        assert_eq!(requests.len(), 3);
        assert!(
            requests
                .iter()
                .all(|request| request.starts_with("POST /Environment/ValidatePath?"))
        );
    }

    #[test]
    fn library_list_marks_locations_unvalidated_without_calling_validation() {
        let libraries = map_library_summaries_without_validation(
            "main",
            vec![VirtualFolder {
                id: Some("lib1".to_string()),
                item_id: None,
                name: Some("Movies".to_string()),
                locations: vec!["/data/movies".to_string(), "  ".to_string()],
                collection_type: None,
            }],
        );
        assert_eq!(libraries.len(), 1);
        assert_eq!(
            libraries[0].location_statuses,
            vec![LibraryLocationStatusResponse {
                path: "/data/movies".to_string(),
                status: "unvalidated".to_string(),
                error: None,
            }]
        );
    }

    #[test]
    fn normalize_library_validation_paths_rejects_invalid_input_and_preserves_order() {
        assert_eq!(
            normalize_library_validation_paths(vec![
                " /data/movies ".to_string(),
                "/data/tv".to_string(),
            ])
            .unwrap(),
            vec!["/data/movies".to_string(), "/data/tv".to_string()]
        );
        assert!(normalize_library_validation_paths(Vec::new()).is_err());
        assert!(normalize_library_validation_paths(vec!["".to_string()]).is_err());
        assert!(normalize_library_validation_paths(vec!["bad\npath".to_string()]).is_err());
        assert!(normalize_library_validation_paths(vec!["a".repeat(4097)]).is_err());
        assert!(normalize_library_validation_paths(vec!["/path".to_string(); 65]).is_err());
    }

    #[test]
    fn library_location_validation_paths_skip_blanks_and_preserve_order() {
        let paths = library_location_validation_paths(&[
            "".to_string(),
            " /data/movies ".to_string(),
            "\t".to_string(),
            "/data/tv".to_string(),
        ]);
        assert_eq!(
            paths,
            vec![" /data/movies ".to_string(), "/data/tv".to_string()]
        );
    }

    #[test]
    fn unvalidated_location_status_has_no_error_summary() {
        assert_eq!(
            map_unvalidated_library_location_status("/data/movies".to_string()),
            LibraryLocationStatusResponse {
                path: "/data/movies".to_string(),
                status: "unvalidated".to_string(),
                error: None,
            }
        );
    }

    #[test]
    fn library_location_statuses_isolate_failures_and_omit_success_errors() {
        let statuses = vec![
            map_library_location_status("/first".to_string(), Ok(())),
            map_library_location_status(
                "/second".to_string(),
                Err(AppError::BadGateway("upstream rejected path".to_string())),
            ),
            map_library_location_status("/third".to_string(), Ok(())),
        ];
        assert_eq!(
            statuses,
            vec![
                LibraryLocationStatusResponse {
                    path: "/first".to_string(),
                    status: "valid".to_string(),
                    error: None,
                },
                LibraryLocationStatusResponse {
                    path: "/second".to_string(),
                    status: "validation_failed".to_string(),
                    error: Some("upstream rejected path".to_string()),
                },
                LibraryLocationStatusResponse {
                    path: "/third".to_string(),
                    status: "valid".to_string(),
                    error: None,
                },
            ]
        );
        assert_eq!(
            serde_json::to_value(&statuses[0]).unwrap(),
            serde_json::json!({"path": "/first", "status": "valid"})
        );
    }

    #[tokio::test]
    async fn collect_server_queries_preserves_input_order_for_path_validations() {
        let validations = [(30, "/first"), (0, "/second"), (10, "/third")]
            .into_iter()
            .map(|(delay_ms, path)| async move {
                sleep(Duration::from_millis(delay_ms)).await;
                path
            });
        assert_eq!(
            collect_server_queries(validations).await,
            vec!["/first", "/second", "/third"]
        );
    }

    #[test]
    fn map_library_task_preserves_real_zero_progress_and_last_run() {
        let response = map_library_task(
            "main",
            ScheduledTask {
                id: "task-1".to_string(),
                name: "Scan".to_string(),
                description: Some("LibraryScan".to_string()),
                category: None,
                key: None,
                state: "Idle".to_string(),
                is_hidden: None,
                is_enabled: None,
                triggers: Vec::new(),
                current_progress_percentage: Some(0.0),
                last_execution_result: Some(TaskExecutionResult {
                    status: Some("Failed".to_string()),
                    name: None,
                    key: None,
                    id: None,
                    error_message: Some(" disk full ".to_string()),
                    long_error_message: None,
                    start_time_utc: Some("2026-08-15T01:02:03Z".to_string()),
                    end_time_utc: Some("2026-08-15T01:03:03Z".to_string()),
                }),
            },
        );
        assert_eq!(response.progress, Some(0.0));
        assert_eq!(response.last_run.as_deref(), Some("2026-08-15T01:02:03Z"));
        assert_eq!(response.task_type.as_deref(), Some("LibraryScan"));
        assert_eq!(response.error.as_deref(), Some("disk full"));
        assert_eq!(
            response.capabilities,
            LibraryTaskCapabilities {
                path: FieldCapability::unsupported(),
                progress: FieldCapability::available(),
                last_run: FieldCapability::available(),
            }
        );
    }

    #[test]
    fn map_library_task_marks_missing_fields_unavailable_without_defaults() {
        let response = map_library_task(
            "main",
            ScheduledTask {
                id: "task-1".to_string(),
                name: "Scan".to_string(),
                description: None,
                category: None,
                key: None,
                state: "Idle".to_string(),
                is_hidden: None,
                is_enabled: None,
                triggers: Vec::new(),
                current_progress_percentage: None,
                last_execution_result: None,
            },
        );
        assert_eq!(response.path, None);
        assert_eq!(response.progress, None);
        assert_eq!(response.last_run, None);
        assert_eq!(response.task_type, None);
        assert_eq!(
            response.capabilities,
            LibraryTaskCapabilities {
                path: FieldCapability::unsupported(),
                progress: FieldCapability::emby_not_provided(),
                last_run: FieldCapability::emby_not_provided(),
            }
        );
    }

    #[test]
    fn flatten_server_results_keeps_successes_and_reports_partial_failures() {
        let results = flatten_server_results(vec![
            ("main".to_string(), Ok(vec!["task-1"])),
            (
                "backup".to_string(),
                Err(crate::error::AppError::BadGateway("offline".to_string())),
            ),
        ])
        .unwrap();
        assert_eq!(results.items, vec!["task-1"]);
        assert_eq!(
            results.failures,
            vec![ServerQueryFailureResponse {
                server_id: "backup".to_string(),
                error: "offline".to_string(),
            }]
        );
    }

    #[test]
    fn task_failure_response_matches_frontend_shape() {
        let body = TaskFailureResponse {
            summary: Some("boom".to_string()),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json, serde_json::json!({"summary":"boom"}));
        let empty = TaskFailureResponse { summary: None };
        assert_eq!(
            serde_json::to_value(&empty).unwrap(),
            serde_json::json!({"summary": null})
        );
    }

    #[test]
    fn task_failure_detail_preserves_summary_and_exposes_history() {
        let history = crate::db::ScheduledTaskFailureHistory {
            id: 7,
            server_id: "main".to_string(),
            task_id: "task-1".to_string(),
            run_identity: "sha256:run".to_string(),
            observed_at: 123,
            status: "failed".to_string(),
            safe_error_summary: Some("disk full".to_string()),
            started_at: Some(456),
            ended_at: Some(789),
        };
        let body = TaskFailureDetailResponse {
            compatibility: TaskFailureResponse {
                summary: Some("disk full".to_string()),
            },
            history: vec![history],
        };
        let json = serde_json::to_value(body).unwrap();
        assert_eq!(json["summary"], "disk full");
        assert_eq!(json["history"][0]["server_id"], "main");
        assert_eq!(json["history"][0]["task_id"], "task-1");
    }

    #[test]
    fn failure_observation_is_idempotent_and_scoped_by_server_and_task() {
        let (store, path) = test_settings_store("history");
        let history_store = store.task_failure_history_store();
        let task = failed_task("task-1", Some("execution-1"));
        let input = failure_observation_input("main", &task, 100).unwrap();
        history_store.record_failure(input.clone()).unwrap();

        let mut replay = input.clone();
        replay.observed_at = 200;
        history_store.record_failure(replay).unwrap();

        let mut other_server = input.clone();
        other_server.server_id = "backup".to_string();
        history_store.record_failure(other_server).unwrap();
        let mut other_task = input;
        other_task.task_id = "task-2".to_string();
        history_store.record_failure(other_task).unwrap();

        let main_task = history_store
            .list_failures(&TaskFailureListParams {
                server_id: Some("main".to_string()),
                task_id: Some("task-1".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(main_task.len(), 1);
        assert_eq!(main_task[0].observed_at, 200);

        let backup_task = history_store
            .list_failures(&TaskFailureListParams {
                server_id: Some("backup".to_string()),
                task_id: Some("task-1".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(backup_task.len(), 1);
        assert_eq!(backup_task[0].server_id, "backup");

        let other_task = history_store
            .list_failures(&TaskFailureListParams {
                server_id: Some("main".to_string()),
                task_id: Some("task-2".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(other_task.len(), 1);
        assert_eq!(other_task[0].task_id, "task-2");
        remove_test_database(&path);
    }

    #[test]
    fn failure_run_identity_prefers_execution_id_over_timestamps() {
        let mut task = failed_task("task-1", Some("execution-1"));
        let first = task_failure_run_identity(&task).unwrap();
        let result = task.last_execution_result.as_mut().unwrap();
        result.start_time_utc = Some("2026-08-16T01:02:03Z".to_string());
        result.end_time_utc = Some("2026-08-16T01:03:03Z".to_string());
        let second = task_failure_run_identity(&task).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn failure_run_identity_ignores_error_summary_without_execution_metadata() {
        let mut task = failed_task("task-1", None);
        {
            let result = task.last_execution_result.as_mut().unwrap();
            result.start_time_utc = None;
            result.end_time_utc = None;
            result.error_message = Some("token=do-not-store".to_string());
        }
        let first = task_failure_run_identity(&task).unwrap();

        task.last_execution_result.as_mut().unwrap().error_message =
            Some("different upstream error".to_string());
        let second = task_failure_run_identity(&task).unwrap();

        assert_eq!(first, second);
        assert!(first.starts_with("sha256:"));
        assert!(!first.contains("token=do-not-store"));
        assert!(!first.contains("different upstream error"));
    }

    #[test]
    fn library_path_validation_request_requires_server_and_paths() {
        let payload: LibraryPathValidationRequest =
            serde_json::from_str(r#"{"server_id":"main","paths":["/data/movies","/data/tv"]}"#)
                .unwrap();
        assert_eq!(payload.server_id, "main");
        assert_eq!(payload.paths, vec!["/data/movies", "/data/tv"]);
        assert!(
            serde_json::from_str::<LibraryPathValidationRequest>(r#"{"paths":["/data/movies"]}"#)
                .is_err()
        );
    }

    #[test]
    fn task_id_request_requires_server_id_field() {
        let payload: TaskIdRequest = serde_json::from_str(r#"{"server_id":"main"}"#).unwrap();
        assert_eq!(payload.server_id, "main");
        assert!(serde_json::from_str::<TaskIdRequest>(r#"{}"#).is_err());
    }
}
