use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use openflow_protocol::ProtocolError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("configuration error: {0}")]
    Configuration(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("administrator authorization required")]
    AdminRequired,
    #[error("request forbidden: {0}")]
    Forbidden(String),
    #[error("service unavailable: {0}")]
    ServiceUnavailable(String),
    #[error("resource not found: {0}")]
    NotFound(String),
    #[error("request conflict: {0}")]
    Conflict(String),
    #[error("invalid request: {0}")]
    BadRequest(String),
    #[error("payload too large: {0}")]
    PayloadTooLarge(String),
    #[error("inference unavailable: {0}")]
    Inference(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("download error: {0}")]
    Download(String),
}

impl ServerError {
    pub fn protocol_error(&self) -> ProtocolError {
        let (code, retryable) = match self {
            Self::Unauthorized | Self::AdminRequired => ("unauthorized", false),
            Self::Forbidden(_) => ("forbidden", false),
            Self::ServiceUnavailable(_) => ("service_unavailable", true),
            Self::NotFound(_) => ("not_found", false),
            Self::Conflict(_) => ("conflict", true),
            Self::BadRequest(_) => ("bad_request", false),
            Self::PayloadTooLarge(_) => ("payload_too_large", false),
            Self::Inference(_) => ("inference_unavailable", true),
            Self::Download(_) => ("download_failed", true),
            Self::Configuration(_) | Self::Io(_) | Self::Serialization(_) => {
                ("internal_error", true)
            }
        };
        ProtocolError {
            code: code.into(),
            message: self.to_string(),
            retryable,
        }
    }
}

impl IntoResponse for ServerError {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Unauthorized | Self::AdminRequired => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::ServiceUnavailable(_) | Self::Inference(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::Configuration(_) | Self::Io(_) | Self::Serialization(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
            Self::Download(_) => StatusCode::BAD_GATEWAY,
        };
        (status, Json(self.protocol_error())).into_response()
    }
}
