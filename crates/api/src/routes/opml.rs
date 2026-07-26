//! OPML import/export routes

use axum::{
    body::Bytes,
    extract::State,
    http::header,
    routing::{get, post},
    Json, Router,
};
use std::{future::Future, pin::Pin};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::extractors::auth::CurrentUser;
use crate::state::AppState;

use feedmind_opml::{OpmlDocument, OpmlExporter, OpmlOutline, OpmlParser};

// =============================================================================
// Request/Response DTOs
// =============================================================================

/// Import OPML request (JSON with base64 content)
#[derive(Debug, Deserialize)]
pub struct ImportOpmlJsonRequest {
    /// Base64-encoded OPML content
    pub content: String,
}

/// Import result
#[derive(Serialize)]
pub struct ImportOpmlResponse {
    pub data: ImportResult,
}

#[derive(Serialize)]
pub struct ImportResult {
    pub feeds_imported: i64,
    pub feeds_skipped: i64,
    pub folders_created: i64,
    pub errors: Vec<ImportError>,
}

#[derive(Serialize)]
pub struct ImportError {
    pub url: String,
    pub reason: String,
}

/// Export response headers
const OPML_CONTENT_TYPE: &str = "application/xml";

// =============================================================================
// Route handlers
// =============================================================================

/// Import OPML file (accepts raw bytes)
async fn import_opml(
    State(state): State<AppState>,
    user: CurrentUser,
    body: Bytes,
) -> ApiResult<Json<ImportOpmlResponse>> {
    // Try to parse as JSON first (base64 encoded)
    let content = if let Ok(json) = serde_json::from_slice::<ImportOpmlJsonRequest>(&body) {
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &json.content)
            .map_err(|e| ApiError::BadRequest(format!("Invalid base64: {}", e)))?
    } else {
        // Try as raw XML/bytes
        body.to_vec()
    };

    let doc = OpmlParser::parse_bytes(&content)
        .map_err(|e| ApiError::BadRequest(format!("Invalid OPML: {}", e)))?;

    // Check and import within one tenant transaction.
    let mut tx = state.tenant_tx(user.id).await?;
    let current_feed_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM feeds WHERE user_id = $1")
            .bind(user.id)
            .fetch_one(tx.connection())
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let new_feeds_count = doc.feed_count() as i64;
    let max_feeds = user.tier.max_feeds() as i64;

    if current_feed_count + new_feeds_count > max_feeds {
        return Err(ApiError::Forbidden(format!(
            "Import would exceed feed limit ({} existing + {} new > {} max). Upgrade or import fewer feeds.",
            current_feed_count, new_feeds_count, max_feeds
        )));
    }

    let mut feeds_imported = 0i64;
    let mut feeds_skipped = 0i64;
    let mut folders_created = 0i64;
    let mut errors: Vec<ImportError> = Vec::new();

    // Process each outline recursively
    for outline in &doc.outlines {
        let result = import_outline(&mut tx, user.id, outline, None).await;
        match result {
            Ok((fi, fs, fc, errs)) => {
                feeds_imported += fi;
                feeds_skipped += fs;
                folders_created += fc;
                errors.extend(errs);
            }
            Err(e) => {
                errors.push(ImportError {
                    url: outline.text.clone(),
                    reason: e.to_string(),
                });
            }
        }
    }

    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Commit error: {}", e)))?;

    Ok(Json(ImportOpmlResponse {
        data: ImportResult {
            feeds_imported,
            feeds_skipped,
            folders_created,
            errors,
        },
    }))
}

type ImportOutlineResult = Result<(i64, i64, i64, Vec<ImportError>), ApiError>;
type ImportOutlineFuture<'a> = Pin<Box<dyn Future<Output = ImportOutlineResult> + Send + 'a>>;

/// Recursively import an outline (folder or feed)
fn import_outline<'a>(
    tx: &'a mut feedmind_storage::TenantTransaction,
    user_id: Uuid,
    outline: &'a OpmlOutline,
    parent_folder_id: Option<Uuid>,
) -> ImportOutlineFuture<'a> {
    Box::pin(async move {
        let mut feeds_imported = 0i64;
        let mut feeds_skipped = 0i64;
        let mut folders_created = 0i64;
        let mut errors: Vec<ImportError> = Vec::new();

        if outline.is_feed() {
            let url = outline.xml_url.as_ref().unwrap();

            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM feeds WHERE user_id = $1 AND url = $2)",
            )
            .bind(user_id)
            .bind(url)
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

            if exists {
                feeds_skipped += 1;
                return Ok((feeds_imported, feeds_skipped, folders_created, errors));
            }

            // Insert feed (without fetching - will be queued for fetch later)
            let title = outline.title.as_ref().unwrap_or(&outline.text).to_string();

            let result = sqlx::query(
                r#"
            INSERT INTO feeds (user_id, url, title, site_url, folder_id, priority)
            VALUES ($1, $2, $3, $4, $5, 'warm')
            "#,
            )
            .bind(user_id)
            .bind(url)
            .bind(&title)
            .bind(&outline.html_url)
            .bind(parent_folder_id)
            .execute(&mut **tx)
            .await;

            match result {
                Ok(_) => {
                    feeds_imported += 1;
                }
                Err(e) => {
                    errors.push(ImportError {
                        url: url.clone(),
                        reason: e.to_string(),
                    });
                }
            }
        } else if !outline.children.is_empty() {
            // This is a folder - create it and recurse
            let folder_name = outline.text.clone();

            let existing_folder_id: Option<Uuid> = sqlx::query_scalar(
                r#"
            SELECT id FROM folders
            WHERE user_id = $1 AND name = $2
            AND (parent_id IS NOT DISTINCT FROM $3)
            "#,
            )
            .bind(user_id)
            .bind(&folder_name)
            .bind(parent_folder_id)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

            let folder_id = if let Some(id) = existing_folder_id {
                id
            } else {
                let next_position: i32 = sqlx::query_scalar(
                    r#"
                SELECT COALESCE(MAX(position), -1) + 1
                FROM folders
                WHERE user_id = $1 AND (parent_id IS NOT DISTINCT FROM $2)
                "#,
                )
                .bind(user_id)
                .bind(parent_folder_id)
                .fetch_one(&mut **tx)
                .await
                .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

                let new_id: Uuid = sqlx::query_scalar(
                "INSERT INTO folders (user_id, name, parent_id, position) VALUES ($1, $2, $3, $4) RETURNING id",
            )
            .bind(user_id)
            .bind(&folder_name)
            .bind(parent_folder_id)
            .bind(next_position)
            .fetch_one(&mut **tx)
            .await
            .map_err(|e| ApiError::Internal(format!("Failed to create folder: {}", e)))?;

                folders_created += 1;
                new_id
            };

            for child in &outline.children {
                let (fi, fs, fc, errs) =
                    import_outline(tx, user_id, child, Some(folder_id)).await?;
                feeds_imported += fi;
                feeds_skipped += fs;
                folders_created += fc;
                errors.extend(errs);
            }
        }

        Ok((feeds_imported, feeds_skipped, folders_created, errors))
    })
}

/// Export feeds as OPML
async fn export_opml(
    State(state): State<AppState>,
    user: CurrentUser,
) -> ApiResult<([(header::HeaderName, &'static str); 2], String)> {
    let mut tx = state.tenant_tx(user.id).await?;
    // Get all folders
    #[derive(sqlx::FromRow)]
    struct FolderForExport {
        id: Uuid,
        name: String,
        parent_id: Option<Uuid>,
    }

    let folders: Vec<FolderForExport> = sqlx::query_as(
        "SELECT id, name, parent_id FROM folders WHERE user_id = $1 ORDER BY parent_id NULLS FIRST, position, name",
    )
    .bind(user.id)
    .fetch_all(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    // Get all feeds
    #[derive(sqlx::FromRow)]
    struct FeedForExport {
        id: Uuid,
        url: String,
        title: String,
        site_url: Option<String>,
        folder_id: Option<Uuid>,
    }

    let feeds: Vec<FeedForExport> = sqlx::query_as(
        "SELECT id, url, title, site_url, folder_id FROM feeds WHERE user_id = $1 ORDER BY position, title",
    )
    .bind(user.id)
    .fetch_all(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;
    tx.commit().await?;

    let mut doc = OpmlDocument::new(Some("FeedMind Export".to_string()));
    doc.owner_email = Some(user.email.clone());
    doc.date_created = Some(Utc::now().to_rfc2822());

    // Build folder hierarchy
    let mut folder_map: std::collections::HashMap<Uuid, OpmlOutline> =
        std::collections::HashMap::new();

    for folder in &folders {
        folder_map.insert(folder.id, OpmlOutline::folder(folder.name.clone()));
    }

    // Add feeds to their folders
    for feed in &feeds {
        let outline =
            OpmlOutline::feed(feed.title.clone(), feed.url.clone(), feed.site_url.clone());

        if let Some(folder_id) = feed.folder_id {
            if let Some(folder) = folder_map.get_mut(&folder_id) {
                folder.children.push(outline);
            } else {
                // Folder not found, add to root
                doc.outlines.push(outline);
            }
        } else {
            // No folder, add to root
            doc.outlines.push(outline);
        }
    }

    // Build folder tree (nested structure)
    // First, identify root folders and folders with parents
    let mut root_folder_ids: Vec<Uuid> = Vec::new();
    let mut child_folder_map: std::collections::HashMap<Uuid, Vec<Uuid>> =
        std::collections::HashMap::new();

    for folder in &folders {
        if let Some(parent_id) = folder.parent_id {
            child_folder_map
                .entry(parent_id)
                .or_default()
                .push(folder.id);
        } else {
            root_folder_ids.push(folder.id);
        }
    }

    // Recursively build folder outlines
    fn build_folder_outline(
        folder_id: Uuid,
        folder_map: &mut std::collections::HashMap<Uuid, OpmlOutline>,
        child_folder_map: &std::collections::HashMap<Uuid, Vec<Uuid>>,
    ) -> Option<OpmlOutline> {
        let mut outline = folder_map.remove(&folder_id)?;

        if let Some(child_ids) = child_folder_map.get(&folder_id) {
            for child_id in child_ids {
                if let Some(child_outline) =
                    build_folder_outline(*child_id, folder_map, child_folder_map)
                {
                    outline.children.push(child_outline);
                }
            }
        }

        Some(outline)
    }

    for folder_id in root_folder_ids {
        if let Some(folder_outline) =
            build_folder_outline(folder_id, &mut folder_map, &child_folder_map)
        {
            if !folder_outline.children.is_empty() {
                doc.outlines.push(folder_outline);
            }
        }
    }

    let xml = OpmlExporter::export(&doc);

    let headers = [
        (header::CONTENT_TYPE, OPML_CONTENT_TYPE),
        (
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"feedmind-export.opml\"",
        ),
    ];

    Ok((headers, xml))
}

// =============================================================================
// Router
// =============================================================================

/// Build OPML routes
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/opml/import", post(import_opml))
        .route("/api/v1/opml/export", get(export_opml))
}
