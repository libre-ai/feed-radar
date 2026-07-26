//! Stripe webhook handler
//!
//! Handles events from Stripe with idempotency protection.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use chrono::DateTime;
use serde_json::json;
use sha2::{Digest, Sha256};
use stripe::{Event, EventObject, EventType, Webhook};
use uuid::Uuid;

use super::models::*;
use crate::error::ApiError;
use crate::state::AppState;

fn sha256_tag(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    format!(
        "sha256:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

/// Handle Stripe webhook
pub async fn handle_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    let _stripe_client = match state.stripe() {
        Some(c) => c,
        None => {
            tracing::warn!("Received Stripe webhook but billing is not enabled");
            return StatusCode::OK;
        }
    };

    let signature = match headers.get("Stripe-Signature") {
        Some(sig) => match sig.to_str() {
            Ok(s) => s,
            Err(_) => {
                tracing::warn!("Invalid Stripe-Signature header encoding");
                return StatusCode::BAD_REQUEST;
            }
        },
        None => {
            tracing::warn!("Missing Stripe-Signature header");
            return StatusCode::BAD_REQUEST;
        }
    };

    // Verify webhook signature
    let webhook_secret = state.stripe_config().webhook_secret();
    let event = match Webhook::construct_event(&body, signature, webhook_secret) {
        Ok(event) => event,
        Err(e) => {
            tracing::warn!("Failed to verify webhook signature: {}", e);
            return StatusCode::BAD_REQUEST;
        }
    };

    // Check idempotency - skip if already processed
    let already_processed = check_idempotency(state.worker_db(), event.id.as_ref()).await;
    if already_processed {
        tracing::debug!(event_hash = %safe_ref(event.id.as_ref()), "Webhook event already processed, skipping");
        return StatusCode::OK;
    }

    if let Err(e) = process_event(&state, &event).await {
        tracing::error!(event_hash = %safe_ref(event.id.as_ref()), error = %e, "Failed to process webhook event");
        // Don't return error to Stripe - we'll retry via our own logic if needed
    }

    if let Err(e) = mark_processed(
        state.worker_db(),
        event.id.as_ref(),
        &event.type_.to_string(),
    )
    .await
    {
        tracing::error!(event_hash = %safe_ref(event.id.as_ref()), error = %e, "Failed to mark webhook as processed");
    }

    StatusCode::OK
}

/// Check if event was already processed
async fn check_idempotency(db: &sqlx::PgPool, event_id: &str) -> bool {
    let result = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM webhook_events WHERE stripe_event_id = $1",
    )
    .bind(event_id)
    .fetch_one(db)
    .await;

    matches!(result, Ok(count) if count > 0)
}

/// Mark event as processed
async fn mark_processed(
    db: &sqlx::PgPool,
    event_id: &str,
    event_type: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO webhook_events (stripe_event_id, event_type) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(event_id)
    .bind(event_type)
    .execute(db)
    .await?;
    Ok(())
}

/// Process a Stripe event
async fn process_event(state: &AppState, event: &Event) -> Result<(), ApiError> {
    let db = state.worker_db();

    match event.type_ {
        // ====================================================================
        // SUBSCRIPTION EVENTS
        // ====================================================================
        EventType::CustomerSubscriptionCreated | EventType::CustomerSubscriptionUpdated => {
            if let EventObject::Subscription(sub) = &event.data.object {
                handle_subscription_update(db, sub).await?;
            }
        }

        EventType::CustomerSubscriptionDeleted => {
            if let EventObject::Subscription(sub) = &event.data.object {
                handle_subscription_deleted(db, sub).await?;
            }
        }

        // ====================================================================
        // INVOICE EVENTS
        // ====================================================================
        EventType::InvoiceCreated | EventType::InvoiceUpdated | EventType::InvoiceFinalized => {
            if let EventObject::Invoice(invoice) = &event.data.object {
                handle_invoice_update(db, invoice).await?;
            }
        }

        EventType::InvoicePaid => {
            if let EventObject::Invoice(invoice) = &event.data.object {
                handle_invoice_paid(db, invoice).await?;
            }
        }

        EventType::InvoicePaymentFailed => {
            if let EventObject::Invoice(invoice) = &event.data.object {
                handle_invoice_payment_failed(db, invoice).await?;
            }
        }

        // ====================================================================
        // PAYMENT METHOD EVENTS
        // ====================================================================
        EventType::PaymentMethodAttached => {
            // Handled by our own add_payment_method endpoint
            tracing::debug!("Payment method attached via webhook");
        }

        EventType::PaymentMethodDetached => {
            if let EventObject::PaymentMethod(pm) = &event.data.object {
                handle_payment_method_detached(db, pm.id.as_ref()).await?;
            }
        }

        // ====================================================================
        // CUSTOMER EVENTS
        // ====================================================================
        EventType::CustomerUpdated => {
            if let EventObject::Customer(customer) = &event.data.object {
                handle_customer_updated(db, customer).await?;
            }
        }

        _ => {
            tracing::debug!(event_type = %event.type_, "Unhandled webhook event type");
        }
    }

    Ok(())
}

// ============================================================================
// EVENT HANDLERS
// ============================================================================

async fn handle_subscription_update(
    db: &sqlx::PgPool,
    sub: &stripe::Subscription,
) -> Result<(), ApiError> {
    let status = match sub.status {
        stripe::SubscriptionStatus::Active => SubscriptionStatus::Active,
        stripe::SubscriptionStatus::Trialing => SubscriptionStatus::Trialing,
        stripe::SubscriptionStatus::PastDue => SubscriptionStatus::PastDue,
        stripe::SubscriptionStatus::Canceled => SubscriptionStatus::Canceled,
        stripe::SubscriptionStatus::Incomplete => SubscriptionStatus::Incomplete,
        stripe::SubscriptionStatus::IncompleteExpired => SubscriptionStatus::IncompleteExpired,
        stripe::SubscriptionStatus::Paused => SubscriptionStatus::Paused,
        stripe::SubscriptionStatus::Unpaid => SubscriptionStatus::Unpaid,
    };

    let stripe_sub_id = sub.id.to_string();

    sqlx::query(
        r#"
        UPDATE subscriptions SET
            status = $1,
            current_period_start = $2,
            current_period_end = $3,
            cancel_at_period_end = $4,
            canceled_at = $5,
            updated_at = NOW()
        WHERE stripe_subscription_id = $6
        "#,
    )
    .bind(status)
    .bind(DateTime::from_timestamp(sub.current_period_start, 0))
    .bind(DateTime::from_timestamp(sub.current_period_end, 0))
    .bind(sub.cancel_at_period_end)
    .bind(
        sub.canceled_at
            .and_then(|ts| DateTime::from_timestamp(ts, 0)),
    )
    .bind(&stripe_sub_id)
    .execute(db)
    .await?;

    // If past_due, start dunning
    if status == SubscriptionStatus::PastDue {
        sqlx::query(
            r#"
            UPDATE subscriptions SET
                dunning_started_at = COALESCE(dunning_started_at, NOW())
            WHERE stripe_subscription_id = $1 AND dunning_started_at IS NULL
            "#,
        )
        .bind(&stripe_sub_id)
        .execute(db)
        .await?;
    }

    // If active again, clear dunning
    if status == SubscriptionStatus::Active {
        sqlx::query(
            r#"
            UPDATE subscriptions SET
                dunning_started_at = NULL,
                last_payment_error = NULL
            WHERE stripe_subscription_id = $1
            "#,
        )
        .bind(&stripe_sub_id)
        .execute(db)
        .await?;

        // Also restore user account status if needed
        let user_id: Option<(Uuid,)> =
            sqlx::query_as("SELECT user_id FROM subscriptions WHERE stripe_subscription_id = $1")
                .bind(&stripe_sub_id)
                .fetch_optional(db)
                .await?;

        if let Some((user_id,)) = user_id {
            sqlx::query(
                "UPDATE users SET account_status = 'active', suspended_at = NULL, suspension_reason = NULL WHERE id = $1",
            )
            .bind(user_id)
            .execute(db)
            .await?;
        }
    }

    tracing::info!(subscription_hash = %safe_ref(&stripe_sub_id), status = ?status, "Updated subscription from webhook");
    Ok(())
}

async fn handle_subscription_deleted(
    db: &sqlx::PgPool,
    sub: &stripe::Subscription,
) -> Result<(), ApiError> {
    let stripe_sub_id = sub.id.to_string();

    // Get user_id before updating
    let user_result: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT user_id, plan_name FROM subscriptions WHERE stripe_subscription_id = $1",
    )
    .bind(&stripe_sub_id)
    .fetch_optional(db)
    .await?;

    sqlx::query(
        r#"
        UPDATE subscriptions SET
            status = 'canceled',
            canceled_at = NOW(),
            updated_at = NOW()
        WHERE stripe_subscription_id = $1
        "#,
    )
    .bind(&stripe_sub_id)
    .execute(db)
    .await?;

    // Downgrade user to free tier
    if let Some((user_id, _plan)) = user_result {
        sqlx::query("UPDATE users SET tier = 'free', updated_at = NOW() WHERE id = $1")
            .bind(user_id)
            .execute(db)
            .await?;

        tracing::info!(user_id_hash = %sha256_tag(user_id.to_string().as_bytes()), subscription_hash = %safe_ref(&stripe_sub_id), "Subscription deleted, user downgraded to free");
    }

    Ok(())
}

async fn handle_invoice_update(
    db: &sqlx::PgPool,
    invoice: &stripe::Invoice,
) -> Result<(), ApiError> {
    let stripe_invoice_id = invoice.id.to_string();

    // Get customer to find user
    let customer_id = match &invoice.customer {
        Some(stripe::Expandable::Id(id)) => id.to_string(),
        Some(stripe::Expandable::Object(c)) => c.id.to_string(),
        None => return Ok(()),
    };

    let user_result: Option<(Uuid,)> =
        sqlx::query_as("SELECT user_id FROM stripe_customers WHERE stripe_customer_id = $1")
            .bind(&customer_id)
            .fetch_optional(db)
            .await?;

    let user_id = match user_result {
        Some((id,)) => id,
        None => {
            tracing::warn!(customer_hash = %safe_ref(&customer_id), "Invoice webhook: customer not found");
            return Ok(());
        }
    };

    let subscription_id: Option<(Uuid,)> = if let Some(sub) = &invoice.subscription {
        let sub_id = match sub {
            stripe::Expandable::Id(id) => id.to_string(),
            stripe::Expandable::Object(s) => s.id.to_string(),
        };
        sqlx::query_as("SELECT id FROM subscriptions WHERE stripe_subscription_id = $1")
            .bind(&sub_id)
            .fetch_optional(db)
            .await?
    } else {
        None
    };

    sqlx::query(
        r#"
        INSERT INTO invoices (
            user_id, subscription_id, stripe_invoice_id, stripe_invoice_number,
            status, currency, amount_due, amount_paid, amount_remaining, subtotal, tax, total,
            hosted_invoice_url, invoice_pdf, period_start, period_end, due_date
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
        ON CONFLICT (stripe_invoice_id) DO UPDATE SET
            status = EXCLUDED.status,
            amount_due = EXCLUDED.amount_due,
            amount_paid = EXCLUDED.amount_paid,
            amount_remaining = EXCLUDED.amount_remaining,
            subtotal = EXCLUDED.subtotal,
            tax = EXCLUDED.tax,
            total = EXCLUDED.total,
            hosted_invoice_url = EXCLUDED.hosted_invoice_url,
            invoice_pdf = EXCLUDED.invoice_pdf,
            due_date = EXCLUDED.due_date,
            updated_at = NOW()
        "#,
    )
    .bind(user_id)
    .bind(subscription_id.map(|(id,)| id))
    .bind(&stripe_invoice_id)
    .bind(&invoice.number)
    .bind(
        invoice
            .status
            .as_ref()
            .map(|s| format!("{:?}", s).to_lowercase()),
    )
    .bind(
        invoice
            .currency
            .as_ref()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "eur".to_string()),
    )
    .bind(invoice.amount_due.unwrap_or(0))
    .bind(invoice.amount_paid.unwrap_or(0))
    .bind(invoice.amount_remaining.unwrap_or(0))
    .bind(invoice.subtotal.unwrap_or(0))
    .bind(invoice.tax)
    .bind(invoice.total.unwrap_or(0))
    .bind(&invoice.hosted_invoice_url)
    .bind(&invoice.invoice_pdf)
    .bind(
        invoice
            .period_start
            .and_then(|ts| DateTime::from_timestamp(ts, 0)),
    )
    .bind(
        invoice
            .period_end
            .and_then(|ts| DateTime::from_timestamp(ts, 0)),
    )
    .bind(
        invoice
            .due_date
            .and_then(|ts| DateTime::from_timestamp(ts, 0)),
    )
    .execute(db)
    .await?;

    tracing::debug!(invoice_hash = %safe_ref(&stripe_invoice_id), "Updated invoice from webhook");
    Ok(())
}

async fn handle_invoice_paid(db: &sqlx::PgPool, invoice: &stripe::Invoice) -> Result<(), ApiError> {
    let stripe_invoice_id = invoice.id.to_string();

    sqlx::query(
        r#"
        UPDATE invoices SET
            status = 'paid',
            paid_at = NOW(),
            amount_paid = $1,
            amount_remaining = 0,
            updated_at = NOW()
        WHERE stripe_invoice_id = $2
        "#,
    )
    .bind(invoice.amount_paid.unwrap_or(0))
    .bind(&stripe_invoice_id)
    .execute(db)
    .await?;

    tracing::info!(invoice_hash = %safe_ref(&stripe_invoice_id), "Invoice paid");
    Ok(())
}

async fn handle_invoice_payment_failed(
    db: &sqlx::PgPool,
    invoice: &stripe::Invoice,
) -> Result<(), ApiError> {
    let stripe_invoice_id = invoice.id.to_string();

    // Get subscription to update payment error
    if let Some(sub) = &invoice.subscription {
        let sub_id = match sub {
            stripe::Expandable::Id(id) => id.to_string(),
            stripe::Expandable::Object(s) => s.id.to_string(),
        };

        let error_msg = invoice
            .last_finalization_error
            .as_ref()
            .map(|e| {
                e.message
                    .clone()
                    .unwrap_or_else(|| "Payment failed".to_string())
            })
            .unwrap_or_else(|| "Payment failed".to_string());

        sqlx::query(
            r#"
            UPDATE subscriptions SET
                last_payment_error = $1,
                dunning_started_at = COALESCE(dunning_started_at, NOW()),
                updated_at = NOW()
            WHERE stripe_subscription_id = $2
            "#,
        )
        .bind(&error_msg)
        .bind(&sub_id)
        .execute(db)
        .await?;

        // Get user to update account status
        let user_result: Option<(Uuid,)> =
            sqlx::query_as("SELECT user_id FROM subscriptions WHERE stripe_subscription_id = $1")
                .bind(&sub_id)
                .fetch_optional(db)
                .await?;

        if let Some((user_id,)) = user_result {
            // Set to grace_period if not already
            sqlx::query(
                "UPDATE users SET account_status = 'grace_period' WHERE id = $1 AND account_status = 'active'",
            )
            .bind(user_id)
            .execute(db)
            .await?;

            sqlx::query(
                r#"
                INSERT INTO dunning_history (user_id, subscription_id, action, details)
                SELECT $1, id, 'email_sent', $2
                FROM subscriptions WHERE stripe_subscription_id = $3
                "#,
            )
            .bind(user_id)
            .bind(json!({ "reason": "payment_failed", "invoice_id": stripe_invoice_id }))
            .bind(&sub_id)
            .execute(db)
            .await?;
        }
    }

    tracing::warn!(invoice_hash = %safe_ref(&stripe_invoice_id), "Invoice payment failed");
    Ok(())
}

async fn handle_payment_method_detached(db: &sqlx::PgPool, pm_id: &str) -> Result<(), ApiError> {
    sqlx::query("DELETE FROM payment_methods WHERE stripe_payment_method_id = $1")
        .bind(pm_id)
        .execute(db)
        .await?;

    tracing::debug!(payment_method_hash = %safe_ref(pm_id), "Payment method detached");
    Ok(())
}

async fn handle_customer_updated(
    db: &sqlx::PgPool,
    customer: &stripe::Customer,
) -> Result<(), ApiError> {
    let customer_id = customer.id.to_string();

    sqlx::query(
        r#"
        UPDATE stripe_customers SET
            email = $1,
            name = $2,
            default_payment_method_id = $3,
            updated_at = NOW()
        WHERE stripe_customer_id = $4
        "#,
    )
    .bind(&customer.email)
    .bind(&customer.name)
    .bind(customer.invoice_settings.as_ref().and_then(|s| {
        s.default_payment_method.as_ref().map(|pm| match pm {
            stripe::Expandable::Id(id) => id.to_string(),
            stripe::Expandable::Object(obj) => obj.id.to_string(),
        })
    }))
    .bind(&customer_id)
    .execute(db)
    .await?;

    tracing::debug!(customer_hash = %safe_ref(&customer_id), "Customer updated");
    Ok(())
}

fn safe_ref(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..16]
        .to_string()
}
