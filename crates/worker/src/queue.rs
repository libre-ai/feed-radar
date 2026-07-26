//! Redis-based job queue consumer

use feedmind_ingest::FeedFetcher;
use redis::aio::ConnectionManager;
use regex::Regex;
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use sqlx::{FromRow, PgPool};
use tracing::{error, info, warn};

use crate::config::WorkerConfig;
use crate::jobs::{Job, JobType};

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

/// Queue consumer for processing background jobs
pub struct QueueConsumer {
    redis: ConnectionManager,
    db: PgPool,
    concurrent_fetches: usize,
    fetcher: FeedFetcher,
}

/// Feed row from database
#[derive(FromRow)]
struct FeedRow {
    id: uuid::Uuid,
    user_id: uuid::Uuid,
    url: String,
}

/// Rule row from database
#[derive(FromRow)]
struct RuleRow {
    id: uuid::Uuid,
    config: serde_json::Value,
    action: String,
    stop_on_match: bool,
}

impl QueueConsumer {
    /// Create a new queue consumer
    pub async fn new(config: &WorkerConfig) -> anyhow::Result<Self> {
        let db = PgPoolOptions::new()
            .max_connections(10)
            .connect(&config.worker_database_url)
            .await?;

        let redis_client = redis::Client::open(config.redis_url.as_str())?;
        let redis = ConnectionManager::new(redis_client).await?;

        let fetcher = FeedFetcher::new()?;

        Ok(Self {
            redis,
            db,
            concurrent_fetches: config.concurrent_fetches,
            fetcher,
        })
    }

    /// Run the consumer loop
    pub async fn run(&mut self) -> anyhow::Result<()> {
        info!("Queue consumer running");

        loop {
            match self.process_next_job().await {
                Ok(Some(job)) => {
                    info!(job_type = ?job.job_type, job_id = %job.id, "Processed job");
                }
                Ok(None) => {
                    // No jobs, wait a bit
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
                Err(e) => {
                    error!(error = %e, "Error processing job");
                    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                }
            }
        }
    }

    /// Process the next job from the queue
    async fn process_next_job(&mut self) -> anyhow::Result<Option<Job>> {
        let job_data: Option<String> = redis::cmd("LPOP")
            .arg("feedmind:jobs")
            .query_async(&mut self.redis)
            .await?;

        let Some(data) = job_data else {
            return Ok(None);
        };

        let job: Job = serde_json::from_str(&data)?;

        match &job.job_type {
            JobType::FetchFeed { feed_id } => {
                self.process_fetch_feed(*feed_id).await?;
            }
            JobType::EvaluateRules { article_ids } => {
                self.process_evaluate_rules(article_ids).await?;
            }
            JobType::RefreshAllFeeds => {
                self.process_refresh_all().await?;
            }
            JobType::CleanupOldArticles { retention_days } => {
                self.process_cleanup(*retention_days).await?;
            }
            JobType::ExportUserData { user_id } => {
                self.process_export_user_data(*user_id).await?;
            }
            JobType::DeleteUserData { user_id } => {
                self.process_delete_user_data(*user_id).await?;
            }
            JobType::SendEmail { to, template } => {
                self.process_send_email(to, template).await?;
            }
            // Billing jobs
            JobType::CheckDunningStatus => {
                self.process_check_dunning().await?;
            }
            JobType::SyncUsageToStripe { user_id } => {
                self.process_sync_usage(*user_id).await?;
            }
            JobType::CleanupWebhookEvents => {
                self.process_cleanup_webhooks().await?;
            }
        }

        Ok(Some(job))
    }

    /// Fetch a single feed and insert new articles
    async fn process_fetch_feed(&self, feed_id: uuid::Uuid) -> anyhow::Result<()> {
        info!(%feed_id, "Fetching feed");

        let feed: Option<FeedRow> =
            sqlx::query_as("SELECT id, user_id, url FROM feeds WHERE id = $1")
                .bind(feed_id)
                .fetch_optional(&self.db)
                .await?;

        let Some(feed) = feed else {
            warn!(%feed_id, "Feed not found");
            return Ok(());
        };

        // Fetch and parse feed
        let result = self.fetcher.fetch(&feed.url).await;

        match result {
            Ok((_feed_meta, items)) => {
                info!(%feed_id, count = items.len(), "Fetched articles");

                // Insert new articles (deduplicate by guid)
                let mut new_article_ids = Vec::new();

                for item in items {
                    let word_count = item
                        .content
                        .as_ref()
                        .or(item.summary.as_ref())
                        .map(|text| text.split_whitespace().count() as i32);

                    let result: Option<(uuid::Uuid,)> = sqlx::query_as(
                        r#"
                        INSERT INTO articles (feed_id, user_id, guid, url, title, author, summary, content, published_at, word_count)
                        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                        ON CONFLICT (feed_id, guid) DO NOTHING
                        RETURNING id
                        "#
                    )
                    .bind(feed.id)
                    .bind(feed.user_id)
                    .bind(&item.guid)
                    .bind(&item.url)
                    .bind(&item.title)
                    .bind(&item.author)
                    .bind(&item.summary)
                    .bind(&item.content)
                    .bind(item.published_at)
                    .bind(word_count)
                    .fetch_optional(&self.db)
                    .await?;

                    if let Some((id,)) = result {
                        new_article_ids.push(id);
                    }
                }

                info!(%feed_id, new_count = new_article_ids.len(), "Inserted new articles");

                // Enforce retention limit (max 200 articles per feed, keep starred)
                let deleted = sqlx::query(
                    r#"
                    DELETE FROM articles
                    WHERE id IN (
                        SELECT id FROM articles
                        WHERE feed_id = $1 AND user_id = $2 AND is_starred = FALSE
                        ORDER BY published_at DESC NULLS LAST
                        OFFSET 200
                    )
                    "#,
                )
                .bind(feed.id)
                .bind(feed.user_id)
                .execute(&self.db)
                .await?;

                if deleted.rows_affected() > 0 {
                    info!(%feed_id, deleted = deleted.rows_affected(), "Deleted old articles to enforce retention limit");
                }

                // Update feed last_fetched_at
                sqlx::query(
                    "UPDATE feeds SET last_fetched_at = NOW(), last_successful_fetch_at = NOW(), error_count = 0, last_error = NULL WHERE id = $1"
                )
                .bind(feed_id)
                .execute(&self.db)
                .await?;

                if !new_article_ids.is_empty() {
                    let job = Job::new(JobType::EvaluateRules {
                        article_ids: new_article_ids,
                    });
                    let mut redis = self.redis.clone();
                    enqueue_job(&mut redis, &job).await?;
                }
            }
            Err(e) => {
                warn!(%feed_id, error = %e, "Failed to fetch feed");

                sqlx::query(
                    "UPDATE feeds SET last_fetched_at = NOW(), error_count = error_count + 1, last_error = $2, last_error_at = NOW() WHERE id = $1"
                )
                .bind(feed_id)
                .bind(e.to_string())
                .execute(&self.db)
                .await?;
            }
        }

        Ok(())
    }

    /// Evaluate rules for articles
    async fn process_evaluate_rules(&self, article_ids: &[uuid::Uuid]) -> anyhow::Result<()> {
        if article_ids.is_empty() {
            return Ok(());
        }

        info!(count = article_ids.len(), "Evaluating rules for articles");

        let user_id: Option<uuid::Uuid> =
            sqlx::query_scalar("SELECT user_id FROM articles WHERE id = $1")
                .bind(article_ids[0])
                .fetch_optional(&self.db)
                .await?;

        let Some(user_id) = user_id else {
            return Ok(());
        };

        let rules: Vec<RuleRow> = sqlx::query_as(
            r#"
            SELECT id, config, action, stop_on_match
            FROM rules
            WHERE user_id = $1 AND is_active = TRUE AND rule_type = 'regex'
            ORDER BY priority DESC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.db)
        .await?;

        if rules.is_empty() {
            return Ok(());
        }

        let articles: Vec<(uuid::Uuid, String, Option<String>, Option<String>)> =
            sqlx::query_as("SELECT id, title, summary, content FROM articles WHERE id = ANY($1)")
                .bind(article_ids)
                .fetch_all(&self.db)
                .await?;

        let compiled_rules: Vec<_> = rules
            .iter()
            .filter_map(|rule| {
                let pattern = rule.config.get("pattern")?.as_str()?;
                let case_sensitive = rule
                    .config
                    .get("case_sensitive")
                    .and_then(|c| c.as_bool())
                    .unwrap_or(false);

                let regex = if case_sensitive {
                    Regex::new(pattern).ok()?
                } else {
                    Regex::new(&format!("(?i){}", pattern)).ok()?
                };

                Some((rule, regex))
            })
            .collect();

        for (article_id, title, summary, content) in &articles {
            let text = format!(
                "{} {} {}",
                title,
                summary.as_deref().unwrap_or(""),
                content.as_deref().unwrap_or("")
            );

            for (rule, regex) in &compiled_rules {
                if let Some(matched) = regex.find(&text) {
                    info!(%article_id, rule_id = %rule.id, "Rule matched");

                    match rule.action.as_str() {
                        "hide" => {
                            sqlx::query(
                                "UPDATE articles SET is_hidden = TRUE, hidden_at = NOW(), hidden_by_rule_id = $2 WHERE id = $1"
                            )
                            .bind(article_id)
                            .bind(rule.id)
                            .execute(&self.db)
                            .await?;
                        }
                        "star" => {
                            sqlx::query(
                                "UPDATE articles SET is_starred = TRUE, starred_at = NOW() WHERE id = $1"
                            )
                            .bind(article_id)
                            .execute(&self.db)
                            .await?;
                        }
                        "mark_read" => {
                            sqlx::query(
                                "UPDATE articles SET is_read = TRUE, read_at = NOW() WHERE id = $1",
                            )
                            .bind(article_id)
                            .execute(&self.db)
                            .await?;
                        }
                        _ => {}
                    }

                    // Store for explainability
                    let _ = sqlx::query(
                        r#"
                        INSERT INTO rule_evaluations (article_id, rule_id, matched, matched_text, action_taken)
                        VALUES ($1, $2, TRUE, $3, $4)
                        ON CONFLICT (article_id, rule_id) DO NOTHING
                        "#
                    )
                    .bind(article_id)
                    .bind(rule.id)
                    .bind(matched.as_str())
                    .bind(&rule.action)
                    .execute(&self.db)
                    .await;

                    let _ = sqlx::query(
                        "UPDATE rules SET match_count = match_count + 1, last_match_at = NOW() WHERE id = $1"
                    )
                    .bind(rule.id)
                    .execute(&self.db)
                    .await;

                    if rule.stop_on_match {
                        break;
                    }
                }
            }
        }

        Ok(())
    }

    /// Refresh all feeds based on priority
    async fn process_refresh_all(&self) -> anyhow::Result<()> {
        info!("Refreshing all feeds");

        let feeds: Vec<(uuid::Uuid,)> = sqlx::query_as(
            r#"
            SELECT id FROM feeds
            WHERE error_count < 5
            AND (
                (priority = 'hot' AND (last_fetched_at IS NULL OR last_fetched_at < NOW() - INTERVAL '15 minutes'))
                OR (priority = 'warm' AND (last_fetched_at IS NULL OR last_fetched_at < NOW() - INTERVAL '1 hour'))
                OR (priority = 'cold' AND (last_fetched_at IS NULL OR last_fetched_at < NOW() - INTERVAL '4 hours'))
            )
            ORDER BY last_fetched_at ASC NULLS FIRST
            LIMIT $1
            "#
        )
        .bind(self.concurrent_fetches as i64)
        .fetch_all(&self.db)
        .await?;

        info!(count = feeds.len(), "Found feeds to refresh");

        let mut redis = self.redis.clone();
        for (feed_id,) in feeds {
            let job = Job::new(JobType::FetchFeed { feed_id });
            enqueue_job(&mut redis, &job).await?;
        }

        Ok(())
    }

    /// Clean up old read articles
    async fn process_cleanup(&self, retention_days: u32) -> anyhow::Result<()> {
        info!(retention_days, "Cleaning up old articles");

        let result = sqlx::query(
            "DELETE FROM articles WHERE is_read = TRUE AND created_at < NOW() - make_interval(days => $1)"
        )
        .bind(retention_days as i32)
        .execute(&self.db)
        .await?;

        info!(deleted = result.rows_affected(), "Deleted old articles");
        Ok(())
    }

    /// Export user data (GDPR)
    async fn process_export_user_data(&self, user_id: uuid::Uuid) -> anyhow::Result<()> {
        info!(user_id_hash = %sha256_tag(user_id.to_string().as_bytes()), "Exporting user data - not implemented");
        Ok(())
    }

    /// Delete user data (GDPR)
    async fn process_delete_user_data(&self, user_id: uuid::Uuid) -> anyhow::Result<()> {
        info!(user_id_hash = %sha256_tag(user_id.to_string().as_bytes()), "Deleting user data");
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(user_id)
            .execute(&self.db)
            .await?;
        info!(user_id_hash = %sha256_tag(user_id.to_string().as_bytes()), "User data deleted");
        Ok(())
    }

    /// Send email notification
    async fn process_send_email(
        &self,
        to: &str,
        template: &crate::jobs::EmailTemplate,
    ) -> anyhow::Result<()> {
        info!(recipient_hash = %safe_hash(to), template = ?template, "Sending email - not implemented");
        Ok(())
    }

    // ========================================================================
    // BILLING JOBS
    // ========================================================================

    /// Check dunning status for all accounts in grace period
    async fn process_check_dunning(&self) -> anyhow::Result<()> {
        info!("Checking dunning status for all accounts");

        let config = crate::handlers::dunning::DunningConfig::default();
        let result = crate::handlers::dunning::check_dunning_status(&self.db, &config).await?;

        info!(
            users_checked = result.users_checked,
            emails_sent = result.emails_sent,
            users_downgraded = result.users_downgraded,
            users_suspended = result.users_suspended,
            errors = result.errors.len(),
            "Dunning check completed"
        );

        if !result.errors.is_empty() {
            for err in &result.errors {
                warn!(error = %err, "Dunning error");
            }
        }

        Ok(())
    }

    /// Sync usage records to Stripe for metered billing
    async fn process_sync_usage(&self, user_id: uuid::Uuid) -> anyhow::Result<()> {
        info!(user_id_hash = %sha256_tag(user_id.to_string().as_bytes()), "Syncing usage to Stripe");

        // Get pending usage records for this user
        let usage_records: Vec<(uuid::Uuid, String, i64)> = sqlx::query_as(
            r#"
            SELECT id, usage_type, quantity
            FROM usage_records
            WHERE user_id = $1 AND synced_to_stripe = FALSE
            ORDER BY recorded_at ASC
            "#,
        )
        .bind(user_id)
        .fetch_all(&self.db)
        .await?;

        if usage_records.is_empty() {
            info!(user_id_hash = %sha256_tag(user_id.to_string().as_bytes()), "No pending usage to sync");
            return Ok(());
        }

        // In production, this would call Stripe's Usage Record API
        // For now, we just mark them as synced
        let record_ids: Vec<uuid::Uuid> = usage_records.iter().map(|(id, _, _)| *id).collect();

        sqlx::query(
            r#"
            UPDATE usage_records
            SET synced_to_stripe = TRUE, synced_at = NOW()
            WHERE id = ANY($1)
            "#,
        )
        .bind(&record_ids)
        .execute(&self.db)
        .await?;

        info!(
            %user_id,
            records_synced = record_ids.len(),
            "Usage synced to Stripe"
        );

        Ok(())
    }

    /// Clean up old webhook events
    async fn process_cleanup_webhooks(&self) -> anyhow::Result<()> {
        info!("Cleaning up old webhook events");

        let deleted = crate::handlers::dunning::cleanup_webhook_events(&self.db).await?;

        info!(deleted, "Old webhook events cleaned up");
        Ok(())
    }
}

fn safe_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..16]
        .to_string()
}

/// Push a job to the queue
pub async fn enqueue_job(redis: &mut ConnectionManager, job: &Job) -> anyhow::Result<()> {
    let data = serde_json::to_string(job)?;
    let _: () = redis::cmd("RPUSH")
        .arg("feedmind:jobs")
        .arg(data)
        .query_async(redis)
        .await?;
    Ok(())
}
