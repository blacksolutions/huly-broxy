// Some modules carry helpers retained for the upcoming P5 tracker / markup
// rewire; suppress dead-code warnings at the binary level rather than
// scattering #[allow] attributes through the codebase.
#![allow(dead_code)]

mod audit;
mod config;
mod huly_client_factory;
mod jwt_broker_client;
mod mcp;
mod schema_invalidator;
mod sync;
mod txcud;

use clap::Parser;
use rmcp::ServiceExt;
use std::path::PathBuf;
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

    // Resolve agent_id (D8): operator config required, with optional override
    // by rmcp clientInfo.name once the handshake completes.
    let agent_id = config
        .mcp
        .agent_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!(
            "[mcp] agent_id is required (P4 / D8). Set it in your mcp.toml; \
             this id is logged by the bridge JWT broker for audit and \
             rate-limit attribution."
        ))?;

    // Wire the factory: every tool call resolves a per-workspace
    // RestHulyClient via the JWT broker on `huly.bridge.mint`.
    let factory =
        huly_client_factory::HulyClientFactory::new(nats_client.clone(), agent_id.clone());

    let sync_runner = config.mcp.sync.as_ref().map(sync::SyncRunner::new);
    if sync_runner.is_some() {
        tracing::info!("sync subprocess tools enabled (huly_sync_status, huly_sync_cards)");
    } else {
        tracing::info!("sync subprocess tools disabled (no [mcp.sync] config)");
    }
    let server = mcp::server::HulyMcpServer::new(factory.clone(), nats_client.clone(), agent_id)
        .with_sync_runner(sync_runner);

    // Subscribe-first schema invalidation (D9). Runs alongside the rmcp
    // server; cancelled when the process is shutting down.
    let invalidator_cancel = tokio_util::sync::CancellationToken::new();
    let invalidator_cancel_for_task = invalidator_cancel.clone();
    let invalidator_factory = factory.clone();
    let invalidator_nats = nats_client.clone();
    let subject_prefix = config
        .nats
        .subject_prefix
        .clone()
        .unwrap_or_else(|| "huly".to_string());
    let invalidator_handle = tokio::spawn(async move {
        schema_invalidator::run_schema_invalidator(
            invalidator_nats,
            invalidator_factory,
            &subject_prefix,
            invalidator_cancel_for_task,
        )
        .await;
    });

    // Serve via stdio
    let transport = rmcp::transport::io::stdio();
    tracing::info!("MCP server ready, waiting for client on stdio");

    let service = server.serve(transport).await?;
    service.waiting().await?;

    // Tear down the invalidator before exit.
    invalidator_cancel.cancel();
    let _ = invalidator_handle.await;

    tracing::info!("huly-mcp stopped");

    Ok(())
}
