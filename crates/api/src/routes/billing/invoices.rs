//! Invoice endpoints

use axum::{
    extract::{Path, Query, State},
    response::{IntoResponse, Redirect},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

/// List response with metadata
#[derive(Serialize)]
pub struct ListResponse<T> {
    data: Vec<T>,
    meta: ListMeta,
}

#[derive(Serialize)]
pub struct ListMeta {
    total: usize,
}

/// Query params for invoice list
#[derive(Debug, Deserialize)]
pub struct InvoiceListParams {
    /// Maximum number of invoices to return (default: 20, max: 100)
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_limit() -> u32 {
    20
}

/// List invoices
pub async fn list_invoices(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(params): Query<InvoiceListParams>,
) -> ApiResult<Json<ListResponse<InvoiceResponse>>> {
    let stripe = state
        .stripe()
        .ok_or_else(|| ApiError::BadRequest("Billing not enabled".to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;
    let mut service = BillingService::new(tx.connection(), stripe, state.stripe_config());

    let limit = params.limit.min(100);

    let invoices = service.list_invoices(user.id, limit).await?;
    let total = invoices.len();
    service.release();
    tx.commit().await?;

    Ok(Json(ListResponse {
        data: invoices.into_iter().map(|i| i.into()).collect(),
        meta: ListMeta { total },
    }))
}

/// Get single invoice
pub async fn get_invoice(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<DataResponse<InvoiceResponse>>> {
    let stripe = state
        .stripe()
        .ok_or_else(|| ApiError::BadRequest("Billing not enabled".to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;
    let mut service = BillingService::new(tx.connection(), stripe, state.stripe_config());

    let invoice = service.get_invoice(user.id, id).await?;
    service.release();
    tx.commit().await?;

    Ok(Json(DataResponse {
        data: invoice.into(),
    }))
}

/// Get invoice PDF (redirect to Stripe)
pub async fn get_invoice_pdf(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<Uuid>,
) -> ApiResult<impl IntoResponse> {
    let stripe = state
        .stripe()
        .ok_or_else(|| ApiError::BadRequest("Billing not enabled".to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;
    let mut service = BillingService::new(tx.connection(), stripe, state.stripe_config());

    let invoice = service.get_invoice(user.id, id).await?;

    let pdf_url = invoice
        .invoice_pdf
        .ok_or_else(|| ApiError::NotFound("Invoice PDF not available".to_string()))?;
    service.release();
    tx.commit().await?;

    Ok(Redirect::temporary(&pdf_url))
}
