use axum::{Json, extract::State, http::HeaderMap};
use serde::Deserialize;

use crate::{
    AppState, DirectLinkCache, auth,
    config::Config,
    crypto_api::EncryptedRequest,
    error::{AppError, AppResult},
};

#[derive(Debug, Deserialize)]
pub struct RestartProxyRequest {
    server_id: String,
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

pub async fn update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<Config>> {
    auth::require_auth(&state, &headers).await?;
    let mut payload: Config = state.crypto_keys.decrypt_named(&request, "settings")?;
    let existing = state.config.read().await.clone();
    if payload.servers.is_empty() && !existing.servers.is_empty() {
        payload.servers = existing.servers.clone();
    }
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
    if payload.emby_api_key.trim().is_empty() {
        payload.emby_api_key = existing.emby_api_key;
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
        payload.openlist_token = existing.openlist_token;
    }
    payload
        .validate_for_storage()
        .map_err(|err| AppError::Config(err.to_string()))?;
    state.settings_store.save_config(&payload)?;
    *state.config.write().await = payload.clone();
    *state.cache.write().await =
        DirectLinkCache::new(payload.cache_ttl_seconds, payload.cache_max_capacity);
    if let Some(proxy_manager) = state.proxy_manager.as_ref() {
        proxy_manager.restart_all(state.clone()).await?;
    }
    Ok(Json(redact_config_secrets(payload)))
}

pub async fn restart_proxy_server(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<RestartProxyRequest>,
) -> AppResult<Json<Config>> {
    auth::require_auth(&state, &headers).await?;
    let server_id = payload.server_id.trim();
    if server_id.is_empty() {
        return Err(AppError::Validation("server_id is required".to_string()));
    }
    if let Some(proxy_manager) = state.proxy_manager.as_ref() {
        proxy_manager
            .restart_server(state.clone(), server_id)
            .await?;
    }
    Ok(Json(redact_config_secrets(
        state.config.read().await.clone(),
    )))
}

fn redact_config_secrets(mut config: Config) -> Config {
    config.openlist_token = None;
    config
}
