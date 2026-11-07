use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
};
use serde::Deserialize;

const SESSIONS_PERMISSION: &str = "sessions.control";

use crate::{
    AppState, auth,
    crypto_api::EncryptedRequest,
    emby,
    error::{AppError, AppResult, safe_error_message},
};

#[derive(Debug, Deserialize)]
struct StopSessionRequest {
    server_id: String,
}

#[derive(Debug, Deserialize)]
struct SendMessageRequest {
    server_id: String,
    message: String,
}

pub async fn stop_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<()>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    let admin_user_id = auth_context.user_id;
    if let Err(error) = auth::require_permission(&auth_context, SESSIONS_PERMISSION) {
        state.settings_store.record_audit_best_effort(
            Some(admin_user_id),
            "sessions.stop",
            &format!("停止 Emby 会话失败：{}", safe_error_message(&error)),
            "error",
        );
        return Err(error);
    }
    let payload: StopSessionRequest =
        match state.crypto_keys.decrypt_named(&request, "stop_session") {
            Ok(payload) => payload,
            Err(error) => {
                state.settings_store.record_audit_best_effort(
                    Some(admin_user_id),
                    "sessions.stop",
                    &format!("停止 Emby 会话失败：{}", safe_error_message(&error)),
                    "error",
                );
                return Err(error);
            }
        };
    let result = async {
        let server_id = normalize_required_server_id(&payload.server_id)?;
        auth::require_server_access(&auth_context, server_id)?;
        let session_id = normalize_required_session_id(&session_id)?;
        let config = load_server_config(&state, server_id).await?;
        require_session_target(&state.client, &config, server_id, session_id).await?;
        emby::stop_session(&state.client, &config, session_id).await
    }
    .await;
    match result {
        Ok(()) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "sessions.stop",
                &format!("停止 Emby 会话 {session_id}"),
                "success",
            );
            Ok(Json(()))
        }
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "sessions.stop",
                &format!("停止 Emby 会话失败：{}", safe_error_message(&error)),
                "error",
            );
            Err(error)
        }
    }
}

pub async fn send_session_message(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(session_id): Path<String>,
    Json(request): Json<EncryptedRequest>,
) -> AppResult<Json<()>> {
    let auth_context = auth::require_auth_context(&state, &headers).await?;
    let admin_user_id = auth_context.user_id;
    if let Err(error) = auth::require_permission(&auth_context, SESSIONS_PERMISSION) {
        state.settings_store.record_audit_best_effort(
            Some(admin_user_id),
            "sessions.message",
            &format!("发送消息到会话失败：{}", safe_error_message(&error)),
            "error",
        );
        return Err(error);
    }
    let payload: SendMessageRequest = match state
        .crypto_keys
        .decrypt_named(&request, "send_session_message")
    {
        Ok(payload) => payload,
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "sessions.message",
                &format!("发送消息到会话失败：{}", safe_error_message(&error)),
                "error",
            );
            return Err(error);
        }
    };
    let result = async {
        let server_id = normalize_required_server_id(&payload.server_id)?;
        auth::require_server_access(&auth_context, server_id)?;
        let session_id = normalize_required_session_id(&session_id)?;
        let config = load_server_config(&state, server_id).await?;
        require_session_target(&state.client, &config, server_id, session_id).await?;
        emby::send_session_message(&state.client, &config, session_id, &payload.message).await
    }
    .await;
    match result {
        Ok(()) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "sessions.message",
                &format!("发送消息到会话 {session_id}"),
                "success",
            );
            Ok(Json(()))
        }
        Err(error) => {
            state.settings_store.record_audit_best_effort(
                Some(admin_user_id),
                "sessions.message",
                &format!("发送消息到会话失败：{}", safe_error_message(&error)),
                "error",
            );
            Err(error)
        }
    }
}

fn normalize_required_session_id(session_id: &str) -> AppResult<&str> {
    let session_id = session_id.trim();
    if session_id.is_empty()
        || session_id.len() > 100
        || !session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(AppError::Validation("invalid session id".to_string()));
    }
    Ok(session_id)
}

async fn load_server_config(state: &AppState, server_id: &str) -> AppResult<crate::config::Config> {
    match emby::get_config_for_server(state, server_id).await? {
        Some(config) => Ok(config),
        None => Err(AppError::Validation("unknown server".to_string())),
    }
}

async fn require_session_target(
    client: &reqwest::Client,
    config: &crate::config::Config,
    server_id: &str,
    session_id: &str,
) -> AppResult<()> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Err(AppError::Validation("session_id is required".to_string()));
    }
    let sessions = emby::get_active_playback_sessions(client, config).await?;
    if sessions
        .iter()
        .any(|session| session.server_id == server_id && session.id == session_id)
    {
        Ok(())
    } else {
        Err(AppError::Validation(
            "session not found on server".to_string(),
        ))
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

#[cfg(test)]
mod tests {
    use super::{
        SendMessageRequest, StopSessionRequest, normalize_required_server_id,
        normalize_required_session_id, require_session_target,
    };
    use crate::config::Config;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn serve_sessions(body: &str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let addr = listener.local_addr().expect("local addr");
        let body = body.to_string();
        let handle = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buf = vec![0_u8; 8192];
            let n = stream.read(&mut buf).await.expect("read request");
            let request = String::from_utf8_lossy(&buf[..n]).into_owned();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
            request
        });
        (format!("http://{addr}"), handle)
    }

    #[test]
    fn normalize_server_id_rejects_blank_control_and_oversized() {
        assert!(normalize_required_server_id("").is_err());
        assert!(normalize_required_server_id("\t").is_err());
        assert!(normalize_required_server_id("a\0b").is_err());
        assert!(normalize_required_server_id(&"x".repeat(129)).is_err());
        assert_eq!(normalize_required_server_id(" abc ").unwrap(), "abc");
    }

    #[test]
    fn normalize_session_id_rejects_path_injection_and_oversized_values() {
        assert!(normalize_required_session_id("").is_err());
        assert!(normalize_required_session_id("../other").is_err());
        assert!(normalize_required_session_id("id with spaces").is_err());
        assert!(normalize_required_session_id(&"x".repeat(101)).is_err());
        assert_eq!(
            normalize_required_session_id(" session_01-").unwrap(),
            "session_01-"
        );
    }

    #[tokio::test]
    async fn session_target_check_is_read_only_and_requires_the_server_session_pair() {
        let sessions = r#"[{"Id":"session-a","Client":"Emby Web","DeviceName":"Browser","NowPlayingItem":{"Id":"item-a","Name":"Movie"},"PlayState":{"PositionTicks":0,"PlayMethod":"DirectPlay"}}]"#;
        let (base, handle) = serve_sessions(sessions).await;
        let mut config = Config::default_runtime();
        config.emby_host = base;
        config.emby_api_key = "test-key".to_string();
        let client = reqwest::Client::new();

        require_session_target(&client, &config, "default", "session-a")
            .await
            .expect("matching target");
        let request = handle.await.expect("server task");
        assert!(request.starts_with("GET /Sessions?"));
        assert!(!request.contains("/Terminate"));
        assert!(!request.contains("/Message"));

        let (base, handle) = serve_sessions(sessions).await;
        config.emby_host = base;
        assert!(
            require_session_target(&client, &config, "other", "session-a")
                .await
                .is_err()
        );
        handle.await.expect("server task");
    }

    #[test]
    fn stop_session_request_shape() {
        let payload: StopSessionRequest = serde_json::from_str(r#"{"server_id":"main"}"#).unwrap();
        assert_eq!(payload.server_id, "main");
        assert!(serde_json::from_str::<StopSessionRequest>(r#"{}"#).is_err());
    }

    #[test]
    fn send_message_request_shape() {
        let payload: SendMessageRequest =
            serde_json::from_str(r#"{"server_id":"main","message":"hello"}"#).unwrap();
        assert_eq!(payload.server_id, "main");
        assert_eq!(payload.message, "hello");
        assert!(serde_json::from_str::<SendMessageRequest>(r#"{"server_id":"main"}"#).is_err());
    }
}
