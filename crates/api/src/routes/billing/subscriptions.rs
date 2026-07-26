//! Subscription management endpoints

use std::str::FromStr;

use axum::{extract::State, Json};
use serde::Serialize;
use stripe::{
    CheckoutSession, CheckoutSessionMode, CreateCheckoutSession, CreateCheckoutSessionLineItems,
};

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

/// Get current subscription
pub async fn get_subscription(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<Json<DataResponse<Option<SubscriptionResponse>>>> {
    let stripe = state
        .stripe()
        .ok_or_else(|| ApiError::BadRequest("Billing not enabled".to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;
    let mut service = BillingService::new(tx.connection(), stripe, state.stripe_config());

    let sub = service.get_subscription(user.id).await?;
    service.release();
    tx.commit().await?;

    Ok(Json(DataResponse {
        data: sub.map(|s| s.into()),
    }))
}

/// Create a new subscription
pub async fn create_subscription(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<CreateSubscriptionRequest>,
) -> ApiResult<Json<DataResponse<SubscriptionResponse>>> {
    let stripe = state
        .stripe()
        .ok_or_else(|| ApiError::BadRequest("Billing not enabled".to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;
    let mut service = BillingService::new(tx.connection(), stripe, state.stripe_config());

    // Check if user already has an active subscription
    if let Some(existing) = service.get_subscription(user.id).await? {
        if existing.status != SubscriptionStatus::Canceled {
            return Err(ApiError::Conflict(
                "User already has an active subscription".to_string(),
            ));
        }
    }

    let sub = service
        .create_subscription(
            user.id,
            &user.email,
            None, // Could get display name from user
            req.plan,
            req.interval,
            req.payment_method_id,
        )
        .await?;
    service.release();
    tx.commit().await?;

    Ok(Json(DataResponse { data: sub.into() }))
}

/// Change subscription plan
pub async fn change_plan(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<ChangePlanRequest>,
) -> ApiResult<Json<DataResponse<SubscriptionResponse>>> {
    let stripe = state
        .stripe()
        .ok_or_else(|| ApiError::BadRequest("Billing not enabled".to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;
    let mut service = BillingService::new(tx.connection(), stripe, state.stripe_config());

    let sub = service
        .change_plan(user.id, req.plan, req.interval, req.prorate)
        .await?;
    service.release();
    tx.commit().await?;

    Ok(Json(DataResponse { data: sub.into() }))
}

/// Cancel subscription
pub async fn cancel_subscription(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<CancelSubscriptionRequest>,
) -> ApiResult<Json<DataResponse<SubscriptionResponse>>> {
    let stripe = state
        .stripe()
        .ok_or_else(|| ApiError::BadRequest("Billing not enabled".to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;
    let mut service = BillingService::new(tx.connection(), stripe, state.stripe_config());

    let sub = service
        .cancel_subscription(user.id, req.reason, req.immediate)
        .await?;
    service.release();
    tx.commit().await?;

    Ok(Json(DataResponse { data: sub.into() }))
}

/// Reactivate canceled subscription
pub async fn reactivate_subscription(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<Json<DataResponse<SubscriptionResponse>>> {
    let stripe = state
        .stripe()
        .ok_or_else(|| ApiError::BadRequest("Billing not enabled".to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;
    let mut service = BillingService::new(tx.connection(), stripe, state.stripe_config());

    let sub = service.reactivate_subscription(user.id).await?;
    service.release();
    tx.commit().await?;

    Ok(Json(DataResponse { data: sub.into() }))
}

/// Create Stripe Customer Portal session
pub async fn create_portal_session(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<CreateSessionRequest>,
) -> ApiResult<Json<DataResponse<SessionResponse>>> {
    let stripe_client = state
        .stripe()
        .ok_or_else(|| ApiError::BadRequest("Billing not enabled".to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;
    let mut service = BillingService::new(tx.connection(), stripe_client, state.stripe_config());

    let customer = service
        .get_or_create_stripe_customer(user.id, &user.email, None)
        .await?;
    service.release();
    tx.commit().await?;

    let customer_id = stripe::CustomerId::from_str(&customer.stripe_customer_id)
        .map_err(|_| ApiError::Internal("Invalid customer ID".to_string()))?;
    let mut params = stripe::CreateBillingPortalSession::new(customer_id);
    params.return_url = Some(&req.return_url);

    let session = stripe::BillingPortalSession::create(stripe_client, params)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to create portal session: {}", e)))?;

    Ok(Json(DataResponse {
        data: SessionResponse { url: session.url },
    }))
}

/// Create Stripe Checkout session for new subscriptions
pub async fn create_checkout_session(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<CreateSessionRequest>,
) -> ApiResult<Json<DataResponse<SessionResponse>>> {
    let stripe_client = state
        .stripe()
        .ok_or_else(|| ApiError::BadRequest("Billing not enabled".to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;
    let mut service = BillingService::new(tx.connection(), stripe_client, state.stripe_config());
    let config = state.stripe_config();

    let plan = req
        .plan
        .ok_or_else(|| ApiError::BadRequest("Plan is required".to_string()))?;
    let interval = req
        .interval
        .ok_or_else(|| ApiError::BadRequest("Interval is required".to_string()))?;

    let price_id = match (plan, interval) {
        (PlanTier::Pro, BillingInterval::Month) => config.stripe_price_pro_monthly.clone(),
        (PlanTier::Pro, BillingInterval::Year) => config.stripe_price_pro_annual.clone(),
        (PlanTier::Team, BillingInterval::Month) => config.stripe_price_team_monthly.clone(),
        (PlanTier::Team, BillingInterval::Year) => config.stripe_price_team_annual.clone(),
        (PlanTier::Free, _) => {
            return Err(ApiError::BadRequest(
                "Cannot subscribe to free plan".to_string(),
            ))
        }
    }
    .ok_or_else(|| ApiError::Internal("Price not configured".to_string()))?;

    let customer = service
        .get_or_create_stripe_customer(user.id, &user.email, None)
        .await?;
    service.release();
    tx.commit().await?;

    let customer_id = stripe::CustomerId::from_str(&customer.stripe_customer_id)
        .map_err(|_| ApiError::Internal("Invalid customer ID".to_string()))?;
    let mut params = CreateCheckoutSession::new();
    params.customer = Some(customer_id);
    params.mode = Some(CheckoutSessionMode::Subscription);
    params.success_url = Some(&req.return_url);
    params.cancel_url = Some(&req.return_url);
    params.line_items = Some(vec![CreateCheckoutSessionLineItems {
        price: Some(price_id),
        quantity: Some(1),
        ..Default::default()
    }]);
    params.automatic_tax = Some(stripe::CreateCheckoutSessionAutomaticTax {
        enabled: true,
        ..Default::default()
    });
    params.customer_update = Some(stripe::CreateCheckoutSessionCustomerUpdate {
        address: Some(stripe::CreateCheckoutSessionCustomerUpdateAddress::Auto),
        ..Default::default()
    });
    params.metadata = Some(
        [
            ("user_id".to_string(), user.id.to_string()),
            ("plan".to_string(), plan.as_str().to_string()),
            ("interval".to_string(), format!("{:?}", interval)),
        ]
        .into_iter()
        .collect(),
    );

    let session = CheckoutSession::create(stripe_client, params)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to create checkout session: {}", e)))?;

    let url = session
        .url
        .ok_or_else(|| ApiError::Internal("No checkout URL returned".to_string()))?;

    Ok(Json(DataResponse {
        data: SessionResponse { url },
    }))
}
