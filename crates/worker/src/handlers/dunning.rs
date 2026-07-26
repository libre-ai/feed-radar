//! Dunning job handler
//!
//! Handles the dunning process for failed payments:
//! - Day 1: First notification email
//! - Day 3: Reminder email
//! - Day 7: Final warning, downgrade to Free tier
//! - Day 30: Account suspension

use chrono::Utc;
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

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

/// Dunning configuration
pub struct DunningConfig {
    /// Days before downgrading to free (grace period)
    pub grace_period_days: i64,
    /// Days after grace period before suspension
    pub suspension_days: i64,
}

impl Default for DunningConfig {
    fn default() -> Self {
        Self {
            grace_period_days: 7,
            suspension_days: 30,
        }
    }
}

/// Result of dunning check
#[derive(Debug)]
pub struct DunningResult {
    pub users_checked: u64,
    pub emails_sent: u64,
    pub users_downgraded: u64,
    pub users_suspended: u64,
    pub errors: Vec<String>,
}

/// Check dunning status for all users
pub async fn check_dunning_status(
    db: &PgPool,
    config: &DunningConfig,
) -> Result<DunningResult, sqlx::Error> {
    let mut result = DunningResult {
        users_checked: 0,
        emails_sent: 0,
        users_downgraded: 0,
        users_suspended: 0,
        errors: Vec::new(),
    };

    let now = Utc::now();

    // Find all subscriptions in dunning
    let subscriptions: Vec<DunningSubscription> = sqlx::query_as(
        r#"
        SELECT s.id, s.user_id, s.plan_name, s.dunning_started_at,
               u.email, u.account_status
        FROM subscriptions s
        JOIN users u ON s.user_id = u.id
        WHERE s.dunning_started_at IS NOT NULL
          AND s.status = 'past_due'
          AND u.deleted_at IS NULL
        ORDER BY s.dunning_started_at ASC
        "#,
    )
    .fetch_all(db)
    .await?;

    result.users_checked = subscriptions.len() as u64;

    for sub in subscriptions {
        let dunning_start = sub.dunning_started_at;
        let days_in_dunning = (now - dunning_start).num_days();

        let action = if days_in_dunning >= config.suspension_days {
            DunningAction::Suspend
        } else if days_in_dunning >= config.grace_period_days {
            DunningAction::Downgrade
        } else if days_in_dunning >= 7 {
            DunningAction::SendDay7Warning
        } else if days_in_dunning >= 3 {
            DunningAction::SendDay3Reminder
        } else if days_in_dunning >= 1 {
            DunningAction::SendDay1Notice
        } else {
            DunningAction::None
        };

        // Skip if already processed (check dunning_history)
        if should_skip_action(db, sub.user_id, &action).await? {
            continue;
        }

        match action {
            DunningAction::SendDay1Notice
            | DunningAction::SendDay3Reminder
            | DunningAction::SendDay7Warning => {
                if let Err(e) = queue_dunning_email(db, &sub, &action).await {
                    result.errors.push(format!(
                        "Failed to queue email for user {}: {}",
                        sub.user_id, e
                    ));
                } else {
                    result.emails_sent += 1;
                    log_dunning_action(db, &sub, "email_sent", &action).await?;
                }
            }
            DunningAction::Downgrade => {
                if sub.account_status != "grace_period" {
                    // Already downgraded or suspended
                    continue;
                }
                if let Err(e) = downgrade_to_free(db, sub.user_id, &sub.plan_name).await {
                    result
                        .errors
                        .push(format!("Failed to downgrade user {}: {}", sub.user_id, e));
                } else {
                    result.users_downgraded += 1;
                    log_dunning_action(db, &sub, "downgrade", &action).await?;
                }
            }
            DunningAction::Suspend => {
                if sub.account_status == "suspended" {
                    continue;
                }
                if let Err(e) = suspend_account(db, sub.user_id).await {
                    result
                        .errors
                        .push(format!("Failed to suspend user {}: {}", sub.user_id, e));
                } else {
                    result.users_suspended += 1;
                    log_dunning_action(db, &sub, "suspend", &action).await?;
                }
            }
            DunningAction::None => {}
        }
    }

    Ok(result)
}

/// Restore an account after payment is recovered
pub async fn restore_account(db: &PgPool, user_id: Uuid, plan: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE users SET
            tier = $1,
            account_status = 'active',
            suspended_at = NULL,
            suspension_reason = NULL,
            updated_at = NOW()
        WHERE id = $2
        "#,
    )
    .bind(plan)
    .bind(user_id)
    .execute(db)
    .await?;

    // Clear dunning state
    sqlx::query(
        r#"
        UPDATE subscriptions SET
            dunning_started_at = NULL,
            last_payment_error = NULL,
            updated_at = NOW()
        WHERE user_id = $1
        "#,
    )
    .bind(user_id)
    .execute(db)
    .await?;

    // Log the restoration
    sqlx::query(
        r#"
        INSERT INTO dunning_history (user_id, subscription_id, action, details)
        SELECT $1, id, 'restore', $2
        FROM subscriptions WHERE user_id = $1
        "#,
    )
    .bind(user_id)
    .bind(json!({ "restored_to_plan": plan }))
    .execute(db)
    .await?;

    Ok(())
}

// ============================================================================
// INTERNAL TYPES AND HELPERS
// ============================================================================

#[derive(Debug, sqlx::FromRow)]
struct DunningSubscription {
    id: Uuid,
    user_id: Uuid,
    plan_name: String,
    dunning_started_at: chrono::DateTime<Utc>,
    email: String,
    account_status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DunningAction {
    None,
    SendDay1Notice,
    SendDay3Reminder,
    SendDay7Warning,
    Downgrade,
    Suspend,
}

impl DunningAction {
    fn as_str(&self) -> &'static str {
        match self {
            DunningAction::None => "none",
            DunningAction::SendDay1Notice => "day1_notice",
            DunningAction::SendDay3Reminder => "day3_reminder",
            DunningAction::SendDay7Warning => "day7_warning",
            DunningAction::Downgrade => "downgrade",
            DunningAction::Suspend => "suspend",
        }
    }
}

async fn should_skip_action(
    db: &PgPool,
    user_id: Uuid,
    action: &DunningAction,
) -> Result<bool, sqlx::Error> {
    if *action == DunningAction::None {
        return Ok(true);
    }

    // Check if this action was already taken
    let action_str = action.as_str();
    let exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM dunning_history
            WHERE user_id = $1 AND details->>'action_type' = $2
            AND created_at > NOW() - INTERVAL '24 hours'
        )
        "#,
    )
    .bind(user_id)
    .bind(action_str)
    .fetch_one(db)
    .await?;

    Ok(exists)
}

async fn queue_dunning_email(
    _db: &PgPool,
    sub: &DunningSubscription,
    action: &DunningAction,
) -> Result<(), sqlx::Error> {
    // In a real implementation, this would queue an email job
    // For now, we just log it
    tracing::info!(
        user_id_hash = %sha256_tag(sub.user_id.to_string().as_bytes()),
        email_hash = %safe_hash(&sub.email),
        action = ?action,
        "Would queue dunning email"
    );
    Ok(())
}

async fn log_dunning_action(
    db: &PgPool,
    sub: &DunningSubscription,
    action_type: &str,
    action: &DunningAction,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO dunning_history (user_id, subscription_id, action, details)
        VALUES ($1, $2, $3, $4)
        "#,
    )
    .bind(sub.user_id)
    .bind(sub.id)
    .bind(action_type)
    .bind(json!({
        "action_type": action.as_str(),
        "plan_name": sub.plan_name,
        "email": sub.email
    }))
    .execute(db)
    .await?;

    Ok(())
}

async fn downgrade_to_free(
    db: &PgPool,
    user_id: Uuid,
    previous_plan: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE users SET
            tier = 'free',
            account_status = 'active',
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(user_id)
    .execute(db)
    .await?;

    tracing::info!(user_id_hash = %sha256_tag(user_id.to_string().as_bytes()), previous_plan = %previous_plan, "User downgraded to free tier");
    Ok(())
}

async fn suspend_account(db: &PgPool, user_id: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE users SET
            account_status = 'suspended',
            suspended_at = NOW(),
            suspension_reason = 'Non-payment after 30 days',
            updated_at = NOW()
        WHERE id = $1
        "#,
    )
    .bind(user_id)
    .execute(db)
    .await?;

    tracing::warn!(user_id_hash = %sha256_tag(user_id.to_string().as_bytes()), "User account suspended due to non-payment");
    Ok(())
}

fn safe_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..16]
        .to_string()
}

/// Clean up webhook events older than 30 days
pub async fn cleanup_webhook_events(db: &PgPool) -> Result<u64, sqlx::Error> {
    let result =
        sqlx::query("DELETE FROM webhook_events WHERE created_at < NOW() - INTERVAL '30 days'")
            .execute(db)
            .await?;

    let deleted = result.rows_affected();
    if deleted > 0 {
        tracing::info!(deleted = deleted, "Cleaned up old webhook events");
    }

    Ok(deleted)
}
