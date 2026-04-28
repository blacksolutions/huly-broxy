//! Live integration smoke against an upstream Huly 0.7.19 server.
//!
//! These tests are gated TWICE:
//!   1. `#[ignore]` — they only run with `--ignored`.
//!   2. `HULY_INTEGRATION_URL` env var — required for any test to do real work.
//!
//! Usage:
//!     HULY_INTEGRATION_URL=https://huly.example.com \
//!     HULY_INTEGRATION_TOKEN=... \
//!     HULY_INTEGRATION_WORKSPACE=ws-slug \
//!         cargo test -p huly-bridge --test smoke_0_7_19 -- --ignored --nocapture
//!
//! With no env vars set, every test prints
//! "skipping: HULY_INTEGRATION_URL not set" and exits 0. This keeps the
//! suite safe to run on any dev machine while still allowing CI to opt-in.

use huly_bridge::huly::rest::{RestClient, SearchOptions};

/// Env-var name carrying the upstream Huly base URL
/// (e.g. `https://huly.example.com`). When unset, every test in this file
/// becomes a no-op.
const ENV_URL: &str = "HULY_INTEGRATION_URL";
/// Env-var name carrying a bearer token for authenticated calls.
const ENV_TOKEN: &str = "HULY_INTEGRATION_TOKEN";
/// Env-var name carrying the workspace slug to target.
const ENV_WORKSPACE: &str = "HULY_INTEGRATION_WORKSPACE";

/// Read the live base URL from the environment, returning `None` (with a
/// visible skip message) when the var is missing.
///
/// Centralizes the skip semantics so every test in this file decides
/// "should I run?" identically.
fn live_url() -> Option<String> {
    match std::env::var(ENV_URL) {
        Ok(v) if !v.is_empty() => Some(v),
        _ => {
            eprintln!("skipping: {ENV_URL} not set");
            None
        }
    }
}

/// Read URL + token + workspace as a triple. Returns `None` (with skip
/// message) when any of the three is missing — the test cannot do useful
/// work without all three, so partial creds are treated as "not configured".
fn live_creds() -> Option<(String, String, String)> {
    let url = live_url()?;
    let token = match std::env::var(ENV_TOKEN) {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("skipping: {ENV_TOKEN} not set");
            return None;
        }
    };
    let workspace = match std::env::var(ENV_WORKSPACE) {
        Ok(v) if !v.is_empty() => v,
        _ => {
            eprintln!("skipping: {ENV_WORKSPACE} not set");
            return None;
        }
    };
    Some((url, token, workspace))
}

/// `GET {base}/config.json` — the unauthenticated bootstrap call.
///
/// Asserts only that the response decodes into `ServerConfig`; specific
/// URLs are deployment-dependent and intentionally not checked.
#[tokio::test]
#[ignore]
async fn live_get_config_returns_known_urls() {
    let Some(url) = live_url() else { return };
    // Token is ignored on this endpoint, but `RestClient::new` requires one.
    let client = RestClient::new(url, "");
    let (cfg, _rl) = client
        .get_config()
        .await
        .expect("live get_config should succeed against a real 0.7.19 server");
    // Wire-contract assertion: the struct decoded. We do not assert any
    // specific URL because that varies per deployment.
    let _ = cfg;
}

/// `GET /api/v1/account/{workspace}` — bearer-authed account lookup.
///
/// Asserts the response decodes and carries a non-empty `uuid`. Other
/// fields (role, social IDs) vary per account and are not asserted.
#[tokio::test]
#[ignore]
async fn live_get_account_for_workspace_succeeds() {
    let Some((url, token, workspace)) = live_creds() else { return };
    let client = RestClient::new(url, token);
    let (account, _rl) = client
        .get_account(&workspace)
        .await
        .expect("live get_account should succeed with valid creds");
    assert!(
        !account.uuid.is_empty(),
        "account uuid should be non-empty on a real server"
    );
}

/// `GET /api/v1/search-fulltext/{workspace}` with a benign query.
///
/// Asserts the response decodes into `SearchResult`; an empty `docs` is
/// acceptable since the deployment may have nothing matching "test".
#[tokio::test]
#[ignore]
async fn live_search_fulltext_decodes_response() {
    let Some((url, token, workspace)) = live_creds() else { return };
    let client = RestClient::new(url, token);
    let opts = SearchOptions::default();
    let (result, _rl) = client
        .search_fulltext(&workspace, "test", &opts)
        .await
        .expect("live search_fulltext should succeed with valid creds");
    // Wire-contract assertion only: docs is a Vec (may be empty), total
    // is optional. Touching the field forces decode validation above.
    let _ = result.docs;
    let _ = result.total;
}

/// `GET /api/v1/load-model/{workspace}` — non-empty Tx array on any real
/// server. Model bootstrap is mandatory in 0.7.19, so an empty array
/// would indicate a contract regression.
#[tokio::test]
#[ignore]
async fn live_load_model_returns_array() {
    let Some((url, token, workspace)) = live_creds() else { return };
    let client = RestClient::new(url, token);
    let (model, _rl) = client
        .get_model(&workspace, false)
        .await
        .expect("live get_model should succeed with valid creds");
    assert!(
        !model.is_empty(),
        "model array should be non-empty on a real 0.7.19 server"
    );
}
