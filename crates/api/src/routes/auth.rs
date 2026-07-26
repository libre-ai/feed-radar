//! Authentication routes

use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{extract::State, routing::post, Json, Router};
use chrono::{Duration, Utc};
use jsonwebtoken::{encode, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;
use validator::Validate;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

/// JWT Claims
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String, // User ID
    pub email: String,
    pub tier: String,
    pub exp: i64, // Expiration timestamp
    pub iat: i64, // Issued at
}

/// Register request
#[derive(Debug, Deserialize, Validate)]
pub struct RegisterRequest {
    #[validate(email(message = "Invalid email address"))]
    pub email: String,
    #[validate(length(min = 8, message = "Password must be at least 8 characters"))]
    pub password: String,
    pub display_name: Option<String>,
}

/// Login request
#[derive(Debug, Deserialize, Validate)]
pub struct LoginRequest {
    #[validate(email)]
    pub email: String,
    pub password: String,
}

/// Auth response
#[derive(Serialize)]
pub struct AuthResponse {
    pub data: AuthData,
}

#[derive(Serialize)]
pub struct AuthData {
    pub token: String,
    pub expires_at: String,
    pub user: UserData,
}

#[derive(Serialize)]
pub struct UserData {
    pub id: String,
    pub email: String,
    pub display_name: Option<String>,
    pub tier: String,
}

/// Database user row
#[derive(FromRow)]
struct UserRow {
    id: Uuid,
    email: String,
    password_hash: String,
    display_name: Option<String>,
    tier: String,
}

/// Register new user
async fn register(
    State(state): State<AppState>,
    Json(req): Json<RegisterRequest>,
) -> ApiResult<Json<AuthResponse>> {
    req.validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;

    let email = req.email.to_lowercase();

    // Hash password with argon2
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = argon2
        .hash_password(req.password.as_bytes(), &salt)
        .map_err(|e| ApiError::Internal(format!("Password hashing failed: {}", e)))?
        .to_string();

    // Create user in database
    let user: UserRow = sqlx::query_as(
        r#"
        SELECT id, email, password_hash, display_name, tier
        FROM feed_radar_auth_register($1, $2, $3)
        "#,
    )
    .bind(&email)
    .bind(&password_hash)
    .bind(&req.display_name)
    .fetch_one(state.auth_db())
    .await
    .map_err(|error| {
        if error
            .as_database_error()
            .and_then(sqlx::error::DatabaseError::code)
            .is_some_and(|code| code == "23505")
        {
            ApiError::Conflict("Email already registered".to_string())
        } else {
            ApiError::Internal("Failed to create user".to_string())
        }
    })?;

    let (token, expires_at) = generate_jwt(&user, state.jwt_secret(), state.jwt_expiration())?;

    // Update last login through the reviewed authentication boundary.
    let _ = sqlx::query("SELECT feed_radar_auth_touch_last_login($1)")
        .bind(user.id)
        .execute(state.auth_db())
        .await;

    Ok(Json(AuthResponse {
        data: AuthData {
            token,
            expires_at: expires_at.to_rfc3339(),
            user: UserData {
                id: user.id.to_string(),
                email: user.email,
                display_name: user.display_name,
                tier: user.tier,
            },
        },
    }))
}

/// Login user
async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> ApiResult<Json<AuthResponse>> {
    req.validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;

    let email = req.email.to_lowercase();

    let user: Option<UserRow> = sqlx::query_as(
        "SELECT id, email, password_hash, display_name, tier FROM feed_radar_auth_find_user($1)",
    )
    .bind(&email)
    .fetch_optional(state.auth_db())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let user =
        user.ok_or_else(|| ApiError::Unauthorized("Invalid email or password".to_string()))?;

    // Verify password with argon2
    let parsed_hash = PasswordHash::new(&user.password_hash)
        .map_err(|_| ApiError::Internal("Invalid password hash format".to_string()))?;

    Argon2::default()
        .verify_password(req.password.as_bytes(), &parsed_hash)
        .map_err(|_| ApiError::Unauthorized("Invalid email or password".to_string()))?;

    let (token, expires_at) = generate_jwt(&user, state.jwt_secret(), state.jwt_expiration())?;

    // Update last login through the reviewed authentication boundary.
    let _ = sqlx::query("SELECT feed_radar_auth_touch_last_login($1)")
        .bind(user.id)
        .execute(state.auth_db())
        .await;

    Ok(Json(AuthResponse {
        data: AuthData {
            token,
            expires_at: expires_at.to_rfc3339(),
            user: UserData {
                id: user.id.to_string(),
                email: user.email,
                display_name: user.display_name,
                tier: user.tier,
            },
        },
    }))
}

/// Refresh token
async fn refresh(
    State(state): State<AppState>,
    claims: crate::extractors::auth::CurrentUser,
) -> ApiResult<Json<AuthResponse>> {
    // Get fresh user data inside the JWT tenant boundary.
    let mut tx = state.tenant_tx(claims.id).await?;
    let user: UserRow = sqlx::query_as(
        "SELECT id, email, password_hash, display_name, tier FROM users WHERE id = $1 AND deleted_at IS NULL"
    )
    .bind(claims.id)
    .fetch_optional(tx.connection())
    .await
    .map_err(|_| ApiError::Internal("Failed to load user".to_string()))?
    .ok_or_else(|| ApiError::Unauthorized("User not found".to_string()))?;
    tx.commit().await?;

    let (token, expires_at) = generate_jwt(&user, state.jwt_secret(), state.jwt_expiration())?;

    Ok(Json(AuthResponse {
        data: AuthData {
            token,
            expires_at: expires_at.to_rfc3339(),
            user: UserData {
                id: user.id.to_string(),
                email: user.email,
                display_name: user.display_name,
                tier: user.tier,
            },
        },
    }))
}

/// Logout (invalidate token)
async fn logout() -> ApiResult<Json<serde_json::Value>> {
    // In a stateless JWT system, logout is handled client-side
    // For enhanced security, we could add the token to a Redis blacklist
    Ok(Json(serde_json::json!({ "data": { "success": true } })))
}

/// Get current user
async fn me(
    State(state): State<AppState>,
    claims: crate::extractors::auth::CurrentUser,
) -> ApiResult<Json<serde_json::Value>> {
    let mut tx = state.tenant_tx(claims.id).await?;
    let user: UserRow = sqlx::query_as(
        "SELECT id, email, password_hash, display_name, tier FROM users WHERE id = $1 AND deleted_at IS NULL"
    )
    .bind(claims.id)
    .fetch_optional(tx.connection())
    .await
    .map_err(|_| ApiError::Internal("Failed to load user".to_string()))?
    .ok_or_else(|| ApiError::NotFound("User not found".to_string()))?;
    tx.commit().await?;

    Ok(Json(serde_json::json!({
        "data": {
            "id": user.id.to_string(),
            "email": user.email,
            "display_name": user.display_name,
            "tier": user.tier
        }
    })))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    JwtSession,
    BiscuitDelegation,
}

impl TokenKind {
    pub fn can_authorize_harness_delegation(self) -> bool {
        matches!(self, Self::BiscuitDelegation)
    }
}

/// Generate JWT token
fn generate_jwt(
    user: &UserRow,
    secret: &str,
    expiration_secs: u64,
) -> ApiResult<(String, chrono::DateTime<Utc>)> {
    let now = Utc::now();
    let expires_at = now + Duration::seconds(expiration_secs as i64);

    let claims = Claims {
        sub: user.id.to_string(),
        email: user.email.clone(),
        tier: user.tier.clone(),
        exp: expires_at.timestamp(),
        iat: now.timestamp(),
    };

    let token = encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| ApiError::Internal(format!("JWT encoding failed: {}", e)))?;

    Ok((token, expires_at))
}

/// Build auth routes
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/auth/register", post(register))
        .route("/api/v1/auth/login", post(login))
        .route("/api/v1/auth/refresh", post(refresh))
        .route("/api/v1/auth/logout", post(logout))
        .route("/api/v1/auth/me", axum::routing::get(me))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extractors::auth::decode_session_claims;

    const TEST_SECRET: &str = "test-secret-for-jwt-round-trip-proof";

    fn probe_user() -> UserRow {
        UserRow {
            id: Uuid::nil(),
            email: "round-trip@example.test".to_string(),
            password_hash: "unused-by-token-minting".to_string(),
            display_name: Some("Round Trip".to_string()),
            tier: "pro".to_string(),
        }
    }

    /// Signs with the production minting function and verifies with the
    /// production verification function.
    ///
    /// `jsonwebtoken` 10.x resolves its crypto backend from crate features and
    /// panics on the first sign *or* verify when none is enabled, so this test
    /// is the guard for that backend being wired into the production build.
    /// Without the provider it aborts; with it, it passes. Nothing here touches
    /// the database, so it runs on every machine and in CI rather than skipping.
    #[test]
    fn session_token_signs_and_verifies_round_trip() {
        let user = probe_user();

        let (token, expires_at) =
            generate_jwt(&user, TEST_SECRET, 3600).expect("minting a session token must succeed");

        let claims =
            decode_session_claims(&token, TEST_SECRET).expect("a freshly minted token must verify");

        assert_eq!(claims.sub, user.id.to_string());
        assert_eq!(claims.email, user.email);
        assert_eq!(claims.tier, user.tier);
        assert_eq!(claims.exp, expires_at.timestamp());
        assert!(claims.exp > claims.iat);
    }

    #[test]
    fn session_token_is_refused_under_a_different_secret() {
        let (token, _) = generate_jwt(&probe_user(), TEST_SECRET, 3600).expect("mint");

        let rejection = decode_session_claims(&token, "a-completely-different-secret")
            .expect_err("a token signed with another secret must not verify");

        assert!(matches!(rejection, ApiError::Unauthorized(_)));
    }

    #[test]
    fn tampered_payload_is_refused() {
        let (token, _) = generate_jwt(&probe_user(), TEST_SECRET, 3600).expect("mint");

        let mut parts = token.split('.');
        let header = parts.next().expect("header segment");
        let signature = parts.next_back().expect("signature segment");
        let forged_claims = Claims {
            sub: Uuid::new_v4().to_string(),
            email: "attacker@example.test".to_string(),
            tier: "team".to_string(),
            exp: (Utc::now() + Duration::seconds(3600)).timestamp(),
            iat: Utc::now().timestamp(),
        };
        let forged_payload = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            serde_json::to_vec(&forged_claims).expect("serialize forged claims"),
        );
        let forged = format!("{header}.{forged_payload}.{signature}");

        let rejection = decode_session_claims(&forged, TEST_SECRET)
            .expect_err("a re-written payload must not verify under the original signature");

        assert!(matches!(rejection, ApiError::Unauthorized(_)));
    }

    /// The verification side pins `HS256`, and `jsonwebtoken` checks the header
    /// algorithm against that list before it constructs a verifier. A token
    /// announcing `RS256` is therefore refused without any RSA code running —
    /// which is what keeps the `rsa` backend of the `rust_crypto` provider off
    /// every reachable path here (see `docs/adr/0005`).
    #[test]
    fn token_announcing_another_algorithm_is_refused() {
        let (token, _) = generate_jwt(&probe_user(), TEST_SECRET, 3600).expect("mint");

        let mut parts = token.split('.');
        let _ = parts.next();
        let payload = parts.next().expect("payload segment");
        let signature = parts.next().expect("signature segment");
        let rs256_header = base64::Engine::encode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            br#"{"typ":"JWT","alg":"RS256"}"#,
        );
        let forged = format!("{rs256_header}.{payload}.{signature}");

        let rejection = decode_session_claims(&forged, TEST_SECRET)
            .expect_err("a token announcing RS256 must be refused");

        assert!(matches!(rejection, ApiError::Unauthorized(_)));
    }

    #[test]
    fn expired_token_is_refused() {
        // Past `exp` by well over the 60s leeway `Validation` allows by default.
        let claims = Claims {
            sub: Uuid::nil().to_string(),
            email: "round-trip@example.test".to_string(),
            tier: "pro".to_string(),
            exp: (Utc::now() - Duration::seconds(3600)).timestamp(),
            iat: (Utc::now() - Duration::seconds(7200)).timestamp(),
        };
        let stale = encode(
            &Header::default(),
            &claims,
            &EncodingKey::from_secret(TEST_SECRET.as_bytes()),
        )
        .expect("minting a stale token must succeed");

        let rejection = decode_session_claims(&stale, TEST_SECRET)
            .expect_err("an expired token must not verify");

        assert!(matches!(rejection, ApiError::Unauthorized(_)));
    }

    #[test]
    fn jwt_session_tokens_never_authorize_harness_delegation() {
        assert!(!TokenKind::JwtSession.can_authorize_harness_delegation());
    }

    #[test]
    fn biscuit_delegation_tokens_authorize_harness_delegation_boundary() {
        assert!(TokenKind::BiscuitDelegation.can_authorize_harness_delegation());
    }
}
