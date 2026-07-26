//! Rules management routes

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;
use validator::Validate;

use crate::error::{ApiError, ApiResult};
use crate::extractors::auth::CurrentUser;
use crate::state::AppState;

// =============================================================================
// Request/Response DTOs
// =============================================================================

/// Create rule request
#[derive(Debug, Deserialize, Validate)]
pub struct CreateRuleRequest {
    #[validate(length(min = 1, max = 255))]
    pub name: String,
    pub description: Option<String>,
    pub rule_type: Option<String>, // "regex" (default)
    pub config: RuleConfig,
    pub action: String, // "hide", "star", "tag", "mark_read"
    pub action_params: Option<serde_json::Value>,
    pub feed_id: Option<Uuid>,
    pub folder_id: Option<Uuid>,
    pub priority: Option<i32>,
    pub stop_on_match: Option<bool>,
}

/// Update rule request
#[derive(Debug, Deserialize)]
pub struct UpdateRuleRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub config: Option<RuleConfig>,
    pub action: Option<String>,
    pub action_params: Option<serde_json::Value>,
    pub feed_id: Option<Uuid>,
    pub folder_id: Option<Uuid>,
    pub priority: Option<i32>,
    pub stop_on_match: Option<bool>,
    pub is_active: Option<bool>,
}

/// Rule configuration for regex rules
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RuleConfig {
    pub pattern: String,
    #[serde(default = "default_fields")]
    pub fields: Vec<String>, // accepted values: see VALID_FIELDS
    #[serde(default)]
    pub case_sensitive: bool,
}

fn default_fields() -> Vec<String> {
    vec!["title".to_string()]
}

/// List rules query
#[derive(Debug, Deserialize)]
pub struct ListRulesQuery {
    pub feed_id: Option<Uuid>,
    pub folder_id: Option<Uuid>,
    pub is_active: Option<bool>,
}

/// Preview rule request (can reuse create request)
#[derive(Debug, Deserialize, Validate)]
pub struct PreviewRuleRequest {
    pub config: RuleConfig,
    pub feed_id: Option<Uuid>,
    pub folder_id: Option<Uuid>,
    #[serde(default = "default_limit")]
    pub limit: i32,
}

fn default_limit() -> i32 {
    50
}

/// Toggle rule request
#[derive(Debug, Deserialize)]
pub struct ToggleRuleRequest {
    pub is_active: bool,
}

/// Reorder rules request
#[derive(Debug, Deserialize)]
pub struct ReorderRulesRequest {
    pub rule_ids: Vec<Uuid>,
}

/// Rule row from database
#[derive(Debug, Serialize, FromRow)]
pub struct RuleRow {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub rule_type: String,
    pub config: serde_json::Value,
    pub action: String,
    pub action_params: Option<serde_json::Value>,
    pub feed_id: Option<Uuid>,
    pub folder_id: Option<Uuid>,
    pub priority: i32,
    pub stop_on_match: bool,
    pub is_active: bool,
    pub match_count: i32,
    pub last_match_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Single rule response
#[derive(Serialize)]
pub struct RuleResponse {
    pub data: RuleRow,
}

/// List rules response
#[derive(Serialize)]
pub struct RulesListResponse {
    pub data: Vec<RuleRow>,
    pub meta: ListMeta,
}

#[derive(Serialize)]
pub struct ListMeta {
    pub total: i64,
}

/// Preview result
#[derive(Serialize)]
pub struct PreviewResponse {
    pub data: PreviewData,
}

#[derive(Serialize)]
pub struct PreviewData {
    pub total_articles: i64,
    pub matched_articles: i64,
    pub sample_matches: Vec<PreviewMatch>,
}

#[derive(Serialize)]
pub struct PreviewMatch {
    pub article_id: Uuid,
    pub title: String,
    pub matched_field: String,
    pub matched_text: String,
}

// =============================================================================
// Validation helpers
// =============================================================================

const VALID_ACTIONS: &[&str] = &["hide", "star", "tag", "mark_read"];
const VALID_FIELDS: &[&str] = &["title", "content", "summary", "author", "url"];

fn validate_action(action: &str) -> ApiResult<()> {
    if !VALID_ACTIONS.contains(&action) {
        return Err(ApiError::Validation(format!(
            "Invalid action '{}'. Valid actions: {}",
            action,
            VALID_ACTIONS.join(", ")
        )));
    }
    Ok(())
}

fn validate_config(config: &RuleConfig) -> ApiResult<regex::Regex> {
    // Validate pattern compiles as regex
    let re = regex::RegexBuilder::new(&config.pattern)
        .case_insensitive(!config.case_sensitive)
        .build()
        .map_err(|e| ApiError::Validation(format!("Invalid regex pattern: {}", e)))?;

    for field in &config.fields {
        if !VALID_FIELDS.contains(&field.as_str()) {
            return Err(ApiError::Validation(format!(
                "Invalid field '{}'. Valid fields: {}",
                field,
                VALID_FIELDS.join(", ")
            )));
        }
    }

    if config.fields.is_empty() {
        return Err(ApiError::Validation(
            "At least one field must be specified".to_string(),
        ));
    }

    Ok(re)
}

fn validate_action_params(action: &str, params: Option<&serde_json::Value>) -> ApiResult<()> {
    if action == "tag" {
        match params {
            Some(p) => {
                let tags = p.get("tags").and_then(|t| t.as_array());
                if tags.is_none() || tags.unwrap().is_empty() {
                    return Err(ApiError::Validation(
                        "Tag action requires 'tags' array in action_params".to_string(),
                    ));
                }
            }
            None => {
                return Err(ApiError::Validation(
                    "Tag action requires action_params with 'tags' array".to_string(),
                ));
            }
        }
    }
    Ok(())
}

// =============================================================================
// Route handlers
// =============================================================================

/// List all rules for user
async fn list_rules(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<ListRulesQuery>,
) -> ApiResult<Json<RulesListResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let rules: Vec<RuleRow> = if let Some(feed_id) = query.feed_id {
        sqlx::query_as(
            r#"
            SELECT id, name, description, rule_type, config, action, action_params,
                   feed_id, folder_id, priority, stop_on_match, is_active,
                   match_count, last_match_at, created_at, updated_at
            FROM rules
            WHERE user_id = $1 AND (feed_id = $2 OR feed_id IS NULL)
            ORDER BY priority DESC, created_at ASC
            "#,
        )
        .bind(user.id)
        .bind(feed_id)
        .fetch_all(tx.connection())
        .await
    } else if let Some(folder_id) = query.folder_id {
        sqlx::query_as(
            r#"
            SELECT id, name, description, rule_type, config, action, action_params,
                   feed_id, folder_id, priority, stop_on_match, is_active,
                   match_count, last_match_at, created_at, updated_at
            FROM rules
            WHERE user_id = $1 AND (folder_id = $2 OR folder_id IS NULL)
            ORDER BY priority DESC, created_at ASC
            "#,
        )
        .bind(user.id)
        .bind(folder_id)
        .fetch_all(tx.connection())
        .await
    } else if let Some(is_active) = query.is_active {
        sqlx::query_as(
            r#"
            SELECT id, name, description, rule_type, config, action, action_params,
                   feed_id, folder_id, priority, stop_on_match, is_active,
                   match_count, last_match_at, created_at, updated_at
            FROM rules
            WHERE user_id = $1 AND is_active = $2
            ORDER BY priority DESC, created_at ASC
            "#,
        )
        .bind(user.id)
        .bind(is_active)
        .fetch_all(tx.connection())
        .await
    } else {
        sqlx::query_as(
            r#"
            SELECT id, name, description, rule_type, config, action, action_params,
                   feed_id, folder_id, priority, stop_on_match, is_active,
                   match_count, last_match_at, created_at, updated_at
            FROM rules
            WHERE user_id = $1
            ORDER BY priority DESC, created_at ASC
            "#,
        )
        .bind(user.id)
        .fetch_all(tx.connection())
        .await
    }
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let total = rules.len() as i64;
    tx.commit().await?;

    Ok(Json(RulesListResponse {
        data: rules,
        meta: ListMeta { total },
    }))
}

/// Create a new rule
async fn create_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<CreateRuleRequest>,
) -> ApiResult<Json<RuleResponse>> {
    req.validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;

    let action = req.action.to_lowercase();
    validate_action(&action)?;
    validate_config(&req.config)?;
    validate_action_params(&action, req.action_params.as_ref())?;
    let mut tx = state.tenant_tx(user.id).await?;

    // Check rule limit
    let rule_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rules WHERE user_id = $1")
        .bind(user.id)
        .fetch_one(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if rule_count >= user.tier.max_rules() as i64 {
        return Err(ApiError::Forbidden(format!(
            "Rule limit reached ({}/{}). Upgrade to add more rules.",
            rule_count,
            user.tier.max_rules()
        )));
    }

    // Verify feed_id if provided
    if let Some(feed_id) = req.feed_id {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM feeds WHERE id = $1 AND user_id = $2)")
                .bind(feed_id)
                .bind(user.id)
                .fetch_one(tx.connection())
                .await
                .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

        if !exists {
            return Err(ApiError::NotFound("Feed not found".to_string()));
        }
    }

    // Verify folder_id if provided
    if let Some(folder_id) = req.folder_id {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM folders WHERE id = $1 AND user_id = $2)",
        )
        .bind(folder_id)
        .bind(user.id)
        .fetch_one(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

        if !exists {
            return Err(ApiError::NotFound("Folder not found".to_string()));
        }
    }

    let rule_type = req.rule_type.unwrap_or_else(|| "regex".to_string());
    let config_json = serde_json::to_value(&req.config)
        .map_err(|e| ApiError::Internal(format!("Config serialization error: {}", e)))?;
    let priority = req.priority.unwrap_or(0);
    let stop_on_match = req.stop_on_match.unwrap_or(false);

    let rule: RuleRow = sqlx::query_as(
        r#"
        INSERT INTO rules (user_id, name, description, rule_type, config, action, action_params,
                          feed_id, folder_id, priority, stop_on_match)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        RETURNING id, name, description, rule_type, config, action, action_params,
                  feed_id, folder_id, priority, stop_on_match, is_active,
                  match_count, last_match_at, created_at, updated_at
        "#,
    )
    .bind(user.id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&rule_type)
    .bind(&config_json)
    .bind(&action)
    .bind(&req.action_params)
    .bind(req.feed_id)
    .bind(req.folder_id)
    .bind(priority)
    .bind(stop_on_match)
    .fetch_one(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to create rule: {}", e)))?;
    tx.commit().await?;

    Ok(Json(RuleResponse { data: rule }))
}

/// Get a single rule
async fn get_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(rule_id): Path<Uuid>,
) -> ApiResult<Json<RuleResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let rule: Option<RuleRow> = sqlx::query_as(
        r#"
        SELECT id, name, description, rule_type, config, action, action_params,
               feed_id, folder_id, priority, stop_on_match, is_active,
               match_count, last_match_at, created_at, updated_at
        FROM rules
        WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(rule_id)
    .bind(user.id)
    .fetch_optional(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let rule = rule.ok_or_else(|| ApiError::NotFound("Rule not found".to_string()))?;
    tx.commit().await?;

    Ok(Json(RuleResponse { data: rule }))
}

/// Update a rule
async fn update_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(rule_id): Path<Uuid>,
    Json(req): Json<UpdateRuleRequest>,
) -> ApiResult<Json<RuleResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    // Verify rule exists
    let existing: Option<RuleRow> =
        sqlx::query_as("SELECT * FROM rules WHERE id = $1 AND user_id = $2")
            .bind(rule_id)
            .bind(user.id)
            .fetch_optional(tx.connection())
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let existing = existing.ok_or_else(|| ApiError::NotFound("Rule not found".to_string()))?;

    // Validate new values if provided
    if let Some(ref action) = req.action {
        validate_action(action)?;
    }
    if let Some(ref config) = req.config {
        validate_config(config)?;
    }

    let action = req.action.as_ref().unwrap_or(&existing.action);
    let action_params = req
        .action_params
        .as_ref()
        .or(existing.action_params.as_ref());
    validate_action_params(action, action_params)?;

    let config_json = req
        .config
        .as_ref()
        .and_then(|c| serde_json::to_value(c).ok());

    let rule: RuleRow = sqlx::query_as(
        r#"
        UPDATE rules SET
            name = COALESCE($3, name),
            description = COALESCE($4, description),
            config = COALESCE($5, config),
            action = COALESCE($6, action),
            action_params = COALESCE($7, action_params),
            feed_id = COALESCE($8, feed_id),
            folder_id = COALESCE($9, folder_id),
            priority = COALESCE($10, priority),
            stop_on_match = COALESCE($11, stop_on_match),
            is_active = COALESCE($12, is_active),
            updated_at = NOW()
        WHERE id = $1 AND user_id = $2
        RETURNING id, name, description, rule_type, config, action, action_params,
                  feed_id, folder_id, priority, stop_on_match, is_active,
                  match_count, last_match_at, created_at, updated_at
        "#,
    )
    .bind(rule_id)
    .bind(user.id)
    .bind(&req.name)
    .bind(&req.description)
    .bind(&config_json)
    .bind(&req.action)
    .bind(&req.action_params)
    .bind(req.feed_id)
    .bind(req.folder_id)
    .bind(req.priority)
    .bind(req.stop_on_match)
    .bind(req.is_active)
    .fetch_one(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to update rule: {}", e)))?;
    tx.commit().await?;

    Ok(Json(RuleResponse { data: rule }))
}

/// Delete a rule
async fn delete_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(rule_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let result = sqlx::query("DELETE FROM rules WHERE id = $1 AND user_id = $2")
        .bind(rule_id)
        .bind(user.id)
        .execute(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Rule not found".to_string()));
    }

    tx.commit().await?;
    Ok(Json(serde_json::json!({ "data": { "success": true } })))
}

/// Toggle rule active status
async fn toggle_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(rule_id): Path<Uuid>,
    Json(req): Json<ToggleRuleRequest>,
) -> ApiResult<Json<RuleResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let rule: Option<RuleRow> = sqlx::query_as(
        r#"
        UPDATE rules SET is_active = $3, updated_at = NOW()
        WHERE id = $1 AND user_id = $2
        RETURNING id, name, description, rule_type, config, action, action_params,
                  feed_id, folder_id, priority, stop_on_match, is_active,
                  match_count, last_match_at, created_at, updated_at
        "#,
    )
    .bind(rule_id)
    .bind(user.id)
    .bind(req.is_active)
    .fetch_optional(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let rule = rule.ok_or_else(|| ApiError::NotFound("Rule not found".to_string()))?;
    tx.commit().await?;

    Ok(Json(RuleResponse { data: rule }))
}

/// Preview rule effect on recent articles
async fn preview_rule(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<PreviewRuleRequest>,
) -> ApiResult<Json<PreviewResponse>> {
    let re = validate_config(&req.config)?;
    let mut tx = state.tenant_tx(user.id).await?;

    let seven_days_ago = Utc::now() - Duration::days(7);
    let limit = req.limit.min(100);

    #[derive(FromRow)]
    struct ArticleForPreview {
        id: Uuid,
        title: String,
        content: Option<String>,
        summary: Option<String>,
        author: Option<String>,
        url: Option<String>,
    }

    let articles: Vec<ArticleForPreview> = if let Some(feed_id) = req.feed_id {
        sqlx::query_as(
            r#"
            SELECT id, title, content, summary, author, url
            FROM articles
            WHERE user_id = $1 AND feed_id = $2 AND created_at > $3
            ORDER BY published_at DESC NULLS LAST
            LIMIT $4
            "#,
        )
        .bind(user.id)
        .bind(feed_id)
        .bind(seven_days_ago)
        .bind(limit as i64)
        .fetch_all(tx.connection())
        .await
    } else if let Some(folder_id) = req.folder_id {
        sqlx::query_as(
            r#"
            SELECT a.id, a.title, a.content, a.summary, a.author, a.url
            FROM articles a
            INNER JOIN feeds f ON f.id = a.feed_id
            WHERE a.user_id = $1 AND f.folder_id = $2 AND a.created_at > $3
            ORDER BY a.published_at DESC NULLS LAST
            LIMIT $4
            "#,
        )
        .bind(user.id)
        .bind(folder_id)
        .bind(seven_days_ago)
        .bind(limit as i64)
        .fetch_all(tx.connection())
        .await
    } else {
        sqlx::query_as(
            r#"
            SELECT id, title, content, summary, author, url
            FROM articles
            WHERE user_id = $1 AND created_at > $2
            ORDER BY published_at DESC NULLS LAST
            LIMIT $3
            "#,
        )
        .bind(user.id)
        .bind(seven_days_ago)
        .bind(limit as i64)
        .fetch_all(tx.connection())
        .await
    }
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;
    tx.commit().await?;

    let total_articles = articles.len() as i64;
    let mut matched_articles = 0i64;
    let mut sample_matches: Vec<PreviewMatch> = Vec::new();

    for article in articles {
        for field in &req.config.fields {
            let text = match field.as_str() {
                "title" => Some(article.title.as_str()),
                "content" => article.content.as_deref(),
                "summary" => article.summary.as_deref(),
                "author" => article.author.as_deref(),
                "url" => article.url.as_deref(),
                _ => None,
            };

            if let Some(text) = text {
                if let Some(m) = re.find(text) {
                    matched_articles += 1;

                    // Extract context around match
                    let start = m.start().saturating_sub(20);
                    let end = (m.end() + 20).min(text.len());
                    let matched_text = format!(
                        "{}{}{}",
                        if start > 0 { "..." } else { "" },
                        &text[start..end],
                        if end < text.len() { "..." } else { "" }
                    );

                    if sample_matches.len() < 10 {
                        sample_matches.push(PreviewMatch {
                            article_id: article.id,
                            title: article.title.clone(),
                            matched_field: field.clone(),
                            matched_text,
                        });
                    }
                    break; // Only count each article once
                }
            }
        }
    }

    Ok(Json(PreviewResponse {
        data: PreviewData {
            total_articles,
            matched_articles,
            sample_matches,
        },
    }))
}

/// Reorder rules priorities
async fn reorder_rules(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<ReorderRulesRequest>,
) -> ApiResult<Json<RulesListResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;

    // Update priorities in order (highest priority first)
    for (index, rule_id) in req.rule_ids.iter().enumerate() {
        let priority = (req.rule_ids.len() - index) as i32;
        sqlx::query(
            "UPDATE rules SET priority = $3, updated_at = NOW() WHERE id = $1 AND user_id = $2",
        )
        .bind(rule_id)
        .bind(user.id)
        .bind(priority)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to update rule priority: {}", e)))?;
    }

    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Commit error: {}", e)))?;

    list_rules(
        State(state),
        user,
        Query(ListRulesQuery {
            feed_id: None,
            folder_id: None,
            is_active: None,
        }),
    )
    .await
}

// =============================================================================
// Router
// =============================================================================

/// Build rules routes
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/rules", get(list_rules).post(create_rule))
        .route("/api/v1/rules/preview", axum::routing::post(preview_rule))
        .route("/api/v1/rules/reorder", axum::routing::post(reorder_rules))
        .route(
            "/api/v1/rules/{id}",
            get(get_rule).put(update_rule).delete(delete_rule),
        )
        .route(
            "/api/v1/rules/{id}/toggle",
            axum::routing::post(toggle_rule),
        )
}
