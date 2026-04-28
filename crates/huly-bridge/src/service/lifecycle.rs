use crate::admin::health::HealthState;
use crate::admin::metrics;
use crate::admin::platform_api::PlatformClientHandle;
use crate::admin::router::{AppState, create_router};
use crate::bridge::announcer::{self, SocialIdHandle};
use crate::bridge::event_loop;
use crate::bridge::nats_publisher::{EventPublisher, NatsPublisher};
use crate::config::{AuthConfig, BridgeConfig};
use crate::huly::accounts::{AccountsClient, AccountsError, WorkspaceLoginInfo};
use crate::huly::auth;
use crate::huly::client::{HulyClient, PlatformClient};
use crate::huly::collaborator::CollaboratorClient;
use crate::huly::connection::{HulyConnection, WsConnection};
use crate::huly::rest::{self, RestClient, ServerConfigCache};
use crate::huly::rpc::ProtocolOptions;
use crate::service::watchdog;
use crate::service::workspace_token::WorkspaceTokenCache;
use secrecy::SecretString;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

/// Run the full bridge lifecycle
pub async fn run(config: BridgeConfig) -> anyhow::Result<()> {
    let cancel = CancellationToken::new();
    let health = HealthState::new();
    let start_time = Instant::now();

    // Initialize metrics
    let metrics_handle = metrics::init_metrics()
        .map_err(|e| anyhow::anyhow!("metrics init failed: {e}"))?;

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

    let accounts = AccountsClient::from_config(
        &config.huly.url,
        config.huly.accounts_url.as_deref(),
    );

    // Best-effort: fetch GET {huly.url}/config.json once and cache the
    // server-advertised ACCOUNTS_URL / COLLABORATOR_URL / FILES_URL /
    // UPLOAD_URL so downstream code can prefer them over operator config.
    // Legacy transactors omit this endpoint; the helper logs and continues
    // with an empty cache on any failure (Issue #21 / R8).
    let server_config_cache = ServerConfigCache::new();
    let bootstrap_rest = RestClient::new(&config.huly.url, account_token.clone());
    rest::bootstrap_server_config(&bootstrap_rest, &server_config_cache).await;
    if server_config_cache.is_populated() {
        info!("server config cached from /config.json");
    }

    // Build collaborator client from cached COLLABORATOR_URL (may be None if
    // the server didn't advertise one; handlers return 503 in that case).
    let collaborator_client = server_config_cache
        .collaborator_url()
        .map(|url| CollaboratorClient::new(&url));

    // Cache for the workspace-scoped token (populated on every successful
    // selectWorkspace / getLoginInfoByToken call in the reconnect loop below).
    let workspace_token_cache = WorkspaceTokenCache::new();

    // Connect to NATS
    info!(url = %config.nats.url, "connecting to NATS...");
    let publisher = NatsPublisher::connect(
        &config.nats.url,
        config.nats.subject_prefix.as_deref(),
        config.nats.credentials.as_deref(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("NATS connection failed: {e}"))?;
    health.set_nats_connected(true);
    metrics::set_nats_connected(true);
    info!("NATS connected");

    let protocol = ProtocolOptions {
        binary: config.huly.use_binary_protocol,
        compression: config.huly.use_compression,
    };

    let reconnect_delay_ms = config.huly.reconnect_delay_ms;
    let tls_skip_verify = config.huly.tls_skip_verify;
    let tls_ca_cert = config.huly.tls_ca_cert.clone();
    let subject_prefix = config
        .nats
        .subject_prefix
        .unwrap_or_else(|| "huly".to_string());

    let nats_client_for_announcer = publisher.client().clone();
    let publisher = Arc::new(publisher) as Arc<dyn EventPublisher>;
    let publisher_ref = publisher.clone();

    // Start admin API server. Platform routes are mounted unconditionally;
    // the handle starts empty and is populated on first WS connect (handlers
    // return 503 in the meantime).
    let metrics_handle = Arc::new(metrics_handle);
    let admin_addr = format!("{}:{}", config.admin.host, config.admin.port);
    let admin_listener = TcpListener::bind(&admin_addr).await?;
    info!(addr = %admin_addr, "admin API listening");

    let platform_client_handle: PlatformClientHandle = Arc::new(RwLock::new(None));
    // Workspace social identity (PersonId) — populated after each successful
    // connect; consumed by the announcer so MCP receives it via NATS discovery.
    let social_id_handle: SocialIdHandle = Arc::new(RwLock::new(None));

    let admin_state = AppState {
        health: health.clone(),
        metrics_handle: metrics_handle.clone(),
        start_time,
        platform_client: platform_client_handle.clone(),
        api_token: config.admin.api_token.clone(),
        collaborator_client: collaborator_client.clone(),
        workspace_token_cache: workspace_token_cache.clone(),
    };
    let admin_cancel = cancel.clone();
    let admin_handle = tokio::spawn(async move {
        let router = create_router(admin_state);
        axum::serve(admin_listener, router)
            .with_graceful_shutdown(async move { admin_cancel.cancelled().await })
            .await
            .ok();
    });

    // Start watchdog
    let watchdog_cancel = cancel.clone();
    let watchdog_health = health.clone();
    let sd_notifier = watchdog::SdNotifier;
    let watchdog_handle = tokio::spawn(async move {
        watchdog::run_watchdog(
            watchdog_health,
            Duration::from_secs(10),
            watchdog_cancel,
            &sd_notifier,
        )
        .await;
    });

    // Start NATS announcer for MCP discovery
    let announcer_cancel = cancel.clone();
    let announcer_health = health.clone();
    let announcer_workspace = config.huly.workspace.clone();
    let announcer_proxy_url = config.admin.proxy_url();
    let announcer_social_id = social_id_handle.clone();
    let announcer_handle = tokio::spawn(async move {
        announcer::run_announcer(
            nats_client_for_announcer,
            announcer_workspace,
            announcer_proxy_url,
            announcer_health,
            start_time,
            announcer_social_id,
            announcer_cancel,
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

        // Exponential backoff delay on reconnection (skip on first connect)
        if !first_connect {
            let backoff = calculate_backoff(reconnect_delay_ms, consecutive_failures);
            info!(delay_ms = backoff, attempt = consecutive_failures + 1, "reconnecting to Huly WebSocket...");
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

        // Resolve transactor endpoint + workspace-scoped token via accounts.selectWorkspace.
        // Re-resolved every (re)connect attempt so token expiry / endpoint changes are picked up.
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
                health.set_huly_connected(false);
                metrics::set_ws_connected(false);
                metrics::record_ws_reconnect();
                continue;
            }
        };

        // Cache the workspace-scoped token for collaborator service calls.
        workspace_token_cache.set(SecretString::from(ws_login.token.clone()));

        // Connect via `{endpoint}/{token}?sessionId=` — matching the official TS client.
        info!(
            endpoint = %ws_login.endpoint,
            workspace = %ws_login.workspace,
            "connecting to Huly WebSocket..."
        );
        let (conn, events) = match WsConnection::connect_with_tls(
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
                health.set_huly_connected(false);
                metrics::set_ws_connected(false);
                metrics::record_ws_reconnect();
                continue;
            }
        };

        // Hot-swap the platform client into the admin handle. Holding the only
        // strong ref here means dropping the handle entry on disconnect runs
        // WsConnection::Drop and aborts the read/write/ping tasks.
        let conn_arc: Arc<dyn HulyConnection> = Arc::new(conn);
        let huly_client: Arc<dyn PlatformClient> = Arc::new(HulyClient::new(conn_arc));
        *platform_client_handle
            .write()
            .expect("platform client handle poisoned") = Some(huly_client);
        *social_id_handle
            .write()
            .expect("social id handle poisoned") = ws_login.social_id.clone();

        health.set_huly_connected(true);
        metrics::set_ws_connected(true);
        info!("Huly WebSocket connected");

        // Notify systemd we're ready
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

        // Wait for either: event loop ends (WS disconnected) or shutdown signal
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
                // Connection dropped — update health and try reconnect
                health.set_huly_connected(false);
                metrics::set_ws_connected(false);
                // Clear the platform handle so admin handlers go back to 503
                // until the next successful connect.
                *platform_client_handle
                    .write()
                    .expect("platform client handle poisoned") = None;
                *social_id_handle
                    .write()
                    .expect("social id handle poisoned") = None;

                if cancel.is_cancelled() {
                    break;
                }

                metrics::record_ws_reconnect();
                info!("WebSocket disconnected, will attempt reconnection");
            }
            _ = shutdown_signal() => {
                info!("shutdown signal received");
                break;
            }
        }
    }

    // Notify systemd we're stopping
    #[cfg(target_os = "linux")]
    {
        let _ = sd_notify::notify(false, &[sd_notify::NotifyState::Stopping]);
    }

    // Cancel all tasks
    cancel.cancel();

    // Flush NATS to ensure buffered messages are sent
    if let Err(e) = publisher_ref.flush().await {
        error!(error = %e, "NATS flush failed during shutdown");
    }

    // Await admin, watchdog, and announcer tasks (short timeout, they should stop quickly)
    let _ = tokio::time::timeout(Duration::from_secs(5), admin_handle).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), watchdog_handle).await;
    let _ = tokio::time::timeout(Duration::from_secs(5), announcer_handle).await;

    info!("service stopped");
    Ok(())
}

const MAX_BACKOFF_MS: u64 = 30_000;

/// Calculate exponential backoff delay for reconnection attempts.
fn calculate_backoff(base_delay_ms: u64, attempt: u32) -> u64 {
    std::cmp::min(
        base_delay_ms.saturating_mul(2u64.saturating_pow(attempt)),
        MAX_BACKOFF_MS,
    )
}

fn to_ws_scheme(url: &str) -> String {
    let base = url.trim_end_matches('/');
    if base.starts_with("https://") {
        base.replacen("https://", "wss://", 1)
    } else if base.starts_with("http://") {
        base.replacen("http://", "ws://", 1)
    } else {
        format!("wss://{base}")
    }
}

/// Resolve transactor endpoint + workspace-scoped token.
///
/// For **token auth** the JWT already names the workspace, so `getLoginInfoByToken`
/// returns the right endpoint deterministically. `selectWorkspace` is avoided here
/// because the server can return a different workspace than the slug requests when
/// the account owns multiple — which then yields `platform:status:Unauthorized` at
/// the transactor.
///
/// For **password auth** there is no workspace claim in the freshly-issued account
/// token, so the slug-driven `selectWorkspace` call is the only option.
///
/// Self-hosted Huly omits `socialId` from `selectWorkspace` (and sometimes
/// `getLoginInfoByToken`) responses; if the primary call yielded none, fetch
/// it best-effort via the account-token-scoped `getLoginInfoByToken` and
/// merge it in. Failure is non-fatal — consumers fall back to
/// `core:account:System` and surface `AccountMismatch` rather than silently
/// writing under the wrong identity.
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
    if info.social_id.is_none() {
        match accounts.get_login_info(account_token).await {
            Ok(login) => {
                info.social_id = login.social_id;
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "fetching socialId from getLoginInfoByToken failed; \
                     transactions will be stamped with core:account:System and \
                     rejected by the transactor as AccountMismatch"
                );
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
        assert_eq!(calculate_backoff(1000, 3), 8_000);
        assert_eq!(calculate_backoff(1000, 4), 16_000);
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
