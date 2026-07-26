//! Feed management routes

use sha2::{Digest, Sha256};

use axum::{
    extract::{Path, Query, State},
    routing::{get, post},
    Json, Router,
};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;
use validator::Validate;

use crate::error::{ApiError, ApiResult};
use crate::extractors::auth::CurrentUser;
use crate::state::AppState;

use feedmind_domain::feed::FeedItem;
use feedmind_ingest::FeedFetcher;

/// Create feed request
#[derive(Debug, Deserialize, Validate)]
pub struct CreateFeedRequest {
    #[validate(length(min = 1, max = 2048))]
    pub url: String,
    pub folder_id: Option<Uuid>,
    pub title: Option<String>,
}

/// Category filter mode
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CategoryFilterMode {
    Include,
    Exclude,
}

/// Category filter configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryFilter {
    pub mode: CategoryFilterMode,
    pub categories: Vec<String>,
}

/// Update feed request
#[derive(Debug, Deserialize, Validate)]
pub struct UpdateFeedRequest {
    #[validate(length(max = 500))]
    pub title: Option<String>,
    pub folder_id: Option<Uuid>,
    pub priority: Option<String>,
    pub category_filter: Option<CategoryFilter>,
}

/// List feeds query
#[derive(Debug, Deserialize)]
pub struct ListFeedsQuery {
    pub folder_id: Option<Uuid>,
    pub priority: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

/// Feed response
#[derive(Serialize, FromRow)]
pub struct FeedRow {
    pub id: Uuid,
    pub url: String,
    pub title: String,
    pub description: Option<String>,
    pub site_url: Option<String>,
    pub icon_url: Option<String>,
    pub feed_type: Option<String>,
    pub priority: String,
    pub folder_id: Option<Uuid>,
    pub article_count: i32,
    pub unread_count: i32,
    pub error_count: i32,
    pub last_error: Option<String>,
    pub last_fetched_at: Option<chrono::DateTime<chrono::Utc>>,
    pub category_filter: Option<sqlx::types::Json<CategoryFilter>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Serialize)]
pub struct FeedResponse {
    pub data: FeedRow,
}

#[derive(Serialize)]
pub struct FeedsListResponse {
    pub data: Vec<FeedRow>,
    pub meta: ListMeta,
}

#[derive(Serialize)]
pub struct ListMeta {
    pub total: i64,
    pub cursor: Option<String>,
    pub has_more: bool,
}

// =============================================================================
// Helper functions
// =============================================================================

/// Normalize a URL or domain into a proper URL
fn normalize_url(input: &str) -> String {
    let trimmed = input.trim();

    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return trimmed.to_string();
    }

    format!("https://{}", trimmed)
}

/// Discover RSS/Atom feed URL from an HTML page
async fn discover_feed_url(page_url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent("FeedMind/1.0")
        .build()
        .map_err(|e| e.to_string())?;

    let response = client
        .get(page_url)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch page: {}", e))?;

    let html = response.text().await.map_err(|e| e.to_string())?;

    // Look for RSS/Atom link tags
    let feed_patterns = [
        r#"<link[^>]+type="application/rss\+xml"[^>]+href="([^"]+)""#,
        r#"<link[^>]+href="([^"]+)"[^>]+type="application/rss\+xml""#,
        r#"<link[^>]+type="application/atom\+xml"[^>]+href="([^"]+)""#,
        r#"<link[^>]+href="([^"]+)"[^>]+type="application/atom\+xml""#,
    ];

    for pattern in feed_patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if let Some(caps) = re.captures(&html) {
                if let Some(href) = caps.get(1) {
                    let feed_url = href.as_str();
                    // Handle relative URLs
                    if feed_url.starts_with("http") {
                        return Ok(feed_url.to_string());
                    } else if feed_url.starts_with("//") {
                        return Ok(format!("https:{}", feed_url));
                    } else if feed_url.starts_with('/') {
                        let base = page_url.split('/').take(3).collect::<Vec<_>>().join("/");
                        return Ok(format!("{}{}", base, feed_url));
                    } else {
                        let base = page_url
                            .rsplit_once('/')
                            .map(|(b, _)| b)
                            .unwrap_or(page_url);
                        return Ok(format!("{}/{}", base, feed_url));
                    }
                }
            }
        }
    }

    // Try common feed paths as fallback
    let base_url = page_url.trim_end_matches('/');
    let common_paths = [
        "/feed",
        "/feed.xml",
        "/rss",
        "/rss.xml",
        "/atom.xml",
        "/index.xml",
        "/feeds/posts/default",
    ];

    for path in common_paths {
        let test_url = format!("{}{}", base_url, path);
        if let Ok(resp) = client.head(&test_url).send().await {
            if resp.status().is_success() {
                let content_type = resp
                    .headers()
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                if content_type.contains("xml")
                    || content_type.contains("rss")
                    || content_type.contains("atom")
                {
                    return Ok(test_url);
                }
            }
        }
    }

    Err("No RSS/Atom feed found on this page".to_string())
}

/// Generate icon URL from site domain using Google Favicon service
fn generate_icon_url(site_url: Option<&String>) -> Option<String> {
    site_url.map(|site| {
        let domain = site
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or(site);
        format!("https://www.google.com/s2/favicons?domain={}&sz=64", domain)
    })
}

/// Filter articles to last 12 months and limit to max 20
fn filter_articles_for_initial_import(items: Vec<FeedItem>) -> Vec<FeedItem> {
    let twelve_months_ago = Utc::now() - Duration::days(365);

    let mut filtered: Vec<FeedItem> = items
        .into_iter()
        .filter(|item| {
            item.published_at
                .map(|d| d > twelve_months_ago)
                .unwrap_or(true) // Keep items without date
        })
        .collect();

    // Sort by date descending (newest first)
    filtered.sort_by(|a, b| {
        let date_a = a.published_at.unwrap_or(Utc::now());
        let date_b = b.published_at.unwrap_or(Utc::now());
        date_b.cmp(&date_a)
    });

    filtered.truncate(20);
    filtered
}

/// Calculate priority based on article frequency in last 12 months
fn calculate_priority(items: &[FeedItem]) -> &'static str {
    let twelve_months_ago = Utc::now() - Duration::days(365);

    let articles_in_window = items
        .iter()
        .filter(|item| {
            item.published_at
                .map(|d| d > twelve_months_ago)
                .unwrap_or(false)
        })
        .count();

    match articles_in_window {
        n if n > 365 => "hot",  // > 1/day
        n if n >= 52 => "warm", // 1-7/week
        _ => "cold",            // < 1/week
    }
}

/// Maximum articles to retain per feed
const MAX_ARTICLES_PER_FEED: i64 = 200;

/// Enforce article retention limit - delete oldest articles beyond limit
async fn enforce_article_retention(
    tx: &mut sqlx::PgConnection,
    feed_id: Uuid,
    user_id: Uuid,
) -> Result<i64, sqlx::Error> {
    // Delete articles beyond the limit, keeping the newest ones
    // Also keep starred articles regardless of limit
    let result = sqlx::query(
        r#"
        DELETE FROM articles
        WHERE id IN (
            SELECT id FROM articles
            WHERE feed_id = $1 AND user_id = $2 AND is_starred = FALSE
            ORDER BY published_at DESC NULLS LAST
            OFFSET $3
        )
        "#,
    )
    .bind(feed_id)
    .bind(user_id)
    .bind(MAX_ARTICLES_PER_FEED)
    .execute(&mut *tx)
    .await?;

    Ok(result.rows_affected() as i64)
}

// =============================================================================
// Route handlers
// =============================================================================

/// Create a new feed with sync fetch
async fn create_feed(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<CreateFeedRequest>,
) -> ApiResult<Json<FeedResponse>> {
    req.validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;

    let normalized_url = normalize_url(&req.url);

    // Check feed limit in a short tenant transaction before network I/O.
    let mut preflight_tx = state.tenant_tx(user.id).await?;
    let feed_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feeds WHERE user_id = $1")
        .bind(user.id)
        .fetch_one(preflight_tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;
    preflight_tx.commit().await?;

    if feed_count >= user.tier.max_feeds() as i64 {
        return Err(ApiError::Forbidden(format!(
            "Feed limit reached ({}/{}). Upgrade to add more feeds.",
            feed_count,
            user.tier.max_feeds()
        )));
    }

    // Fetch and parse the feed
    let fetcher =
        FeedFetcher::new().map_err(|e| ApiError::Internal(format!("Fetcher error: {}", e)))?;

    // Try to fetch as feed first, then try to discover feed from HTML page
    let (feed_url, feed_meta, items) = match fetcher.fetch(&normalized_url).await {
        Ok((meta, items)) => (normalized_url.clone(), meta, items),
        Err(_) => {
            let discovered_url = discover_feed_url(&normalized_url)
                .await
                .map_err(|e| ApiError::Validation(format!("Could not find RSS feed: {}", e)))?;

            let (meta, items) = fetcher.fetch(&discovered_url).await.map_err(|e| {
                ApiError::Validation(format!("Failed to fetch discovered feed: {}", e))
            })?;

            (discovered_url, meta, items)
        }
    };

    // Check uniqueness and write atomically inside the tenant boundary.
    let mut tx = state.tenant_tx(user.id).await?;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM feeds WHERE user_id = $1 AND url = $2)")
            .bind(user.id)
            .bind(&feed_url)
            .fetch_one(tx.connection())
            .await
            .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if exists {
        return Err(ApiError::Conflict("Feed already exists".to_string()));
    }

    let priority = calculate_priority(&items);

    let articles_to_import = filter_articles_for_initial_import(items);
    let initial_article_count = articles_to_import.len() as i32;

    // Prepare feed data
    let title = req.title.unwrap_or(feed_meta.title);
    let feed_type_str = feed_meta.feed_type.to_string();
    let icon_url = generate_icon_url(feed_meta.site_url.as_ref());

    let feed: FeedRow = sqlx::query_as(
        r#"
        INSERT INTO feeds (user_id, url, title, description, site_url, icon_url, feed_type, folder_id, priority, last_fetched_at, article_count, unread_count)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW(), $10, $10)
        RETURNING id, url, title, description, site_url, icon_url, feed_type, priority,
                  folder_id, article_count, unread_count, error_count, last_error,
                  last_fetched_at, category_filter, created_at
        "#,
    )
    .bind(user.id)
    .bind(&feed_url)
    .bind(&title)
    .bind(&feed_meta.description)
    .bind(&feed_meta.site_url)
    .bind(&icon_url)
    .bind(&feed_type_str)
    .bind(req.folder_id)
    .bind(priority)
    .bind(initial_article_count)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to create feed: {}", e)))?;

    for item in articles_to_import {
        let guid = if item.guid.is_empty() {
            item.url
                .clone()
                .unwrap_or_else(|| Uuid::new_v4().to_string())
        } else {
            item.guid.clone()
        };

        // Convert categories Vec<String> to JSONB
        let categories_json = serde_json::to_value(&item.categories).unwrap_or_default();

        let result = sqlx::query(
            r#"
            INSERT INTO articles (feed_id, user_id, guid, url, title, content, summary, author, published_at, fetched_at, categories)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, NOW(), $10)
            ON CONFLICT (feed_id, guid) DO NOTHING
            "#,
        )
        .bind(feed.id)
        .bind(user.id)
        .bind(&guid)
        .bind(&item.url)
        .bind(&item.title)
        .bind(&item.content)
        .bind(&item.summary)
        .bind(&item.author)
        .bind(item.published_at)
        .bind(&categories_json)
        .execute(&mut *tx)
        .await;

        if let Err(e) = result {
            tracing::warn!(
                article_hash = %safe_hash(&item.title),
                error = %e,
                "Failed to insert article"
            );
        }
    }

    if let Ok(deleted) = enforce_article_retention(&mut tx, feed.id, user.id).await {
        if deleted > 0 {
            tracing::info!(
                deleted,
                feed_id = %feed.id,
                "Deleted old articles to enforce retention limit"
            );
        }
    }

    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(format!("Commit error: {}", e)))?;

    Ok(Json(FeedResponse { data: feed }))
}

/// List all feeds
async fn list_feeds(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<ListFeedsQuery>,
) -> ApiResult<Json<FeedsListResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let limit = query.limit.unwrap_or(50).min(100);

    let feeds: Vec<FeedRow> = if let Some(folder_id) = query.folder_id {
        sqlx::query_as(
            r#"
            SELECT id, url, title, description, site_url, icon_url, feed_type, priority,
                   folder_id, article_count, unread_count, error_count, last_error,
                   last_fetched_at, category_filter, created_at
            FROM feeds
            WHERE user_id = $1 AND folder_id = $2
            ORDER BY position ASC, title ASC
            LIMIT $3
            "#,
        )
        .bind(user.id)
        .bind(folder_id)
        .bind(limit)
        .fetch_all(tx.connection())
        .await
    } else {
        sqlx::query_as(
            r#"
            SELECT id, url, title, description, site_url, icon_url, feed_type, priority,
                   folder_id, article_count, unread_count, error_count, last_error,
                   last_fetched_at, category_filter, created_at
            FROM feeds
            WHERE user_id = $1
            ORDER BY position ASC, title ASC
            LIMIT $2
            "#,
        )
        .bind(user.id)
        .bind(limit)
        .fetch_all(tx.connection())
        .await
    }
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feeds WHERE user_id = $1")
        .bind(user.id)
        .fetch_one(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;
    tx.commit().await?;

    let has_more = feeds.len() as i64 == limit;

    Ok(Json(FeedsListResponse {
        data: feeds,
        meta: ListMeta {
            total,
            cursor: None,
            has_more,
        },
    }))
}

/// Get a single feed
async fn get_feed(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(feed_id): Path<Uuid>,
) -> ApiResult<Json<FeedResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let feed: Option<FeedRow> = sqlx::query_as(
        r#"
        SELECT id, url, title, description, site_url, icon_url, feed_type, priority,
               folder_id, article_count, unread_count, error_count, last_error,
               last_fetched_at, category_filter, created_at
        FROM feeds
        WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(feed_id)
    .bind(user.id)
    .fetch_optional(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let feed = feed.ok_or_else(|| ApiError::NotFound("Feed not found".to_string()))?;
    tx.commit().await?;

    Ok(Json(FeedResponse { data: feed }))
}

/// Update a feed
async fn update_feed(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(feed_id): Path<Uuid>,
    Json(req): Json<UpdateFeedRequest>,
) -> ApiResult<Json<FeedResponse>> {
    req.validate()
        .map_err(|e| ApiError::Validation(e.to_string()))?;

    let mut tx = state.tenant_tx(user.id).await?;
    // Verify ownership
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

    // Validate priority if provided
    if let Some(ref priority) = req.priority {
        if !["hot", "warm", "cold"].contains(&priority.as_str()) {
            return Err(ApiError::Validation(
                "Invalid priority. Use: hot, warm, cold".to_string(),
            ));
        }
    }

    // Convert category_filter to JSON if provided
    let category_filter_json = req
        .category_filter
        .as_ref()
        .and_then(|cf| serde_json::to_value(cf).ok());

    let feed: FeedRow = sqlx::query_as(
        r#"
        UPDATE feeds SET
            title = COALESCE($3, title),
            folder_id = COALESCE($4, folder_id),
            priority = COALESCE($5, priority),
            category_filter = COALESCE($6, category_filter),
            updated_at = NOW()
        WHERE id = $1 AND user_id = $2
        RETURNING id, url, title, description, site_url, icon_url, feed_type, priority,
                  folder_id, article_count, unread_count, error_count, last_error,
                  last_fetched_at, category_filter, created_at
        "#,
    )
    .bind(feed_id)
    .bind(user.id)
    .bind(&req.title)
    .bind(req.folder_id)
    .bind(&req.priority)
    .bind(&category_filter_json)
    .fetch_one(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Failed to update feed: {}", e)))?;
    tx.commit().await?;

    Ok(Json(FeedResponse { data: feed }))
}

/// Delete a feed
async fn delete_feed(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(feed_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let result = sqlx::query("DELETE FROM feeds WHERE id = $1 AND user_id = $2")
        .bind(feed_id)
        .bind(user.id)
        .execute(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound("Feed not found".to_string()));
    }

    tx.commit().await?;
    Ok(Json(serde_json::json!({ "data": { "success": true } })))
}

/// Refresh a feed manually
async fn refresh_feed(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(feed_id): Path<Uuid>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut tx = state.tenant_tx(user.id).await?;
    // Verify ownership
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

    tx.commit().await?;

    // Queue fetch job only after the ownership transaction is closed.
    let mut redis = state.redis();
    let job = serde_json::json!({
        "id": Uuid::new_v4().to_string(),
        "job_type": { "type": "FetchFeed", "data": { "feed_id": feed_id.to_string() } },
        "attempts": 0,
        "max_attempts": 3,
        "created_at": chrono::Utc::now().to_rfc3339()
    });
    let _: () = redis::cmd("RPUSH")
        .arg("feedmind:jobs")
        .arg(job.to_string())
        .query_async(&mut redis)
        .await
        .map_err(|e| ApiError::Internal(format!("Failed to queue job: {}", e)))?;

    Ok(Json(serde_json::json!({ "data": { "queued": true } })))
}

fn safe_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..16]
        .to_string()
}

/// Build feed routes
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/feeds", get(list_feeds).post(create_feed))
        .route(
            "/api/v1/feeds/{id}",
            get(get_feed).put(update_feed).delete(delete_feed),
        )
        .route("/api/v1/feeds/{id}/refresh", post(refresh_feed))
}
