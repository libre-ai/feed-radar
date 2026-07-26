//! FeedMind Background Worker

// TODO(refactor): remove once scheduled jobs and billing recovery paths are fully wired.
#![allow(dead_code)]

use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod config;
mod handlers;
mod jobs;
mod queue;
mod scheduler;

use config::WorkerConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "feedmind_worker=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer().json())
        .init();

    let config = WorkerConfig::load()?;
    info!("Worker configuration loaded");

    let mut consumer = queue::QueueConsumer::new(&config).await?;
    info!("Queue consumer initialized");

    let scheduler = scheduler::Scheduler::new(&config).await?;
    scheduler.start().await?;
    info!("Scheduler started");

    // Run consumer (blocking)
    info!("Starting worker...");
    consumer.run().await?;

    Ok(())
}
