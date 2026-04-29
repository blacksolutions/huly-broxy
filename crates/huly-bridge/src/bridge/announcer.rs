use crate::admin::health::HealthState;
use huly_common::announcement::{
    ANNOUNCE_INTERVAL_SECS, ANNOUNCE_SUBJECT, BridgeAnnouncement, LOOKUP_SUBJECT,
};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Hot-swappable handle to the active workspace's social identity.
/// Empty until the first successful Huly connect populates it; cleared on
/// disconnect so consumers don't act on a stale identity after reconnect.
pub type SocialIdHandle = Arc<RwLock<Option<String>>>;

fn build_announcement(
    workspace: &str,
    proxy_url: &str,
    health: &HealthState,
    start_time: Instant,
    version: &str,
    social_id: Option<String>,
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
        social_id,
    }
}

async fn publish_announcement_to(
    client: &async_nats::Client,
    subject: async_nats::Subject,
    announcement: &BridgeAnnouncement,
) {
    match serde_json::to_vec(announcement) {
        Ok(payload) => {
            if let Err(e) = client.publish(subject, payload.into()).await {
                error!(error = %e, "failed to publish bridge announcement");
            }
        }
        Err(e) => {
            error!(error = %e, "failed to serialize bridge announcement");
        }
    }
}

/// Run the periodic bridge announcement loop.
///
/// Publishes immediately on entry, then on every `ANNOUNCE_INTERVAL_SECS`
/// tick. The eager publish closes the bridge-cold-start window where
/// MCP subscribers would otherwise wait up to one full interval before
/// seeing any state.
pub async fn run_announcer(
    client: async_nats::Client,
    workspace: String,
    proxy_url: String,
    health: HealthState,
    start_time: Instant,
    social_id_handle: SocialIdHandle,
    cancel: CancellationToken,
) {
    let interval = Duration::from_secs(ANNOUNCE_INTERVAL_SECS);
    let version = env!("CARGO_PKG_VERSION").to_string();
    let subject: async_nats::Subject = ANNOUNCE_SUBJECT.into();

    loop {
        let social_id = social_id_handle
            .read()
            .expect("social id handle poisoned")
            .clone();
        let announcement = build_announcement(
            &workspace,
            &proxy_url,
            &health,
            start_time,
            &version,
            social_id,
        );
        publish_announcement_to(&client, subject.clone(), &announcement).await;
        debug!(workspace = %announcement.workspace, "published bridge announcement");

        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("announcer stopping");
                return;
            }
            _ = tokio::time::sleep(interval) => {}
        }
    }
}

/// Respond to NATS request/reply lookups on `LOOKUP_SUBJECT`.
///
/// Lets late-starting MCP subscribers seed their registry without waiting
/// for the next periodic announcement. Each request is answered with the
/// current `BridgeAnnouncement` snapshot. Requests without a reply-to
/// subject are dropped (they would have nowhere to deliver the response).
pub async fn run_lookup_responder(
    client: async_nats::Client,
    workspace: String,
    proxy_url: String,
    health: HealthState,
    start_time: Instant,
    social_id_handle: SocialIdHandle,
    cancel: CancellationToken,
) {
    use futures::StreamExt;

    let version = env!("CARGO_PKG_VERSION").to_string();
    let mut subscriber = match client.subscribe(LOOKUP_SUBJECT.to_string()).await {
        Ok(sub) => sub,
        Err(e) => {
            error!(error = %e, "failed to subscribe to bridge lookup subject");
            return;
        }
    };

    info!(subject = LOOKUP_SUBJECT, "listening for bridge lookup requests");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("lookup responder stopping");
                return;
            }
            msg = subscriber.next() => {
                match msg {
                    Some(msg) => {
                        let Some(reply_to) = msg.reply else {
                            debug!("lookup request without reply-to — ignoring");
                            continue;
                        };
                        let social_id = social_id_handle
                            .read()
                            .expect("social id handle poisoned")
                            .clone();
                        let announcement = build_announcement(
                            &workspace,
                            &proxy_url,
                            &health,
                            start_time,
                            &version,
                            social_id,
                        );
                        publish_announcement_to(&client, reply_to, &announcement).await;
                    }
                    None => {
                        warn!("lookup subscription closed");
                        return;
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

        let ann = build_announcement(
            "ws1", "http://localhost:9090", &health, Instant::now(), "0.1.0", None,
        );
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

        let ann = build_announcement(
            "ws1", "http://localhost:9090", &health, start, "0.1.0", None,
        );
        assert!(ann.uptime_secs >= 120);
    }

    #[test]
    fn build_announcement_serialization_roundtrip() {
        let health = HealthState::new();
        health.set_huly_connected(true);
        health.set_nats_connected(true);

        let ann = build_announcement(
            "ws1", "http://bridge:9090", &health, Instant::now(), "0.1.0", None,
        );
        let json = serde_json::to_vec(&ann).unwrap();
        let parsed: BridgeAnnouncement = serde_json::from_slice(&json).unwrap();

        assert_eq!(parsed.workspace, ann.workspace);
        assert_eq!(parsed.proxy_url, ann.proxy_url);
        assert_eq!(parsed.ready, ann.ready);
        assert_eq!(parsed.huly_connected, ann.huly_connected);
    }

    #[test]
    fn build_announcement_propagates_social_id() {
        let health = HealthState::new();
        let ann = build_announcement(
            "ws1",
            "http://h:9090",
            &health,
            Instant::now(),
            "0.1.0",
            Some("soc-7".into()),
        );
        assert_eq!(ann.social_id.as_deref(), Some("soc-7"));
    }

    #[test]
    fn build_announcement_omits_social_id_when_handle_empty() {
        let health = HealthState::new();
        let ann = build_announcement(
            "ws1", "http://h:9090", &health, Instant::now(), "0.1.0", None,
        );
        assert!(ann.social_id.is_none());
    }
}
