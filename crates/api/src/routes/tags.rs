//! Tags management routes

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use chrono::{DateTime, Utc};
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

/// Create tag request
#[derive(Debug, Deserialize, Validate)]
pub struct CreateTagRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    pub color: Option<String>,
}

/// Update tag request
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateTagRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    pub color: Option<String>,
}

/// Add tag to article request
#[derive(Debug, Deserialize)]
pub struct AddTagRequest {
    pub tag_id: Uuid,
}

/// Tag row from database
#[derive(Debug, Serialize, FromRow)]
pub struct TagRow {
    pub id: Uuid,
    pub name: String,
    pub color: Option<String>,
    pub article_count: i64,
    pub created_at: DateTime<Utc>,
}

/// Article tag row
#[derive(Debug, Serialize, FromRow)]
pub struct ArticleTagRow {
    pub tag_id: Uuid,
    pub tag_name: String,
    pub tag_color: Option<String>,
    pub applied_by: String,
    pub created_at: DateTime<Utc>,
}

/// Single tag response
#[derive(Serialize)]
pub struct TagResponse {
    pub data: TagRow,
}

/// List tags response
#[derive(Serialize)]
pub struct TagsListResponse {
    pub data: Vec<TagRow>,
    pub meta: ListMeta,
}

/// Article tags response
#[derive(Serialize)]
pub struct ArticleTagsResponse {
    pub data: Vec<ArticleTagRow>,
}

#[derive(Serialize)]
pub struct ListMeta {
    pub total: i64,
}

use std::sync::OnceLock;

pub fn hex_color_regex() -> &'static regex::Regex {
    static REGEX: OnceLock<regex::Regex> = OnceLock::new();
    REGEX.get_or_init(|| regex::Regex::new(r"^#[0-9A-Fa-f]{6}$").unwrap())
}

// =============================================================================
// Route handlers
// =============================================================================

/// List all tags for user
async fn list_tags(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<Json<TagsListResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let tags: Vec<TagRow> = sqlx::query_as(
        r#"
        SELECT
            t.id, t.name, t.color, t.created_at,
            COALESCE(COUNT(at.article_id), 0)::bigint as article_count
        FROM tags t
        LEFT JOIN article_tags at ON at.tag_id = t.id
        WHERE t.user_id = $1
        GROUP BY t.id
        ORDER BY t.name ASC
        "#,
    )
    .bind(user.id)
    .fetch_all(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let total = tags.len() as i64;
    tx.commit().await?;

    Ok(Json(TagsListResponse {
        data: tags,
        meta: ListMeta { total },
    }))
}

/// Validate hex color format
fn validate_color(color: Option<&String>) -> ApiResult<()> {
    if let Some(c) = color {
        if !hex_color_regex().is_match(c) {
            return Err(ApiError::Validation(
                "Invalid hex color format. Use #RRGGBB format (e.g., #FF5733)".to_string(),
            ));
        }
    }
    Ok(())
}

/// Create a new tag
async fn create_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<CreateTagRequest>,
) -> ApiResult<Json<TagResponse>> {
    req.validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;
    validate_color(req.color.as_ref())?;
    let mut tx = state.tenant_tx(user.id).await?;

    // Check for duplicate name
    let duplicate_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tags WHERE user_id = $1 AND name = $2)")
            .bind(user.id)
            .bind(&req.name)
            .fetch_one(tx.connection())
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if duplicate_exists {
        return Err(ApiError::Conflict(
            "A tag with this name already exists".to_string(),
        ));
    }

    let tag: TagRow = sqlx::query_as(
        r#"
        WITH inserted AS (
            INSERT INTO tags (user_id, name, color)
            VALUES ($1, $2, $3)
            RETURNING id, name, color, created_at
        )
        SELECT
            i.id, i.name, i.color, i.created_at,
            0::bigint as article_count
        FROM inserted i
        "#,
    )
    .bind(user.id)
    .bind(&req.name)
    .bind(&req.color)
    .fetch_one(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to create tag: {}", e)))?;
    tx.commit().await?;

    Ok(Json(TagResponse { data: tag }))
}

/// Get a single tag
async fn get_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(tag_id): Path<Uuid>,
) -> ApiResult<Json<TagResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let tag: Option<TagRow> = sqlx::query_as(
        r#"
        SELECT
            t.id, t.name, t.color, t.created_at,
            COALESCE(COUNT(at.article_id), 0)::bigint as article_count
        FROM tags t
        LEFT JOIN article_tags at ON at.tag_id = t.id
        WHERE t.id = $1 AND t.user_id = $2
        GROUP BY t.id
        "#,
    )
    .bind(tag_id)
    .bind(user.id)
    .fetch_optional(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let tag = tag.ok_or_else(|| ApiError::NotFound("Tag not found".to_string()))?;
    tx.commit().await?;

    Ok(Json(TagResponse { data: tag }))
}

/// Update a tag
async fn update_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(tag_id): Path<Uuid>,
    Json(req): Json<UpdateTagRequest>,
) -> ApiResult<Json<TagResponse>> {
    req.validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;
    validate_color(req.color.as_ref())?;
    let mut tx = state.tenant_tx(user.id).await?;

    // Verify tag exists
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tags WHERE id = $1 AND user_id = $2)")
            .bind(tag_id)
            .bind(user.id)
            .fetch_one(tx.connection())
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if !exists {
        return Err(ApiError::NotFound("Tag not found".to_string()));
    }

    // Check for duplicate name if renaming
    if let Some(ref new_name) = req.name {
        let duplicate_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM tags WHERE user_id = $1 AND name = $2 AND id != $3)",
        )
        .bind(user.id)
        .bind(new_name)
        .bind(tag_id)
        .fetch_one(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

        if duplicate_exists {
            return Err(ApiError::Conflict(
                "A tag with this name already exists".to_string(),
            ));
        }
    }

    let tag: TagRow = sqlx::query_as(
        r#"
        WITH updated AS (
            UPDATE tags SET
                name = COALESCE($3, name),
                color = COALESCE($4, color)
            WHERE id = $1 AND user_id = $2
            RETURNING id, name, color, created_at
        )
        SELECT
            u.id, u.name, u.color, u.created_at,
            COALESCE(COUNT(at.article_id), 0)::bigint as article_count
        FROM updated u
        LEFT JOIN article_tags at ON at.tag_id = u.id
        GROUP BY u.id, u.name, u.color, u.created_at
        "#,
    )
    .bind(tag_id)
    .bind(user.id)
    .bind(&req.name)
    .bind(&req.color)
    .fetch_one(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to update tag: {}", e)))?;
    tx.commit().await?;

    Ok(Json(TagResponse { data: tag }))
}

/// Delete a tag
async fn delete_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(tag_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let result = sqlx::query("DELETE FROM tags WHERE id = $1 AND user_id = $2")
        .bind(tag_id)
        .bind(user.id)
        .execute(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Tag not found".to_string()));
    }

    tx.commit().await?;
    Ok(Json(serde_json::json!({ "data": { "success": true } })))
}

/// Get tags for an article
async fn get_article_tags(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(article_id): Path<Uuid>,
) -> ApiResult<Json<ArticleTagsResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    // Verify article belongs to user
    let article_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM articles WHERE id = $1 AND user_id = $2)")
            .bind(article_id)
            .bind(user.id)
            .fetch_one(tx.connection())
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if !article_exists {
        return Err(ApiError::NotFound("Article not found".to_string()));
    }

    let tags: Vec<ArticleTagRow> = sqlx::query_as(
        r#"
        SELECT
            t.id as tag_id,
            t.name as tag_name,
            t.color as tag_color,
            at.applied_by,
            at.created_at
        FROM article_tags at
        INNER JOIN tags t ON t.id = at.tag_id
        WHERE at.article_id = $1
        ORDER BY t.name ASC
        "#,
    )
    .bind(article_id)
    .fetch_all(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;
    tx.commit().await?;

    Ok(Json(ArticleTagsResponse { data: tags }))
}

/// Add tag to article
async fn add_tag_to_article(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(article_id): Path<Uuid>,
    Json(req): Json<AddTagRequest>,
) -> ApiResult<Json<ArticleTagsResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    // Verify article belongs to user
    let article_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM articles WHERE id = $1 AND user_id = $2)")
            .bind(article_id)
            .bind(user.id)
            .fetch_one(tx.connection())
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if !article_exists {
        return Err(ApiError::NotFound("Article not found".to_string()));
    }

    // Verify tag belongs to user
    let tag_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM tags WHERE id = $1 AND user_id = $2)")
            .bind(req.tag_id)
            .bind(user.id)
            .fetch_one(tx.connection())
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if !tag_exists {
        return Err(ApiError::NotFound("Tag not found".to_string()));
    }

    // Insert tag association (ignore if already exists)
    sqlx::query(
        r#"
        INSERT INTO article_tags (article_id, tag_id, applied_by)
        VALUES ($1, $2, 'user')
        ON CONFLICT (article_id, tag_id) DO NOTHING
        "#,
    )
    .bind(article_id)
    .bind(req.tag_id)
    .execute(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to add tag: {}", e)))?;
    tx.commit().await?;

    get_article_tags(State(state), user, Path(article_id)).await
}

/// Remove tag from article
async fn remove_tag_from_article(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((article_id, tag_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut tx = state.tenant_tx(user.id).await?;
    // Verify article belongs to user
    let article_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM articles WHERE id = $1 AND user_id = $2)")
            .bind(article_id)
            .bind(user.id)
            .fetch_one(tx.connection())
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if !article_exists {
        return Err(ApiError::NotFound("Article not found".to_string()));
    }

    let result = sqlx::query("DELETE FROM article_tags WHERE article_id = $1 AND tag_id = $2")
        .bind(article_id)
        .bind(tag_id)
        .execute(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound(
            "Tag not associated with article".to_string(),
        ));
    }

    tx.commit().await?;
    Ok(Json(serde_json::json!({ "data": { "success": true } })))
}

// =============================================================================
// Router
// =============================================================================

/// Build tags routes
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/tags", get(list_tags).post(create_tag))
        .route(
            "/api/v1/tags/{id}",
            get(get_tag).put(update_tag).delete(delete_tag),
        )
        .route(
            "/api/v1/articles/{article_id}/tags",
            get(get_article_tags).post(add_tag_to_article),
        )
        .route(
            "/api/v1/articles/{article_id}/tags/{tag_id}",
            axum::routing::delete(remove_tag_from_article),
        )
}
