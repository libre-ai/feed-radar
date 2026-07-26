//! Auth extractor for getting current user from JWT

use axum::{extract::FromRequestParts, http::request::Parts};
use jsonwebtoken::{decode, DecodingKey, Validation};
use uuid::Uuid;

use crate::error::ApiError;
use crate::routes::auth::Claims;
use crate::state::AppState;

/// Account status for request authorization checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "account_status", rename_all = "snake_case")]
pub enum AccountStatus {
    Active,
    GracePeriod,
    Suspended,
}

/// Current authenticated user extracted from JWT
#[derive(Debug, Clone)]
pub struct CurrentUser {
    pub id: Uuid,
    pub email: String,
    pub tier: UserTier,
    pub account_status: AccountStatus,
}

/// User subscription tier
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserTier {
    Free,
    ProTrial,
    Pro,
    Team,
}

impl FromRequestParts<AppState> for CurrentUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let auth_header = parts
            .headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::Unauthorized("Missing Authorization header".to_string()))?;

        let token = auth_header.strip_prefix("Bearer ").ok_or_else(|| {
            ApiError::Unauthorized("Invalid Authorization header format".to_string())
        })?;

        // Decode and validate JWT
        let token_data = decode::<Claims>(
            token,
            &DecodingKey::from_secret(state.jwt_secret().as_bytes()),
            &Validation::default(),
        )
        .map_err(|e| match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => {
                ApiError::Unauthorized("Token expired".to_string())
            }
            jsonwebtoken::errors::ErrorKind::InvalidToken => {
                ApiError::Unauthorized("Invalid token".to_string())
            }
            _ => ApiError::Unauthorized(format!("Token validation failed: {}", e)),
        })?;

        let claims = token_data.claims;

        let user_id = Uuid::parse_str(&claims.sub)
            .map_err(|_| ApiError::Unauthorized("Invalid user ID in token".to_string()))?;

        let tier = UserTier::from_str(&claims.tier);

        // Get account status inside a transaction-local tenant boundary.
        let mut tx = state
            .tenant_tx(user_id)
            .await
            .map_err(|_| ApiError::Internal("Failed to establish tenant context".to_string()))?;
        let account_status = sqlx::query_scalar::<_, AccountStatus>(
            "SELECT account_status FROM users WHERE id = $1",
        )
        .bind(user_id)
        .fetch_optional(tx.connection())
        .await
        .map_err(|_| ApiError::Internal("Failed to check account status".to_string()))?
        .ok_or_else(|| ApiError::Unauthorized("User no longer exists".to_string()))?;
        tx.commit()
            .await
            .map_err(|_| ApiError::Internal("Failed to close tenant context".to_string()))?;

        // A suspended account keeps only the paths that let it pay or leave:
        // billing, the Stripe webhook and the OPML export. The test is on the
        // path alone — the HTTP method is never consulted, so billing writes
        // stay reachable and a new read endpoint does NOT become reachable by
        // virtue of being a GET.
        if account_status == AccountStatus::Suspended {
            let path = parts.uri.path();
            let is_billing =
                path.starts_with("/api/v1/billing") || path.starts_with("/webhooks/stripe");
            let is_opml_export = path == "/api/v1/opml/export";

            if !is_billing && !is_opml_export {
                return Err(ApiError::Forbidden(
                    "Account suspended. Please update your payment method to continue.".to_string(),
                ));
            }
        }

        Ok(CurrentUser {
            id: user_id,
            email: claims.email,
            tier,
            account_status,
        })
    }
}

impl UserTier {
    /// Parse tier from string
    pub fn from_str(s: &str) -> Self {
        match s {
            "pro_trial" => UserTier::ProTrial,
            "pro" => UserTier::Pro,
            "team" => UserTier::Team,
            _ => UserTier::Free,
        }
    }

    /// Get rate limit for this tier (requests per minute)
    pub fn rate_limit(&self) -> u32 {
        match self {
            UserTier::Free => 60,
            UserTier::ProTrial | UserTier::Pro => 300,
            UserTier::Team => 500,
        }
    }

    /// Get max feeds for this tier (AMD-017: revised limits)
    pub fn max_feeds(&self) -> u32 {
        match self {
            UserTier::Free => 100,
            UserTier::ProTrial | UserTier::Pro | UserTier::Team => 10_000,
        }
    }

    /// Get max rules for this tier (AMD-017: revised limits)
    pub fn max_rules(&self) -> u32 {
        match self {
            UserTier::Free => 10,
            UserTier::ProTrial | UserTier::Pro | UserTier::Team => 500,
        }
    }

    /// Get max stored articles for this tier
    pub fn max_articles(&self) -> u32 {
        match self {
            UserTier::Free => 2_000,
            UserTier::ProTrial | UserTier::Pro | UserTier::Team => 100_000,
        }
    }
}

/// Optional auth - returns None if no valid token
pub struct OptionalUser(pub Option<CurrentUser>);

impl FromRequestParts<AppState> for OptionalUser {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        match CurrentUser::from_request_parts(parts, state).await {
            Ok(user) => Ok(OptionalUser(Some(user))),
            Err(_) => Ok(OptionalUser(None)),
        }
    }
}
