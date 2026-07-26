use anyhow::{Context, Result};
use chrono::SecondsFormat;
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::info;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

use feedmind_domain::article::Article;
use feedmind_domain::feed::Feed;
use feedmind_domain::rules::{Rule, RuleAction};
use feedmind_domain::DecisionOutcome;
use feedmind_ingest::{FeedFetcher, FetcherConfig};
use feedmind_opml::{OpmlDocument, OpmlExporter, OpmlOutline, OpmlParser};
use feedmind_rules::RuleEvaluator;
use feedmind_sync::curated::{
    CuratedArtifactRef, CuratedExportConstraints, CuratedExportCuration, CuratedExportItem,
    CuratedItemExport, CuratedProvenanceRef, CuratedRuleEvidence, CuratedSourceRef,
    CURATED_ITEM_EXPORT_FORMAT, CURATED_ITEM_EXPORT_ORIGIN, CURATED_ITEM_EXPORT_PURPOSE,
};
use feedmind_sync::local::{BoundedSyncLimits, LocalSyncSnapshot};

#[derive(Parser)]
#[command(name = "feedmind-cli")]
#[command(about = "FeedMind CLI - Import/Export and management tools")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Import feeds from an OPML file
    Import {
        /// Path to the OPML file
        #[arg(short, long)]
        file: PathBuf,

        /// User email to import feeds for (creates user if not exists)
        #[arg(short, long)]
        email: String,

        /// User password used to authenticate or create the account
        #[arg(short, long)]
        password: String,
    },

    /// Export feeds to an OPML file
    Export {
        /// User email to export feeds for
        #[arg(short, long)]
        email: String,

        /// User password used to authenticate the tenant
        #[arg(short, long)]
        password: String,

        /// Output file path
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Create a new user
    CreateUser {
        /// User email
        #[arg(short, long)]
        email: String,

        /// User password
        #[arg(short, long)]
        password: String,

        /// Subscription tier (free, pro_trial, pro, team)
        #[arg(short, long, default_value = "free")]
        tier: String,
    },

    /// Apply database migrations using the dedicated migration/owner principal
    Migrate,

    /// Show database statistics using the explicit operator principal
    Stats,

    /// Parse an OPML file and print a JSON summary without requiring a database
    OpmlSummary {
        /// Path to the OPML file
        #[arg(short, long)]
        file: PathBuf,
    },

    /// Fetch a feed and print a JSON summary without storing it
    FetchFeed {
        /// Feed URL to fetch
        #[arg(short, long)]
        url: String,
    },

    /// Evaluate one article JSON against one rule JSON without requiring a database
    EvaluateRule {
        /// Path to an Article JSON file
        #[arg(short, long)]
        article: PathBuf,

        /// Path to a Rule JSON file
        #[arg(short, long)]
        rule: PathBuf,
    },

    /// Build a deterministic CuratedItemExport demo without requiring a database or network
    DemoCurate {
        /// Path to the OPML subscriptions file used as source context
        #[arg(long)]
        opml: PathBuf,

        /// Path to an Article JSON file
        #[arg(long)]
        article: PathBuf,

        /// Path to a Rule JSON file
        #[arg(long)]
        rule: PathBuf,

        /// Output CuratedItemExport JSON path
        #[arg(short, long)]
        output: PathBuf,

        /// Actor reference to write in the export metadata
        #[arg(long, default_value = "actor-demo-local")]
        actor: String,
    },

    /// Fetch one live feed and export the first matching item; networked and not used by CI
    DemoCurateLive {
        /// RSS/Atom/JSON Feed URL to fetch
        #[arg(long)]
        feed_url: String,

        /// Path to a Rule JSON file
        #[arg(long)]
        rule: PathBuf,

        /// Output CuratedItemExport JSON path
        #[arg(short, long)]
        output: PathBuf,

        /// Actor reference to write in the export metadata
        #[arg(long, default_value = "actor-demo-live")]
        actor: String,

        /// Maximum fetched items to inspect before giving up
        #[arg(long, default_value_t = 20)]
        max_items: usize,
    },

    /// Import a bounded OPML source set, fetch it and export the first new explained signal
    SyncCurated {
        /// OPML subscription file; capped before any network request
        #[arg(long)]
        opml: PathBuf,

        /// Explicit rule used to qualify fetched items
        #[arg(long)]
        rule: PathBuf,

        /// Client-safe CuratedItemExport output path
        #[arg(short, long)]
        output: PathBuf,

        /// Payload-minimized replay state path
        #[arg(long)]
        state: PathBuf,

        /// Exact public host allowed for fetch and redirects; repeat for each host
        #[arg(long = "allow-host", required = true)]
        allowed_hosts: Vec<String>,

        /// Actor reference written to provenance; no email or display name
        #[arg(long, default_value = "actor-radar-local-sync")]
        actor: String,

        #[arg(long, default_value_t = 8)]
        max_sources: usize,

        #[arg(long, default_value_t = 50)]
        max_items_per_source: usize,

        #[arg(long, default_value_t = 200)]
        max_total_items: usize,
    },

    /// Validate the local CuratedItemExport contract invariants without requiring network access
    ValidateCuratedExport {
        /// Path to a CuratedItemExport JSON file
        #[arg(short, long)]
        file: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("feedmind_cli=info".parse()?))
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::OpmlSummary { file } => {
            opml_summary(&file)?;
        }
        Commands::FetchFeed { url } => {
            fetch_feed(&url).await?;
        }
        Commands::EvaluateRule { article, rule } => {
            evaluate_rule(&article, &rule)?;
        }
        Commands::DemoCurate {
            opml,
            article,
            rule,
            output,
            actor,
        } => {
            demo_curate(&opml, &article, &rule, &output, &actor)?;
        }
        Commands::DemoCurateLive {
            feed_url,
            rule,
            output,
            actor,
            max_items,
        } => {
            demo_curate_live(&feed_url, &rule, &output, &actor, max_items).await?;
        }
        Commands::SyncCurated {
            opml,
            rule,
            output,
            state,
            allowed_hosts,
            actor,
            max_sources,
            max_items_per_source,
            max_total_items,
        } => {
            let limits = BoundedSyncLimits::new(max_sources, max_items_per_source, max_total_items)
                .context("Invalid bounded sync limits")?;
            sync_curated(&opml, &rule, &output, &state, allowed_hosts, &actor, limits).await?;
        }
        Commands::ValidateCuratedExport { file } => {
            validate_curated_export(&file)?;
        }
        command => match command {
            Commands::Import {
                file,
                email,
                password,
            } => {
                let app_pool = connect_pool("DATABASE_URL").await?;
                let auth_pool = connect_pool("AUTH_DATABASE_URL").await?;
                import_opml(&app_pool, &auth_pool, &file, &email, &password).await?;
            }
            Commands::Export {
                email,
                password,
                output,
            } => {
                let app_pool = connect_pool("DATABASE_URL").await?;
                let auth_pool = connect_pool("AUTH_DATABASE_URL").await?;
                export_opml(&app_pool, &auth_pool, &email, &password, &output).await?;
            }
            Commands::CreateUser {
                email,
                password,
                tier,
            } => {
                let owner_pool = connect_pool("MIGRATION_DATABASE_URL").await?;
                create_user(&owner_pool, &email, &password, &tier).await?;
            }
            Commands::Migrate => {
                let owner_pool = connect_pool("MIGRATION_DATABASE_URL").await?;
                sqlx::migrate!("../../migrations")
                    .run(&owner_pool)
                    .await
                    .context("Failed to apply database migrations")?;
            }
            Commands::Stats => {
                let owner_pool = connect_pool("MIGRATION_DATABASE_URL").await?;
                show_stats(&owner_pool).await?;
            }
            Commands::OpmlSummary { .. }
            | Commands::FetchFeed { .. }
            | Commands::EvaluateRule { .. }
            | Commands::DemoCurate { .. }
            | Commands::DemoCurateLive { .. }
            | Commands::SyncCurated { .. }
            | Commands::ValidateCuratedExport { .. } => unreachable!("handled before DB setup"),
        },
    }

    Ok(())
}

#[derive(Serialize)]
struct OpmlSummary {
    title: Option<String>,
    feed_count: usize,
    folder_count: usize,
    feeds: Vec<OpmlSummaryFeed>,
}

#[derive(Serialize)]
struct OpmlSummaryFeed {
    title: String,
    xml_url: String,
    html_url: Option<String>,
    folder: Option<String>,
}

#[derive(Serialize)]
struct FetchFeedSummary {
    feed: FetchFeedMeta,
    item_count: usize,
    items: Vec<FetchFeedItem>,
}

#[derive(Serialize)]
struct FetchFeedMeta {
    url: String,
    title: String,
    feed_type: String,
    site_url: Option<String>,
}

#[derive(Serialize)]
struct FetchFeedItem {
    guid: String,
    title: String,
    url: Option<String>,
    published_at: Option<String>,
}

#[derive(Serialize)]
struct EvaluateRuleSummary {
    matched: bool,
    action: Option<RuleAction>,
    deciding_rule: Option<String>,
    decisions: Vec<EvaluateRuleDecision>,
}

#[derive(Serialize)]
struct EvaluateRuleDecision {
    rule_id: Uuid,
    outcome: String,
    actions: Vec<RuleAction>,
    confidence: f32,
    explanation: String,
    evidence: Vec<EvaluateRuleEvidence>,
}

#[derive(Serialize)]
struct EvaluateRuleEvidence {
    field: String,
    excerpt: String,
    pattern: Option<String>,
}

#[derive(Clone, Deserialize)]
struct RuleInput {
    user_id: Option<Uuid>,
    name: String,
    pattern: String,
    action: RuleAction,
    feed_id: Option<Uuid>,
    priority: Option<i32>,
    stop_on_match: Option<bool>,
}

/// Flattened feed info for import
#[derive(Clone)]
struct FlatFeed {
    title: String,
    xml_url: String,
    html_url: Option<String>,
    folder: Option<String>,
}

struct LiveSelection {
    feed: Feed,
    article: Article,
}

struct SyncSelection {
    source: FlatFeed,
    article: Article,
}

#[derive(Serialize)]
struct SyncCuratedSummary {
    status: &'static str,
    sources_imported: usize,
    sources_fetched: usize,
    items_inspected: usize,
    previously_seen_items: usize,
    export_id: Option<String>,
    output_written: bool,
    state_format: &'static str,
}

fn opml_summary(file: &PathBuf) -> Result<()> {
    let content = std::fs::read_to_string(file).context("Failed to read OPML file")?;
    let doc = OpmlParser::parse(&content).context("Failed to parse OPML file")?;
    let feeds = flatten_outlines(&doc.outlines, None);

    let summary = OpmlSummary {
        title: doc.title.clone(),
        feed_count: feeds.len(),
        folder_count: doc.folder_count(),
        feeds: feeds
            .into_iter()
            .map(|feed| OpmlSummaryFeed {
                title: feed.title,
                xml_url: feed.xml_url,
                html_url: feed.html_url,
                folder: feed.folder,
            })
            .collect(),
    };

    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

async fn fetch_feed(url: &str) -> Result<()> {
    let fetcher = FeedFetcher::new().context("Failed to create feed fetcher")?;
    let (feed, items) = fetcher
        .fetch(url)
        .await
        .with_context(|| format!("Failed to fetch feed: {url}"))?;

    let summary = FetchFeedSummary {
        feed: FetchFeedMeta {
            url: feed.url,
            title: feed.title,
            feed_type: feed.feed_type.to_string(),
            site_url: feed.site_url,
        },
        item_count: items.len(),
        items: items
            .into_iter()
            .take(20)
            .map(|item| FetchFeedItem {
                guid: item.guid,
                title: item.title,
                url: item.url,
                published_at: item.published_at.map(|date| date.to_rfc3339()),
            })
            .collect(),
    };

    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn evaluate_rule(article_file: &PathBuf, rule_file: &PathBuf) -> Result<()> {
    let article = load_article(article_file)?;
    let input = load_rule_input(rule_file)?;
    let rule = build_rule(&input);

    let evaluator = RuleEvaluator::new(vec![rule]).context("Failed to compile rule")?;
    let result = evaluator.evaluate(&article, article.feed_id);

    let summary = EvaluateRuleSummary {
        matched: result.action.is_some(),
        action: result.action,
        deciding_rule: result.deciding_rule,
        decisions: result
            .decisions
            .into_iter()
            .map(|decision| EvaluateRuleDecision {
                rule_id: decision.rule_id,
                outcome: format!("{:?}", decision.outcome),
                actions: decision.actions,
                confidence: decision.confidence,
                explanation: decision.explanation,
                evidence: decision
                    .evidence
                    .into_iter()
                    .map(|evidence| EvaluateRuleEvidence {
                        field: evidence.field,
                        excerpt: evidence.excerpt,
                        pattern: evidence.pattern,
                    })
                    .collect(),
            })
            .collect(),
    };

    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn demo_curate(
    opml_file: &PathBuf,
    article_file: &PathBuf,
    rule_file: &PathBuf,
    output: &Path,
    actor: &str,
) -> Result<()> {
    validate_actor(actor)?;
    let opml_content = std::fs::read_to_string(opml_file).context("Failed to read OPML file")?;
    let opml = OpmlParser::parse(&opml_content).context("Failed to parse OPML file")?;
    let feeds = flatten_outlines(&opml.outlines, None);
    anyhow::ensure!(
        !feeds.is_empty(),
        "OPML file must contain at least one feed for demo-curate"
    );

    let article = load_article(article_file)?;
    let input = load_rule_input(rule_file)?;
    let export = build_curated_export(&opml, &feeds, &article, &input, actor)?;
    write_curated_export(output, &export)?;

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "export_id": export.export_id,
            "output": output.display().to_string(),
            "matched": export.rule_evidence.iter().any(|evidence| evidence.decision == "match")
        }))?
    );
    Ok(())
}

async fn demo_curate_live(
    feed_url: &str,
    rule_file: &PathBuf,
    output: &Path,
    actor: &str,
    max_items: usize,
) -> Result<()> {
    validate_actor(actor)?;
    anyhow::ensure!(
        (1..=100).contains(&max_items),
        "--max-items must be between 1 and 100"
    );
    let input = load_rule_input(rule_file)?;
    let selection = select_live_article(feed_url, &input, max_items).await?;
    let opml = OpmlDocument {
        title: Some("FeedMind live demo subscriptions".to_string()),
        date_created: None,
        owner_email: None,
        outlines: vec![OpmlOutline::feed(
            selection.feed.title.clone(),
            selection.feed.url.clone(),
            selection.feed.site_url.clone(),
        )],
    };
    let feeds = vec![FlatFeed {
        title: selection.feed.title.clone(),
        xml_url: selection.feed.url.clone(),
        html_url: selection.feed.site_url.clone(),
        folder: None,
    }];
    let export = build_curated_export(&opml, &feeds, &selection.article, &input, actor)?;
    write_curated_export(output, &export)?;

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "export_id": export.export_id,
            "feed": selection.feed.title,
            "article": selection.article.title,
            "output": output.display().to_string(),
            "matched": export.rule_evidence.iter().any(|evidence| evidence.decision == "match")
        }))?
    );
    Ok(())
}

async fn sync_curated(
    opml_file: &Path,
    rule_file: &Path,
    output: &Path,
    state_file: &Path,
    allowed_hosts: Vec<String>,
    actor: &str,
    limits: BoundedSyncLimits,
) -> Result<()> {
    validate_actor(actor)?;
    anyhow::ensure!(
        !allowed_hosts.is_empty() && allowed_hosts.len() <= 64,
        "--allow-host must contain between 1 and 64 exact hosts"
    );

    let opml_content = read_bounded_text(opml_file, 512 * 1024, "OPML")?;
    let opml = OpmlParser::parse(&opml_content).context("Failed to parse OPML file")?;
    let feeds = flatten_outlines(&opml.outlines, None);
    anyhow::ensure!(
        !feeds.is_empty(),
        "OPML file must contain at least one feed"
    );
    anyhow::ensure!(
        feeds.len() <= limits.max_sources,
        "OPML source count exceeds --max-sources"
    );

    let rule_content = read_bounded_text(rule_file, 64 * 1024, "rule")?;
    let rule_input: RuleInput = serde_json::from_str(&rule_content).context("Invalid rule JSON")?;
    validate_rule_input(&rule_input)?;
    let evaluator =
        RuleEvaluator::new(vec![build_rule(&rule_input)]).context("Failed to compile rule")?;
    let fetcher = FeedFetcher::with_config(FetcherConfig::for_public_sources(allowed_hosts))
        .context("Failed to create bounded public feed fetcher")?;
    let mut state = load_sync_state(state_file)?;

    let source_hashes = feeds
        .iter()
        .map(|feed| sha256_tag(feed.xml_url.as_bytes()))
        .collect::<Vec<_>>();
    let mut inspected_hashes = Vec::new();
    let mut selection = None;
    let mut sources_fetched = 0;
    let mut items_inspected = 0;
    let mut previously_seen_items = 0;

    'sources: for source in &feeds {
        if items_inspected >= limits.max_total_items {
            break;
        }
        let (feed, items) = fetcher.fetch(&source.xml_url).await.with_context(|| {
            format!(
                "Failed to synchronize source {}",
                sha256_tag(source.xml_url.as_bytes())
            )
        })?;
        sources_fetched += 1;

        for item in items.into_iter().take(limits.max_items_per_source) {
            if items_inspected >= limits.max_total_items {
                break 'sources;
            }
            items_inspected += 1;
            let item_hash = sha256_tag(item.guid.as_bytes());
            inspected_hashes.push(item_hash.clone());
            if state.has_seen(&item_hash) {
                previously_seen_items += 1;
                continue;
            }

            let article = Article::from_feed_item(feed.id, item);
            if selection.is_none()
                && evaluator
                    .evaluate(&article, article.feed_id)
                    .action
                    .is_some()
            {
                selection = Some(SyncSelection {
                    source: source.clone(),
                    article,
                });
            }
        }
    }

    let (export_id, selected_export_hash) = if let Some(selection) = selection {
        let mut source_order = feeds.clone();
        if let Some(index) = source_order
            .iter()
            .position(|source| source.xml_url == selection.source.xml_url)
        {
            source_order.swap(0, index);
        }
        let export =
            build_curated_export(&opml, &source_order, &selection.article, &rule_input, actor)?;
        let serialized = serialize_curated_export(&export)?;
        let export_hash = sha256_tag(serialized.as_bytes());
        atomic_write(output, serialized.as_bytes())
            .context("Failed to write synchronized export")?;
        (Some(export.export_id), Some(export_hash))
    } else {
        if output.exists() {
            std::fs::remove_file(output).context("Failed to remove stale synchronized export")?;
        }
        (None, None)
    };

    state
        .advance(source_hashes, inspected_hashes, selected_export_hash)
        .context("Refusing unsafe local sync state")?;
    write_sync_state(state_file, &state)?;

    let summary = SyncCuratedSummary {
        status: if export_id.is_some() {
            "ready"
        } else {
            "empty"
        },
        sources_imported: feeds.len(),
        sources_fetched,
        items_inspected,
        previously_seen_items,
        output_written: export_id.is_some(),
        export_id,
        state_format: feedmind_sync::local::LOCAL_SYNC_SNAPSHOT_FORMAT,
    };
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

async fn select_live_article(
    feed_url: &str,
    input: &RuleInput,
    max_items: usize,
) -> Result<LiveSelection> {
    let fetcher = FeedFetcher::new().context("Failed to create feed fetcher")?;
    let (feed, items) = fetcher
        .fetch(feed_url)
        .await
        .with_context(|| format!("Failed to fetch feed: {feed_url}"))?;
    anyhow::ensure!(!items.is_empty(), "Fetched feed does not contain items");

    let evaluator =
        RuleEvaluator::new(vec![build_rule(input)]).context("Failed to compile rule")?;
    let fallback = items.first().cloned().expect("non-empty items checked");
    let selected = items
        .into_iter()
        .take(max_items)
        .map(|item| Article::from_feed_item(feed.id, item))
        .find(|article| {
            evaluator
                .evaluate(article, article.feed_id)
                .action
                .is_some()
        })
        .unwrap_or_else(|| Article::from_feed_item(feed.id, fallback));

    Ok(LiveSelection {
        feed,
        article: selected,
    })
}

fn write_curated_export(output: &Path, export: &CuratedItemExport) -> Result<()> {
    let json = serialize_curated_export(export)?;
    atomic_write(output, json.as_bytes()).context("Failed to write CuratedItemExport JSON")
}

fn serialize_curated_export(export: &CuratedItemExport) -> Result<String> {
    export
        .validate_client_safe()
        .context("Refusing to write a CuratedItemExport that is not client-safe")?;
    Ok(format!("{}\n", serde_json::to_string_pretty(export)?))
}

fn load_sync_state(path: &Path) -> Result<LocalSyncSnapshot> {
    if !path.exists() {
        return Ok(LocalSyncSnapshot::empty());
    }
    let raw = read_bounded_text(path, 1024 * 1024, "sync state")?;
    let state: LocalSyncSnapshot =
        serde_json::from_str(&raw).context("Invalid local sync state JSON")?;
    state
        .validate()
        .context("Local sync state failed validation")?;
    Ok(state)
}

fn write_sync_state(path: &Path, state: &LocalSyncSnapshot) -> Result<()> {
    state
        .validate()
        .context("Refusing unsafe local sync state")?;
    let serialized = format!("{}\n", serde_json::to_string_pretty(state)?);
    atomic_write(path, serialized.as_bytes()).context("Failed to write local sync state")
}

fn read_bounded_text(path: &Path, max_bytes: usize, label: &str) -> Result<String> {
    let metadata = std::fs::metadata(path).with_context(|| format!("Failed to inspect {label}"))?;
    anyhow::ensure!(
        metadata.len() <= max_bytes as u64,
        "{label} exceeds the {max_bytes}-byte limit"
    );
    let bytes = std::fs::read(path).with_context(|| format!("Failed to read {label}"))?;
    anyhow::ensure!(
        bytes.len() <= max_bytes,
        "{label} exceeds the {max_bytes}-byte limit"
    );
    String::from_utf8(bytes).with_context(|| format!("{label} must be UTF-8"))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).context("Failed to create output directory")?;
        }
    }
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("Output path must have a UTF-8 file name")?;
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    std::fs::write(&temporary, bytes).context("Failed to write temporary output")?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error).context("Failed to atomically replace output");
    }
    Ok(())
}

fn validate_actor(actor: &str) -> Result<()> {
    anyhow::ensure!(
        !actor.is_empty()
            && actor.len() <= 64
            && actor
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "-_:".contains(character))
            && !actor.contains('@'),
        "actor must be a non-PII reference of at most 64 ASCII characters"
    );
    Ok(())
}

fn validate_curated_export(file: &PathBuf) -> Result<()> {
    let content = std::fs::read_to_string(file).context("Failed to read CuratedItemExport JSON")?;
    let export: CuratedItemExport =
        serde_json::from_str(&content).context("Invalid CuratedItemExport JSON")?;
    export
        .validate_client_safe()
        .context("CuratedItemExport is not client-safe")?;

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "file": file.display().to_string(),
            "valid": true,
            "format": CURATED_ITEM_EXPORT_FORMAT
        }))?
    );
    Ok(())
}

fn load_article(article_file: &PathBuf) -> Result<Article> {
    let article_content =
        std::fs::read_to_string(article_file).context("Failed to read article JSON")?;
    serde_json::from_str(&article_content).context("Invalid Article JSON")
}

fn load_rule_input(rule_file: &PathBuf) -> Result<RuleInput> {
    let rule_content = std::fs::read_to_string(rule_file).context("Failed to read rule JSON")?;
    let input = serde_json::from_str(&rule_content).context("Invalid rule JSON")?;
    validate_rule_input(&input)?;
    Ok(input)
}

fn validate_rule_input(input: &RuleInput) -> Result<()> {
    anyhow::ensure!(
        !input.name.trim().is_empty() && input.name.chars().count() <= 200,
        "rule name must contain between 1 and 200 characters"
    );
    anyhow::ensure!(
        !input.pattern.is_empty() && input.pattern.chars().count() <= 900,
        "rule pattern must contain between 1 and 900 characters"
    );
    Ok(())
}

fn build_rule(input: &RuleInput) -> Rule {
    let mut rule = Rule::new_regex(
        input.user_id.unwrap_or_else(Uuid::new_v4),
        input.name.clone(),
        input.pattern.clone(),
        input.action.clone(),
    );
    rule.feed_id = input.feed_id;
    rule.priority = input.priority.unwrap_or_default();
    rule.stop_on_match = input.stop_on_match.unwrap_or(false);
    rule
}

fn build_curated_export(
    opml: &OpmlDocument,
    feeds: &[FlatFeed],
    article: &Article,
    input: &RuleInput,
    actor: &str,
) -> Result<CuratedItemExport> {
    let evaluator =
        RuleEvaluator::new(vec![build_rule(input)]).context("Failed to compile rule")?;
    let result = evaluator.evaluate(article, article.feed_id);
    let first_decision = result.decisions.first();

    let content_material = format!(
        "{}\n{}\n{}",
        article.title,
        article.summary.as_deref().unwrap_or_default(),
        article.content.as_deref().unwrap_or_default()
    );
    let content_hash = sha256_tag(content_material.as_bytes());
    let source_url_hash = sha256_tag(article.url.as_deref().unwrap_or(&article.guid).as_bytes());
    let evidence_material = first_decision
        .map(|decision| {
            let evidence = decision
                .evidence
                .iter()
                .map(|item| {
                    format!(
                        "{}:{}:{}",
                        item.field,
                        item.excerpt,
                        item.pattern.as_deref().unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("|");
            format!("{}:{}:{evidence}", input.name, decision.explanation)
        })
        .unwrap_or_else(|| format!("{}:{}:not_evaluated", input.name, input.pattern));
    let evidence_hash = sha256_tag(evidence_material.as_bytes());
    // Feed GUIDs are often raw URLs. Keep public identifiers stable without
    // copying source URLs or private path material into the portable export.
    let item_reference = &sha256_hex(article.guid.as_bytes())[..24];
    let export_id = format!("export:{item_reference}");
    let provenance_id = format!("provenance:{export_id}");
    let artifact_hash = sha256_tag(
        format!("{export_id}:{content_hash}:{source_url_hash}:{evidence_hash}").as_bytes(),
    );
    let created_at = article
        .created_at
        .to_rfc3339_opts(SecondsFormat::Secs, true);
    let decision = first_decision
        .map(|decision| match decision.outcome {
            DecisionOutcome::Matched => "match",
            DecisionOutcome::NotMatched => "no_match",
            DecisionOutcome::Skipped => "not_evaluated",
        })
        .unwrap_or("not_evaluated");
    let confidence = first_decision
        .map(|decision| decision.confidence)
        .unwrap_or_default();
    let explanation = first_decision
        .map(|decision| decision.explanation.clone())
        .unwrap_or_else(|| "Rule was not evaluated".to_string());

    Ok(CuratedItemExport {
        format: CURATED_ITEM_EXPORT_FORMAT.to_string(),
        export_id: export_id.clone(),
        origin_product: CURATED_ITEM_EXPORT_ORIGIN.to_string(),
        workspace_id: "workspace:local-demo".to_string(),
        created_by: actor.to_string(),
        created_at: created_at.clone(),
        purpose: CURATED_ITEM_EXPORT_PURPOSE.to_string(),
        privacy_classification: "normal".to_string(),
        item: CuratedExportItem {
            item_id: article.id.to_string(),
            title: truncate_for_export(&article.title, 300),
            content_excerpt: content_excerpt(article),
            content_hash: content_hash.clone(),
            source_url_hash,
            published_at: article
                .published_at
                .map(|date| date.to_rfc3339_opts(SecondsFormat::Secs, true)),
            tags: article
                .categories
                .iter()
                .take(64)
                .map(|tag| truncate_for_export(tag, 64))
                .collect(),
        },
        source_ref: CuratedSourceRef {
            source_id: format!("source:{item_reference}"),
            source_type: "feed_item".to_string(),
            origin_product: CURATED_ITEM_EXPORT_ORIGIN.to_string(),
            content_hash,
            provenance_id: provenance_id.clone(),
            opml_title: opml
                .title
                .as_deref()
                .map(|title| truncate_for_export(title, 300)),
            opml_feed_count: feeds.len(),
            first_feed_title: feeds
                .first()
                .map(|feed| truncate_for_export(&feed.title, 300)),
        },
        curation: CuratedExportCuration {
            decision: if result.action.is_some() {
                "saved".to_string()
            } else {
                "rejected".to_string()
            },
            reason: result
                .deciding_rule
                .unwrap_or_else(|| "No rule matched".to_string()),
            curated_by: actor.to_string(),
            curated_at: created_at.clone(),
        },
        rule_evidence: vec![CuratedRuleEvidence {
            rule_id: stable_rule_id(input),
            decision: decision.to_string(),
            explanation,
            evidence_hash,
            confidence,
        }],
        constraints: CuratedExportConstraints {
            contains_raw_private_content: false,
            contains_secrets: false,
            contains_byok_material: false,
            allow_downstream_execution: false,
            data_residency: "EU/local-first".to_string(),
            retention_policy_ref: "retention:feedmind-local-demo".to_string(),
        },
        artifact_ref: CuratedArtifactRef {
            artifact_id: format!("artifact:{export_id}"),
            artifact_type: "curated_export".to_string(),
            hash: artifact_hash,
            manifest_ref: "manifest:feedmind-local-demo".to_string(),
        },
        provenance_ref: CuratedProvenanceRef {
            provenance_id,
            operation: "exported".to_string(),
            timestamp: created_at,
        },
    })
}

fn truncate_for_export(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn content_excerpt(article: &Article) -> String {
    let source = article
        .summary
        .as_deref()
        .or(article.content.as_deref())
        .unwrap_or(&article.title);
    source.chars().take(2_000).collect()
}

fn stable_rule_id(input: &RuleInput) -> String {
    format!(
        "rule:{}",
        &sha256_hex(format!("{}:{}", input.name, input.pattern).as_bytes())[..16]
    )
}

fn sha256_tag(bytes: &[u8]) -> String {
    format!("sha256:{}", sha256_hex(bytes))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Flatten OPML outlines to a list of feeds with folder info
fn flatten_outlines(outlines: &[OpmlOutline], parent_folder: Option<&str>) -> Vec<FlatFeed> {
    let mut feeds = Vec::new();

    for outline in outlines {
        if outline.is_feed() {
            if let Some(xml_url) = &outline.xml_url {
                feeds.push(FlatFeed {
                    title: outline
                        .title
                        .clone()
                        .unwrap_or_else(|| outline.text.clone()),
                    xml_url: xml_url.clone(),
                    html_url: outline.html_url.clone(),
                    folder: parent_folder.map(|s| s.to_string()),
                });
            }
        }

        // Recurse into children (folders)
        if !outline.children.is_empty() {
            let folder_name = if outline.xml_url.is_none() {
                Some(outline.text.as_str())
            } else {
                parent_folder
            };
            feeds.extend(flatten_outlines(&outline.children, folder_name));
        }
    }

    feeds
}

async fn connect_pool(variable: &str) -> Result<sqlx::PgPool> {
    let database_url =
        std::env::var(variable).with_context(|| format!("{variable} must be set"))?;
    PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .with_context(|| format!("Failed to connect using {variable}"))
}

async fn import_opml(
    pool: &sqlx::PgPool,
    auth_pool: &sqlx::PgPool,
    file: &PathBuf,
    email: &str,
    password: &str,
) -> Result<()> {
    info!(opml_path = ?file, email_hash = %sha256_tag(email.as_bytes()), "Importing OPML");

    let content = std::fs::read_to_string(file).context("Failed to read OPML file")?;

    let doc = OpmlParser::parse(&content).context("Failed to parse OPML file")?;

    let feeds = flatten_outlines(&doc.outlines, None);

    let folder_names: std::collections::HashSet<_> =
        feeds.iter().filter_map(|f| f.folder.as_ref()).collect();

    info!(
        "Parsed OPML: {} feeds in {} folders",
        feeds.len(),
        folder_names.len()
    );

    let user_id = get_or_create_user(auth_pool, email, password).await?;
    let mut tx = feedmind_storage::TenantTransaction::begin(pool, user_id)
        .await
        .context("Failed to establish tenant context")?;
    info!(user_id_hash = %sha256_tag(user_id.as_bytes()), "User context initialized");

    // Create folders and feeds
    let mut folder_map: HashMap<String, Uuid> = HashMap::new();
    let mut feeds_created = 0;
    let mut feeds_skipped = 0;

    for feed in &feeds {
        // Create folder if needed
        let folder_id = if let Some(folder_name) = &feed.folder {
            if let Some(id) = folder_map.get(folder_name) {
                Some(*id)
            } else {
                let folder_id = create_folder(tx.connection(), user_id, folder_name).await?;
                folder_map.insert(folder_name.clone(), folder_id);
                Some(folder_id)
            }
        } else {
            None
        };

        // Create feed (skip if already exists)
        match create_feed(
            tx.connection(),
            user_id,
            folder_id,
            &feed.title,
            &feed.xml_url,
            feed.html_url.as_deref(),
        )
        .await
        {
            Ok(_) => feeds_created += 1,
            Err(e) => {
                if e.to_string().contains("duplicate") || e.to_string().contains("unique") {
                    feeds_skipped += 1;
                } else {
                    tracing::warn!(feed_hash = %sha256_tag(feed.title.as_bytes()), error = %e, "Failed to create feed");
                    feeds_skipped += 1;
                }
            }
        }
    }

    tx.commit().await.context("Failed to commit OPML import")?;

    info!(
        "Import complete: {} feeds created, {} skipped (duplicates), {} folders created",
        feeds_created,
        feeds_skipped,
        folder_map.len()
    );

    Ok(())
}

async fn export_opml(
    pool: &sqlx::PgPool,
    auth_pool: &sqlx::PgPool,
    email: &str,
    password: &str,
    output: &PathBuf,
) -> Result<()> {
    info!(email_hash = %sha256_tag(email.as_bytes()), output = ?output, "Exporting OPML");

    // Get user ID
    let user_id = authenticate_user(auth_pool, email, password).await?;
    let mut tx = feedmind_storage::TenantTransaction::begin(pool, user_id)
        .await
        .context("Failed to establish tenant context")?;

    let folders: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, name FROM folders WHERE user_id = $1 ORDER BY name")
            .bind(user_id)
            .fetch_all(tx.connection())
            .await?;

    let feeds: Vec<(String, String, Option<String>, Option<Uuid>)> = sqlx::query_as(
        "SELECT title, url, site_url, folder_id FROM feeds WHERE user_id = $1 ORDER BY title",
    )
    .bind(user_id)
    .fetch_all(tx.connection())
    .await?;
    tx.commit()
        .await
        .context("Failed to close tenant context")?;

    let folder_map: HashMap<Uuid, String> = folders.into_iter().collect();

    // Group feeds by folder
    let mut folder_feeds: HashMap<Option<Uuid>, Vec<OpmlOutline>> = HashMap::new();

    for (title, xml_url, html_url, folder_id) in feeds.iter() {
        let outline = OpmlOutline::feed(title.clone(), xml_url.clone(), html_url.clone());
        folder_feeds.entry(*folder_id).or_default().push(outline);
    }

    let mut outlines = Vec::new();

    // Root-level feeds (no folder)
    if let Some(root_feeds) = folder_feeds.remove(&None) {
        outlines.extend(root_feeds);
    }

    for (folder_id, folder_name) in &folder_map {
        if let Some(feed_outlines) = folder_feeds.remove(&Some(*folder_id)) {
            let mut folder = OpmlOutline::folder(folder_name.clone());
            folder.children = feed_outlines;
            outlines.push(folder);
        }
    }

    let doc = OpmlDocument {
        title: Some(format!("FeedMind export for {}", email)),
        date_created: Some(chrono::Utc::now().to_rfc2822()),
        owner_email: Some(email.to_string()),
        outlines,
    };

    let xml = OpmlExporter::export(&doc);
    std::fs::write(output, xml)?;

    info!("Exported {} feeds to {:?}", feeds.len(), output);

    Ok(())
}

async fn create_user(pool: &sqlx::PgPool, email: &str, password: &str, tier: &str) -> Result<()> {
    use argon2::{
        password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
        Argon2,
    };

    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("Failed to hash password: {}", e))?
        .to_string();

    let user_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO users (email, password_hash, tier)
        VALUES ($1, $2, $3)
        RETURNING id
        "#,
    )
    .bind(email)
    .bind(&password_hash)
    .bind(tier)
    .fetch_one(pool)
    .await
    .context("Failed to create user - email may already exist")?;

    info!(email_hash = %sha256_tag(email.as_bytes()), user_id_hash = %sha256_tag(user_id.to_string().as_bytes()), "Created user");

    Ok(())
}

async fn show_stats(pool: &sqlx::PgPool) -> Result<()> {
    let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE deleted_at IS NULL")
        .fetch_one(pool)
        .await?;

    let feeds: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM feeds")
        .fetch_one(pool)
        .await?;

    let articles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM articles")
        .fetch_one(pool)
        .await?;

    let rules: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rules")
        .fetch_one(pool)
        .await?;

    println!("FeedMind Statistics:");
    println!("  Users:    {}", users);
    println!("  Feeds:    {}", feeds);
    println!("  Articles: {}", articles);
    println!("  Rules:    {}", rules);

    Ok(())
}

fn verify_password_hash(password: &str, password_hash: &str) -> Result<()> {
    use argon2::{password_hash::PasswordHash, Argon2, PasswordVerifier};

    let parsed = PasswordHash::new(password_hash)
        .map_err(|_| anyhow::anyhow!("Invalid email or password"))?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| anyhow::anyhow!("Invalid email or password"))
}

async fn authenticate_user(pool: &sqlx::PgPool, email: &str, password: &str) -> Result<Uuid> {
    let (user_id, password_hash): (Uuid, String) =
        sqlx::query_as("SELECT id, password_hash FROM feed_radar_auth_find_user($1)")
            .bind(email)
            .fetch_one(pool)
            .await
            .context("Invalid email or password")?;
    verify_password_hash(password, &password_hash)?;
    Ok(user_id)
}

async fn get_or_create_user(pool: &sqlx::PgPool, email: &str, password: &str) -> Result<Uuid> {
    // Authenticate an existing user before selecting their tenant context.
    let existing: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id, password_hash FROM feed_radar_auth_find_user($1)")
            .bind(email)
            .fetch_optional(pool)
            .await?;

    if let Some((id, password_hash)) = existing {
        verify_password_hash(password, &password_hash)?;
        return Ok(id);
    }

    // Create new user
    use argon2::{
        password_hash::{rand_core::OsRng, PasswordHasher, SaltString},
        Argon2,
    };

    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let password_hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("Failed to hash password: {}", e))?
        .to_string();

    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM feed_radar_auth_register($1, $2, NULL)")
        .bind(email)
        .bind(&password_hash)
        .fetch_one(pool)
        .await?;

    info!(email_hash = %sha256_tag(email.as_bytes()), user_id_hash = %sha256_tag(user_id.to_string().as_bytes()), "Created new user");

    Ok(user_id)
}

async fn create_folder(
    connection: &mut sqlx::PgConnection,
    user_id: Uuid,
    name: &str,
) -> Result<Uuid> {
    let existing: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM folders WHERE user_id = $1 AND name = $2")
            .bind(user_id)
            .bind(name)
            .fetch_optional(&mut *connection)
            .await?;

    if let Some(id) = existing {
        return Ok(id);
    }

    let folder_id: Uuid =
        sqlx::query_scalar("INSERT INTO folders (user_id, name) VALUES ($1, $2) RETURNING id")
            .bind(user_id)
            .bind(name)
            .fetch_one(&mut *connection)
            .await?;

    Ok(folder_id)
}

async fn create_feed(
    connection: &mut sqlx::PgConnection,
    user_id: Uuid,
    folder_id: Option<Uuid>,
    title: &str,
    feed_url: &str,
    site_url: Option<&str>,
) -> Result<Uuid> {
    let feed_id: Uuid = sqlx::query_scalar(
        r#"
        INSERT INTO feeds (user_id, folder_id, title, url, site_url, feed_type)
        VALUES ($1, $2, $3, $4, $5, 'rss')
        RETURNING id
        "#,
    )
    .bind(user_id)
    .bind(folder_id)
    .bind(title)
    .bind(feed_url)
    .bind(site_url)
    .fetch_one(&mut *connection)
    .await?;

    Ok(feed_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_path(relative: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative)
    }

    #[test]
    fn demo_curate_builds_expected_curated_item_export() {
        let opml_content = std::fs::read_to_string(fixture_path("examples/demo.opml")).unwrap();
        let opml = OpmlParser::parse(&opml_content).unwrap();
        let feeds = flatten_outlines(&opml.outlines, None);
        let article = load_article(&fixture_path("examples/demo-article.json")).unwrap();
        let rule = load_rule_input(&fixture_path("examples/demo-rule.json")).unwrap();

        let export =
            build_curated_export(&opml, &feeds, &article, &rule, "actor-demo-local").unwrap();
        let actual = serde_json::to_value(export).unwrap();
        let expected: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(fixture_path("examples/expected-curated-export.json"))
                .unwrap(),
        )
        .unwrap();

        assert_eq!(actual, expected);
        assert_eq!(actual["constraints"]["contains_secrets"], false);
        assert_eq!(actual["constraints"]["contains_byok_material"], false);
        assert_eq!(actual["constraints"]["allow_downstream_execution"], false);
        assert!(!actual["export_id"].as_str().unwrap().contains("demo-1"));
        assert!(!actual["source_ref"]["source_id"]
            .as_str()
            .unwrap()
            .contains("demo-1"));
    }

    #[test]
    fn public_actor_reference_rejects_email_and_unbounded_values() {
        assert!(validate_actor("actor-radar-local-sync").is_ok());
        assert!(validate_actor("person@example.com").is_err());
        assert!(validate_actor(&"a".repeat(65)).is_err());
        assert!(validate_actor("actor with spaces").is_err());
    }
}
