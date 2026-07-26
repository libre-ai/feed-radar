//! Article routes

use axum::{
    extract::{Path, Query, State},
    routing::{get, put},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::extractors::auth::CurrentUser;
use crate::state::AppState;

/// List articles query
#[derive(Debug, Deserialize)]
pub struct ListArticlesQuery {
    pub feed_id: Option<Uuid>,
    pub folder_id: Option<Uuid>,
    pub status: Option<String>,
    pub search: Option<String>,
    pub categories: Option<String>, // Comma-separated list of categories
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

/// Update article request
#[derive(Debug, Deserialize)]
pub struct UpdateArticleRequest {
    pub is_read: Option<bool>,
    pub is_starred: Option<bool>,
    pub tags: Option<Vec<String>>,
}

/// Batch update request
#[derive(Debug, Deserialize)]
pub struct BatchUpdateRequest {
    pub article_ids: Vec<Uuid>,
    pub is_read: Option<bool>,
    pub is_starred: Option<bool>,
}

/// Article row
#[derive(Serialize, FromRow)]
pub struct ArticleRow {
    pub id: Uuid,
    pub feed_id: Uuid,
    pub url: Option<String>,
    pub title: String,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub image_url: Option<String>,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub is_read: bool,
    pub is_starred: bool,
    pub is_hidden: bool,
    pub word_count: Option<i32>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Article list item (without full content)
#[derive(Serialize, FromRow)]
pub struct ArticleListItem {
    pub id: Uuid,
    pub feed_id: Uuid,
    pub feed_title: Option<String>,
    pub url: Option<String>,
    pub title: String,
    pub author: Option<String>,
    pub summary: Option<String>,
    pub image_url: Option<String>,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub is_read: bool,
    pub is_starred: bool,
    pub word_count: Option<i32>,
    pub categories: sqlx::types::Json<Vec<String>>,
}

#[derive(Serialize)]
pub struct ArticleResponse {
    pub data: ArticleRow,
}

#[derive(Serialize)]
pub struct ArticlesListResponse {
    pub data: Vec<ArticleListItem>,
    pub meta: ListMeta,
}

#[derive(Serialize)]
pub struct ListMeta {
    pub total: i64,
    pub cursor: Option<String>,
    pub has_more: bool,
}

/// List articles
async fn list_articles(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<ListArticlesQuery>,
) -> ApiResult<Json<ArticlesListResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let limit = query.limit.unwrap_or(50).min(100);

    let categories = query
        .categories
        .as_deref()
        .map(|values| {
            values
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty());
    let feed_id = query.feed_id;
    // Preserve the public contract: an explicit feed takes precedence over a folder.
    let folder_id = if feed_id.is_some() {
        None
    } else {
        query.folder_id
    };

    let articles: Vec<ArticleListItem> = sqlx::query_as(
        r#"
        SELECT a.id, a.feed_id, f.title as feed_title, a.url, a.title, a.author,
               a.summary, a.image_url, a.published_at, a.is_read, a.is_starred, a.word_count,
               a.categories
        FROM articles a
        JOIN feeds f ON f.id = a.feed_id
        WHERE a.user_id = $1
          AND CASE $2
                WHEN 'unread' THEN a.is_read = FALSE AND a.is_hidden = FALSE
                WHEN 'read' THEN a.is_read = TRUE
                WHEN 'starred' THEN a.is_starred = TRUE
                WHEN 'hidden' THEN a.is_hidden = TRUE
                ELSE a.is_hidden = FALSE
              END
          AND ($3::text[] IS NULL OR a.categories ?| $3)
          AND ($4::uuid IS NULL OR a.feed_id = $4)
          AND ($5::uuid IS NULL OR f.folder_id = $5)
        ORDER BY a.published_at DESC NULLS LAST
        LIMIT $6
        "#,
    )
    .bind(user.id)
    .bind(query.status.as_deref())
    .bind(categories)
    .bind(feed_id)
    .bind(folder_id)
    .bind(limit)
    .fetch_all(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let total: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM articles WHERE user_id = $1 AND is_hidden = FALSE",
    )
    .bind(user.id)
    .fetch_one(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;
    tx.commit().await?;

    let has_more = articles.len() as i64 == limit;

    Ok(Json(ArticlesListResponse {
        data: articles,
        meta: ListMeta {
            total,
            cursor: None,
            has_more,
        },
    }))
}

/// Get a single article
async fn get_article(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(article_id): Path<Uuid>,
) -> ApiResult<Json<ArticleResponse>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let article: Option<ArticleRow> = sqlx::query_as(
        r#"
        SELECT id, feed_id, url, title, author, summary, content, image_url,
               published_at, is_read, is_starred, is_hidden, word_count, created_at
        FROM articles
        WHERE id = $1 AND user_id = $2
        "#,
    )
    .bind(article_id)
    .bind(user.id)
    .fetch_optional(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    let article = article.ok_or_else(|| ApiError::NotFound("Article not found".to_string()))?;
    tx.commit().await?;

    Ok(Json(ArticleResponse { data: article }))
}

/// Update article (read/star status)
async fn update_article(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(article_id): Path<Uuid>,
    Json(req): Json<UpdateArticleRequest>,
) -> ApiResult<Json<ArticleResponse>> {
    if req.is_read.is_none() && req.is_starred.is_none() {
        // Nothing to update, just return the article.
        return get_article(State(state), user, Path(article_id)).await;
    }

    let mut tx = state.tenant_tx(user.id).await?;
    let article = sqlx::query_as::<_, ArticleRow>(
        r#"
        UPDATE articles
        SET is_read = COALESCE($3, is_read),
            read_at = CASE WHEN $3 IS TRUE THEN NOW() ELSE read_at END,
            is_starred = COALESCE($4, is_starred),
            starred_at = CASE WHEN $4 IS TRUE THEN NOW() ELSE starred_at END,
            updated_at = NOW()
        WHERE id = $1 AND user_id = $2
        RETURNING id, feed_id, url, title, author, summary, content, image_url,
                  published_at, is_read, is_starred, is_hidden, word_count, created_at
        "#,
    )
    .bind(article_id)
    .bind(user.id)
    .bind(req.is_read)
    .bind(req.is_starred)
    .fetch_optional(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?
    .ok_or_else(|| ApiError::NotFound("Article not found".to_string()))?;
    tx.commit().await?;

    Ok(Json(ArticleResponse { data: article }))
}

/// Batch update articles
async fn batch_update_articles(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(req): Json<BatchUpdateRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    if req.article_ids.is_empty() {
        return Err(ApiError::Validation("No article IDs provided".to_string()));
    }

    if req.article_ids.len() > 100 {
        return Err(ApiError::Validation(
            "Maximum 100 articles per batch".to_string(),
        ));
    }

    let mut tx = state.tenant_tx(user.id).await?;
    let mut updated = 0;

    if let Some(is_read) = req.is_read {
        let result = if is_read {
            sqlx::query(
                "UPDATE articles SET is_read = TRUE, read_at = NOW(), updated_at = NOW() WHERE id = ANY($1) AND user_id = $2"
            )
        } else {
            sqlx::query(
                "UPDATE articles SET is_read = FALSE, read_at = NULL, updated_at = NOW() WHERE id = ANY($1) AND user_id = $2"
            )
        }
        .bind(&req.article_ids)
        .bind(user.id)
        .execute(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

        updated = result.rows_affected() as i64;
    }

    if let Some(is_starred) = req.is_starred {
        let result = if is_starred {
            sqlx::query(
                "UPDATE articles SET is_starred = TRUE, starred_at = NOW(), updated_at = NOW() WHERE id = ANY($1) AND user_id = $2"
            )
        } else {
            sqlx::query(
                "UPDATE articles SET is_starred = FALSE, starred_at = NULL, updated_at = NOW() WHERE id = ANY($1) AND user_id = $2"
            )
        }
        .bind(&req.article_ids)
        .bind(user.id)
        .execute(tx.connection())
        .await
        .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

        updated = result.rows_affected() as i64;
    }

    tx.commit().await?;
    Ok(Json(serde_json::json!({
        "data": {
            "updated": updated
        }
    })))
}

/// Mark all articles as read
async fn mark_all_read(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<ListArticlesQuery>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut tx = state.tenant_tx(user.id).await?;
    let result = if let Some(feed_id) = query.feed_id {
        sqlx::query(
            "UPDATE articles SET is_read = TRUE, read_at = NOW(), updated_at = NOW() WHERE user_id = $1 AND feed_id = $2 AND is_read = FALSE"
        )
        .bind(user.id)
        .bind(feed_id)
        .execute(tx.connection())
        .await
    } else if let Some(folder_id) = query.folder_id {
        sqlx::query(
            r#"
            UPDATE articles SET is_read = TRUE, read_at = NOW(), updated_at = NOW()
            WHERE user_id = $1 AND is_read = FALSE
            AND feed_id IN (SELECT id FROM feeds WHERE folder_id = $2)
            "#
        )
        .bind(user.id)
        .bind(folder_id)
        .execute(tx.connection())
        .await
    } else {
        sqlx::query(
            "UPDATE articles SET is_read = TRUE, read_at = NOW(), updated_at = NOW() WHERE user_id = $1 AND is_read = FALSE"
        )
        .bind(user.id)
        .execute(tx.connection())
        .await
    }
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;
    tx.commit().await?;

    Ok(Json(serde_json::json!({
        "data": {
            "updated": result.rows_affected()
        }
    })))
}

/// Build article routes
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/v1/articles", get(list_articles))
        .route("/api/v1/articles/batch", put(batch_update_articles))
        .route(
            "/api/v1/articles/mark-all-read",
            axum::routing::post(mark_all_read),
        )
        .route(
            "/api/v1/articles/{id}",
            get(get_article).put(update_article),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `UpdateArticleRequest::tags` is part of the accepted request contract.
    ///
    /// This test exists to make the drop explicit: the field deserialises, and
    /// no handler in this module ever reads it. Whoever makes `tags` meaningful
    /// must delete this test rather than let the contract change in silence.
    #[test]
    fn update_request_accepts_tags_that_no_handler_reads() {
        let req: UpdateArticleRequest =
            serde_json::from_str(r#"{"is_read":true,"tags":["rust","sovereignty"]}"#)
                .expect("a body carrying tags must deserialise");

        assert_eq!(req.is_read, Some(true));
        assert_eq!(
            req.tags,
            Some(vec!["rust".to_string(), "sovereignty".to_string()]),
            "tags is accepted by the request contract"
        );
    }

    /// A tags-only update reaches the "nothing to update" branch of
    /// `update_article`: the request is answered from `get_article` and no
    /// write is issued, even though the caller asked for one.
    #[test]
    fn a_tags_only_update_carries_no_field_the_handler_acts_on() {
        let req: UpdateArticleRequest = serde_json::from_str(r#"{"tags":["rust"]}"#)
            .expect("a tags-only body must deserialise");

        assert!(
            req.is_read.is_none() && req.is_starred.is_none(),
            "update_article treats this as nothing to update and returns the article unchanged"
        );
        assert!(req.tags.is_some(), "yet the caller did ask for a change");
    }

    /// Deleting the field would not surface the loss.
    ///
    /// Serde ignores unknown fields unless `deny_unknown_fields` is set, and no
    /// request type in this module sets it. A shrunk struct still accepts the
    /// same body and still discards `tags` — silently. Removing the field is
    /// therefore not, on its own, a way to inform the caller.
    #[test]
    fn removing_the_field_would_still_accept_and_discard_the_same_body() {
        #[derive(Debug, Deserialize)]
        struct WithoutTags {
            is_read: Option<bool>,
            is_starred: Option<bool>,
        }

        let shrunk: WithoutTags =
            serde_json::from_str(r#"{"is_read":true,"tags":["rust","sovereignty"]}"#)
                .expect("serde ignores unknown fields unless deny_unknown_fields is set");

        assert_eq!(shrunk.is_read, Some(true));
        assert!(shrunk.is_starred.is_none());
    }

    /// `search` and `cursor` share the fate of `tags` on the list endpoint.
    ///
    /// Both are accepted by `ListArticlesQuery` and neither is read by
    /// `list_articles`, which sorts by date and hardcodes `ListMeta::cursor` to
    /// `None`. A caller that filters or paginates gets an unfiltered first page
    /// and no way to advance.
    #[test]
    fn list_query_accepts_search_and_cursor_that_no_handler_reads() {
        let query: ListArticlesQuery =
            serde_json::from_str(r#"{"search":"sovereignty","cursor":"opaque-page-2"}"#)
                .expect("a body carrying search and cursor must deserialise");

        assert_eq!(query.search.as_deref(), Some("sovereignty"));
        assert_eq!(query.cursor.as_deref(), Some("opaque-page-2"));
    }

    const TEST_DATABASE_URL: &str = "FEED_RADAR_TEST_DATABASE_URL";
    const TEST_REDIS_URL: &str = "FEED_RADAR_TEST_REDIS_URL";

    fn probe_config(database_url: String, redis_url: String) -> crate::config::AppConfig {
        crate::config::AppConfig {
            host: "127.0.0.1".to_string(),
            port: 0,
            database_url: database_url.clone(),
            auth_database_url: database_url,
            worker_database_url: None,
            redis_url,
            jwt_secret: "test-only-jwt-secret".to_string(),
            jwt_expiration: 3600,
            master_key: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_string(),
            master_key_version: 1,
            environment: "development".to_string(),
            stripe: crate::config::StripeConfig {
                stripe_secret_key: None,
                stripe_publishable_key: None,
                stripe_webhook_secret: None,
                stripe_price_pro_monthly: None,
                stripe_price_pro_annual: None,
                stripe_price_team_monthly: None,
                stripe_price_team_annual: None,
                stripe_price_ai_tokens: None,
                stripe_price_api_calls: None,
            },
        }
    }

    /// End-to-end proof against a live PostgreSQL: the endpoint answers 200 to
    /// an update carrying tags, applies the rest of the request, and leaves the
    /// article's tags exactly as they were.
    #[tokio::test]
    async fn update_article_answers_success_while_discarding_the_requested_tags() {
        let Ok(database_url) = std::env::var(TEST_DATABASE_URL) else {
            eprintln!("skipping live update_article probe: {TEST_DATABASE_URL} is not set");
            return;
        };
        let redis_url =
            std::env::var(TEST_REDIS_URL).expect("live probe requires FEED_RADAR_TEST_REDIS_URL");

        let state = AppState::new(&probe_config(database_url, redis_url))
            .await
            .expect("probe app state must build");
        let pool = state.db().clone();

        sqlx::migrate!("../../migrations")
            .run(&pool)
            .await
            .expect("migrations must run");

        let user_id = Uuid::new_v4();
        let feed_id = Uuid::new_v4();
        let article_id = Uuid::new_v4();
        let tag_id = Uuid::new_v4();

        sqlx::query("INSERT INTO users (id, email, password_hash) VALUES ($1, $2, 'probe-only')")
            .bind(user_id)
            .bind(format!("tags-probe-{user_id}@example.test"))
            .execute(&pool)
            .await
            .expect("seed user");
        sqlx::query(
            "INSERT INTO feeds (id, user_id, url, title) VALUES ($1, $2, $3, 'Probe feed')",
        )
        .bind(feed_id)
        .bind(user_id)
        .bind(format!("https://example.test/{feed_id}.xml"))
        .execute(&pool)
        .await
        .expect("seed feed");
        sqlx::query(
            r#"INSERT INTO articles (id, feed_id, user_id, guid, title, categories)
               VALUES ($1, $2, $3, $4, 'Probe article', '["seeded-category"]'::jsonb)"#,
        )
        .bind(article_id)
        .bind(feed_id)
        .bind(user_id)
        .bind(format!("guid-{article_id}"))
        .execute(&pool)
        .await
        .expect("seed article");
        sqlx::query("INSERT INTO tags (id, user_id, name) VALUES ($1, $2, 'seeded-tag')")
            .bind(tag_id)
            .bind(user_id)
            .execute(&pool)
            .await
            .expect("seed tag");
        sqlx::query("INSERT INTO article_tags (article_id, tag_id) VALUES ($1, $2)")
            .bind(article_id)
            .bind(tag_id)
            .execute(&pool)
            .await
            .expect("seed article tag");

        let user = CurrentUser {
            id: user_id,
            email: "tags-probe@example.test".to_string(),
            tier: crate::extractors::auth::UserTier::Free,
            account_status: crate::extractors::auth::AccountStatus::Active,
        };

        let requested_tags = vec!["brand-new-tag".to_string(), "another-new-tag".to_string()];
        let response = update_article(
            State(state.clone()),
            user.clone(),
            Path(article_id),
            Json(UpdateArticleRequest {
                is_read: Some(true),
                is_starred: None,
                tags: Some(requested_tags.clone()),
            }),
        )
        .await;

        let http = axum::response::IntoResponse::into_response(response);
        let status = http.status();
        let body = axum::body::to_bytes(http.into_body(), 64 * 1024)
            .await
            .expect("response body must read");
        let body: serde_json::Value =
            serde_json::from_slice(&body).expect("response body must be JSON");

        eprintln!("PROBE status         = {status}");
        eprintln!("PROBE requested tags = {requested_tags:?}");
        eprintln!(
            "PROBE response body  = {}",
            serde_json::to_string(&body).expect("body must serialise")
        );

        let stored_tags: Vec<String> = sqlx::query_scalar(
            r#"SELECT t.name FROM article_tags at
               JOIN tags t ON t.id = at.tag_id
               WHERE at.article_id = $1 ORDER BY t.name"#,
        )
        .bind(article_id)
        .fetch_all(&pool)
        .await
        .expect("stored tags must read");

        let stored_categories: serde_json::Value =
            sqlx::query_scalar("SELECT categories FROM articles WHERE id = $1")
                .bind(article_id)
                .fetch_one(&pool)
                .await
                .expect("stored categories must read");

        let stored_is_read: bool = sqlx::query_scalar("SELECT is_read FROM articles WHERE id = $1")
            .bind(article_id)
            .fetch_one(&pool)
            .await
            .expect("stored is_read must read");

        eprintln!("PROBE stored tags       = {stored_tags:?}");
        eprintln!("PROBE stored categories = {stored_categories}");
        eprintln!("PROBE stored is_read    = {stored_is_read}");

        assert_eq!(
            status,
            axum::http::StatusCode::OK,
            "the endpoint reports success"
        );
        assert!(
            body.get("data").is_some(),
            "the success body carries the article and says nothing about tags"
        );
        assert!(
            body.to_string().find("brand-new-tag").is_none(),
            "no part of the response mentions the tags that were asked for"
        );
        assert!(
            stored_is_read,
            "the rest of the same request WAS applied, so this is not a rejected call"
        );
        assert_eq!(
            stored_tags,
            vec!["seeded-tag".to_string()],
            "the requested tags were discarded: storage still holds only the seeded tag"
        );
        assert_eq!(
            stored_categories,
            serde_json::json!(["seeded-category"]),
            "the article's categories were untouched as well"
        );
    }
}
