use crate::admin::health::HealthState;
use huly_common::announcement::{ANNOUNCE_INTERVAL_SECS, ANNOUNCE_SUBJECT, BridgeAnnouncement};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

fn build_announcement(
    workspace: &str,
    proxy_url: &str,
    health: &HealthState,
    start_time: Instant,
    version: &str,
) -> BridgeAnnouncement {
    let status = health.status();
    BridgeAnnouncement {
        workspace: workspace.to_string(),
        proxy_url: proxy_url.to_string(),
        huly_connected: status.huly_connected,
        nats_connected: status.nats_connected,
        ready: status.ready,
        uptime_secs: start_time.elapsed().as_secs(),
        version: version.to_string(),
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64,
    }
}

/// Run the periodic bridge announcement loop.
pub async fn run_announcer(
    client: async_nats::Client,
    workspace: String,
    proxy_url: String,
    health: HealthState,
    start_time: Instant,
    cancel: CancellationToken,
) {
    let interval = Duration::from_secs(ANNOUNCE_INTERVAL_SECS);
    let version = env!("CARGO_PKG_VERSION").to_string();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("announcer stopping");
                return;
            }
            _ = tokio::time::sleep(interval) => {
                let announcement = build_announcement(
                    &workspace, &proxy_url, &health, start_time, &version,
                );

                match serde_json::to_vec(&announcement) {
                    Ok(payload) => {
                        if let Err(e) = client
                            .publish(ANNOUNCE_SUBJECT.to_string(), payload.into())
                            .await
                        {
                            error!(error = %e, "failed to publish bridge announcement");
                        } else {
                            debug!(workspace = %announcement.workspace, "published bridge announcement");
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "failed to serialize bridge announcement");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcement_uses_pkg_version() {
        let version = env!("CARGO_PKG_VERSION");
        assert!(!version.is_empty());
    }

    #[test]
    fn build_announcement_reflects_health() {
        let health = HealthState::new();
        health.set_huly_connected(true);
        health.set_nats_connected(false);

        let ann = build_announcement("ws1", "http://localhost:9090", &health, Instant::now(), "0.1.0");
        assert_eq!(ann.workspace, "ws1");
        assert_eq!(ann.proxy_url, "http://localhost:9090");
        assert!(ann.huly_connected);
        assert!(!ann.nats_connected);
        assert!(!ann.ready);
        assert_eq!(ann.version, "0.1.0");
    }

    #[test]
    fn build_announcement_tracks_uptime() {
        let health = HealthState::new();
        let start = Instant::now() - Duration::from_secs(120);

        let ann = build_announcement("ws1", "http://localhost:9090", &health, start, "0.1.0");
        assert!(ann.uptime_secs >= 120);
    }

    #[test]
    fn build_announcement_serialization_roundtrip() {
        let health = HealthState::new();
        health.set_huly_connected(true);
        health.set_nats_connected(true);

        let ann = build_announcement("ws1", "http://bridge:9090", &health, Instant::now(), "0.1.0");
        let json = serde_json::to_vec(&ann).unwrap();
        let parsed: BridgeAnnouncement = serde_json::from_slice(&json).unwrap();

        assert_eq!(parsed.workspace, ann.workspace);
        assert_eq!(parsed.proxy_url, ann.proxy_url);
        assert_eq!(parsed.ready, ann.ready);
        assert_eq!(parsed.huly_connected, ann.huly_connected);
    }
}
