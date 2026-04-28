//! REST client for the Huly transactor's `/api/v1/*` surface (0.7.19+).
//!
//! Sibling of [`crate::huly::client::HulyClient`] (which speaks the
//! WebSocket JSON-RPC protocol). The two share no state — REST endpoints
//! exposed in 0.7.19 (search-fulltext, ensure-person, account, model,
//! domain-request, …) are best modelled as plain HTTP instead of being
//! squeezed through the transactor's WS framing.
//!
//! Every REST response can ride snappy compression (`Content-Encoding:
//! snappy`) and may carry rate-limit headers; both are handled centrally
//! by [`RestClient::send`].

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use reqwest::header::{HeaderMap, AUTHORIZATION, CONTENT_ENCODING};
use reqwest::{Client, Method, RequestBuilder, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::bridge::rate_limit::RateLimitInfo;
use crate::huly::rpc::Account;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Bootstrap configuration served from `{base}/config.json` (no auth, no
/// workspace). All fields are optional — small/dev deployments may omit
/// any of them.
///
/// Wire format uses SCREAMING_SNAKE_CASE keys (`ACCOUNTS_URL`, …); this
/// struct mirrors `huly.core/packages/api-client/src/config.ts`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct ServerConfig {
    #[serde(rename = "ACCOUNTS_URL", default, skip_serializing_if = "Option::is_none")]
    pub accounts_url: Option<String>,
    #[serde(rename = "COLLABORATOR_URL", default, skip_serializing_if = "Option::is_none")]
    pub collaborator_url: Option<String>,
    #[serde(rename = "FILES_URL", default, skip_serializing_if = "Option::is_none")]
    pub files_url: Option<String>,
    #[serde(rename = "UPLOAD_URL", default, skip_serializing_if = "Option::is_none")]
    pub upload_url: Option<String>,
}

/// Process-wide cache for the bootstrap [`ServerConfig`].
///
/// Populated once per bridge run by [`bootstrap_server_config`] right
/// after auth, then read by any downstream module that wants to prefer
/// the server-advertised URLs (`ACCOUNTS_URL`, `COLLABORATOR_URL`,
/// `FILES_URL`, `UPLOAD_URL`) over the operator-supplied
/// `huly.accounts_url`.
///
/// Holds an `Option<ServerConfig>`: `None` means the cache is empty
/// (legacy server returned 404 / network blip). Cheap to clone — internally
/// reference-counted.
#[derive(Debug, Clone, Default)]
pub struct ServerConfigCache {
    inner: Arc<RwLock<Option<ServerConfig>>>,
}

impl ServerConfigCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the cached value. Pass `None` to mark "no config available".
    pub fn set(&self, cfg: Option<ServerConfig>) {
        let mut guard = self.inner.write().expect("ServerConfigCache write poisoned");
        *guard = cfg;
    }

    /// Snapshot the current cached value (cloned).
    pub fn get(&self) -> Option<ServerConfig> {
        self.inner.read().expect("ServerConfigCache read poisoned").clone()
    }

    /// True iff the cache currently holds a value.
    pub fn is_populated(&self) -> bool {
        self.inner.read().expect("ServerConfigCache read poisoned").is_some()
    }

    pub fn accounts_url(&self) -> Option<String> {
        self.get().and_then(|c| c.accounts_url)
    }

    pub fn collaborator_url(&self) -> Option<String> {
        self.get().and_then(|c| c.collaborator_url)
    }

    pub fn files_url(&self) -> Option<String> {
        self.get().and_then(|c| c.files_url)
    }

    pub fn upload_url(&self) -> Option<String> {
        self.get().and_then(|c| c.upload_url)
    }
}

/// Best-effort bootstrap: call `GET {base}/config.json` once and store
/// the result in `cache`. On any failure (404 from a legacy server,
/// network error, malformed JSON) log a warning and leave the cache
/// holding `None` so downstream code falls back to operator config.
///
/// This intentionally never returns an error — the caller should not
/// block startup on the optional `/config.json` endpoint.
pub async fn bootstrap_server_config(rest: &RestClient, cache: &ServerConfigCache) {
    match rest.get_config().await {
        Ok((cfg, _rl)) => {
            cache.set(Some(cfg));
        }
        Err(err) => {
            warn!(
                error = %err,
                "GET /config.json failed; continuing without cached server config",
            );
            cache.set(None);
        }
    }
}

// `Account` and `SocialId` are now unified — see `crate::huly::rpc::{Account,
// SocialId}`. The REST `GET /api/v1/account/{workspace}` endpoint and the WS
// hello handshake decode into the same `Account` struct.

/// Result of `GET /api/v1/search-fulltext/{workspace}`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SearchResult {
    #[serde(default)]
    pub docs: Vec<SearchResultDoc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SearchResultDoc {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(rename = "shortTitle", default, skip_serializing_if = "Option::is_none")]
    pub short_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "emojiIcon", default, skip_serializing_if = "Option::is_none")]
    pub emoji_icon: Option<String>,
}

/// Optional filters/limits for a `search_fulltext` call.
#[derive(Debug, Clone, Default)]
pub struct SearchOptions {
    /// Restrict to documents of the given class refs.
    pub classes: Option<Vec<String>>,
    /// Restrict to documents in the given spaces.
    pub spaces: Option<Vec<String>>,
    /// Cap on returned docs.
    pub limit: Option<u32>,
}

/// Wrapper for the response of `POST /api/v1/request/{domain}/{workspace}`.
///
/// `T` is whatever the per-domain handler returns. Callers pick the
/// concrete type at the call site.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DomainResult<T> {
    pub domain: String,
    pub value: T,
}

/// Body of `POST /api/v1/ensure-person/{workspace}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnsurePersonRequest {
    pub social_type: String,
    pub social_value: String,
    pub first_name: String,
    pub last_name: String,
}

/// Response of `POST /api/v1/ensure-person/{workspace}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnsurePersonResponse {
    pub uuid: String,
    pub social_id: String,
    pub local_person: String,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum RestError {
    #[error("network error: {0}")]
    Network(String),

    #[error("upstream error: HTTP {status}: {body}")]
    Upstream { status: u16, body: String },

    /// 429 specifically — surfaces parsed rate-limit metadata so callers
    /// can decide how long to back off.
    #[error("rate limited (HTTP 429); retry-after-ms={:?}", .rate_limit.retry_after_ms)]
    RateLimited { rate_limit: RateLimitInfo, body: String },

    #[error("response decode error: {0}")]
    Decode(String),

    #[error("invalid url: {0}")]
    Url(String),
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// REST client for the Huly transactor.
///
/// Holds a base URL plus a bearer token; constructed once per workspace
/// session. Cheap to clone (`reqwest::Client` is internally
/// reference-counted).
#[derive(Debug, Clone)]
pub struct RestClient {
    http: Client,
    base_url: String,
    token: String,
}

impl RestClient {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token: token.into(),
        }
    }

    /// Fetch `GET {base}/config.json`. Bootstrap call — no auth header,
    /// no workspace segment. The result is small and rarely changes;
    /// callers cache it for the lifetime of the bridge process.
    pub async fn get_config(&self) -> Result<(ServerConfig, RateLimitInfo), RestError> {
        let url = self.url("/config.json", &[])?;
        // Bootstrap is unauthenticated.
        let req = self.http.request(Method::GET, url);
        self.send(req).await
    }

    /// `GET /api/v1/account/{workspace}` → full account view.
    pub async fn get_account(
        &self,
        workspace: &str,
    ) -> Result<(Account, RateLimitInfo), RestError> {
        let path = format!("/api/v1/account/{workspace}");
        let url = self.url(&path, &[])?;
        let req = self.authed(Method::GET, url);
        self.send(req).await
    }

    /// `GET /api/v1/load-model/{workspace}?full={full}` → raw Tx array.
    ///
    /// The bridge does not interpret model state; we keep the response
    /// as `Vec<serde_json::Value>` so callers can pass it through
    /// untouched.
    pub async fn get_model(
        &self,
        workspace: &str,
        full: bool,
    ) -> Result<(Vec<serde_json::Value>, RateLimitInfo), RestError> {
        let path = format!("/api/v1/load-model/{workspace}");
        let q: &[(&str, &str)] = if full { &[("full", "true")] } else { &[] };
        let url = self.url(&path, q)?;
        let req = self.authed(Method::GET, url);
        self.send(req).await
    }

    /// `POST /api/v1/ensure-person/{workspace}`.
    pub async fn ensure_person(
        &self,
        workspace: &str,
        body: &EnsurePersonRequest,
    ) -> Result<(EnsurePersonResponse, RateLimitInfo), RestError> {
        let path = format!("/api/v1/ensure-person/{workspace}");
        let url = self.url(&path, &[])?;
        let req = self.authed(Method::POST, url).json(body);
        self.send(req).await
    }

    /// `GET /api/v1/search-fulltext/{workspace}`.
    ///
    /// `classes` / `spaces` are encoded as JSON-encoded `{ref: true}`
    /// maps to match the reference TS client's wire format.
    pub async fn search_fulltext(
        &self,
        workspace: &str,
        query: &str,
        opts: &SearchOptions,
    ) -> Result<(SearchResult, RateLimitInfo), RestError> {
        let path = format!("/api/v1/search-fulltext/{workspace}");

        let mut params: Vec<(&'static str, String)> = vec![("query", query.to_string())];
        if let Some(classes) = opts.classes.as_ref() {
            params.push(("classes", encode_ref_map(classes)));
        }
        if let Some(spaces) = opts.spaces.as_ref() {
            params.push(("spaces", encode_ref_map(spaces)));
        }
        if let Some(limit) = opts.limit {
            params.push(("limit", limit.to_string()));
        }
        let pairs: Vec<(&str, &str)> =
            params.iter().map(|(k, v)| (*k, v.as_str())).collect();

        let url = self.url(&path, &pairs)?;
        let req = self.authed(Method::GET, url);
        self.send(req).await
    }

    /// `POST /api/v1/request/{domain}/{workspace}` → `DomainResult<T>`.
    ///
    /// Generic over the request body and response payload so each
    /// per-domain caller can pick its own concrete types.
    pub async fn domain_request<P, T>(
        &self,
        workspace: &str,
        domain: &str,
        params: &P,
    ) -> Result<(DomainResult<T>, RateLimitInfo), RestError>
    where
        P: Serialize + ?Sized,
        T: DeserializeOwned,
    {
        let path = format!("/api/v1/request/{domain}/{workspace}");
        let url = self.url(&path, &[])?;
        let req = self.authed(Method::POST, url).json(params);
        self.send(req).await
    }

    // ---- internals ---------------------------------------------------------

    fn url(&self, path: &str, query: &[(&str, &str)]) -> Result<Url, RestError> {
        let raw = format!("{}{}", self.base_url, path);
        let mut url = Url::parse(&raw).map_err(|e| RestError::Url(e.to_string()))?;
        if !query.is_empty() {
            let mut q = url.query_pairs_mut();
            for (k, v) in query {
                q.append_pair(k, v);
            }
        }
        Ok(url)
    }

    fn authed(&self, method: Method, url: Url) -> RequestBuilder {
        self.http
            .request(method, url)
            .header(AUTHORIZATION, format!("Bearer {}", self.token))
    }

    /// Drive a single request through the shared response pipeline:
    /// 4xx/5xx → structured error (with 429 carrying parsed rate-limit
    /// info); body bytes are snappy-decompressed when
    /// `Content-Encoding: snappy` and then JSON-decoded.
    async fn send<T: DeserializeOwned>(
        &self,
        req: RequestBuilder,
    ) -> Result<(T, RateLimitInfo), RestError> {
        let resp = req
            .send()
            .await
            .map_err(|e| RestError::Network(e.to_string()))?;

        let status = resp.status();
        let headers = resp.headers().clone();
        let rate_limit = RateLimitInfo::from_headers(&headers);
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| RestError::Network(e.to_string()))?;

        if status == StatusCode::TOO_MANY_REQUESTS {
            let body = String::from_utf8_lossy(&bytes).to_string();
            return Err(RestError::RateLimited { rate_limit, body });
        }
        if !status.is_success() {
            let body = String::from_utf8_lossy(&bytes).to_string();
            return Err(RestError::Upstream { status: status.as_u16(), body });
        }

        let payload = decode_body(&headers, &bytes)?;
        let value: T = serde_json::from_slice(&payload)
            .map_err(|e| RestError::Decode(e.to_string()))?;
        Ok((value, rate_limit))
    }
}

/// Snappy-decode the response body when `Content-Encoding: snappy`,
/// otherwise pass through unchanged.
fn decode_body(headers: &HeaderMap, bytes: &[u8]) -> Result<Vec<u8>, RestError> {
    let snappy = headers
        .get(CONTENT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.eq_ignore_ascii_case("snappy"))
        .unwrap_or(false);
    if !snappy {
        return Ok(bytes.to_vec());
    }
    let mut dec = snap::raw::Decoder::new();
    dec.decompress_vec(bytes)
        .map_err(|e| RestError::Decode(format!("snappy decode: {e}")))
}

/// Encode a list of refs as a JSON-encoded `{ref: true}` map. Mirrors the
/// reference TS client which sends `JSON.stringify({...refs.reduce(...)})`.
fn encode_ref_map(refs: &[String]) -> String {
    // BTreeMap → deterministic key order, helpful in tests.
    let map: BTreeMap<&str, bool> = refs.iter().map(|r| (r.as_str(), true)).collect();
    serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{
        body_json, header, header_exists, method as m_method, path as m_path, query_param,
    };
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ---- helpers ----------------------------------------------------------

    fn snappy_encode(bytes: &[u8]) -> Vec<u8> {
        let mut enc = snap::raw::Encoder::new();
        enc.compress_vec(bytes).expect("snappy encode")
    }

    // ---- get_config -------------------------------------------------------

    #[tokio::test]
    async fn get_config_decodes_screaming_snake_case() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/config.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ACCOUNTS_URL": "https://acc.example",
                "COLLABORATOR_URL": "https://collab.example",
                "FILES_URL": "https://files.example",
                "UPLOAD_URL": "https://upload.example"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "ignored");
        let (cfg, rl) = client.get_config().await.unwrap();
        assert_eq!(cfg.accounts_url.as_deref(), Some("https://acc.example"));
        assert_eq!(cfg.collaborator_url.as_deref(), Some("https://collab.example"));
        assert_eq!(cfg.files_url.as_deref(), Some("https://files.example"));
        assert_eq!(cfg.upload_url.as_deref(), Some("https://upload.example"));
        assert!(rl.is_empty());
    }

    #[tokio::test]
    async fn get_config_does_not_send_auth_header() {
        let server = MockServer::start().await;
        // Mount a strict matcher: request must NOT carry Authorization.
        // wiremock has no negative header matcher, so we record requests
        // and assert against the captured set.
        Mock::given(m_method("GET"))
            .and(m_path("/config.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "should-not-be-sent");
        let _ = client.get_config().await.unwrap();

        let received = server.received_requests().await.expect("requests");
        assert_eq!(received.len(), 1);
        assert!(
            received[0].headers.get("authorization").is_none(),
            "expected no authorization header on /config.json"
        );
    }

    #[tokio::test]
    async fn get_config_optional_fields_default_to_none() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/config.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "");
        let (cfg, _) = client.get_config().await.unwrap();
        assert!(cfg.accounts_url.is_none());
        assert!(cfg.collaborator_url.is_none());
        assert!(cfg.files_url.is_none());
        assert!(cfg.upload_url.is_none());
    }

    #[tokio::test]
    async fn get_config_decodes_snappy_response() {
        let server = MockServer::start().await;
        let body = json!({"ACCOUNTS_URL": "https://acc.example"});
        let plain = serde_json::to_vec(&body).unwrap();
        let compressed = snappy_encode(&plain);
        Mock::given(m_method("GET"))
            .and(m_path("/config.json"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Encoding", "snappy")
                    .set_body_bytes(compressed),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "");
        let (cfg, _) = client.get_config().await.unwrap();
        assert_eq!(cfg.accounts_url.as_deref(), Some("https://acc.example"));
    }

    // ---- get_account ------------------------------------------------------

    #[tokio::test]
    async fn get_account_sends_bearer_and_decodes() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/api/v1/account/ws-1"))
            .and(header("authorization", "Bearer tok"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "uuid": "u-1",
                "role": "OWNER",
                "primarySocialId": "s-1",
                "socialIds": ["s-1", "s-2"],
                "fullSocialIds": [
                    {"type": "github", "value": "alice"},
                    {"type": "email",  "value": "alice@example.com"}
                ]
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "tok");
        let (acc, _rl) = client.get_account("ws-1").await.unwrap();
        assert_eq!(acc.uuid, "u-1");
        assert_eq!(acc.role.as_deref(), Some("OWNER"));
        assert_eq!(acc.primary_social_id.as_deref(), Some("s-1"));
        assert_eq!(acc.social_ids, vec!["s-1".to_string(), "s-2".to_string()]);
        assert_eq!(acc.full_social_ids.len(), 2);
        assert_eq!(acc.full_social_ids[0].kind, "github");
        assert_eq!(acc.full_social_ids[0].value, "alice");
    }

    #[tokio::test]
    async fn get_account_rate_limit_headers_propagated() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/api/v1/account/ws"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("X-RateLimit-Limit", "100")
                    .insert_header("X-RateLimit-Remaining", "42")
                    .set_body_json(json!({"uuid": "u"})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "tok");
        let (_, rl) = client.get_account("ws").await.unwrap();
        assert_eq!(rl.limit, Some(100));
        assert_eq!(rl.remaining, Some(42));
    }

    // ---- get_model --------------------------------------------------------

    #[tokio::test]
    async fn get_model_sends_full_query_when_requested() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/api/v1/load-model/ws"))
            .and(query_param("full", "true"))
            .and(header("authorization", "Bearer t"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"_class": "core:class:TxCreate", "objectId": "x"},
                {"_class": "core:class:TxUpdate", "objectId": "y"}
            ])))
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "t");
        let (txs, _) = client.get_model("ws", true).await.unwrap();
        assert_eq!(txs.len(), 2);
        assert_eq!(txs[0]["objectId"], "x");
    }

    #[tokio::test]
    async fn get_model_omits_full_param_by_default() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/api/v1/load-model/ws"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "t");
        let (txs, _) = client.get_model("ws", false).await.unwrap();
        assert!(txs.is_empty());

        // Sanity: no `full` query param was sent.
        let received = server.received_requests().await.expect("requests");
        let url = &received[0].url;
        assert!(
            url.query().unwrap_or("").is_empty(),
            "expected no query string, got {:?}",
            url.query()
        );
    }

    // ---- ensure_person ----------------------------------------------------

    #[tokio::test]
    async fn ensure_person_posts_camel_case_body() {
        let server = MockServer::start().await;
        Mock::given(m_method("POST"))
            .and(m_path("/api/v1/ensure-person/ws"))
            .and(header("authorization", "Bearer tok"))
            .and(body_json(json!({
                "socialType": "email",
                "socialValue": "alice@example.com",
                "firstName": "Alice",
                "lastName": "Example"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "uuid": "person-1",
                "socialId": "soc-1",
                "localPerson": "alice"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "tok");
        let req = EnsurePersonRequest {
            social_type: "email".into(),
            social_value: "alice@example.com".into(),
            first_name: "Alice".into(),
            last_name: "Example".into(),
        };
        let (resp, _) = client.ensure_person("ws", &req).await.unwrap();
        assert_eq!(resp.uuid, "person-1");
        assert_eq!(resp.social_id, "soc-1");
        assert_eq!(resp.local_person, "alice");
    }

    // ---- search_fulltext --------------------------------------------------

    #[tokio::test]
    async fn search_fulltext_minimal_query() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/api/v1/search-fulltext/ws"))
            .and(query_param("query", "hello"))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "docs": [
                    {"id": "d1", "title": "Hello world"},
                    {"id": "d2", "shortTitle": "Hi", "emojiIcon": "👋"}
                ],
                "total": 2
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "tok");
        let (res, _) = client
            .search_fulltext("ws", "hello", &SearchOptions::default())
            .await
            .unwrap();
        assert_eq!(res.total, Some(2));
        assert_eq!(res.docs.len(), 2);
        assert_eq!(res.docs[0].id, "d1");
        assert_eq!(res.docs[0].title.as_deref(), Some("Hello world"));
        assert_eq!(res.docs[1].short_title.as_deref(), Some("Hi"));
        assert_eq!(res.docs[1].emoji_icon.as_deref(), Some("👋"));
    }

    #[tokio::test]
    async fn search_fulltext_encodes_classes_spaces_limit() {
        let server = MockServer::start().await;
        // Refs are wrapped in a JSON `{ref: true}` map, ordered for
        // determinism by BTreeMap.
        Mock::given(m_method("GET"))
            .and(m_path("/api/v1/search-fulltext/ws"))
            .and(query_param("query", "q"))
            .and(query_param(
                "classes",
                r#"{"core:class:Issue":true,"tracker:class:Bug":true}"#,
            ))
            .and(query_param("spaces", r#"{"space:1":true}"#))
            .and(query_param("limit", "25"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"docs": []})))
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "tok");
        let opts = SearchOptions {
            classes: Some(vec![
                "core:class:Issue".into(),
                "tracker:class:Bug".into(),
            ]),
            spaces: Some(vec!["space:1".into()]),
            limit: Some(25),
        };
        let (res, _) = client.search_fulltext("ws", "q", &opts).await.unwrap();
        assert!(res.docs.is_empty());
    }

    // ---- domain_request ---------------------------------------------------

    #[tokio::test]
    async fn domain_request_round_trip() {
        let server = MockServer::start().await;
        Mock::given(m_method("POST"))
            .and(m_path("/api/v1/request/contact/ws"))
            .and(header("authorization", "Bearer tok"))
            .and(body_json(json!({"op": "ping", "n": 1})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "domain": "contact",
                "value": {"reply": "pong"}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "tok");
        let body = json!({"op": "ping", "n": 1});
        let (res, _): (DomainResult<serde_json::Value>, _) = client
            .domain_request("ws", "contact", &body)
            .await
            .unwrap();
        assert_eq!(res.domain, "contact");
        assert_eq!(res.value["reply"], "pong");
    }

    #[tokio::test]
    async fn domain_request_decodes_typed_value() {
        #[derive(Deserialize, Debug, PartialEq)]
        struct Reply {
            ok: bool,
        }

        let server = MockServer::start().await;
        Mock::given(m_method("POST"))
            .and(m_path("/api/v1/request/d/w"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "domain": "d",
                "value": {"ok": true}
            })))
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "tok");
        let (res, _): (DomainResult<Reply>, _) =
            client.domain_request("w", "d", &json!({})).await.unwrap();
        assert_eq!(res.value, Reply { ok: true });
    }

    // ---- error paths ------------------------------------------------------

    #[tokio::test]
    async fn upstream_4xx_returns_upstream_error() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .respond_with(ResponseTemplate::new(404).set_body_string("missing"))
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "tok");
        let err = client.get_account("ws").await.unwrap_err();
        match err {
            RestError::Upstream { status, body } => {
                assert_eq!(status, 404);
                assert_eq!(body, "missing");
            }
            other => panic!("expected Upstream, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn upstream_429_returns_rate_limited_with_metadata() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After-ms", "1500")
                    .insert_header("X-RateLimit-Remaining", "0")
                    .set_body_string("slow down"),
            )
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "tok");
        let err = client.get_account("ws").await.unwrap_err();
        match err {
            RestError::RateLimited { rate_limit, body } => {
                assert_eq!(rate_limit.retry_after_ms, Some(1500));
                assert_eq!(rate_limit.remaining, Some(0));
                assert_eq!(body, "slow down");
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn network_error_on_unreachable_host() {
        let client = RestClient::new("http://127.0.0.1:1", "tok");
        let err = client.get_config().await.unwrap_err();
        assert!(matches!(err, RestError::Network(_)));
    }

    #[tokio::test]
    async fn malformed_response_yields_decode_error() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/config.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "");
        let err = client.get_config().await.unwrap_err();
        assert!(matches!(err, RestError::Decode(_)), "got {err:?}");
    }

    // ---- helpers ----------------------------------------------------------

    #[test]
    fn encode_ref_map_is_sorted_for_determinism() {
        let s = encode_ref_map(&["b".into(), "a".into(), "c".into()]);
        assert_eq!(s, r#"{"a":true,"b":true,"c":true}"#);
    }

    #[test]
    fn url_strips_trailing_slash_from_base() {
        let c = RestClient::new("http://x.example/", "tok");
        let u = c.url("/api/v1/account/ws", &[]).unwrap();
        assert_eq!(u.as_str(), "http://x.example/api/v1/account/ws");
    }

    // ---- ServerConfigCache + bootstrap_server_config --------------------

    #[test]
    fn server_config_cache_starts_empty() {
        let cache = ServerConfigCache::new();
        assert!(!cache.is_populated());
        assert!(cache.get().is_none());
        assert!(cache.accounts_url().is_none());
        assert!(cache.collaborator_url().is_none());
        assert!(cache.files_url().is_none());
        assert!(cache.upload_url().is_none());
    }

    #[test]
    fn server_config_cache_exposes_individual_urls() {
        let cache = ServerConfigCache::new();
        cache.set(Some(ServerConfig {
            accounts_url: Some("https://acc.example".into()),
            collaborator_url: Some("https://collab.example".into()),
            files_url: Some("https://files.example".into()),
            upload_url: Some("https://upload.example".into()),
        }));
        assert!(cache.is_populated());
        assert_eq!(cache.accounts_url().as_deref(), Some("https://acc.example"));
        assert_eq!(cache.collaborator_url().as_deref(), Some("https://collab.example"));
        assert_eq!(cache.files_url().as_deref(), Some("https://files.example"));
        assert_eq!(cache.upload_url().as_deref(), Some("https://upload.example"));
    }

    #[test]
    fn server_config_cache_clone_shares_state() {
        let a = ServerConfigCache::new();
        let b = a.clone();
        a.set(Some(ServerConfig {
            accounts_url: Some("https://x".into()),
            ..Default::default()
        }));
        assert_eq!(b.accounts_url().as_deref(), Some("https://x"));
    }

    #[tokio::test]
    async fn bootstrap_populates_cache_on_success() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/config.json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "ACCOUNTS_URL": "https://acc.example",
                "COLLABORATOR_URL": "https://collab.example",
                "FILES_URL": "https://files.example",
                "UPLOAD_URL": "https://upload.example"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "ignored");
        let cache = ServerConfigCache::new();
        bootstrap_server_config(&client, &cache).await;

        assert!(cache.is_populated());
        assert_eq!(cache.accounts_url().as_deref(), Some("https://acc.example"));
        assert_eq!(cache.collaborator_url().as_deref(), Some("https://collab.example"));
        assert_eq!(cache.files_url().as_deref(), Some("https://files.example"));
        assert_eq!(cache.upload_url().as_deref(), Some("https://upload.example"));
    }

    #[tokio::test]
    async fn bootstrap_leaves_cache_none_on_404() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/config.json"))
            .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
            .expect(1)
            .mount(&server)
            .await;

        let client = RestClient::new(server.uri(), "ignored");
        let cache = ServerConfigCache::new();
        bootstrap_server_config(&client, &cache).await;

        assert!(!cache.is_populated(), "cache must remain None on legacy 404");
        assert!(cache.accounts_url().is_none());
    }

    #[tokio::test]
    async fn bootstrap_leaves_cache_none_on_network_error() {
        // Pointing at a closed port forces RestError::Network.
        let client = RestClient::new("http://127.0.0.1:1", "ignored");
        let cache = ServerConfigCache::new();
        bootstrap_server_config(&client, &cache).await;
        assert!(!cache.is_populated());
    }

    #[test]
    fn url_appends_query_pairs() {
        let c = RestClient::new("http://x.example", "tok");
        let u = c.url("/p", &[("a", "1"), ("b", "two")]).unwrap();
        assert_eq!(u.as_str(), "http://x.example/p?a=1&b=two");
    }
}
