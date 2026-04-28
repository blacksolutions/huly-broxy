mod bridge_client;
mod config;
mod discovery;
mod mcp;
mod sync;
mod txcud;

use clap::Parser;
use rmcp::ServiceExt;
use std::path::PathBuf;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "huly-mcp", about = "Huly MCP Server - Model Context Protocol server for Huly bridge")]
struct Cli {
    /// Path to the TOML configuration file
    #[arg(short, long, env = "HULY_MCP_CONFIG")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = config::McpConfig::from_file(&cli.config)?;

    // Initialize tracing (to stderr so it doesn't interfere with MCP stdio)
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.log.level));

    if config.log.json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_target(true)
            .with_writer(std::io::stderr)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .with_writer(std::io::stderr)
            .init();
    }

    tracing::info!("huly-mcp starting");

    let cancel = CancellationToken::new();

    // Connect to NATS
    tracing::info!(url = %config.nats.url, "connecting to NATS...");
    let nats_client = if let Some(ref creds_path) = config.nats.credentials {
        tracing::info!(creds = %creds_path, "using NATS credentials file");
        let options = async_nats::ConnectOptions::with_credentials_file(creds_path)
            .await
            .map_err(|e| anyhow::anyhow!("failed to load NATS credentials: {e}"))?;
        options
            .connect(&config.nats.url)
            .await
            .map_err(|e| anyhow::anyhow!("NATS connection failed: {e}"))?
    } else {
        async_nats::connect(&config.nats.url)
            .await
            .map_err(|e| anyhow::anyhow!("NATS connection failed: {e}"))?
    };
    tracing::info!("NATS connected");

    // Start bridge discovery
    let registry = discovery::BridgeRegistry::new();

    let subscriber_cancel = cancel.clone();
    let subscriber_registry = registry.clone();
    let subscriber_client = nats_client.clone();
    let subscriber_handle = tokio::spawn(async move {
        discovery::run_subscriber(subscriber_client, subscriber_registry, subscriber_cancel).await;
    });

    let reaper_cancel = cancel.clone();
    let reaper_registry = registry.clone();
    let stale_timeout = Duration::from_secs(config.mcp.stale_timeout_secs);
    let reaper_handle = tokio::spawn(async move {
        discovery::run_reaper(reaper_registry, stale_timeout, reaper_cancel).await;
    });

    // Create MCP server
    let http_client = bridge_client::BridgeHttpClient::new(config.mcp.bridge_api_token);
    let catalog_unknown = config.mcp.catalog.unknown_keys();
    if !catalog_unknown.is_empty() {
        tracing::warn!(
            keys = ?catalog_unknown,
            "[mcp.catalog] override(s) reference unknown names — typo? Defaults will apply"
        );
    }
    let catalog = mcp::catalog::Catalog::new(&config.mcp.catalog);
    let sync_runner = config.mcp.sync.as_ref().map(sync::SyncRunner::new);
    if sync_runner.is_some() {
        tracing::info!("sync subprocess tools enabled (huly_sync_status, huly_sync_cards)");
    } else {
        tracing::info!("sync subprocess tools disabled (no [mcp.sync] config)");
    }
    let server = mcp::server::HulyMcpServer::with_catalog(registry, http_client, catalog)
        .with_sync_runner(sync_runner);

    // Serve via stdio
    let transport = rmcp::transport::io::stdio();
    tracing::info!("MCP server ready, waiting for client on stdio");

    let service = server.serve(transport).await?;
    service.waiting().await?;

    // Shutdown
    cancel.cancel();

    let shutdown_timeout = Duration::from_secs(5);
    if tokio::time::timeout(shutdown_timeout, async {
        if let Err(e) = subscriber_handle.await {
            tracing::error!(error = %e, "subscriber task panicked");
        }
        if let Err(e) = reaper_handle.await {
            tracing::error!(error = %e, "reaper task panicked");
        }
    })
    .await
    .is_err()
    {
        tracing::warn!("background tasks did not finish within timeout");
    }

    tracing::info!("huly-mcp stopped");

    Ok(())
}
