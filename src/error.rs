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
    BadGateway(String),
    #[error("{0}")]
    Internal(String),
}

impl AppError {
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
            Self::Http(_) | Self::Url(_) | Self::BadGateway(_) => StatusCode::BAD_GATEWAY,
        }
    }

    fn client_message(&self) -> String {
        match self {
            Self::Unauthorized(_) => "unauthorized".to_string(),
            Self::Validation(message) | Self::PayloadTooLarge(message) => message.clone(),
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
            Self::Unauthorized(_) => tracing::info!(error = %self, "request unauthorized"),
            Self::Validation(_) => tracing::warn!(error = %self, "request validation failed"),
            _ => tracing::error!(error = %self, "request failed"),
        }
        (self.status(), self.client_message()).into_response()
    }
}
