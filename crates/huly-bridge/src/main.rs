use clap::Parser;
use huly_bridge::{config, service};
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "huly-bridge", about = "Huly.io Bridge Server")]
struct Cli {
    /// Path to the TOML configuration file
    #[arg(short, long, env = "HULY_BRIDGE_CONFIG")]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let config = config::BridgeConfig::from_file(&cli.config)?;

    // Initialize tracing
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&config.log.level));

    if config.log.json {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_target(true)
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(true)
            .init();
    }

    tracing::info!(
        url = %config.huly.url,
        workspace = %config.huly.workspace,
        "huly-bridge starting"
    );

    service::lifecycle::run(config).await
}
