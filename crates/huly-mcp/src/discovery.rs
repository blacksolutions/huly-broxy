use huly_common::announcement::{ANNOUNCE_SUBJECT, BridgeAnnouncement, LOOKUP_SUBJECT};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

#[derive(Debug)]
struct BridgeInfo {
    announcement: BridgeAnnouncement,
    last_seen: Instant,
}

#[derive(Clone, Debug)]
pub struct BridgeRegistry {
    bridges: Arc<RwLock<HashMap<String, BridgeInfo>>>,
}

impl BridgeRegistry {
    pub fn new() -> Self {
        Self {
            bridges: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn update(&self, announcement: BridgeAnnouncement) {
        let workspace = announcement.workspace.clone();
        let mut bridges = self.bridges.write().await;
        bridges.insert(
            workspace,
            BridgeInfo {
                announcement,
                last_seen: Instant::now(),
            },
        );
    }

    pub async fn get(&self, workspace: &str) -> Option<BridgeAnnouncement> {
        let bridges = self.bridges.read().await;
        bridges.get(workspace).map(|info| info.announcement.clone())
    }

    pub async fn list_workspaces(&self) -> Vec<BridgeAnnouncement> {
        let bridges = self.bridges.read().await;
        bridges
            .values()
            .map(|info| info.announcement.clone())
            .collect()
    }

    pub async fn remove_stale(&self, timeout: Duration) -> Vec<String> {
        let mut bridges = self.bridges.write().await;
        let now = Instant::now();
        let stale: Vec<String> = bridges
            .iter()
            .filter(|(_, info)| now.duration_since(info.last_seen) > timeout)
            .map(|(k, _)| k.clone())
            .collect();

        for workspace in &stale {
            bridges.remove(workspace);
        }

        stale
    }
}

/// Subscribe to bridge announcements on NATS and update the registry.
pub async fn run_subscriber(
    client: async_nats::Client,
    registry: BridgeRegistry,
    cancel: CancellationToken,
) {
    use futures::StreamExt;

    let mut subscriber = match client.subscribe(ANNOUNCE_SUBJECT.to_string()).await {
        Ok(sub) => sub,
        Err(e) => {
            error!(error = %e, "failed to subscribe to bridge announcements");
            return;
        }
    };

    info!(subject = ANNOUNCE_SUBJECT, "listening for bridge announcements");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("announcement subscriber stopping");
                return;
            }
            msg = subscriber.next() => {
                match msg {
                    Some(msg) => {
                        match serde_json::from_slice::<BridgeAnnouncement>(&msg.payload) {
                            Ok(announcement) => {
                                debug!(
                                    workspace = %announcement.workspace,
                                    ready = announcement.ready,
                                    "received bridge announcement"
                                );
                                registry.update(announcement).await;
                            }
                            Err(e) => {
                                warn!(error = %e, "failed to parse bridge announcement");
                            }
                        }
                    }
                    None => {
                        warn!("announcement subscription closed");
                        return;
                    }
                }
            }
        }
    }
}

/// Seed the registry on startup by sending a NATS request/reply lookup
/// to currently-running bridges.
///
/// NATS core pub/sub has no replay, so a freshly-started MCP would
/// otherwise have to wait up to `ANNOUNCE_INTERVAL_SECS` for the next
/// periodic announcement before any tool call could resolve a workspace.
/// Bridges respond to `LOOKUP_SUBJECT` with their current announcement,
/// closing that startup gap to roughly one round-trip.
///
/// Uses scatter-gather: collects every reply that arrives within
/// `gather_window`, supporting multi-bridge deployments. Returns silently
/// on failure — the periodic subscriber will eventually populate the
/// registry on its own.
pub async fn seed_via_lookup(
    client: &async_nats::Client,
    registry: &BridgeRegistry,
    gather_window: Duration,
) {
    use futures::StreamExt;

    let inbox = client.new_inbox();
    let mut subscription = match client.subscribe(inbox.clone()).await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "lookup seed: failed to subscribe to reply inbox");
            return;
        }
    };

    if let Err(e) = client
        .publish_with_reply(LOOKUP_SUBJECT.to_string(), inbox, Vec::new().into())
        .await
    {
        warn!(error = %e, "lookup seed: failed to publish lookup request");
        return;
    }

    let deadline = tokio::time::Instant::now() + gather_window;
    let mut seeded = 0u32;
    loop {
        let remaining = match deadline.checked_duration_since(tokio::time::Instant::now()) {
            Some(d) if !d.is_zero() => d,
            _ => break,
        };
        match tokio::time::timeout(remaining, subscription.next()).await {
            Ok(Some(msg)) => match serde_json::from_slice::<BridgeAnnouncement>(&msg.payload) {
                Ok(announcement) => {
                    info!(
                        workspace = %announcement.workspace,
                        ready = announcement.ready,
                        "lookup seed: registered bridge"
                    );
                    registry.update(announcement).await;
                    seeded += 1;
                }
                Err(e) => warn!(error = %e, "lookup seed: failed to parse reply"),
            },
            Ok(None) => break,
            Err(_) => break,
        }
    }

    if seeded == 0 {
        debug!("lookup seed: no bridges responded within window");
    } else {
        info!(count = seeded, "lookup seed: seeded bridges");
    }
}

/// Periodically remove stale bridge entries from the registry.
pub async fn run_reaper(
    registry: BridgeRegistry,
    stale_timeout: Duration,
    cancel: CancellationToken,
) {
    let check_interval = Duration::from_secs(10);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("reaper stopping");
                return;
            }
            _ = tokio::time::sleep(check_interval) => {
                let removed = registry.remove_stale(stale_timeout).await;
                for workspace in &removed {
                    info!(workspace = %workspace, "removed stale bridge");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registry_update_and_get() {
        let registry = BridgeRegistry::new();
        let ann = BridgeAnnouncement {
            workspace: "ws1".into(),
            proxy_url: "http://localhost:9090".into(),
            huly_connected: true,
            nats_connected: true,
            ready: true,
            uptime_secs: 100,
            version: "0.1.0".into(),
            timestamp: 1700000000000,
            social_id: None,
            schema_version: 0,
        };

        registry.update(ann).await;
        let result = registry.get("ws1").await;
        assert!(result.is_some());
        assert_eq!(result.unwrap().proxy_url, "http://localhost:9090");
    }

    #[tokio::test]
    async fn registry_returns_none_for_unknown() {
        let registry = BridgeRegistry::new();
        assert!(registry.get("unknown").await.is_none());
    }

    #[tokio::test]
    async fn registry_list_workspaces() {
        let registry = BridgeRegistry::new();

        for i in 0..3 {
            registry
                .update(BridgeAnnouncement {
                    workspace: format!("ws{i}"),
                    proxy_url: format!("http://host{i}:9090"),
                    huly_connected: true,
                    nats_connected: true,
                    ready: true,
                    uptime_secs: 0,
                    version: "0.1.0".into(),
                    timestamp: 0,
            social_id: None,
            schema_version: 0,
        })
                .await;
        }

        let workspaces = registry.list_workspaces().await;
        assert_eq!(workspaces.len(), 3);
    }

    #[tokio::test]
    async fn registry_removes_stale() {
        let registry = BridgeRegistry::new();
        registry
            .update(BridgeAnnouncement {
                workspace: "old".into(),
                proxy_url: "http://old:9090".into(),
                huly_connected: true,
                nats_connected: true,
                ready: true,
                uptime_secs: 0,
                version: "0.1.0".into(),
                timestamp: 0,
            social_id: None,
            schema_version: 0,
        })
            .await;

        // With a very short timeout, it should be considered stale after a small delay
        tokio::time::sleep(Duration::from_millis(10)).await;
        let removed = registry.remove_stale(Duration::from_millis(1)).await;
        assert_eq!(removed, vec!["old"]);
        assert!(registry.get("old").await.is_none());
    }

    #[tokio::test]
    async fn registry_update_refreshes_last_seen() {
        let registry = BridgeRegistry::new();
        let ann = BridgeAnnouncement {
            workspace: "ws1".into(),
            proxy_url: "http://localhost:9090".into(),
            huly_connected: true,
            nats_connected: true,
            ready: true,
            uptime_secs: 0,
            version: "0.1.0".into(),
            timestamp: 0,
            social_id: None,
            schema_version: 0,
        };

        registry.update(ann.clone()).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        registry.update(ann).await;

        // Should not be stale because we just refreshed
        let removed = registry.remove_stale(Duration::from_millis(50)).await;
        assert!(removed.is_empty());
    }
}
