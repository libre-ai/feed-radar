//! Usage tracking endpoints

use axum::{
    extract::{Query, State},
    Json,
};
use serde::{Deserialize, Serialize};

use super::models::*;
use super::service::BillingService;
use crate::error::{ApiError, ApiResult};
use crate::extractors::auth::CurrentUser;
use crate::state::AppState;

/// Response wrapper
#[derive(Serialize)]
pub struct DataResponse<T> {
    data: T,
}

/// Query params for usage history
#[derive(Debug, Deserialize)]
pub struct UsageHistoryParams {
    /// Number of days to fetch (default: 30, max: 90)
    #[serde(default = "default_days")]
    pub days: u32,
}

fn default_days() -> u32 {
    30
}

/// Get current period usage
pub async fn get_current_usage(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<Json<DataResponse<CurrentUsageResponse>>> {
    let stripe = state
        .stripe()
        .ok_or_else(|| ApiError::BadRequest("Billing not enabled".to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;
    let mut service = BillingService::new(tx.connection(), stripe, state.stripe_config());

    let usage = service.get_current_usage(user.id).await?;
    service.release();
    tx.commit().await?;

    Ok(Json(DataResponse { data: usage }))
}

/// Get usage history
pub async fn get_usage_history(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(params): Query<UsageHistoryParams>,
) -> ApiResult<Json<DataResponse<Vec<UsageHistoryEntry>>>> {
    let stripe = state
        .stripe()
        .ok_or_else(|| ApiError::BadRequest("Billing not enabled".to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;
    let mut service = BillingService::new(tx.connection(), stripe, state.stripe_config());

    let days = params.days.min(90);

    let history = service.get_usage_history(user.id, days).await?;
    service.release();
    tx.commit().await?;

    Ok(Json(DataResponse { data: history }))
}
