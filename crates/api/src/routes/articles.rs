//! Article routes

use axum::{
    extract::{Path, Query, State},
    routing::{get, put},
    Json, Router,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::error::{ApiError, ApiResult};
use crate::extractors::auth::CurrentUser;
use crate::extractors::json::ContractJson;
use crate::state::AppState;

/// Engine for the pagination cursor: URL-safe so the token survives a query
/// string unescaped, unpadded so it carries no `=` for a caller to mangle.
const CURSOR_ENGINE: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Largest page this endpoint will serve, per the documented contract.
const MAX_LIMIT: i64 = 100;

/// Page size used when the caller does not ask for one.
const DEFAULT_LIMIT: i64 = 50;

/// List articles query
#[derive(Debug, Deserialize)]
pub struct ListArticlesQuery {
    pub feed_id: Option<Uuid>,
    pub folder_id: Option<Uuid>,
    pub status: Option<String>,
    /// Full-text search. Declared by the API design record as a v1.1 feature
    /// and implemented by nothing: `list_articles` refuses a request carrying
    /// it rather than answering an unfiltered page that looks like a result.
    pub search: Option<String>,
    pub categories: Option<String>, // Comma-separated list of categories
    /// Opaque token minted by a previous page. See [`ArticleCursor`].
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

/// Mark-all-read query.
///
/// Deliberately not `ListArticlesQuery`: this handler scopes by feed or folder
/// and consults nothing else. Sharing the list query type made it *declare*
/// that it accepted a status, a category set, a search term, a cursor and a
/// limit — none of which it has ever read.
#[derive(Debug, Deserialize)]
pub struct MarkAllReadQuery {
    pub feed_id: Option<Uuid>,
    pub folder_id: Option<Uuid>,
}

/// Update article request
///
/// `deny_unknown_fields` is load-bearing. This type used to carry a `tags`
/// field that no handler read: a request setting `is_read` and `tags` answered
/// 200, applied the first and discarded the second without a word. Deleting the
/// field alone would not have fixed that — serde ignores unknown fields by
/// default, so the same request would still have succeeded and still have
/// dropped the tags, having merely erased the evidence that the field existed.
/// Article tags are applied through `/api/v1/articles/{id}/tags`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateArticleRequest {
    pub is_read: Option<bool>,
    pub is_starred: Option<bool>,
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

/// Position of the last row of a page, in the ordering `list_articles` uses.
///
/// `list_articles` orders by `published_at DESC NULLS LAST, a.id DESC`. That
/// pair is a *total* order — `id` is the primary key, so no two rows tie — and
/// both of its components are immutable for a given row: `published_at` is
/// written once by the `INSERT ... ON CONFLICT (feed_id, guid) DO NOTHING`
/// ingest path in `feeds.rs` and `worker/queue.rs`, and no statement in this
/// workspace updates it. A token therefore keeps designating the same position
/// after concurrent inserts, which is exactly what an `OFFSET` cannot promise:
/// an article ingested while the caller reads page 1 shifts every later offset
/// by one, skipping a row.
///
/// The token is opaque on purpose. Its encoding is an implementation detail;
/// callers must round-trip it unchanged.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct ArticleCursor {
    /// `published_at` of the last row of the page. `None` places the cursor
    /// inside the trailing block of rows that carry no publication date, which
    /// `NULLS LAST` sorts after every dated row.
    #[serde(rename = "p")]
    published_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(rename = "i")]
    id: Uuid,
}

impl ArticleCursor {
    fn encode(&self) -> ApiResult<String> {
        let json = serde_json::to_vec(self)
            .map_err(|e| ApiError::Internal(format!("Cursor encoding failed: {}", e)))?;
        Ok(CURSOR_ENGINE.encode(json))
    }

    fn decode(raw: &str) -> ApiResult<Self> {
        let json = CURSOR_ENGINE
            .decode(raw)
            .map_err(|_| ApiError::Validation("Malformed pagination cursor".to_string()))?;
        serde_json::from_slice(&json)
            .map_err(|_| ApiError::Validation("Malformed pagination cursor".to_string()))
    }
}

/// List articles
async fn list_articles(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<ListArticlesQuery>,
) -> ApiResult<Json<ArticlesListResponse>> {
    // Refused, not ignored. The API design record schedules full-text search
    // for v1.1 and nothing implements it here; answering an unfiltered page
    // would hand the caller a result set that silently means something else.
    if query.search.is_some() {
        return Err(ApiError::Validation(
            "The `search` filter is not implemented on this endpoint. It is refused rather \
             than ignored, because ignoring it would return an unfiltered page that looks \
             like a search result."
                .to_string(),
        ));
    }

    // Clamped low as well as high: a page must always be able to carry the
    // cursor of its own last row, and a zero-length page has no last row.
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let cursor = query
        .cursor
        .as_deref()
        .map(ArticleCursor::decode)
        .transpose()?;
    let (cursor_published_at, cursor_id) = match &cursor {
        Some(cursor) => (cursor.published_at, Some(cursor.id)),
        None => (None, None),
    };

    let mut tx = state.tenant_tx(user.id).await?;

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

    // One row beyond the page: the only way to tell a full last page from a
    // page with a successor. Comparing the row count to the limit — as this
    // handler used to — reports `has_more` on a collection whose size happens
    // to be a multiple of the page size.
    let probe_limit = limit + 1;

    let mut articles: Vec<ArticleListItem> = sqlx::query_as(
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
          AND (
                -- No cursor: the first page starts at the top of the ordering.
                $6::uuid IS NULL
                -- Cursor on a dated row: every undated row sorts after it.
             OR ($7::timestamptz IS NOT NULL AND a.published_at IS NULL)
             OR ($7::timestamptz IS NOT NULL AND a.published_at IS NOT NULL
                 AND (a.published_at, a.id) < ($7::timestamptz, $6::uuid))
                -- Cursor already inside the undated block: only `id` separates.
             OR ($7::timestamptz IS NULL AND a.published_at IS NULL AND a.id < $6::uuid)
          )
        ORDER BY a.published_at DESC NULLS LAST, a.id DESC
        LIMIT $8
        "#,
    )
    .bind(user.id)
    .bind(query.status.as_deref())
    .bind(&categories)
    .bind(feed_id)
    .bind(folder_id)
    .bind(cursor_id)
    .bind(cursor_published_at)
    .bind(probe_limit)
    .fetch_all(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;

    // The same predicate without the cursor: `total` describes the collection
    // the page belongs to, not the whole library. It used to count every
    // non-hidden article of the user regardless of feed, folder, status or
    // category, so a starred-only page of three could report a total of 900.
    let total: i64 = sqlx::query_scalar(
        r#"
        SELECT COUNT(*)
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
        "#,
    )
    .bind(user.id)
    .bind(query.status.as_deref())
    .bind(&categories)
    .bind(feed_id)
    .bind(folder_id)
    .fetch_one(tx.connection())
    .await
    .map_err(|e| ApiError::Internal(format!("Database error: {}", e)))?;
    tx.commit().await?;

    let has_more = articles.len() as i64 > limit;
    articles.truncate(limit as usize);

    // Minted only when there is a next page, so `has_more` and `cursor` can
    // never contradict each other: announcing a successor without handing over
    // the token to reach it is the defect this replaces.
    let next_cursor = if has_more {
        articles
            .last()
            .map(|last| {
                ArticleCursor {
                    published_at: last.published_at,
                    id: last.id,
                }
                .encode()
            })
            .transpose()?
    } else {
        None
    };

    Ok(Json(ArticlesListResponse {
        data: articles,
        meta: ListMeta {
            total,
            cursor: next_cursor,
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
    ContractJson(req): ContractJson<UpdateArticleRequest>,
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
    Query(query): Query<MarkAllReadQuery>,
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

    use axum::body::Body;
    use axum::http::{header, Request as HttpRequest, StatusCode};
    use axum::response::IntoResponse;
    use tower::ServiceExt;

    // =========================================================================
    // The update contract: a field this endpoint never applied
    // =========================================================================

    /// The body that used to answer 200 while discarding its tags is now
    /// refused, and the refusal names the field.
    ///
    /// Replaces `update_request_accepts_tags_that_no_handler_reads`, which
    /// pinned the opposite: that the same body deserialised and yielded
    /// `tags == Some([...])` for nobody to read.
    #[test]
    fn update_request_refuses_the_tags_it_never_applied() {
        let error = serde_json::from_str::<UpdateArticleRequest>(
            r#"{"is_read":true,"tags":["rust","sovereignty"]}"#,
        )
        .expect_err("a body carrying tags must no longer deserialise");

        assert!(
            error.to_string().contains("tags"),
            "the refusal names the offending field: {error}"
        );
    }

    /// A tags-only update no longer reaches the "nothing to update" branch: it
    /// is refused before a handler sees it.
    ///
    /// Replaces `a_tags_only_update_carries_no_field_the_handler_acts_on`,
    /// which pinned that such a body deserialised into a request the handler
    /// answered from `get_article` — a read served in place of the write the
    /// caller asked for.
    #[test]
    fn a_tags_only_update_is_refused_outright() {
        let error = serde_json::from_str::<UpdateArticleRequest>(r#"{"tags":["rust"]}"#)
            .expect_err("a tags-only body must no longer deserialise");

        assert!(
            error.to_string().contains("tags"),
            "the refusal names the offending field: {error}"
        );
    }

    /// The reason `deny_unknown_fields` is on the type, kept as a live
    /// comparison rather than a comment.
    ///
    /// This is `removing_the_field_would_still_accept_and_discard_the_same_body`
    /// with its conclusion attached: the shrunk struct still swallows the body,
    /// and the shipped type does not. Delete the attribute and this test goes
    /// red, which is the whole point of it.
    #[test]
    fn dropping_the_field_alone_would_have_kept_the_silence() {
        const BODY: &str = r#"{"is_read":true,"tags":["rust","sovereignty"]}"#;

        #[derive(Debug, Deserialize)]
        struct WithoutTagsAndWithoutTheGuard {
            is_read: Option<bool>,
        }

        let permissive: WithoutTagsAndWithoutTheGuard = serde_json::from_str(BODY)
            .expect("serde ignores unknown fields unless deny_unknown_fields is set");
        assert_eq!(
            permissive.is_read,
            Some(true),
            "removing the field alone leaves the request succeeding and the tags dropped"
        );

        serde_json::from_str::<UpdateArticleRequest>(BODY)
            .expect_err("the shipped type refuses what the shrunk one swallowed");
    }

    // =========================================================================
    // The list contract: cursor and search
    // =========================================================================

    /// Round-trip of a cursor minted on a dated row.
    #[test]
    fn a_cursor_survives_encoding_and_decoding() {
        let cursor = ArticleCursor {
            published_at: Some(
                chrono::DateTime::parse_from_rfc3339("2026-03-04T05:06:07Z")
                    .expect("fixture timestamp must parse")
                    .with_timezone(&chrono::Utc),
            ),
            id: Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0),
        };

        let token = cursor.encode().expect("a cursor must encode");
        assert!(
            !token.contains('=') && !token.contains('+') && !token.contains('/'),
            "the token must survive a query string unescaped: {token}"
        );
        assert_eq!(
            ArticleCursor::decode(&token).expect("a minted token must decode"),
            cursor
        );
    }

    /// Round-trip of a cursor minted inside the undated block, which
    /// `NULLS LAST` sorts after every dated row.
    #[test]
    fn a_cursor_without_a_publication_date_survives_the_round_trip() {
        let cursor = ArticleCursor {
            published_at: None,
            id: Uuid::from_u128(7),
        };

        let token = cursor.encode().expect("a cursor must encode");
        assert_eq!(
            ArticleCursor::decode(&token).expect("a minted token must decode"),
            cursor
        );
    }

    /// A token the caller mangled is refused, not silently treated as page one.
    #[test]
    fn a_mangled_cursor_is_refused_rather_than_reset_to_the_first_page() {
        for mangled in ["not-base64-!!", "", "aGVsbG8"] {
            let error = ArticleCursor::decode(mangled)
                .expect_err("a mangled token must not decode: {mangled}");
            let response = error.into_response();
            assert_eq!(
                response.status(),
                StatusCode::UNPROCESSABLE_ENTITY,
                "a bad cursor is a client error, not a silent reset"
            );
        }
    }

    // =========================================================================
    // Live PostgreSQL probes
    // =========================================================================

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

    /// One seeded tenant, its router and a bearer token for it.
    struct Probe {
        app: Router,
        token: String,
        pool: sqlx::PgPool,
        user_id: Uuid,
        feed_id: Uuid,
    }

    impl Probe {
        /// `None` when the live database is not configured, so the suite skips
        /// instead of failing on a machine without the development stack.
        async fn start() -> Option<Self> {
            let Ok(database_url) = std::env::var(TEST_DATABASE_URL) else {
                eprintln!("skipping live probe: {TEST_DATABASE_URL} is not set");
                return None;
            };
            let redis_url = std::env::var(TEST_REDIS_URL)
                .expect("live probe requires FEED_RADAR_TEST_REDIS_URL");

            let config = probe_config(database_url, redis_url);
            let state = AppState::new(&config)
                .await
                .expect("probe app state must build");
            let pool = state.db().clone();

            sqlx::migrate!("../../migrations")
                .run(&pool)
                .await
                .expect("migrations must run");

            let user_id = Uuid::new_v4();
            let feed_id = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO users (id, email, password_hash) VALUES ($1, $2, 'probe-only')",
            )
            .bind(user_id)
            .bind(format!("contract-probe-{user_id}@example.test"))
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

            let now = chrono::Utc::now().timestamp();
            let claims = crate::routes::auth::Claims {
                sub: user_id.to_string(),
                email: format!("contract-probe-{user_id}@example.test"),
                tier: "free".to_string(),
                exp: now + 3600,
                iat: now,
            };
            let token = jsonwebtoken::encode(
                &jsonwebtoken::Header::default(),
                &claims,
                &jsonwebtoken::EncodingKey::from_secret(config.jwt_secret.as_bytes()),
            )
            .expect("probe token must sign");

            Some(Self {
                app: Router::new().merge(router()).with_state(state),
                token,
                pool,
                user_id,
                feed_id,
            })
        }

        async fn seed_article(&self, published_at: Option<chrono::DateTime<chrono::Utc>>) -> Uuid {
            let article_id = Uuid::new_v4();
            sqlx::query(
                r#"INSERT INTO articles (id, feed_id, user_id, guid, title, published_at)
                   VALUES ($1, $2, $3, $4, 'Probe article', $5)"#,
            )
            .bind(article_id)
            .bind(self.feed_id)
            .bind(self.user_id)
            .bind(format!("guid-{article_id}"))
            .bind(published_at)
            .execute(&self.pool)
            .await
            .expect("seed article");
            article_id
        }

        /// One real request through the assembled router.
        async fn call(
            &self,
            method: &str,
            uri: &str,
            body: Option<&'static str>,
        ) -> (StatusCode, serde_json::Value) {
            let mut request = HttpRequest::builder()
                .method(method)
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {}", self.token));
            if body.is_some() {
                request = request.header(header::CONTENT_TYPE, "application/json");
            }
            let request = request
                .body(body.map_or_else(Body::empty, Body::from))
                .expect("probe request must build");

            let response = self
                .app
                .clone()
                .oneshot(request)
                .await
                .expect("the router must answer");
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .expect("response body must read");
            let body = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
            });
            (status, body)
        }

        async fn is_read(&self, article_id: Uuid) -> bool {
            sqlx::query_scalar("SELECT is_read FROM articles WHERE id = $1")
                .bind(article_id)
                .fetch_one(&self.pool)
                .await
                .expect("stored is_read must read")
        }
    }

    /// The proof for defect 1, at the HTTP boundary.
    ///
    /// Replaces `update_article_answers_success_while_discarding_the_requested_tags`,
    /// which asserted 200 + `is_read` applied + tags silently gone. Every one of
    /// those assertions is now inverted: the request is refused, the refusal is
    /// machine-readable and names `tags`, and nothing at all was written.
    #[tokio::test]
    async fn update_article_refuses_a_tag_bearing_body_and_writes_nothing() {
        let Some(probe) = Probe::start().await else {
            return;
        };
        let article_id = probe.seed_article(None).await;

        let tag_id = Uuid::new_v4();
        sqlx::query("INSERT INTO tags (id, user_id, name) VALUES ($1, $2, 'seeded-tag')")
            .bind(tag_id)
            .bind(probe.user_id)
            .execute(&probe.pool)
            .await
            .expect("seed tag");
        sqlx::query("INSERT INTO article_tags (article_id, tag_id) VALUES ($1, $2)")
            .bind(article_id)
            .bind(tag_id)
            .execute(&probe.pool)
            .await
            .expect("seed article tag");

        let (status, body) = probe
            .call(
                "PUT",
                &format!("/api/v1/articles/{article_id}"),
                Some(r#"{"is_read":true,"tags":["brand-new-tag","another-new-tag"]}"#),
            )
            .await;

        eprintln!("PROBE tags status = {status}");
        eprintln!(
            "PROBE tags body   = {}",
            serde_json::to_string(&body).expect("body must serialise")
        );

        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "the endpoint refuses what it cannot apply"
        );
        assert_eq!(
            body["error"]["code"], "VALIDATION_ERROR",
            "the refusal arrives in the documented error envelope"
        );
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("tags"),
            "the refusal names the field the caller must stop sending: {body}"
        );

        assert!(
            !probe.is_read(article_id).await,
            "a refused request applies nothing: the rest of the body was NOT written"
        );
        let stored_tags: Vec<String> = sqlx::query_scalar(
            r#"SELECT t.name FROM article_tags at
               JOIN tags t ON t.id = at.tag_id
               WHERE at.article_id = $1 ORDER BY t.name"#,
        )
        .bind(article_id)
        .fetch_all(&probe.pool)
        .await
        .expect("stored tags must read");
        assert_eq!(stored_tags, vec!["seeded-tag".to_string()]);
    }

    /// The control for the test above: the same endpoint, the same article,
    /// the body without the field. A 422 that fired on every request would
    /// prove nothing about `tags`.
    #[tokio::test]
    async fn update_article_still_applies_the_fields_it_implements() {
        let Some(probe) = Probe::start().await else {
            return;
        };
        let article_id = probe.seed_article(None).await;

        let (status, body) = probe
            .call(
                "PUT",
                &format!("/api/v1/articles/{article_id}"),
                Some(r#"{"is_read":true}"#),
            )
            .await;

        eprintln!("PROBE control status = {status}");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["is_read"], true);
        assert!(probe.is_read(article_id).await);
    }

    /// `search` is answered with a refusal, not with an unfiltered page.
    #[tokio::test]
    async fn list_articles_refuses_the_search_filter_it_does_not_implement() {
        let Some(probe) = Probe::start().await else {
            return;
        };
        probe.seed_article(Some(chrono::Utc::now())).await;

        let (status, body) = probe
            .call("GET", "/api/v1/articles?search=sovereignty", None)
            .await;

        eprintln!("PROBE search status = {status}");
        eprintln!(
            "PROBE search body   = {}",
            serde_json::to_string(&body).expect("body must serialise")
        );

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
        assert!(
            body["data"].is_null(),
            "no page is served alongside the refusal: {body}"
        );
    }

    /// The proof for defect 2: the second page is reachable, and paging through
    /// the whole collection yields every row exactly once.
    ///
    /// The seed deliberately mixes dated and undated articles so that the walk
    /// crosses the `NULLS LAST` boundary — the one place a naive keyset
    /// predicate silently drops the tail of the collection.
    #[tokio::test]
    async fn the_whole_collection_is_reachable_page_by_page_without_gap_or_repeat() {
        let Some(probe) = Probe::start().await else {
            return;
        };

        let base = chrono::Utc::now();
        let mut seeded = Vec::new();
        for offset in 0..7 {
            seeded.push(
                probe
                    .seed_article(Some(base - chrono::Duration::minutes(offset)))
                    .await,
            );
        }
        // Two undated rows: `NULLS LAST` sorts them after every dated row, and
        // only their ids separate them from each other.
        for _ in 0..2 {
            seeded.push(probe.seed_article(None).await);
        }

        let mut seen: Vec<Uuid> = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;

        loop {
            let uri = match &cursor {
                Some(token) => format!("/api/v1/articles?limit=4&cursor={token}"),
                None => "/api/v1/articles?limit=4".to_string(),
            };
            let (status, body) = probe.call("GET", &uri, None).await;
            assert_eq!(status, StatusCode::OK, "page {pages} must answer: {body}");
            pages += 1;

            let page: Vec<Uuid> = body["data"]
                .as_array()
                .expect("a page carries a data array")
                .iter()
                .map(|item| {
                    Uuid::parse_str(item["id"].as_str().expect("an id must be a string"))
                        .expect("an id must parse")
                })
                .collect();

            eprintln!(
                "PROBE page {pages}: rows={} has_more={} cursor={}",
                page.len(),
                body["meta"]["has_more"],
                body["meta"]["cursor"]
            );

            assert_eq!(
                body["meta"]["total"], 9,
                "total describes the filtered collection, not the library"
            );

            seen.extend(page);

            let has_more = body["meta"]["has_more"]
                .as_bool()
                .expect("has_more must be a boolean");
            let next = body["meta"]["cursor"].as_str().map(str::to_owned);

            assert_eq!(
                has_more,
                next.is_some(),
                "a page that announces a successor must hand over the token to reach it: {body}"
            );

            match next {
                Some(token) => cursor = Some(token),
                None => break,
            }
            assert!(pages < 10, "the walk must terminate");
        }

        assert_eq!(pages, 3, "9 rows at 4 per page: 4 + 4 + 1");

        let mut deduplicated = seen.clone();
        deduplicated.sort();
        deduplicated.dedup();
        assert_eq!(
            deduplicated.len(),
            seen.len(),
            "no article was served on two pages"
        );

        let mut expected = seeded.clone();
        expected.sort();
        assert_eq!(deduplicated, expected, "no article was skipped");
    }

    /// The property an `OFFSET` cannot hold: an article ingested between two
    /// requests does not push a row out of the walk.
    ///
    /// With `OFFSET 2`, inserting one row that sorts onto page 1 shifts the
    /// whole collection down by one and the second page re-serves a row already
    /// seen while the last row of the collection falls off the end.
    #[tokio::test]
    async fn a_concurrent_insert_between_two_pages_skips_nothing() {
        let Some(probe) = Probe::start().await else {
            return;
        };

        let base = chrono::Utc::now();
        let mut seeded = Vec::new();
        for offset in 0..4 {
            seeded.push(
                probe
                    .seed_article(Some(base - chrono::Duration::minutes(offset)))
                    .await,
            );
        }

        let (status, first) = probe.call("GET", "/api/v1/articles?limit=2", None).await;
        assert_eq!(status, StatusCode::OK);
        let cursor = first["meta"]["cursor"]
            .as_str()
            .expect("a full page with a successor hands over a cursor")
            .to_owned();
        let first_page: Vec<Uuid> = first["data"]
            .as_array()
            .expect("data array")
            .iter()
            .map(|item| Uuid::parse_str(item["id"].as_str().expect("id")).expect("uuid"))
            .collect();

        // Newest article in the collection: it belongs at the very top, i.e.
        // inside the page already served.
        let intruder = probe
            .seed_article(Some(base + chrono::Duration::minutes(5)))
            .await;

        let (status, second) = probe
            .call(
                "GET",
                &format!("/api/v1/articles?limit=2&cursor={cursor}"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        let second_page: Vec<Uuid> = second["data"]
            .as_array()
            .expect("data array")
            .iter()
            .map(|item| Uuid::parse_str(item["id"].as_str().expect("id")).expect("uuid"))
            .collect();

        eprintln!("PROBE first page  = {first_page:?}");
        eprintln!("PROBE intruder    = {intruder}");
        eprintln!("PROBE second page = {second_page:?}");

        assert!(
            !second_page.iter().any(|id| first_page.contains(id)),
            "the insert must not re-serve a row already returned"
        );
        assert!(
            !second_page.contains(&intruder),
            "the intruder sorts before the cursor and is not re-served after it"
        );

        let mut walked = first_page.clone();
        walked.extend(second_page.clone());
        let expected: Vec<Uuid> = seeded.iter().take(4).copied().collect();
        for id in &expected {
            assert!(
                walked.contains(id),
                "article {id} was skipped by the concurrent insert"
            );
        }
    }

    /// A page whose size exactly matches the collection announces no successor
    /// — and therefore mints no cursor.
    #[tokio::test]
    async fn an_exactly_full_last_page_announces_no_successor() {
        let Some(probe) = Probe::start().await else {
            return;
        };

        let base = chrono::Utc::now();
        for offset in 0..3 {
            probe
                .seed_article(Some(base - chrono::Duration::minutes(offset)))
                .await;
        }

        let (status, body) = probe.call("GET", "/api/v1/articles?limit=3", None).await;

        eprintln!(
            "PROBE exact-fit: has_more={} cursor={}",
            body["meta"]["has_more"], body["meta"]["cursor"]
        );

        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"].as_array().expect("data array").len(), 3);
        assert_eq!(
            body["meta"]["has_more"], false,
            "a full page is not evidence of a next one"
        );
        assert!(body["meta"]["cursor"].is_null());
    }

    /// A cursor the caller mangled is refused at the HTTP boundary too, rather
    /// than quietly restarting the walk at page one.
    #[tokio::test]
    async fn a_mangled_cursor_is_refused_by_the_endpoint() {
        let Some(probe) = Probe::start().await else {
            return;
        };
        probe.seed_article(Some(chrono::Utc::now())).await;

        let (status, body) = probe
            .call("GET", "/api/v1/articles?cursor=not-a-real-token", None)
            .await;

        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "VALIDATION_ERROR");
        assert!(body["data"].is_null(), "no page is served: {body}");
    }
}
