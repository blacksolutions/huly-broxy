//! Per-workspace [`PlatformClient`] factory backed by the JWT broker.
//!
//! The MCP server calls [`HulyClientFactory::for_workspace`] on every tool
//! invocation. The factory:
//!
//! 1. Looks up a cached entry. If present and `now < refresh_at_ms`, returns
//!    the cached `Arc<dyn PlatformClient>`.
//! 2. Otherwise issues a `MintRequest` on `huly.bridge.mint` (P3 helper),
//!    decodes the [`MintResponse`], and constructs a fresh
//!    [`RestHulyClient`].
//! 3. Caches the new client + the broker-supplied
//!    `(transactor_url, rest_base_url, workspace_uuid, account_service_jwt,
//!    refresh_at_ms)` tuple keyed by workspace slug.
//!
//! The factory also owns a shared [`SchemaCache`] keyed by workspace slug;
//! callers that need workspace-local MasterTag/Association ids go through
//! [`HulyClientFactory::schema`] which fetches `loadModel` (D9) on miss
//! and caches the resolved [`WorkspaceSchema`].

use crate::jwt_broker_client::{MintClientError, MintOutcome, request_jwt};
use huly_client::client::{ClientError, PlatformClient};
use huly_client::rest_huly_client::RestHulyClient;
use huly_client::schema_resolver::{self, SchemaHandle};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Result of a JWT-broker round-trip plus the constructed client.
#[derive(Clone)]
struct WorkspaceEntry {
    client: Arc<RestHulyClient>,
    /// Wall-clock epoch ms when the JWT becomes invalid. We pre-empt at
    /// `refresh_at_ms` (typically 60s before).
    refresh_at_ms: u64,
    /// Account-service JWT — required by `huly_list_workspaces`.
    account_service_jwt: Option<String>,
    /// Accounts-service base URL minted alongside the JWT. `None` if the
    /// bridge did not advertise one — callers that need it must surface
    /// the gap.
    accounts_url: Option<String>,
    /// Schema cache, lazily populated on first access.
    schema: SchemaHandle,
    /// When the schema was last refreshed (process clock; `None` = never).
    schema_refreshed_at: Option<Instant>,
}

/// Process-wide factory.
///
/// Cheap to clone (`Arc` internally). Wired once at MCP startup and shared
/// across every tool invocation.
#[derive(Clone)]
pub struct HulyClientFactory {
    nats: async_nats::Client,
    agent_id: String,
    /// Schema TTL fallback: refetch every workspace's `loadModel` after
    /// this much wall-clock time, even when no invalidation event arrives
    /// over `huly.event.tx.core.class.*`. D9 prescribes 5 minutes.
    schema_ttl: Duration,
    inner: Arc<RwLock<HashMap<String, WorkspaceEntry>>>,
}

impl HulyClientFactory {
    pub fn new(nats: async_nats::Client, agent_id: impl Into<String>) -> Self {
        Self {
            nats,
            agent_id: agent_id.into(),
            schema_ttl: Duration::from_secs(300),
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn with_schema_ttl(mut self, ttl: Duration) -> Self {
        self.schema_ttl = ttl;
        self
    }

    /// Resolve (or mint) a [`PlatformClient`] for `workspace`. Returns the
    /// client as `Arc<dyn PlatformClient>` so MCP tool code stays generic.
    pub async fn for_workspace(
        &self,
        workspace: &str,
    ) -> Result<Arc<dyn PlatformClient>, FactoryError> {
        let entry = self.entry(workspace).await?;
        Ok(entry.client.clone() as Arc<dyn PlatformClient>)
    }

    /// Same as [`for_workspace`] but exposes the concrete REST client
    /// (so callers that want `load_model` or `raw_tx` can reach the
    /// non-trait surface).
    pub async fn rest_for_workspace(
        &self,
        workspace: &str,
    ) -> Result<Arc<RestHulyClient>, FactoryError> {
        Ok(self.entry(workspace).await?.client)
    }

    /// Account-service JWT for `workspace`'s session, refreshed alongside
    /// the workspace JWT. `None` if the broker did not return one (older
    /// bridge or single-tenant configuration).
    pub async fn account_service_jwt(
        &self,
        workspace: &str,
    ) -> Result<Option<String>, FactoryError> {
        Ok(self.entry(workspace).await?.account_service_jwt)
    }

    /// Accounts-service base URL (e.g. `https://huly.example/_accounts`)
    /// announced by the bridge alongside the workspace JWT. `None` if the
    /// bridge did not advertise one — callers that need it (e.g.
    /// `huly_list_workspaces`) must error rather than guess.
    pub async fn accounts_url(
        &self,
        workspace: &str,
    ) -> Result<Option<String>, FactoryError> {
        Ok(self.entry(workspace).await?.accounts_url)
    }

    /// Get the resolved schema for `workspace`, refreshing if the cache is
    /// older than `schema_ttl` or if it's never been populated. On failure,
    /// returns the previous cached schema (possibly empty) — better stale
    /// than dead, matching the existing `mcp::schema_cache` policy.
    pub async fn schema(&self, workspace: &str) -> Result<SchemaHandle, FactoryError> {
        let entry = self.entry(workspace).await?;
        let needs_refresh = match entry.schema_refreshed_at {
            None => true,
            Some(t) => t.elapsed() >= self.schema_ttl,
        };
        if needs_refresh {
            self.refresh_schema(workspace).await;
        }
        // Re-read after refresh in case the entry was rebuilt.
        Ok(self.entry(workspace).await?.schema)
    }

    /// Force a schema refresh for `workspace`. Used by the event-driven
    /// invalidation path (D9): when MCP sees `huly.event.tx.core.class.*`
    /// it calls this to re-resolve workspace-local MasterTag / Association
    /// ids on the next tool call.
    pub async fn invalidate_schema(&self, workspace: &str) {
        let mut g = self.inner.write().await;
        if let Some(entry) = g.get_mut(workspace) {
            entry.schema_refreshed_at = None;
            debug!(workspace, "schema cache invalidated");
        }
    }

    /// Forget the cached client for `workspace` so the next call mints a
    /// fresh JWT. Used by tool error handlers that observe an
    /// authentication failure.
    pub async fn forget(&self, workspace: &str) {
        let mut g = self.inner.write().await;
        g.remove(workspace);
    }

    // ------------------------------------------------------------------ //

    async fn entry(&self, workspace: &str) -> Result<WorkspaceEntry, FactoryError> {
        // Fast path: cached + not yet due for refresh.
        if let Some(e) = self.inner.read().await.get(workspace).cloned()
            && now_ms() < e.refresh_at_ms
        {
            return Ok(e);
        }
        // Slow path: mint via NATS.
        self.refresh_jwt(workspace).await
    }

    async fn refresh_jwt(&self, workspace: &str) -> Result<WorkspaceEntry, FactoryError> {
        info!(workspace, agent = %self.agent_id, "minting workspace JWT");
        let outcome = request_jwt(&self.nats, workspace, &self.agent_id).await?;
        let resp = match outcome {
            MintOutcome::Ok(r) => r,
            MintOutcome::Failed(err) => return Err(FactoryError::Mint(err.code, err.message)),
        };
        let client = Arc::new(RestHulyClient::new(
            &resp.rest_base_url,
            &resp.workspace_uuid,
            &resp.jwt,
        ));
        let entry = WorkspaceEntry {
            client,
            refresh_at_ms: resp.refresh_at_ms,
            account_service_jwt: resp.account_service_jwt,
            accounts_url: resp.accounts_url,
            schema: SchemaHandle::new(),
            schema_refreshed_at: None,
        };
        let mut g = self.inner.write().await;
        g.insert(workspace.to_string(), entry.clone());
        Ok(entry)
    }

    async fn refresh_schema(&self, workspace: &str) {
        let entry = match self.inner.read().await.get(workspace).cloned() {
            Some(e) => e,
            None => return,
        };
        match schema_resolver::refresh(&*entry.client, &entry.schema).await {
            Ok(version) => {
                debug!(workspace, version, "schema cache refreshed via loadModel");
                let mut g = self.inner.write().await;
                if let Some(e) = g.get_mut(workspace) {
                    e.schema_refreshed_at = Some(Instant::now());
                }
            }
            Err(err) => {
                warn!(workspace, error = %err, "schema refresh failed; keeping previous cache");
            }
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, thiserror::Error)]
pub enum FactoryError {
    #[error("jwt broker request failed: {0}")]
    BrokerRequest(#[from] MintClientError),
    #[error("jwt broker rejected mint: {0}: {1}")]
    Mint(String, String),
    #[error("client error: {0}")]
    Client(#[from] ClientError),
}


#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use huly_common::mint::{MINT_SUBJECT, MintReply, MintRequest, MintResponse};

    /// Spin up an in-process NATS server using `async-nats`'s test harness if
    /// available, otherwise gate behind an env var. Most CI runs without a
    /// NATS server, so we treat broker round-trips as ignored unless one is
    /// present.
    async fn connect_pair() -> Option<(async_nats::Client, async_nats::Client)> {
        let url = std::env::var("HULY_TEST_NATS_URL")
            .unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
        let a = async_nats::connect(&url).await.ok()?;
        let b = async_nats::connect(&url).await.ok()?;
        Some((a, b))
    }

    #[tokio::test]
    #[ignore = "requires a running NATS server (set HULY_TEST_NATS_URL)"]
    async fn for_workspace_mints_jwt_and_caches_client() {
        let Some((client, broker)) = connect_pair().await else {
            return;
        };
        let mut sub = broker.subscribe(MINT_SUBJECT.to_string()).await.unwrap();
        let mock_broker = tokio::spawn(async move {
            let msg = sub.next().await.unwrap();
            let reply_to = msg.reply.unwrap();
            let req: MintRequest = serde_json::from_slice(&msg.payload).unwrap();
            assert_eq!(req.workspace, "muhasebot");
            assert_eq!(req.agent_id, "agent-test");
            let resp = MintResponse {
                jwt: "ws-jwt".into(),
                account_service_jwt: Some("acct-jwt".into()),
                expires_at_ms: now_ms() + 3_600_000,
                refresh_at_ms: now_ms() + 3_540_000,
                transactor_url: "wss://t.example/".into(),
                rest_base_url: "https://r.example".into(),
                workspace_uuid: "uuid-1".into(),
                accounts_url: Some("https://r.example/_accounts".into()),
            };
            broker
                .publish(reply_to, serde_json::to_vec(&MintReply::Ok(resp)).unwrap().into())
                .await
                .unwrap();
            broker.flush().await.unwrap();
        });
        let factory = HulyClientFactory::new(client, "agent-test");
        let c1 = factory.for_workspace("muhasebot").await.unwrap();
        let c2 = factory.for_workspace("muhasebot").await.unwrap();
        // Same Arc instance after the warm-cache hit.
        assert!(Arc::ptr_eq(&c1, &c2));
        let acct = factory.account_service_jwt("muhasebot").await.unwrap();
        assert_eq!(acct.as_deref(), Some("acct-jwt"));
        mock_broker.await.unwrap();
    }

    /// Without a NATS broker we can still drive the cache directly via
    /// `inner` to verify `invalidate_schema` clears the timestamp so the
    /// next `schema()` call would re-fetch.
    #[tokio::test]
    async fn invalidate_schema_clears_refreshed_at() {
        // Build a factory we don't connect — just need an async_nats::Client
        // value. If no NATS server is running, skip the test.
        let Ok(c) = async_nats::connect("nats://127.0.0.1:4222").await else {
            return;
        };
        let factory = HulyClientFactory::new(c, "agent");
        let entry = WorkspaceEntry {
            client: Arc::new(RestHulyClient::new(
                "http://x.example",
                "uuid-test",
                "tok",
            )),
            refresh_at_ms: now_ms() + 60_000,
            account_service_jwt: None,
            accounts_url: None,
            schema: SchemaHandle::new(),
            schema_refreshed_at: Some(Instant::now()),
        };
        factory
            .inner
            .write()
            .await
            .insert("ws".into(), entry);

        factory.invalidate_schema("ws").await;
        let g = factory.inner.read().await;
        assert!(g.get("ws").unwrap().schema_refreshed_at.is_none());
    }

    #[tokio::test]
    async fn accounts_url_returned_from_seeded_entry() {
        let Ok(c) = async_nats::connect("nats://127.0.0.1:4222").await else {
            return;
        };
        let factory = HulyClientFactory::new(c, "agent");
        let entry = WorkspaceEntry {
            client: Arc::new(RestHulyClient::new("http://x", "u", "t")),
            refresh_at_ms: now_ms() + 60_000,
            account_service_jwt: Some("acct".into()),
            accounts_url: Some("https://h.example/_accounts".into()),
            schema: SchemaHandle::new(),
            schema_refreshed_at: None,
        };
        factory.inner.write().await.insert("ws".into(), entry);
        let url = factory.accounts_url("ws").await.unwrap();
        assert_eq!(url.as_deref(), Some("https://h.example/_accounts"));
    }

    #[tokio::test]
    async fn forget_removes_workspace_from_cache() {
        let Ok(c) = async_nats::connect("nats://127.0.0.1:4222").await else {
            return;
        };
        let factory = HulyClientFactory::new(c, "agent");
        let entry = WorkspaceEntry {
            client: Arc::new(RestHulyClient::new("http://x", "u", "t")),
            refresh_at_ms: now_ms() + 60_000,
            account_service_jwt: None,
            accounts_url: None,
            schema: SchemaHandle::new(),
            schema_refreshed_at: None,
        };
        factory.inner.write().await.insert("ws".into(), entry);
        factory.forget("ws").await;
        assert!(factory.inner.read().await.get("ws").is_none());
    }

    #[test]
    fn factory_error_displays_mint_code_and_message() {
        let e = FactoryError::Mint("unknown_workspace".into(), "no such ws".into());
        let s = format!("{e}");
        assert!(s.contains("unknown_workspace"));
        assert!(s.contains("no such ws"));
    }

    /// Sanity-check that the factory's TTL knob round-trips. Direct
    /// observability into the cache is intentionally limited to keep the
    /// public surface narrow; this test only asserts the builder works.
    #[tokio::test]
    async fn factory_with_schema_ttl_overrides_default() {
        // Use a dummy NATS-less factory by constructing one with a closed
        // connection. We won't exercise the network; just verify the
        // builder.
        // Constructing async_nats::Client requires connecting; skip if none.
        let Ok(c) = async_nats::connect("nats://127.0.0.1:4222").await else {
            return;
        };
        let f = HulyClientFactory::new(c, "agent")
            .with_schema_ttl(Duration::from_secs(7));
        assert_eq!(f.schema_ttl, Duration::from_secs(7));
    }
}
