use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
use thiserror::Error;

pub type AppResult<T> = Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("config error: {0}")]
    Config(String),
    #[error("http client error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("url error: {0}")]
    Url(#[from] url::ParseError),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    PayloadTooLarge(String),
    #[error("{0}")]
    RateLimited(String),
    #[error("{0}")]
    BadGateway(String),
    #[error("{0}")]
    Internal(String),
}

impl AppError {
    pub fn safe_log_message(&self) -> String {
        match self {
            Self::Http(err) => safe_reqwest_error_message(err),
            Self::Config(message)
            | Self::Unauthorized(message)
            | Self::Validation(message)
            | Self::PayloadTooLarge(message)
            | Self::RateLimited(message)
            | Self::BadGateway(message)
            | Self::Internal(message) => message.clone(),
            Self::Url(err) => format!("url error: {err}"),
            Self::Json(err) => format!("json error: {err}"),
            Self::Io(err) => format!("io error: {err}"),
            Self::Database(err) => format!("database error: {err}"),
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::Config(_)
            | Self::Internal(_)
            | Self::Io(_)
            | Self::Json(_)
            | Self::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Validation(_) => StatusCode::BAD_REQUEST,
            Self::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::RateLimited(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::Http(_) | Self::Url(_) | Self::BadGateway(_) => StatusCode::BAD_GATEWAY,
        }
    }

    fn client_message(&self) -> String {
        match self {
            Self::Unauthorized(_) => "unauthorized".to_string(),
            Self::Validation(message)
            | Self::PayloadTooLarge(message)
            | Self::RateLimited(message) => message.clone(),
            Self::Config(_)
            | Self::Internal(_)
            | Self::Io(_)
            | Self::Json(_)
            | Self::Database(_) => "internal server error".to_string(),
            Self::Http(_) | Self::Url(_) | Self::BadGateway(_) => "bad gateway".to_string(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        match &self {
            Self::Unauthorized(_) => {}
            Self::Validation(_) => {
                tracing::warn!(error = %self.safe_log_message(), "request validation failed")
            }
            _ => tracing::error!(error = %self.safe_log_message(), "request failed"),
        }
        (self.status(), self.client_message()).into_response()
    }
}

pub fn safe_reqwest_error_message(err: &reqwest::Error) -> String {
    if err.is_timeout() {
        return "http client timeout".to_string();
    }
    if err.is_connect() {
        return "http client connect error".to_string();
    }
    if let Some(status) = err.status() {
        return format!("http client status error: {status}");
    }
    if err.is_decode() {
        return "http client decode error".to_string();
    }
    if err.is_redirect() {
        return "http client redirect error".to_string();
    }
    if err.is_request() {
        return "http client request error".to_string();
    }
    "http client error".to_string()
}

pub fn safe_error_message(err: &(dyn std::error::Error + 'static)) -> String {
    if let Some(app_error) = err.downcast_ref::<AppError>() {
        return app_error.safe_log_message();
    }
    if let Some(reqwest_error) = err.downcast_ref::<reqwest::Error>() {
        return safe_reqwest_error_message(reqwest_error);
    }
    err.to_string()
}
