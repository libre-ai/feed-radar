//! API error handling

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use thiserror::Error;

/// API error types
#[derive(Error, Debug)]
pub enum ApiError {
    /// Bad request (400)
    #[error("Bad request: {0}")]
    BadRequest(String),

    /// Unauthorized (401)
    #[error("Unauthorized: {0}")]
    Unauthorized(String),

    /// Forbidden (403)
    #[error("Forbidden: {0}")]
    Forbidden(String),

    /// Not found (404)
    #[error("{0} not found")]
    NotFound(String),

    /// Conflict (409)
    #[error("Conflict: {0}")]
    Conflict(String),

    /// Unprocessable entity (422)
    #[error("Validation error: {0}")]
    Validation(String),

    /// Rate limited (429)
    #[error("Rate limit exceeded")]
    RateLimited,

    /// Internal server error (500)
    #[error("Internal error: {0}")]
    Internal(String),

    /// Database error
    #[error("Database error")]
    Database(#[from] sqlx::Error),
}

/// Error response body
#[derive(Serialize)]
struct ErrorResponse {
    error: ErrorBody,
}

#[derive(Serialize)]
struct ErrorBody {
    code: String,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<serde_json::Value>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match &self {
            ApiError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg.clone()),
            ApiError::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg.clone()),
            ApiError::Forbidden(msg) => (StatusCode::FORBIDDEN, "FORBIDDEN", msg.clone()),
            ApiError::NotFound(msg) => (StatusCode::NOT_FOUND, "NOT_FOUND", msg.clone()),
            ApiError::Conflict(msg) => (StatusCode::CONFLICT, "CONFLICT", msg.clone()),
            ApiError::Validation(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "VALIDATION_ERROR",
                msg.clone(),
            ),
            ApiError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "Rate limit exceeded".to_string(),
            ),
            ApiError::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                msg.clone(),
            ),
            ApiError::Database(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "DATABASE_ERROR",
                "Database error".to_string(),
            ),
        };

        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(error = %self, "Internal server error");
        }

        error_envelope(status, code, message)
    }
}

/// Render one failure in the documented error envelope.
///
/// Handler errors are not the only way a request fails: an extractor can refuse
/// a request before any handler runs, and `axum` answers those rejections in
/// `text/plain`. A caller that parses `{"error":{"code",...}}` cannot read such
/// a body, so it learns *that* it failed without learning *why*. Rejections are
/// routed through this function so that every failure of this API has the same
/// machine-readable shape.
pub fn error_envelope(status: StatusCode, code: &str, message: String) -> Response {
    let body = ErrorResponse {
        error: ErrorBody {
            code: code.to_string(),
            message,
            details: None,
        },
    };

    (status, Json(body)).into_response()
}

/// Result type for API handlers
pub type ApiResult<T> = Result<T, ApiError>;
