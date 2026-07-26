//! Billing service - core business logic

use std::str::FromStr;

use chrono::{DateTime, Datelike, Utc};
use sqlx::PgConnection;
use stripe::Client as StripeClient;
use uuid::Uuid;

use super::models::*;
use crate::config::StripeConfig;
use crate::error::{ApiError, ApiResult};

/// Billing service handles all billing operations
pub struct BillingService<'a> {
    db: &'a mut PgConnection,
    stripe: &'a StripeClient,
    config: &'a StripeConfig,
}

impl<'a> BillingService<'a> {
    pub fn new(
        db: &'a mut PgConnection,
        stripe: &'a StripeClient,
        config: &'a StripeConfig,
    ) -> Self {
        Self { db, stripe, config }
    }

    /// Consume the service and release its transaction connection borrow.
    pub fn release(self) {}

    // ========================================================================
    // STRIPE CUSTOMER MANAGEMENT
    // ========================================================================

    /// Get or create a Stripe customer for a user
    pub async fn get_or_create_stripe_customer(
        &mut self,
        user_id: Uuid,
        email: &str,
        name: Option<&str>,
    ) -> ApiResult<StripeCustomer> {
        if let Some(customer) = self.get_stripe_customer_by_user(user_id).await? {
            return Ok(customer);
        }

        // Create customer in Stripe
        let mut create_customer = stripe::CreateCustomer::new();
        create_customer.email = Some(email);
        if let Some(n) = name {
            create_customer.name = Some(n);
        }
        create_customer.metadata = Some(
            [("user_id".to_string(), user_id.to_string())]
                .into_iter()
                .collect(),
        );

        let stripe_customer = stripe::Customer::create(self.stripe, create_customer)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to create Stripe customer: {}", e)))?;

        let customer = sqlx::query_as::<_, StripeCustomer>(
            r#"
            INSERT INTO stripe_customers (user_id, stripe_customer_id, email, name)
            VALUES ($1, $2, $3, $4)
            RETURNING *
            "#,
        )
        .bind(user_id)
        .bind(stripe_customer.id.to_string())
        .bind(email)
        .bind(name)
        .fetch_one(&mut *self.db)
        .await?;

        self.log_billing_event(
            user_id,
            "customer.created",
            None,
            serde_json::json!({
                "stripe_customer_id": stripe_customer.id.to_string()
            }),
        )
        .await?;

        Ok(customer)
    }

    /// Get Stripe customer by user ID
    pub async fn get_stripe_customer_by_user(
        &mut self,
        user_id: Uuid,
    ) -> ApiResult<Option<StripeCustomer>> {
        let customer = sqlx::query_as::<_, StripeCustomer>(
            "SELECT * FROM stripe_customers WHERE user_id = $1",
        )
        .bind(user_id)
        .fetch_optional(&mut *self.db)
        .await?;

        Ok(customer)
    }

    // ========================================================================
    // SUBSCRIPTION MANAGEMENT
    // ========================================================================

    /// Get active subscription for user
    pub async fn get_subscription(&mut self, user_id: Uuid) -> ApiResult<Option<Subscription>> {
        let sub = sqlx::query_as::<_, Subscription>(
            r#"
            SELECT * FROM subscriptions
            WHERE user_id = $1 AND status NOT IN ('canceled', 'incomplete_expired')
            ORDER BY created_at DESC
            LIMIT 1
            "#,
        )
        .bind(user_id)
        .fetch_optional(&mut *self.db)
        .await?;

        Ok(sub)
    }

    /// Create a new subscription
    pub async fn create_subscription(
        &mut self,
        user_id: Uuid,
        email: &str,
        name: Option<&str>,
        plan: PlanTier,
        interval: BillingInterval,
        _payment_method_id: Option<String>,
    ) -> ApiResult<Subscription> {
        let customer = self
            .get_or_create_stripe_customer(user_id, email, name)
            .await?;

        let price_id = self.get_price_id(plan, interval)?;

        // Create subscription in Stripe
        let customer_id = stripe::CustomerId::from_str(&customer.stripe_customer_id)
            .map_err(|_| ApiError::Internal("Invalid customer ID".to_string()))?;

        let mut create_sub = stripe::CreateSubscription::new(customer_id);
        create_sub.items = Some(vec![stripe::CreateSubscriptionItems {
            price: Some(price_id.clone()),
            ..Default::default()
        }]);

        // Normalize billing anchor to handle end-of-month dates
        let anchor = self.calculate_billing_anchor();
        create_sub.billing_cycle_anchor = Some(anchor.timestamp());

        let stripe_sub = stripe::Subscription::create(self.stripe, create_sub)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to create subscription: {}", e)))?;

        let sub = self
            .store_subscription(&customer, &stripe_sub, plan, interval)
            .await?;

        self.update_user_tier(user_id, plan).await?;

        self.log_billing_event(
            user_id,
            "subscription.created",
            Some(stripe_sub.id.as_ref()),
            serde_json::json!({
                "plan": plan.as_str(),
                "interval": format!("{:?}", interval),
                "stripe_subscription_id": stripe_sub.id.to_string()
            }),
        )
        .await?;

        Ok(sub)
    }

    /// Change subscription plan
    pub async fn change_plan(
        &mut self,
        user_id: Uuid,
        new_plan: PlanTier,
        new_interval: BillingInterval,
        _prorate: bool,
    ) -> ApiResult<Subscription> {
        let sub = self
            .get_subscription(user_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Subscription".to_string()))?;

        let price_id = self.get_price_id(new_plan, new_interval)?;

        // Update in Stripe
        let sub_id = stripe::SubscriptionId::from_str(&sub.stripe_subscription_id)
            .map_err(|_| ApiError::Internal("Invalid subscription ID".to_string()))?;

        let mut update_sub = stripe::UpdateSubscription::new();
        update_sub.items = Some(vec![stripe::UpdateSubscriptionItems {
            price: Some(price_id.clone()),
            ..Default::default()
        }]);

        let stripe_sub = stripe::Subscription::update(self.stripe, &sub_id, update_sub)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to update subscription: {}", e)))?;

        // Update local record
        let updated = sqlx::query_as::<_, Subscription>(
            r#"
            UPDATE subscriptions
            SET plan_name = $1, billing_interval = $2, stripe_price_id = $3, updated_at = NOW()
            WHERE id = $4
            RETURNING *
            "#,
        )
        .bind(new_plan.as_str())
        .bind(new_interval)
        .bind(&price_id)
        .bind(sub.id)
        .fetch_one(&mut *self.db)
        .await?;

        self.update_user_tier(user_id, new_plan).await?;

        self.log_billing_event(
            user_id,
            "subscription.updated",
            Some(stripe_sub.id.as_ref()),
            serde_json::json!({
                "old_plan": sub.plan_name,
                "new_plan": new_plan.as_str()
            }),
        )
        .await?;

        Ok(updated)
    }

    /// Cancel subscription
    pub async fn cancel_subscription(
        &mut self,
        user_id: Uuid,
        reason: Option<String>,
        immediate: bool,
    ) -> ApiResult<Subscription> {
        let sub = self
            .get_subscription(user_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Subscription".to_string()))?;

        let sub_id = stripe::SubscriptionId::from_str(&sub.stripe_subscription_id)
            .map_err(|_| ApiError::Internal("Invalid subscription ID".to_string()))?;

        if immediate {
            // Cancel immediately in Stripe
            stripe::Subscription::cancel(
                self.stripe,
                &sub_id,
                stripe::CancelSubscription::default(),
            )
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to cancel subscription: {}", e)))?;

            // Update local record
            let updated = sqlx::query_as::<_, Subscription>(
                r#"
                UPDATE subscriptions
                SET status = 'canceled', canceled_at = NOW(), cancellation_reason = $1, updated_at = NOW()
                WHERE id = $2
                RETURNING *
                "#,
            )
            .bind(&reason)
            .bind(sub.id)
            .fetch_one(&mut *self.db)
            .await?;

            self.update_user_tier(user_id, PlanTier::Free).await?;

            Ok(updated)
        } else {
            let mut update_sub = stripe::UpdateSubscription::new();
            update_sub.cancel_at_period_end = Some(true);

            stripe::Subscription::update(self.stripe, &sub_id, update_sub)
                .await
                .map_err(|e| ApiError::Internal(format!("Failed to cancel subscription: {}", e)))?;

            // Update local record
            let updated = sqlx::query_as::<_, Subscription>(
                r#"
                UPDATE subscriptions
                SET cancel_at_period_end = TRUE, cancellation_reason = $1, updated_at = NOW()
                WHERE id = $2
                RETURNING *
                "#,
            )
            .bind(&reason)
            .bind(sub.id)
            .fetch_one(&mut *self.db)
            .await?;

            Ok(updated)
        }
    }

    /// Reactivate a canceled subscription
    pub async fn reactivate_subscription(&mut self, user_id: Uuid) -> ApiResult<Subscription> {
        let sub = self
            .get_subscription(user_id)
            .await?
            .ok_or_else(|| ApiError::NotFound("Subscription".to_string()))?;

        if !sub.cancel_at_period_end {
            return Err(ApiError::BadRequest(
                "Subscription is not scheduled for cancellation".to_string(),
            ));
        }

        let sub_id = stripe::SubscriptionId::from_str(&sub.stripe_subscription_id)
            .map_err(|_| ApiError::Internal("Invalid subscription ID".to_string()))?;

        // Reactivate in Stripe
        let mut update_sub = stripe::UpdateSubscription::new();
        update_sub.cancel_at_period_end = Some(false);

        stripe::Subscription::update(self.stripe, &sub_id, update_sub)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to reactivate subscription: {}", e)))?;

        // Update local record
        let updated = sqlx::query_as::<_, Subscription>(
            r#"
            UPDATE subscriptions
            SET cancel_at_period_end = FALSE, cancellation_reason = NULL, updated_at = NOW()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(sub.id)
        .fetch_one(&mut *self.db)
        .await?;

        self.log_billing_event(
            user_id,
            "subscription.reactivated",
            Some(&sub.stripe_subscription_id),
            serde_json::json!({}),
        )
        .await?;

        Ok(updated)
    }

    // ========================================================================
    // USAGE TRACKING
    // ========================================================================

    /// Record usage
    pub async fn record_usage(
        &mut self,
        user_id: Uuid,
        usage_type: UsageType,
        quantity: i64,
        metadata: serde_json::Value,
    ) -> ApiResult<UsageRecord> {
        let sub = self.get_subscription(user_id).await?;

        let record = sqlx::query_as::<_, UsageRecord>(
            r#"
            INSERT INTO usage_records (user_id, subscription_id, usage_type, quantity, metadata)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING *
            "#,
        )
        .bind(user_id)
        .bind(sub.map(|s| s.id))
        .bind(usage_type)
        .bind(quantity)
        .bind(metadata)
        .fetch_one(&mut *self.db)
        .await?;

        // Update daily aggregate
        let today = Utc::now().date_naive();
        let (tokens_delta, calls_delta) = match usage_type {
            UsageType::AiTokens => (quantity, 0i64),
            UsageType::ApiCalls => (0i64, quantity),
        };

        sqlx::query(
            r#"
            INSERT INTO usage_daily (user_id, date, ai_tokens, api_calls)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (user_id, date) DO UPDATE
            SET ai_tokens = usage_daily.ai_tokens + $3,
                api_calls = usage_daily.api_calls + $4,
                updated_at = NOW()
            "#,
        )
        .bind(user_id)
        .bind(today)
        .bind(tokens_delta)
        .bind(calls_delta)
        .execute(&mut *self.db)
        .await?;

        Ok(record)
    }

    /// Get current period usage
    pub async fn get_current_usage(&mut self, user_id: Uuid) -> ApiResult<CurrentUsageResponse> {
        let sub = self.get_subscription(user_id).await?;
        let limits = self.get_user_limits(user_id).await?;

        let (period_start, period_end) = if let Some(ref s) = sub {
            (s.current_period_start, s.current_period_end)
        } else {
            // For free users, use calendar month
            let now = Utc::now();
            let start = now
                .with_day(1)
                .unwrap()
                .date_naive()
                .and_hms_opt(0, 0, 0)
                .unwrap();
            let end = (start + chrono::Duration::days(32)).with_day(1).unwrap();
            (
                DateTime::from_naive_utc_and_offset(start, Utc),
                DateTime::from_naive_utc_and_offset(end, Utc),
            )
        };

        let usage: (Option<i64>, Option<i64>) = sqlx::query_as(
            r#"
            SELECT COALESCE(SUM(ai_tokens), 0), COALESCE(SUM(api_calls), 0)
            FROM usage_daily
            WHERE user_id = $1 AND date >= $2 AND date < $3
            "#,
        )
        .bind(user_id)
        .bind(period_start.date_naive())
        .bind(period_end.date_naive())
        .fetch_one(&mut *self.db)
        .await?;

        let ai_used = usage.0.unwrap_or(0);
        let api_used = usage.1.unwrap_or(0);

        Ok(CurrentUsageResponse {
            period_start,
            period_end,
            ai_tokens: UsageMetric {
                used: ai_used,
                limit: limits.ai_tokens_per_month as i64,
                percentage: if limits.ai_tokens_per_month > 0 {
                    (ai_used as f64 / limits.ai_tokens_per_month as f64) * 100.0
                } else {
                    0.0
                },
            },
            api_calls: UsageMetric {
                used: api_used,
                limit: limits.api_calls_per_month as i64,
                percentage: if limits.api_calls_per_month > 0 {
                    (api_used as f64 / limits.api_calls_per_month as f64) * 100.0
                } else {
                    0.0
                },
            },
        })
    }

    /// Get usage history
    pub async fn get_usage_history(
        &mut self,
        user_id: Uuid,
        days: u32,
    ) -> ApiResult<Vec<UsageHistoryEntry>> {
        let start_date = Utc::now().date_naive() - chrono::Duration::days(days as i64);

        let history = sqlx::query_as::<_, UsageDaily>(
            r#"
            SELECT * FROM usage_daily
            WHERE user_id = $1 AND date >= $2
            ORDER BY date ASC
            "#,
        )
        .bind(user_id)
        .bind(start_date)
        .fetch_all(&mut *self.db)
        .await?;

        Ok(history
            .into_iter()
            .map(|u| UsageHistoryEntry {
                date: u.date,
                ai_tokens: u.ai_tokens,
                api_calls: u.api_calls,
            })
            .collect())
    }

    // ========================================================================
    // INVOICES
    // ========================================================================

    /// List invoices for user
    pub async fn list_invoices(&mut self, user_id: Uuid, limit: u32) -> ApiResult<Vec<Invoice>> {
        let invoices = sqlx::query_as::<_, Invoice>(
            r#"
            SELECT * FROM invoices
            WHERE user_id = $1
            ORDER BY created_at DESC
            LIMIT $2
            "#,
        )
        .bind(user_id)
        .bind(limit as i64)
        .fetch_all(&mut *self.db)
        .await?;

        Ok(invoices)
    }

    /// Get single invoice
    pub async fn get_invoice(&mut self, user_id: Uuid, invoice_id: Uuid) -> ApiResult<Invoice> {
        let invoice =
            sqlx::query_as::<_, Invoice>("SELECT * FROM invoices WHERE id = $1 AND user_id = $2")
                .bind(invoice_id)
                .bind(user_id)
                .fetch_optional(&mut *self.db)
                .await?
                .ok_or_else(|| ApiError::NotFound("Invoice".to_string()))?;

        Ok(invoice)
    }

    // ========================================================================
    // PAYMENT METHODS
    // ========================================================================

    /// List payment methods for user
    pub async fn list_payment_methods(&mut self, user_id: Uuid) -> ApiResult<Vec<PaymentMethod>> {
        let methods = sqlx::query_as::<_, PaymentMethod>(
            "SELECT * FROM payment_methods WHERE user_id = $1 ORDER BY is_default DESC, created_at DESC",
        )
        .bind(user_id)
        .fetch_all(&mut *self.db)
        .await?;

        Ok(methods)
    }

    /// Add payment method
    pub async fn add_payment_method(
        &mut self,
        user_id: Uuid,
        stripe_pm_id: &str,
        set_default: bool,
    ) -> ApiResult<PaymentMethod> {
        let customer = self
            .get_stripe_customer_by_user(user_id)
            .await?
            .ok_or_else(|| ApiError::BadRequest("No Stripe customer found".to_string()))?;

        let pm_id = stripe::PaymentMethodId::from_str(stripe_pm_id)
            .map_err(|_| ApiError::BadRequest("Invalid payment method ID".to_string()))?;

        let customer_id = stripe::CustomerId::from_str(&customer.stripe_customer_id)
            .map_err(|_| ApiError::Internal("Invalid customer ID".to_string()))?;

        // Attach to customer in Stripe
        stripe::PaymentMethod::attach(
            self.stripe,
            &pm_id,
            stripe::AttachPaymentMethod {
                customer: customer_id.clone(),
            },
        )
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to attach payment method: {}", e)))?;

        let pm = stripe::PaymentMethod::retrieve(self.stripe, &pm_id, &[])
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to retrieve payment method: {}", e)))?;

        // Extract card details
        let (brand, last4, exp_month, exp_year) = if let Some(card) = pm.card {
            (
                Some(format!("{:?}", card.brand).to_lowercase()),
                Some(card.last4),
                Some(card.exp_month as i32),
                Some(card.exp_year as i32),
            )
        } else {
            (None, None, None, None)
        };

        // If setting as default, clear other defaults first
        if set_default {
            sqlx::query("UPDATE payment_methods SET is_default = FALSE WHERE user_id = $1")
                .bind(user_id)
                .execute(&mut *self.db)
                .await?;

            // Update default in Stripe
            stripe::Customer::update(
                self.stripe,
                &customer_id,
                stripe::UpdateCustomer {
                    invoice_settings: Some(stripe::CustomerInvoiceSettings {
                        default_payment_method: Some(stripe_pm_id.to_string()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| {
                ApiError::Internal(format!("Failed to set default payment method: {}", e))
            })?;
        }

        let method = sqlx::query_as::<_, PaymentMethod>(
            r#"
            INSERT INTO payment_methods (user_id, stripe_customer_id, stripe_payment_method_id, card_brand, card_last4, card_exp_month, card_exp_year, is_default)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING *
            "#,
        )
        .bind(user_id)
        .bind(customer.id)
        .bind(stripe_pm_id)
        .bind(&brand)
        .bind(&last4)
        .bind(exp_month)
        .bind(exp_year)
        .bind(set_default)
        .fetch_one(&mut *self.db)
        .await?;

        Ok(method)
    }

    /// Delete payment method
    pub async fn delete_payment_method(&mut self, user_id: Uuid, method_id: Uuid) -> ApiResult<()> {
        let method = sqlx::query_as::<_, PaymentMethod>(
            "SELECT * FROM payment_methods WHERE id = $1 AND user_id = $2",
        )
        .bind(method_id)
        .bind(user_id)
        .fetch_optional(&mut *self.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Payment method".to_string()))?;

        let pm_id = stripe::PaymentMethodId::from_str(&method.stripe_payment_method_id)
            .map_err(|_| ApiError::Internal("Invalid payment method ID".to_string()))?;

        stripe::PaymentMethod::detach(self.stripe, &pm_id)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to detach payment method: {}", e)))?;

        sqlx::query("DELETE FROM payment_methods WHERE id = $1")
            .bind(method_id)
            .execute(&mut *self.db)
            .await?;

        Ok(())
    }

    /// Set payment method as default
    pub async fn set_default_payment_method(
        &mut self,
        user_id: Uuid,
        method_id: Uuid,
    ) -> ApiResult<PaymentMethod> {
        let method = sqlx::query_as::<_, PaymentMethod>(
            "SELECT * FROM payment_methods WHERE id = $1 AND user_id = $2",
        )
        .bind(method_id)
        .bind(user_id)
        .fetch_optional(&mut *self.db)
        .await?
        .ok_or_else(|| ApiError::NotFound("Payment method".to_string()))?;

        let customer = self
            .get_stripe_customer_by_user(user_id)
            .await?
            .ok_or_else(|| ApiError::BadRequest("No Stripe customer found".to_string()))?;

        let customer_id = stripe::CustomerId::from_str(&customer.stripe_customer_id)
            .map_err(|_| ApiError::Internal("Invalid customer ID".to_string()))?;

        // Update default in Stripe
        stripe::Customer::update(
            self.stripe,
            &customer_id,
            stripe::UpdateCustomer {
                invoice_settings: Some(stripe::CustomerInvoiceSettings {
                    default_payment_method: Some(method.stripe_payment_method_id.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to set default payment method: {}", e)))?;

        sqlx::query("UPDATE payment_methods SET is_default = FALSE WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *self.db)
            .await?;

        let updated = sqlx::query_as::<_, PaymentMethod>(
            r#"
            UPDATE payment_methods SET is_default = TRUE, updated_at = NOW()
            WHERE id = $1
            RETURNING *
            "#,
        )
        .bind(method_id)
        .fetch_one(&mut *self.db)
        .await?;

        Ok(updated)
    }

    // ========================================================================
    // HELPER METHODS
    // ========================================================================

    /// Get price ID for plan and interval
    fn get_price_id(&mut self, plan: PlanTier, interval: BillingInterval) -> ApiResult<String> {
        let price_id = match (plan, interval) {
            (PlanTier::Pro, BillingInterval::Month) => self.config.stripe_price_pro_monthly.clone(),
            (PlanTier::Pro, BillingInterval::Year) => self.config.stripe_price_pro_annual.clone(),
            (PlanTier::Team, BillingInterval::Month) => {
                self.config.stripe_price_team_monthly.clone()
            }
            (PlanTier::Team, BillingInterval::Year) => self.config.stripe_price_team_annual.clone(),
            (PlanTier::Free, _) => {
                return Err(ApiError::BadRequest(
                    "Free plan has no subscription".to_string(),
                ))
            }
        };

        price_id.ok_or_else(|| ApiError::Internal("Price ID not configured".to_string()))
    }

    /// Calculate normalized billing anchor
    fn calculate_billing_anchor(&self) -> DateTime<Utc> {
        let now = Utc::now();
        let day = now.day();

        if day > 28 {
            let next_month = if now.month() == 12 {
                now.with_month(1)
                    .unwrap()
                    .with_year(now.year() + 1)
                    .unwrap()
            } else {
                now.with_month(now.month() + 1).unwrap()
            };
            next_month.with_day(1).unwrap()
        } else {
            now
        }
    }

    /// Store subscription in database
    async fn store_subscription(
        &mut self,
        customer: &StripeCustomer,
        stripe_sub: &stripe::Subscription,
        plan: PlanTier,
        interval: BillingInterval,
    ) -> ApiResult<Subscription> {
        let status = match stripe_sub.status {
            stripe::SubscriptionStatus::Active => SubscriptionStatus::Active,
            stripe::SubscriptionStatus::Trialing => SubscriptionStatus::Trialing,
            stripe::SubscriptionStatus::PastDue => SubscriptionStatus::PastDue,
            stripe::SubscriptionStatus::Canceled => SubscriptionStatus::Canceled,
            stripe::SubscriptionStatus::Incomplete => SubscriptionStatus::Incomplete,
            stripe::SubscriptionStatus::IncompleteExpired => SubscriptionStatus::IncompleteExpired,
            stripe::SubscriptionStatus::Paused => SubscriptionStatus::Paused,
            stripe::SubscriptionStatus::Unpaid => SubscriptionStatus::Unpaid,
        };

        let price_id = stripe_sub
            .items
            .data
            .first()
            .and_then(|i| i.price.as_ref())
            .map(|p| p.id.to_string())
            .unwrap_or_default();

        let product_id = stripe_sub
            .items
            .data
            .first()
            .and_then(|i| i.price.as_ref())
            .and_then(|p| p.product.as_ref())
            .map(|p| match p {
                stripe::Expandable::Id(id) => id.to_string(),
                stripe::Expandable::Object(obj) => obj.id.to_string(),
            })
            .unwrap_or_default();

        let sub = sqlx::query_as::<_, Subscription>(
            r#"
            INSERT INTO subscriptions (
                user_id, stripe_customer_id, stripe_subscription_id, stripe_price_id, stripe_product_id,
                plan_name, billing_interval, status,
                current_period_start, current_period_end, billing_cycle_anchor,
                trial_start, trial_end, cancel_at_period_end
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
            RETURNING *
            "#,
        )
        .bind(customer.user_id)
        .bind(customer.id)
        .bind(stripe_sub.id.to_string())
        .bind(&price_id)
        .bind(&product_id)
        .bind(plan.as_str())
        .bind(interval)
        .bind(status)
        .bind(DateTime::from_timestamp(stripe_sub.current_period_start, 0))
        .bind(DateTime::from_timestamp(stripe_sub.current_period_end, 0))
        .bind(DateTime::from_timestamp(stripe_sub.billing_cycle_anchor, 0))
        .bind(stripe_sub.trial_start.and_then(|ts| DateTime::from_timestamp(ts, 0)))
        .bind(stripe_sub.trial_end.and_then(|ts| DateTime::from_timestamp(ts, 0)))
        .bind(stripe_sub.cancel_at_period_end)
        .fetch_one(&mut *self.db)
        .await?;

        Ok(sub)
    }

    /// Update user tier
    async fn update_user_tier(&mut self, user_id: Uuid, tier: PlanTier) -> ApiResult<()> {
        sqlx::query("UPDATE users SET tier = $1, updated_at = NOW() WHERE id = $2")
            .bind(tier.as_str())
            .bind(user_id)
            .execute(&mut *self.db)
            .await?;
        Ok(())
    }

    /// Get user limits based on tier
    async fn get_user_limits(&mut self, user_id: Uuid) -> ApiResult<PlanLimits> {
        let tier: (String,) = sqlx::query_as("SELECT tier FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(&mut *self.db)
            .await?;

        Ok(get_plan_limits(PlanTier::from_str(&tier.0)))
    }

    /// Log billing event
    async fn log_billing_event(
        &mut self,
        user_id: Uuid,
        event_type: &str,
        stripe_event_id: Option<&str>,
        payload: serde_json::Value,
    ) -> ApiResult<()> {
        sqlx::query(
            r#"
            INSERT INTO billing_events (user_id, event_type, stripe_event_id, payload)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(user_id)
        .bind(event_type)
        .bind(stripe_event_id)
        .bind(payload)
        .execute(&mut *self.db)
        .await?;
        Ok(())
    }
}

/// Get plan limits for a tier
pub fn get_plan_limits(tier: PlanTier) -> PlanLimits {
    match tier {
        PlanTier::Free => PlanLimits {
            feeds: 100,
            rules: 10,
            articles: 2_000,
            api_calls_per_month: 1_000,
            ai_tokens_per_month: 10_000,
        },
        PlanTier::Pro => PlanLimits {
            feeds: 10_000,
            rules: 500,
            articles: 100_000,
            api_calls_per_month: 50_000,
            ai_tokens_per_month: 500_000,
        },
        PlanTier::Team => PlanLimits {
            feeds: 10_000,
            rules: 500,
            articles: 100_000,
            api_calls_per_month: 200_000,
            ai_tokens_per_month: 2_000_000,
        },
    }
}
