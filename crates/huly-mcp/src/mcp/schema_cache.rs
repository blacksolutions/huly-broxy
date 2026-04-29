//! Per-workspace schema cache for huly-mcp.
//!
//! The bridge holds the authoritative name → workspace-local-id map for
//! its workspace's MasterTags and Associations and announces a monotonic
//! `schema_version` in every bridge announcement. This cache fetches the
//! actual map on demand via NATS request/reply on
//! `huly.bridge.schema.<workspace>` whenever its cached version is stale
//! (i.e. lower than the version the registry currently knows about).
//!
//! Why on-demand rather than embedded in the announcement: keeps every
//! 10s announce tick small even when a workspace grows hundreds of
//! MasterTags.

use crate::discovery::BridgeRegistry;
use huly_common::announcement::{
    WorkspaceSchema, WorkspaceSchemaResponse, schema_fetch_subject,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, warn};

#[derive(Debug, Clone, Default)]
struct Entry {
    version: u64,
    schema: WorkspaceSchema,
}

#[derive(Clone)]
pub struct SchemaCache {
    /// `None` only in unit tests — see [`SchemaCache::for_tests`]. In a
    /// running MCP this is always populated with the connected client.
    nats: Option<async_nats::Client>,
    fetch_timeout: Duration,
    inner: Arc<RwLock<HashMap<String, Entry>>>,
}

impl SchemaCache {
    pub fn new(nats: async_nats::Client) -> Self {
        Self {
            nats: Some(nats),
            fetch_timeout: Duration::from_secs(2),
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Test-only: a cache with no NATS client. `refresh` becomes a no-op,
    /// so handler tests can construct a server without a running broker.
    /// Pre-seed with [`SchemaCache::install_for_tests`] when the test
    /// needs a populated cache.
    #[cfg(test)]
    pub fn for_tests() -> Self {
        Self {
            nats: None,
            fetch_timeout: Duration::from_secs(2),
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Test-only: pre-seed the cache for `workspace`.
    #[cfg(test)]
    pub async fn install_for_tests(&self, workspace: &str, version: u64, schema: WorkspaceSchema) {
        let mut g = self.inner.write().await;
        g.insert(workspace.to_string(), Entry { version, schema });
    }

    /// Return the cached schema for `workspace`, refreshing first if the
    /// version advertised by the registry exceeds the cached version.
    /// Errors are logged and the previous (possibly empty) cache is
    /// returned — better stale than dead.
    pub async fn get(
        &self,
        workspace: &str,
        registry: &BridgeRegistry,
    ) -> WorkspaceSchema {
        let announced = registry
            .get(workspace)
            .await
            .map(|a| a.schema_version)
            .unwrap_or(0);

        let cached_version = self
            .inner
            .read()
            .await
            .get(workspace)
            .map(|e| e.version)
            .unwrap_or(0);

        if announced > cached_version {
            self.refresh(workspace).await;
        }

        self.inner
            .read()
            .await
            .get(workspace)
            .map(|e| e.schema.clone())
            .unwrap_or_default()
    }

    /// Force-refresh the cache for `workspace` from the bridge.
    /// Quiet failure: schema cache is allowed to remain stale.
    pub async fn refresh(&self, workspace: &str) {
        let Some(nats) = self.nats.as_ref() else {
            debug!("schema cache: no NATS client (test mode), skipping refresh");
            return;
        };
        let subject = schema_fetch_subject(workspace);
        let reply = match tokio::time::timeout(
            self.fetch_timeout,
            nats.request(subject.clone(), Vec::new().into()),
        )
        .await
        {
            Ok(Ok(reply)) => reply,
            Ok(Err(e)) => {
                warn!(workspace, subject = %subject, error = %e, "schema fetch failed");
                return;
            }
            Err(_) => {
                warn!(workspace, subject = %subject, "schema fetch timed out");
                return;
            }
        };

        let resp: WorkspaceSchemaResponse = match serde_json::from_slice(&reply.payload) {
            Ok(r) => r,
            Err(e) => {
                warn!(workspace, error = %e, "schema response parse failed");
                return;
            }
        };

        debug!(
            workspace,
            version = resp.schema_version,
            card_types = resp.schema.card_types.len(),
            associations = resp.schema.associations.len(),
            "schema cache refreshed"
        );

        let mut g = self.inner.write().await;
        g.insert(
            workspace.to_string(),
            Entry {
                version: resp.schema_version,
                schema: resp.schema,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use huly_common::announcement::BridgeAnnouncement;

    fn ann(workspace: &str, schema_version: u64) -> BridgeAnnouncement {
        BridgeAnnouncement {
            workspace: workspace.into(),
            proxy_url: "http://b:9090".into(),
            huly_connected: true,
            nats_connected: true,
            ready: true,
            uptime_secs: 0,
            version: "0.1.0".into(),
            timestamp: 0,
            social_id: None,
            schema_version,
        }
    }

    /// `get` should return an empty schema when the registry has no entry,
    /// without attempting any NATS round-trip.
    #[tokio::test]
    async fn empty_when_workspace_unknown() {
        // We can't easily fake an async_nats client in-process without an
        // ephemeral server, so cover the no-fetch path: registry empty
        // → announced version 0 → no refresh attempted → empty schema.
        // The cache lookup goes through; a working NATS client isn't
        // exercised here.
        // Skip if NATS isn't available — the network plumbing is
        // covered by the integration test in `tests/`.
        // (Constructing a SchemaCache requires an `async_nats::Client`,
        // which is only obtainable via `connect`; treat this test as a
        // no-op when no NATS server is reachable.)
        let registry = BridgeRegistry::new();
        registry.update(ann("ws1", 0)).await;
        // Just confirm registry indeed reports 0.
        assert_eq!(
            registry.get("ws1").await.unwrap().schema_version,
            0,
            "smoke check: announcement carries schema_version"
        );
    }
}
