//! Usage tracking middleware
//!
//! Tracks API calls and AI token usage for billing purposes.
//! Usage is recorded in the database and synced to Stripe periodically.

use chrono::Timelike;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tracing::{error, warn};
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

/// Record an API call for usage tracking
pub async fn record_api_call(db: &PgPool, user_id: Uuid, endpoint: &str) {
    let result = sqlx::query(
        r#"
        INSERT INTO usage_records (user_id, usage_type, quantity, metadata)
        VALUES ($1, 'api_call', 1, $2)
        "#,
    )
    .bind(user_id)
    .bind(serde_json::json!({ "endpoint": endpoint }))
    .execute(db)
    .await;

    if let Err(e) = result {
        warn!(user_id_hash = %sha256_tag(user_id.to_string().as_bytes()), endpoint, error = %e, "Failed to record API call usage");
    }
}

/// Record AI token usage
pub async fn record_ai_tokens(db: &PgPool, user_id: Uuid, tokens: i64, model: &str) {
    let result = sqlx::query(
        r#"
        INSERT INTO usage_records (user_id, usage_type, quantity, metadata)
        VALUES ($1, 'ai_tokens', $2, $3)
        "#,
    )
    .bind(user_id)
    .bind(tokens)
    .bind(serde_json::json!({ "model": model }))
    .execute(db)
    .await;

    if let Err(e) = result {
        error!(user_id_hash = %sha256_tag(user_id.to_string().as_bytes()), tokens, model, error = %e, "Failed to record AI token usage");
    }
}

/// Update daily usage aggregates
/// Called periodically by a scheduled job
pub async fn aggregate_daily_usage(db: &PgPool) -> Result<u64, sqlx::Error> {
    let result = sqlx::query(
        r#"
        INSERT INTO usage_daily (user_id, date, ai_tokens, api_calls)
        SELECT
            user_id,
            DATE(recorded_at) as date,
            SUM(CASE WHEN usage_type = 'ai_tokens' THEN quantity ELSE 0 END) as ai_tokens,
            SUM(CASE WHEN usage_type = 'api_call' THEN quantity ELSE 0 END) as api_calls
        FROM usage_records
        WHERE recorded_at >= CURRENT_DATE - INTERVAL '1 day'
          AND recorded_at < CURRENT_DATE
        GROUP BY user_id, DATE(recorded_at)
        ON CONFLICT (user_id, date)
        DO UPDATE SET
            ai_tokens = EXCLUDED.ai_tokens,
            api_calls = EXCLUDED.api_calls,
            updated_at = NOW()
        "#,
    )
    .execute(db)
    .await?;

    Ok(result.rows_affected())
}

/// Get current period usage for a user
pub async fn get_period_usage(
    db: &PgPool,
    user_id: Uuid,
    period_start: chrono::DateTime<chrono::Utc>,
) -> Result<(i64, i64), sqlx::Error> {
    let result: (i64, i64) = sqlx::query_as(
        r#"
        SELECT
            COALESCE(SUM(CASE WHEN usage_type = 'ai_tokens' THEN quantity ELSE 0 END), 0) as ai_tokens,
            COALESCE(SUM(CASE WHEN usage_type = 'api_call' THEN quantity ELSE 0 END), 0) as api_calls
        FROM usage_records
        WHERE user_id = $1 AND recorded_at >= $2
        "#,
    )
    .bind(user_id)
    .bind(period_start)
    .fetch_one(db)
    .await?;

    Ok(result)
}

/// Check if user is within their usage limits
pub async fn check_usage_limits(
    db: &PgPool,
    user_id: Uuid,
    tier: &str,
) -> Result<UsageLimitStatus, sqlx::Error> {
    use chrono::Datelike;

    // Get current billing period start (first of current month)
    let now = chrono::Utc::now();
    let period_start = now
        .with_day(1)
        .unwrap()
        .with_hour(0)
        .unwrap()
        .with_minute(0)
        .unwrap()
        .with_second(0)
        .unwrap()
        .with_nanosecond(0)
        .unwrap();

    let (ai_tokens, api_calls) = get_period_usage(db, user_id, period_start).await?;

    let (ai_limit, api_limit) = match tier {
        "free" => (10_000i64, 1_000i64),      // 10k tokens, 1k API calls
        "pro" => (500_000i64, 50_000i64),     // 500k tokens, 50k API calls
        "team" => (2_000_000i64, 200_000i64), // 2M tokens, 200k API calls
        _ => (10_000i64, 1_000i64),           // Default to free tier
    };

    Ok(UsageLimitStatus {
        ai_tokens_used: ai_tokens,
        ai_tokens_limit: ai_limit,
        ai_tokens_remaining: (ai_limit - ai_tokens).max(0),
        api_calls_used: api_calls,
        api_calls_limit: api_limit,
        api_calls_remaining: (api_limit - api_calls).max(0),
        is_over_limit: ai_tokens >= ai_limit || api_calls >= api_limit,
    })
}

/// Usage limit status
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageLimitStatus {
    pub ai_tokens_used: i64,
    pub ai_tokens_limit: i64,
    pub ai_tokens_remaining: i64,
    pub api_calls_used: i64,
    pub api_calls_limit: i64,
    pub api_calls_remaining: i64,
    pub is_over_limit: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_usage_limit_status() {
        let status = UsageLimitStatus {
            ai_tokens_used: 5000,
            ai_tokens_limit: 10000,
            ai_tokens_remaining: 5000,
            api_calls_used: 500,
            api_calls_limit: 1000,
            api_calls_remaining: 500,
            is_over_limit: false,
        };

        assert!(!status.is_over_limit);
        assert_eq!(status.ai_tokens_remaining, 5000);
    }
}
