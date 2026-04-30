use crate::bridge::event_loop;
use crate::bridge::mint_responder::{self, AccountsLogin, MintBrokerConfig};
use crate::bridge::nats_publisher::{EventPublisher, NatsPublisher};
use crate::config::{AuthConfig, BridgeConfig, WorkspaceCredential};
use crate::service::watchdog;
use huly_client::accounts::{
    AccountsClient, AccountsError, WorkspaceLoginInfo, pick_primary_social_id,
};
use huly_client::auth;
use huly_client::connection::WsConnection;
use huly_client::rest::{self, RestClient, ServerConfigCache};
use huly_client::rpc::ProtocolOptions;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

/// Run the full bridge lifecycle.
///
/// Post-P4: the bridge has no HTTP listener. Its responsibilities collapse to
///
/// - WebSocket connect to the transactor + reconnect with backoff,
/// - NATS event forwarder (`huly.event.*`),
/// - JWT broker responder (`huly.bridge.mint`),
/// - watchdog / systemd notifications.
pub async fn run(config: BridgeConfig) -> anyhow::Result<()> {
    let cancel = CancellationToken::new();
    let start_time = Instant::now();

    // Authenticate with Huly accounts service → account-scoped token
    info!("authenticating with Huly...");
    let account_token = auth::authenticate(
        &config.huly.url,
        config.huly.accounts_url.as_deref(),
        &config.huly.auth,
    )
    .await
    .map_err(|e| anyhow::anyhow!("auth failed: {e}"))?;
    info!("authenticated successfully");

    let accounts =
        AccountsClient::from_config(&config.huly.url, config.huly.accounts_url.as_deref());

    // Best-effort /config.json — used by the JWT broker for downstream URLs.
    let server_config_cache = ServerConfigCache::new();
    let bootstrap_rest = RestClient::new(&config.huly.url, account_token.clone());
    rest::bootstrap_server_config(&bootstrap_rest, &server_config_cache).await;
    if server_config_cache.is_populated() {
        info!("server config cached from /config.json");
    }

    // Connect to NATS
    info!(url = %config.nats.url, "connecting to NATS...");
    let publisher = NatsPublisher::connect(
        &config.nats.url,
        config.nats.subject_prefix.as_deref(),
        config.nats.credentials.as_deref(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("NATS connection failed: {e}"))?;
    info!("NATS connected");

    let protocol = ProtocolOptions {
        binary: config.huly.use_binary_protocol,
        compression: config.huly.use_compression,
    };

    let reconnect_delay_ms = config.huly.reconnect_delay_ms;
    let tls_skip_verify = config.huly.tls_skip_verify;
    let tls_ca_cert = config.huly.tls_ca_cert.clone();
    let mint_creds = effective_workspace_credentials(&config);
    let mint_rest_base_url = format!("{}/api/v1", config.huly.url.trim_end_matches('/'));
    let mint_accounts_url = config.huly.accounts_url.clone();
    let mint_collaborator_url = server_config_cache.collaborator_url();
    let subject_prefix = config
        .nats
        .subject_prefix
        .clone()
        .unwrap_or_else(|| "huly".to_string());

    let nats_client_for_mint = publisher.client().clone();
    let publisher = Arc::new(publisher) as Arc<dyn EventPublisher>;
    let publisher_ref = publisher.clone();

    // Start watchdog
    let watchdog_cancel = cancel.clone();
    let sd_notifier = watchdog::SdNotifier;
    let watchdog_handle = tokio::spawn(async move {
        watchdog::run_watchdog_simple(
            Duration::from_secs(10),
            watchdog_cancel,
            &sd_notifier,
        )
        .await;
    });

    // Start JWT broker — listens on `huly.bridge.mint`. Runs from boot
    // (does NOT depend on the WS connect loop) so MCP can mint cold-start
    // tokens even while the bridge's own WS session is reconnecting.
    let mint_cfg = MintBrokerConfig::from_credentials(
        mint_rest_base_url,
        mint_accounts_url,
        mint_collaborator_url,
        &mint_creds,
    )
    .map_err(|e| anyhow::anyhow!("mint broker config: {e}"))?;
    let mint_accounts: Arc<dyn AccountsLogin> = Arc::new(accounts.clone());
    let mint_cancel = cancel.clone();
    let mint_handle = tokio::spawn(async move {
        mint_responder::run_mint_responder(
            nats_client_for_mint,
            mint_cfg,
            mint_accounts,
            mint_cancel,
        )
        .await;
    });

    // WebSocket connection loop with reconnection
    let mut first_connect = true;
    let mut consecutive_failures: u32 = 0;

    loop {
        if cancel.is_cancelled() {
            break;
        }

        if !first_connect {
            let backoff = calculate_backoff(reconnect_delay_ms, consecutive_failures);
            info!(
                delay_ms = backoff,
                attempt = consecutive_failures + 1,
                "reconnecting to Huly WebSocket..."
            );
            #[cfg(target_os = "linux")]
            {
                let _ = sd_notify::notify(
                    false,
                    &[sd_notify::NotifyState::Status("Reconnecting to Huly...")],
                );
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(backoff)) => {}
                _ = cancel.cancelled() => break,
            }
        }
        first_connect = false;

        let ws_login = match resolve_workspace(
            &accounts,
            &config.huly.auth,
            &account_token,
            &config.huly.workspace,
        )
        .await
        {
            Ok(info) => info,
            Err(e) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                error!(error = %e, "selectWorkspace failed");
                continue;
            }
        };

        info!(
            endpoint = %ws_login.endpoint,
            workspace = %ws_login.workspace,
            "connecting to Huly WebSocket..."
        );
        let (_conn, events) = match WsConnection::connect_with_tls(
            &ws_login.endpoint,
            &ws_login.token,
            protocol,
            tls_skip_verify,
            tls_ca_cert.as_deref(),
            config.huly.ping_interval_secs,
            config.huly.max_pending_requests,
        )
        .await
        {
            Ok(result) => {
                consecutive_failures = 0;
                result
            }
            Err(e) => {
                consecutive_failures = consecutive_failures.saturating_add(1);
                error!(error = %e, "Huly WS connection failed");
                continue;
            }
        };

        info!("Huly WebSocket connected");
        #[cfg(target_os = "linux")]
        {
            let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Ready]);
            let _ = sd_notify::notify(
                false,
                &[sd_notify::NotifyState::Status("Connected to Huly and NATS")],
            );
        }

        // Run event loop until connection drops or shutdown
        let event_loop_cancel = cancel.child_token();
        let event_publisher = publisher.clone();
        let prefix = subject_prefix.clone();
        let event_loop_handle = tokio::spawn(async move {
            event_loop::run_event_loop(events, event_publisher, &prefix, event_loop_cancel).await
        });

        tokio::select! {
            result = event_loop_handle => {
                match result {
                    Ok(stats) => {
                        info!(
                            forwarded = stats.events_forwarded,
                            failed = stats.events_failed,
                            "event loop finished"
                        );
                    }
                    Err(e) => error!("event loop task error: {e}"),
                }
                if cancel.is_cancelled() {
                    break;
                }
                info!("WebSocket disconnected, will attempt reconnection");
            }
            _ = shutdown_signal() => {
                info!("shutdown signal received");
                break;
            }
        }
    }

    let _ = (start_time,); // silence unused warning when no admin /uptime
    #[cfg(target_os = "linux")]
    {
        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Stopping]);
    }

    cancel.cancel();
    if let Err(e) = publisher_ref.flush().await {
        error!(error = %e, "NATS flush failed during shutdown");
    }
    let _ = tokio::time::timeout(Duration::from_secs(5), watchdog_handle).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), mint_handle).await;

    info!("service stopped");
    Ok(())
}

const MAX_BACKOFF_MS: u64 = 30_000;

fn calculate_backoff(base_delay_ms: u64, attempt: u32) -> u64 {
    std::cmp::min(
        base_delay_ms.saturating_mul(2u64.saturating_pow(attempt)),
        MAX_BACKOFF_MS,
    )
}

fn effective_workspace_credentials(config: &BridgeConfig) -> Vec<WorkspaceCredential> {
    if !config.workspace_credentials.is_empty() {
        return config
            .workspace_credentials
            .iter()
            .map(|c| WorkspaceCredential {
                workspace: c.workspace.clone(),
                email: c.email.clone(),
                password: c.password.clone(),
                token: c.token.clone(),
                jwt_ttl_secs: c.jwt_ttl_secs,
            })
            .collect();
    }
    match &config.huly.auth {
        AuthConfig::Token { token } => vec![WorkspaceCredential {
            workspace: config.huly.workspace.clone(),
            email: String::new(),
            password: None,
            token: Some(token.clone()),
            jwt_ttl_secs: None,
        }],
        AuthConfig::Password { email, password } => vec![WorkspaceCredential {
            workspace: config.huly.workspace.clone(),
            email: email.clone(),
            password: Some(password.clone()),
            token: None,
            jwt_ttl_secs: None,
        }],
    }
}

async fn resolve_workspace(
    accounts: &AccountsClient,
    auth: &AuthConfig,
    account_token: &str,
    workspace_slug: &str,
) -> Result<WorkspaceLoginInfo, AccountsError> {
    let mut info = match auth {
        AuthConfig::Token { .. } => accounts.get_login_info_by_token(account_token).await?,
        AuthConfig::Password { .. } => {
            accounts.select_workspace(account_token, workspace_slug).await?
        }
    };
    match accounts.get_social_ids(account_token, false).await {
        Ok(ids) => {
            if let Some(primary) = pick_primary_social_id(&ids) {
                info.social_id = Some(primary.id.clone());
            } else {
                tracing::warn!("getSocialIds returned no active entries");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "getSocialIds unavailable; falling back to getLoginInfoByToken");
            if info.social_id.is_none()
                && let Ok(login) = accounts.get_login_info(account_token).await
            {
                info.social_id = login.social_id;
            }
        }
    }
    Ok(info)
}

#[cfg(unix)]
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("failed to register SIGTERM handler");
    tokio::select! {
        _ = ctrl_c => info!("received SIGINT"),
        _ = sigterm.recv() => info!("received SIGTERM"),
    }
}

#[cfg(windows)]
async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for Ctrl+C");
    info!("received Ctrl+C");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_exponentially() {
        assert_eq!(calculate_backoff(1000, 0), 1_000);
        assert_eq!(calculate_backoff(1000, 1), 2_000);
        assert_eq!(calculate_backoff(1000, 2), 4_000);
    }

    #[test]
    fn backoff_caps_at_max() {
        assert_eq!(calculate_backoff(5000, 5), MAX_BACKOFF_MS);
        assert_eq!(calculate_backoff(1000, 10), MAX_BACKOFF_MS);
    }

    #[test]
    fn backoff_handles_overflow() {
        assert_eq!(calculate_backoff(u64::MAX, 5), MAX_BACKOFF_MS);
        assert_eq!(calculate_backoff(1000, u32::MAX), MAX_BACKOFF_MS);
    }
}
