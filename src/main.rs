pub mod collect;
pub mod config;
pub mod db;
pub mod metrics;
pub mod msg;
pub mod status;

use std::path::PathBuf;

use clap::Parser;
use futures::future;
use sqlx::SqlitePool;
use tracing::{error, error_span, info, Instrument};

use crate::config::{Config, Endpoint};
use crate::metrics::Metrics;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Collect and analyze txs containing IBC messages, export the collected metrics for Prometheus
#[derive(clap::Parser)]
struct App {
    /// Path to the configuration file
    #[clap(short, long = "config", default_value = "chainpulse.toml")]
    config: PathBuf,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    setup_tracing();
    setup_ctrlc_handler();

    let app = App::parse();
    let config = Config::load(&app.config)?;

    let (metrics, registry) = Metrics::new();

    if config.metrics.enabled {
        tokio::spawn(
            metrics::run(config.metrics.port, registry).instrument(error_span!("metrics")),
        );
    }

    if config.metrics.stuck_packets {
        info!("Monitoring packets stuck on IBC channels");

        tokio::spawn(
            status::run(config.chains.clone(), metrics.clone()).instrument(error_span!("status")),
        );
    }

    let pool = db::connect(&config.database.path).await?;
    db::setup(&pool).await;

    let handles = config
        .chains
        .endpoints
        .into_iter()
        .map(|endpoint| {
            metrics.chainpulse_chains();

            let span = error_span!("collect", chain = %endpoint.name);
            let task = collect(endpoint, pool.clone(), metrics.clone()).instrument(span);
            tokio::spawn(task)
        })
        .collect::<Vec<_>>();

    future::join_all(handles).await;

    Ok(())
}

async fn collect(endpoint: Endpoint, pool: SqlitePool, metrics: Metrics) {
    if let Err(e) = collect::run(
        endpoint.name,
        endpoint.comet_version,
        endpoint.url,
        pool,
        metrics,
    )
    .await
    {
        error!("{e}");
    }
}

fn setup_tracing() {
    use tracing_subscriber::prelude::*;
    use tracing_subscriber::{filter::EnvFilter, fmt};

    let fmt_layer = fmt::layer().with_target(false);

    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("chainpulse=info"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();
}

fn setup_ctrlc_handler() {
    ctrlc::set_handler(move || {
        info!("Ctrl-C received, shutting down");
        std::process::exit(0);
    })
    .expect("Error setting Ctrl-C handler");
}
