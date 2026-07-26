//! Folder management routes

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

/// Create folder request
#[derive(Debug, Deserialize, Validate)]
pub struct CreateFolderRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    pub parent_id: Option<Uuid>,
}

/// Update folder request
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateFolderRequest {
    #[validate(length(min = 1, max = 100))]
    pub name: Option<String>,
    pub parent_id: Option<Uuid>,
    pub position: Option<i32>,
}

/// Reorder folder request
#[derive(Debug, Deserialize)]
pub struct ReorderFolderRequest {
    pub position: i32,
}

/// Folder row from database
#[derive(Debug, Serialize, FromRow)]
pub struct FolderRow {
    pub id: Uuid,
    pub name: String,
    pub parent_id: Option<Uuid>,
    pub position: i32,
    pub feed_count: i64,
    pub unread_count: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Single folder response
#[derive(Serialize)]
pub struct FolderResponse {
    pub data: FolderRow,
}

/// List folders response
#[derive(Serialize)]
pub struct FoldersListResponse {
    pub data: Vec<FolderRow>,
    pub meta: ListMeta,
}

#[derive(Serialize)]
pub struct ListMeta {
    pub total: i64,
}

// =============================================================================
// Route handlers
// =============================================================================

/// List all folders for user (hierarchical)
async fn list_folders(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<Json<FoldersListResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let folders: Vec<FolderRow> = sqlx::query_as(
        r#"
        SELECT
            f.id, f.name, f.parent_id, f.position, f.created_at, f.updated_at,
            COALESCE(COUNT(DISTINCT fd.id), 0)::bigint as feed_count,
            COALESCE(SUM(fd.unread_count), 0)::bigint as unread_count
        FROM folders f
        LEFT JOIN feeds fd ON fd.folder_id = f.id AND fd.user_id = $1
        WHERE f.user_id = $1
        GROUP BY f.id
        ORDER BY f.parent_id NULLS FIRST, f.position ASC, f.name ASC
        "#,
    )
    .bind(user.id)
    .fetch_all(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let total = folders.len() as i64;
    tx.commit().await?;

    Ok(Json(FoldersListResponse {
        data: folders,
        meta: ListMeta { total },
    }))
}

/// Create a new folder
async fn create_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<CreateFolderRequest>,
) -> ApiResult<Json<FolderResponse>> {
    req.validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;

    // If parent_id is provided, verify it exists and belongs to user
    if let Some(parent_id) = req.parent_id {
        let parent_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM folders WHERE id = $1 AND user_id = $2)",
        )
        .bind(parent_id)
        .bind(user.id)
        .fetch_one(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

        if !parent_exists {
            return Err(ApiError::NotFound("Parent folder not found".to_string()));
        }
    }

    // Check for duplicate name at same level
    let duplicate_exists: bool = sqlx::query_scalar(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM folders
            WHERE user_id = $1 AND name = $2
            AND (parent_id IS NOT DISTINCT FROM $3)
        )
        "#,
    )
    .bind(user.id)
    .bind(&req.name)
    .bind(req.parent_id)
    .fetch_one(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if duplicate_exists {
        return Err(ApiError::Conflict(
            "A folder with this name already exists at this level".to_string(),
        ));
    }

    let next_position: i32 = sqlx::query_scalar(
        r#"
        SELECT COALESCE(MAX(position), -1) + 1
        FROM folders
        WHERE user_id = $1 AND (parent_id IS NOT DISTINCT FROM $2)
        "#,
    )
    .bind(user.id)
    .bind(req.parent_id)
    .fetch_one(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let folder: FolderRow = sqlx::query_as(
        r#"
        WITH inserted AS (
            INSERT INTO folders (user_id, name, parent_id, position)
            VALUES ($1, $2, $3, $4)
            RETURNING id, name, parent_id, position, created_at, updated_at
        )
        SELECT
            i.id, i.name, i.parent_id, i.position, i.created_at, i.updated_at,
            0::bigint as feed_count,
            0::bigint as unread_count
        FROM inserted i
        "#,
    )
    .bind(user.id)
    .bind(&req.name)
    .bind(req.parent_id)
    .bind(next_position)
    .fetch_one(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to create folder: {}", e)))?;
    tx.commit().await?;

    Ok(Json(FolderResponse { data: folder }))
}

/// Get a single folder with stats
async fn get_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(folder_id): Path<Uuid>,
) -> ApiResult<Json<FolderResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let folder: Option<FolderRow> = sqlx::query_as(
        r#"
        SELECT
            f.id, f.name, f.parent_id, f.position, f.created_at, f.updated_at,
            COALESCE(COUNT(DISTINCT fd.id), 0)::bigint as feed_count,
            COALESCE(SUM(fd.unread_count), 0)::bigint as unread_count
        FROM folders f
        LEFT JOIN feeds fd ON fd.folder_id = f.id AND fd.user_id = $2
        WHERE f.id = $1 AND f.user_id = $2
        GROUP BY f.id
        "#,
    )
    .bind(folder_id)
    .bind(user.id)
    .fetch_optional(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let folder = folder.ok_or_else(|| ApiError::NotFound("Folder not found".to_string()))?;
    tx.commit().await?;

    Ok(Json(FolderResponse { data: folder }))
}

/// Update a folder
async fn update_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(folder_id): Path<Uuid>,
    Json(req): Json<UpdateFolderRequest>,
) -> ApiResult<Json<FolderResponse>> {
    req.validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;
    let mut tx = state.tenant_tx(user.id).await?;

    // Verify folder exists and belongs to user
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM folders WHERE id = $1 AND user_id = $2)")
            .bind(folder_id)
            .bind(user.id)
            .fetch_one(tx.connection())
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if !exists {
        return Err(ApiError::NotFound("Folder not found".to_string()));
    }

    // If changing parent, verify new parent exists (and is not self or descendant)
    if let Some(new_parent_id) = req.parent_id {
        if new_parent_id == folder_id {
            return Err(ApiError::Validation(
                "Cannot set folder as its own parent".to_string(),
            ));
        }

        let parent_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM folders WHERE id = $1 AND user_id = $2)",
        )
        .bind(new_parent_id)
        .bind(user.id)
        .fetch_one(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

        if !parent_exists {
            return Err(ApiError::NotFound("Parent folder not found".to_string()));
        }

        // Check for circular reference (new parent cannot be a descendant)
        let is_descendant: bool = sqlx::query_scalar(
            r#"
            WITH RECURSIVE descendants AS (
                SELECT id FROM folders WHERE parent_id = $1 AND user_id = $3
                UNION ALL
                SELECT f.id FROM folders f
                INNER JOIN descendants d ON f.parent_id = d.id
                WHERE f.user_id = $3
            )
            SELECT EXISTS(SELECT 1 FROM descendants WHERE id = $2)
            "#,
        )
        .bind(folder_id)
        .bind(new_parent_id)
        .bind(user.id)
        .fetch_one(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

        if is_descendant {
            return Err(ApiError::Validation(
                "Cannot move folder under its own descendant".to_string(),
            ));
        }
    }

    // Check for duplicate name at new level
    if let Some(ref new_name) = req.name {
        let target_parent = req.parent_id; // None means keep current parent
        let duplicate_exists: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM folders
                WHERE user_id = $1 AND name = $2 AND id != $3
                AND (parent_id IS NOT DISTINCT FROM COALESCE($4, (SELECT parent_id FROM folders WHERE id = $3)))
            )
            "#,
        )
        .bind(user.id)
        .bind(new_name)
        .bind(folder_id)
        .bind(target_parent)
        .fetch_one(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

        if duplicate_exists {
            return Err(ApiError::Conflict(
                "A folder with this name already exists at this level".to_string(),
            ));
        }
    }

    let folder: FolderRow = sqlx::query_as(
        r#"
        WITH updated AS (
            UPDATE folders SET
                name = COALESCE($3, name),
                parent_id = COALESCE($4, parent_id),
                position = COALESCE($5, position),
                updated_at = NOW()
            WHERE id = $1 AND user_id = $2
            RETURNING id, name, parent_id, position, created_at, updated_at
        )
        SELECT
            u.id, u.name, u.parent_id, u.position, u.created_at, u.updated_at,
            COALESCE(COUNT(DISTINCT fd.id), 0)::bigint as feed_count,
            COALESCE(SUM(fd.unread_count), 0)::bigint as unread_count
        FROM updated u
        LEFT JOIN feeds fd ON fd.folder_id = u.id AND fd.user_id = $2
        GROUP BY u.id, u.name, u.parent_id, u.position, u.created_at, u.updated_at
        "#,
    )
    .bind(folder_id)
    .bind(user.id)
    .bind(&req.name)
    .bind(req.parent_id)
    .bind(req.position)
    .fetch_one(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to update folder: {}", e)))?;
    tx.commit().await?;

    Ok(Json(FolderResponse { data: folder }))
}

/// Delete a folder (cascades to feeds and articles)
async fn delete_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(folder_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut tx = state.tenant_tx(user.id).await?;
    // Delete folder (ON DELETE CASCADE will handle feeds/articles)
    let result = sqlx::query("DELETE FROM folders WHERE id = $1 AND user_id = $2")
        .bind(folder_id)
        .bind(user.id)
        .execute(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Folder not found".to_string()));
    }

    tx.commit().await?;
    Ok(Json(serde_json::json!({ "data": { "success": true } })))
}

/// Reorder a folder within its siblings
async fn reorder_folder(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(folder_id): Path<Uuid>,
    Json(req): Json<ReorderFolderRequest>,
) -> ApiResult<Json<FolderResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let current: Option<(Option<Uuid>, i32)> =
        sqlx::query_as("SELECT parent_id, position FROM folders WHERE id = $1 AND user_id = $2")
            .bind(folder_id)
            .bind(user.id)
            .fetch_optional(tx.connection())
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let (parent_id, current_position) =
        current.ok_or_else(|| ApiError::NotFound("Folder not found".to_string()))?;

    let new_position = req.position;

    if current_position == new_position {
        tx.rollback().await?;
        return get_folder(State(state), user, Path(folder_id)).await;
    }

    // Shift other folders to make room
    if new_position < current_position {
        // Moving up: shift folders down
        sqlx::query(
            r#"
            UPDATE folders
            SET position = position + 1
            WHERE user_id = $1
            AND (parent_id IS NOT DISTINCT FROM $2)
            AND position >= $3 AND position < $4
            AND id != $5
            "#,
        )
        .bind(user.id)
        .bind(parent_id)
        .bind(new_position)
        .bind(current_position)
        .bind(folder_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to shift folders: {}", e)))?;
    } else {
        // Moving down: shift folders up
        sqlx::query(
            r#"
            UPDATE folders
            SET position = position - 1
            WHERE user_id = $1
            AND (parent_id IS NOT DISTINCT FROM $2)
            AND position > $3 AND position <= $4
            AND id != $5
            "#,
        )
        .bind(user.id)
        .bind(parent_id)
        .bind(current_position)
        .bind(new_position)
        .bind(folder_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to shift folders: {}", e)))?;
    }

    let folder: FolderRow = sqlx::query_as(
        r#"
        WITH updated AS (
            UPDATE folders SET position = $3, updated_at = NOW()
            WHERE id = $1 AND user_id = $2
            RETURNING id, name, parent_id, position, created_at, updated_at
        )
        SELECT
            u.id, u.name, u.parent_id, u.position, u.created_at, u.updated_at,
            COALESCE(COUNT(DISTINCT fd.id), 0)::bigint as feed_count,
            COALESCE(SUM(fd.unread_count), 0)::bigint as unread_count
        FROM updated u
        LEFT JOIN feeds fd ON fd.folder_id = u.id AND fd.user_id = $2
        GROUP BY u.id, u.name, u.parent_id, u.position, u.created_at, u.updated_at
        "#,
    )
    .bind(folder_id)
    .bind(user.id)
    .bind(new_position)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to update folder: {}", e)))?;

    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Commit error: {}", e)))?;

    Ok(Json(FolderResponse { data: folder }))
}

// =============================================================================
// Router
// =============================================================================

/// Build folder routes
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/folders", get(list_folders).post(create_folder))
        .route(
            "/api/v1/folders/{id}",
            get(get_folder).put(update_folder).delete(delete_folder),
        )
        .route(
            "/api/v1/folders/{id}/reorder",
            axum::routing::post(reorder_folder),
        )
}
