//! End-to-end integration test for the Phase 2 REST client against the
//! shared `MockHuly` harness. Most behaviour is unit-tested in
//! `huly::rest::tests` with `wiremock`; this file exercises the wiring
//! through the in-process axum mock to guarantee that lib + harness
//! agree on the on-the-wire format.

mod common;

use huly_client::rest::{self, RestClient, ServerConfigCache};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// Minimal axum-based mock that mirrors `MockHuly::config_json`. The full
/// `MockHuly` struct lives in another integration test binary; integration
/// tests in cargo each compile to their own binary, so duplicating this
/// tiny handler is cheaper than carving out a shared crate.
struct MockHuly {
    base_url: String,
    _handle: JoinHandle<()>,
}

impl MockHuly {
    async fn start() -> Self {
        Self::start_with(true).await
    }

    /// Start the mock with `/config.json` either serving the canned 4-URL
    /// payload (`true`) or returning 404 so we can exercise the
    /// legacy-server fallback path (`false`).
    async fn start_with(serve_config: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let app = if serve_config {
            axum::Router::new().route(
                "/config.json",
                axum::routing::get(|| async {
                    axum::Json(json!({
                        "ACCOUNTS_URL": "http://mock/accounts",
                        "COLLABORATOR_URL": "http://mock/collab",
                        "FILES_URL": "http://mock/files",
                        "UPLOAD_URL": "http://mock/upload"
                    }))
                }),
            )
        } else {
            // Legacy server: /config.json absent → 404.
            axum::Router::new()
        };
        let handle = tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service()).await;
        });
        // Give the server a tick to start accepting connections.
        for _ in 0..10 {
            if tokio::net::TcpStream::connect(addr).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        Self {
            base_url: format!("http://{addr}"),
            _handle: handle,
        }
    }

    fn url(&self) -> &str {
        &self.base_url
    }
}

#[tokio::test]
async fn rest_client_get_config_against_mock_huly() {
    let mock = MockHuly::start().await;
    let client = RestClient::new(mock.url(), "ignored-token");

    let (cfg, rl) = client.get_config().await.expect("get_config");
    assert_eq!(cfg.accounts_url.as_deref(), Some("http://mock/accounts"));
    assert_eq!(cfg.collaborator_url.as_deref(), Some("http://mock/collab"));
    assert_eq!(cfg.files_url.as_deref(), Some("http://mock/files"));
    assert_eq!(cfg.upload_url.as_deref(), Some("http://mock/upload"));
    assert!(rl.is_empty(), "no rate-limit headers expected from mock");

    // Touch the helper to silence dead-code warnings — `tests/common`
    // gets re-compiled per integration binary.
    let _ = common::fixture_path("noop");
}

/// Bootstrap path: `bootstrap_server_config` populates a fresh
/// `ServerConfigCache` with all four URLs when the server exposes
/// `/config.json` (Issue #21 / R8).
#[tokio::test]
async fn bootstrap_populates_cache_against_mock_huly() {
    let mock = MockHuly::start().await;
    let client = RestClient::new(mock.url(), "ignored-token");
    let cache = ServerConfigCache::new();

    rest::bootstrap_server_config(&client, &cache).await;

    assert!(cache.is_populated(), "cache should hold a ServerConfig");
    assert_eq!(cache.accounts_url().as_deref(), Some("http://mock/accounts"));
    assert_eq!(cache.collaborator_url().as_deref(), Some("http://mock/collab"));
    assert_eq!(cache.files_url().as_deref(), Some("http://mock/files"));
    assert_eq!(cache.upload_url().as_deref(), Some("http://mock/upload"));
}

/// Legacy-server path: `/config.json` returns 404, the helper logs a
/// warn and the cache stays empty so callers fall back to operator
/// config (Issue #21 / R8).
#[tokio::test]
async fn bootstrap_keeps_cache_empty_when_config_json_missing() {
    let mock = MockHuly::start_with(false).await;
    let client = RestClient::new(mock.url(), "ignored-token");
    let cache = ServerConfigCache::new();

    // Best-effort: must not panic, must not error.
    rest::bootstrap_server_config(&client, &cache).await;

    assert!(!cache.is_populated(), "legacy 404 must leave cache empty");
    assert!(cache.accounts_url().is_none());
    assert!(cache.collaborator_url().is_none());
    assert!(cache.files_url().is_none());
    assert!(cache.upload_url().is_none());
}

/// `ServerConfig` deserializes a fixture-style JSON payload — guards the
/// SCREAMING_SNAKE_CASE serde renames at module boundary.
#[test]
fn server_config_deserializes_camel_screaming_keys() {
    let raw: Value = json!({
        "ACCOUNTS_URL": "https://acc",
        "COLLABORATOR_URL": "https://collab"
    });
    let cfg: huly_client::rest::ServerConfig =
        serde_json::from_value(raw).expect("decode");
    assert_eq!(cfg.accounts_url.as_deref(), Some("https://acc"));
    assert_eq!(cfg.collaborator_url.as_deref(), Some("https://collab"));
    assert!(cfg.files_url.is_none());
    assert!(cfg.upload_url.is_none());
}
