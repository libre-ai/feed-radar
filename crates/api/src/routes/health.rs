//! Health check routes

use axum::{extract::State, http::StatusCode, routing::get, Json, Router};
use serde::Serialize;

use crate::state::AppState;

/// Health check response for basic liveness
#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

/// Readiness check response with dependency status
#[derive(Serialize)]
struct ReadinessResponse {
    status: &'static str,
    version: &'static str,
    checks: HealthChecks,
}

/// Individual health checks
#[derive(Serialize)]
struct HealthChecks {
    database: &'static str,
    redis: &'static str,
}

/// Basic health check handler (liveness probe)
/// Returns 200 OK if the service is running
async fn health_check() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Readiness check handler
/// Verifies database and Redis connections are healthy
async fn readiness_check(
    State(state): State<AppState>,
) -> Result<Json<ReadinessResponse>, (StatusCode, Json<ReadinessResponse>)> {
    let db_status = match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(state.db())
        .await
    {
        Ok(_) => "ok",
        Err(_) => "error",
    };

    let mut redis = state.redis();
    let redis_status = match redis::cmd("PING").query_async::<String>(&mut redis).await {
        Ok(_) => "ok",
        Err(_) => "error",
    };

    let response = ReadinessResponse {
        status: if db_status == "ok" && redis_status == "ok" {
            "ok"
        } else {
            "degraded"
        },
        version: env!("CARGO_PKG_VERSION"),
        checks: HealthChecks {
            database: db_status,
            redis: redis_status,
        },
    };

    if response.status == "ok" {
        Ok(Json(response))
    } else {
        Err((StatusCode::SERVICE_UNAVAILABLE, Json(response)))
    }
}

/// Build health routes (no authentication required)
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health_check))
        .route("/health/ready", get(readiness_check))
}
