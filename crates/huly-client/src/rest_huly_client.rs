//! REST-backed implementation of [`PlatformClient`].
//!
//! Sibling of the WS-based [`crate::client::HulyClient`]. Talks to the
//! transactor's `/api/v1/find-all/{ws}` and `/api/v1/tx/{ws}` endpoints
//! directly so MCP can run without the bridge HTTP gateway (P4 / D10).
//!
//! Per the P1 spike findings:
//! - `workspace_uuid` is the URL key, never the human slug.
//! - The wire `error` field is a `Status` object with `params.message` —
//!   we lift the message out the same way [`crate::rpc::RpcError`] does.
//! - On HTTP 429 we honor `Retry-After[-ms]` and retry up to N times before
//!   surfacing a structured error.

use crate::client::{ApplyIfResult, ClientError, PlatformClient};
use async_trait::async_trait;
use huly_common::api::ApplyIfMatch;
use huly_common::types::{Doc, FindOptions, FindResult, TxResult};
use reqwest::header::{AUTHORIZATION, HeaderMap};
use reqwest::{Client, Method, RequestBuilder, StatusCode};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{debug, warn};

use crate::rate_limit::RateLimitInfo;

/// Configuration for [`RestHulyClient`].
#[derive(Debug, Clone)]
pub struct RestHulyConfig {
    /// Number of times to retry an HTTP 429 response before giving up.
    /// Default: 3.
    pub max_429_retries: u32,
    /// Default delay used when `Retry-After[-ms]` is absent. Default: 1s.
    pub default_retry_after: Duration,
    /// Cap on Retry-After to prevent denial-of-service. Default: 30s.
    pub max_retry_after: Duration,
}

impl Default for RestHulyConfig {
    fn default() -> Self {
        Self {
            max_429_retries: 3,
            default_retry_after: Duration::from_secs(1),
            max_retry_after: Duration::from_secs(30),
        }
    }
}

/// REST client implementing [`PlatformClient`].
///
/// Holds a workspace UUID + bearer JWT. Cheap to clone (`reqwest::Client` is
/// internally reference counted).
#[derive(Debug, Clone)]
pub struct RestHulyClient {
    http: Client,
    /// `{rest_base_url}` — e.g. `https://huly.example.com` (no trailing slash).
    /// Path segments like `/api/v1/tx/{uuid}` are appended.
    base_url: String,
    /// Workspace UUID (REST URL key). NOT the human-readable slug.
    workspace_uuid: String,
    /// Bearer JWT minted by the JWT broker.
    jwt: String,
    cfg: RestHulyConfig,
}

impl RestHulyClient {
    /// Construct a new REST client. `rest_base_url` is e.g. the value the
    /// JWT broker returned in `MintResponse.rest_base_url`; `workspace_uuid`
    /// likewise comes from `MintResponse.workspace_uuid`.
    pub fn new(
        rest_base_url: impl Into<String>,
        workspace_uuid: impl Into<String>,
        jwt: impl Into<String>,
    ) -> Self {
        Self::with_config(rest_base_url, workspace_uuid, jwt, RestHulyConfig::default())
    }

    pub fn with_config(
        rest_base_url: impl Into<String>,
        workspace_uuid: impl Into<String>,
        jwt: impl Into<String>,
        cfg: RestHulyConfig,
    ) -> Self {
        Self {
            http: Client::new(),
            base_url: rest_base_url.into().trim_end_matches('/').to_string(),
            workspace_uuid: workspace_uuid.into(),
            jwt: jwt.into(),
            cfg,
        }
    }

    /// Workspace UUID this client is bound to. Mostly for diagnostics.
    pub fn workspace_uuid(&self) -> &str {
        &self.workspace_uuid
    }

    /// `GET /api/v1/load-model/{ws}?full=...` — for schema cache (D9). Public
    /// so the factory's schema cache can call it without going through the
    /// trait.
    pub async fn load_model(&self, full: bool) -> Result<Vec<Value>, ClientError> {
        let path = format!("/api/v1/load-model/{}", self.workspace_uuid);
        let q = if full { "?full=true" } else { "" };
        let url = format!("{}{}{}", self.base_url, path, q);
        let body: Value = self
            .send_with_retry(|| self.authed(Method::GET, &url))
            .await?;
        // The endpoint returns either a bare array or `{value: [...]}` —
        // accept both for forward-compat.
        if let Some(arr) = body.as_array() {
            return Ok(arr.clone());
        }
        if let Some(arr) = body.get("value").and_then(Value::as_array) {
            return Ok(arr.clone());
        }
        Err(ClientError::Format(format!(
            "load-model response is neither array nor object with `value`: {body}"
        )))
    }

    /// Direct access to the underlying `tx` endpoint — sends a pre-built Tx
    /// envelope as the JSON body of `POST /api/v1/tx/{ws}`. Used by the
    /// trait methods below; exposed publicly so callers that already build
    /// Tx objects (e.g. the apply-if helpers) can bypass the convenience
    /// wrappers.
    ///
    /// If a `request_id` is set in the surrounding [`with_request_id`]
    /// task scope, it is stamped into the TX envelope's `meta`
    /// object (key `request_id`). This is the audit-channel correlator
    /// (P7 / D3): the bridge's `huly.event.tx.*` republish carries it
    /// through unchanged, so MCP `huly.mcp.tool.invoked` events join
    /// to the resulting transactor events on this id.
    pub async fn raw_tx(&self, mut tx: Value) -> Result<Value, ClientError> {
        let url = format!("{}/api/v1/tx/{}", self.base_url, self.workspace_uuid);
        if let Some(rid) = current_request_id() {
            // Insert into `tx.meta.request_id`; create the object if
            // it isn't already there. We deliberately don't overwrite
            // an existing meta.request_id (caller-supplied wins).
            let meta = tx
                .as_object_mut()
                .map(|o| o.entry("meta").or_insert_with(|| serde_json::json!({})));
            if let Some(meta) = meta
                && let Some(meta_obj) = meta.as_object_mut()
                && !meta_obj.contains_key("request_id")
            {
                meta_obj.insert("request_id".to_string(), Value::String(rid));
            }
        }
        let body_clone = tx.clone();
        let body: Value = self
            .send_with_retry(move || self.authed(Method::POST, &url).json(&body_clone))
            .await?;
        Ok(body)
    }

    fn authed(&self, method: Method, url: &str) -> RequestBuilder {
        self.http
            .request(method, url)
            .header(AUTHORIZATION, format!("Bearer {}", self.jwt))
    }

    /// Drive a request through the response pipeline:
    /// - on 2xx, JSON-decode the body and lift any `{error: Status}` field
    ///   into a [`ClientError::Rpc`] (matching `RpcError::deserialize`).
    /// - on 429, parse `Retry-After[-ms]`, sleep, retry up to
    ///   `max_429_retries` before returning [`ClientError::Rpc`] with
    ///   code `rate_limited`.
    /// - other 4xx/5xx → `ClientError::Rpc { code: "http_<status>" }`.
    async fn send_with_retry<T, F>(&self, mut build: F) -> Result<T, ClientError>
    where
        T: for<'de> Deserialize<'de>,
        F: FnMut() -> RequestBuilder,
    {
        let mut attempt: u32 = 0;
        loop {
            let req = build();
            let resp = req
                .send()
                .await
                .map_err(|e| ClientError::Format(format!("network error: {e}")))?;
            let status = resp.status();
            let headers = resp.headers().clone();
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| ClientError::Format(format!("read body: {e}")))?;

            if status == StatusCode::TOO_MANY_REQUESTS {
                if attempt >= self.cfg.max_429_retries {
                    let body = String::from_utf8_lossy(&bytes).to_string();
                    return Err(ClientError::Rpc {
                        code: "rate_limited".into(),
                        message: format!(
                            "exhausted {} retries against rate-limited transactor (last body: {body})",
                            self.cfg.max_429_retries
                        ),
                    });
                }
                let delay = retry_after(&headers, &self.cfg);
                warn!(
                    attempt = attempt + 1,
                    max = self.cfg.max_429_retries,
                    delay_ms = delay.as_millis() as u64,
                    "transactor rate-limited request; retrying"
                );
                tokio::time::sleep(delay).await;
                attempt += 1;
                continue;
            }

            if !status.is_success() {
                let body = String::from_utf8_lossy(&bytes).to_string();
                // Try to lift a Status-shaped error out of the body so the
                // structured `code/message` makes it back to MCP rather than
                // a bare HTTP code.
                if let Some((code, message)) = decode_status_error(&bytes) {
                    return Err(ClientError::Rpc { code, message });
                }
                return Err(ClientError::Rpc {
                    code: format!("http_{}", status.as_u16()),
                    message: body,
                });
            }

            // 2xx: decode the body, then lift {error: Status} if present.
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|e| ClientError::Format(format!("decode body: {e}")))?;
            if let Some(err) = value.get("error") {
                let (code, message) = lift_status(err);
                return Err(ClientError::Rpc { code, message });
            }
            // Re-serialize → deserialize as T. Cheap; lets us keep the
            // Status lift above without bespoke per-method types.
            return serde_json::from_value::<T>(value)
                .map_err(|e| ClientError::Format(format!("decode T: {e}")));
        }
    }
}

fn retry_after(headers: &HeaderMap, cfg: &RestHulyConfig) -> Duration {
    let info = RateLimitInfo::from_headers(headers);
    // Some servers send seconds-only `Retry-After`; rate_limit.rs already
    // covers that. Fall back to the configured default when truly absent.
    let ms = info
        .retry_after_ms
        .unwrap_or(cfg.default_retry_after.as_millis() as u64);
    let ms = ms.min(cfg.max_retry_after.as_millis() as u64);
    Duration::from_millis(ms)
}

/// Decode a `{error: {code, message?, params?}}` body. Returns `None` when
/// the body is not a JSON object or the `error` field is missing.
fn decode_status_error(bytes: &[u8]) -> Option<(String, String)> {
    let v: Value = serde_json::from_slice(bytes).ok()?;
    let err = v.get("error")?;
    Some(lift_status(err))
}

/// Lift a Status-shaped error into `(code, message)` mirroring
/// `RpcError::deserialize`. Falls back to `params.message` when the
/// top-level `message` is absent.
fn lift_status(err: &Value) -> (String, String) {
    let code = match err.get("code") {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "unknown".to_string(),
    };
    let direct = err
        .get("message")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let message = direct.unwrap_or_else(|| {
        err.get("params")
            .and_then(|p| p.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    });
    (code, message)
}

tokio::task_local! {
    /// Per-task audit correlator. MCP's `record_tool` wrapper enters
    /// this scope around every tool body so the underlying
    /// [`RestHulyClient::raw_tx`] can stamp `meta.request_id` on the
    /// TX envelope without threading the id through every trait
    /// method signature.
    static REQUEST_ID: String;
}

/// Run `fut` in a task scope where `current_request_id()` returns
/// `Some(rid)`. Idempotent — nested scopes shadow the outer value
/// (the innermost wins, mirroring how rmcp's tool dispatch works).
pub async fn with_request_id<F, T>(rid: String, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    REQUEST_ID.scope(rid, fut).await
}

/// Snapshot the request id of the currently-executing task, if any.
/// Returns `None` outside of a [`with_request_id`] scope.
pub fn current_request_id() -> Option<String> {
    REQUEST_ID.try_with(|s| s.clone()).ok()
}

/// Current epoch milliseconds.
fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Generate a unique-enough Tx id.
fn gen_tx_id() -> String {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let seq = CTR.fetch_add(1, Ordering::Relaxed);
    let t = epoch_ms() as u64;
    format!("{:x}-{:x}", t, seq)
}

#[async_trait]
impl PlatformClient for RestHulyClient {
    async fn find_all(
        &self,
        class: &str,
        query: Value,
        options: Option<FindOptions>,
    ) -> Result<FindResult, ClientError> {
        // Auto-wrap bare-string `_id` predicates the same way the WS client
        // does (transactor quirk — `{_id: "x"}` returns malformed response).
        let query = normalize_query(query);

        let path = format!("/api/v1/find-all/{}", self.workspace_uuid);
        let mut url = format!("{}{}", self.base_url, path);
        // Append query string. We don't use `reqwest`'s query() so that the
        // builder closure stays cheap and idempotent under retries.
        let mut params: Vec<(&str, String)> = vec![("class", class.to_string())];
        if !is_empty_object(&query) {
            params.push((
                "query",
                serde_json::to_string(&query)
                    .map_err(|e| ClientError::Format(format!("encode query: {e}")))?,
            ));
        }
        if let Some(opts) = &options
            && let Ok(s) = serde_json::to_string(opts)
            && s != "{}"
        {
            params.push(("options", s));
        }
        let qs = params
            .iter()
            .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
            .collect::<Vec<_>>()
            .join("&");
        if !qs.is_empty() {
            url.push('?');
            url.push_str(&qs);
        }

        let body: Value = self
            .send_with_retry(|| self.authed(Method::GET, &url))
            .await?;

        // Same shapes as the WS impl: array, {docs,total}, or
        // {value, total, lookupMap}.
        if let Some(arr) = body.as_array() {
            return Ok(FindResult {
                total: arr.len() as i64,
                docs: serde_json::from_value(Value::Array(arr.clone()))
                    .map_err(|e| ClientError::Format(e.to_string()))?,
                lookup_map: None,
            });
        }
        if let Some(obj) = body.as_object() {
            let key = if obj.contains_key("value") {
                "value"
            } else if obj.contains_key("docs") {
                "docs"
            } else {
                return Err(ClientError::Format(
                    "find-all response object has neither `value` nor `docs`".into(),
                ));
            };
            let docs_val = obj.get(key).cloned().unwrap_or_else(|| Value::Array(vec![]));
            let docs: Vec<Doc> = serde_json::from_value(docs_val)
                .map_err(|e| ClientError::Format(e.to_string()))?;
            let total = obj
                .get("total")
                .and_then(|v| v.as_i64())
                .unwrap_or(docs.len() as i64);
            let lookup_map = obj.get("lookupMap").filter(|v| !v.is_null()).cloned();
            return Ok(FindResult { docs, total, lookup_map });
        }
        Err(ClientError::Format(format!(
            "find-all response is neither array nor object: {body}"
        )))
    }

    async fn find_one(
        &self,
        class: &str,
        query: Value,
        options: Option<FindOptions>,
    ) -> Result<Option<Doc>, ClientError> {
        let mut opts = options.unwrap_or_default();
        opts.limit = Some(1);
        let res = self.find_all(class, query, Some(opts)).await?;
        Ok(res.docs.into_iter().next())
    }

    async fn create_doc(
        &self,
        class: &str,
        space: &str,
        attributes: Value,
    ) -> Result<String, ClientError> {
        let object_id = gen_tx_id();
        let now = epoch_ms();
        let tx = json!({
            "_id": gen_tx_id(),
            "_class": "core:class:TxCreateDoc",
            "space": "core:space:Tx",
            "objectId": object_id,
            "objectClass": class,
            "objectSpace": space,
            "modifiedBy": "core:account:System",
            "modifiedOn": now,
            "createdBy": "core:account:System",
            "attributes": attributes,
        });
        let _result = self.raw_tx(tx).await?;
        Ok(object_id)
    }

    async fn update_doc(
        &self,
        class: &str,
        space: &str,
        id: &str,
        operations: Value,
    ) -> Result<TxResult, ClientError> {
        let now = epoch_ms();
        let tx = json!({
            "_id": gen_tx_id(),
            "_class": "core:class:TxUpdateDoc",
            "space": "core:space:Tx",
            "objectId": id,
            "objectClass": class,
            "objectSpace": space,
            "modifiedBy": "core:account:System",
            "modifiedOn": now,
            "operations": operations,
        });
        let result = self.raw_tx(tx).await?;
        // tx() returns TxResult shape; tolerate both bare {} and {success,id}.
        let success = result.get("success").and_then(Value::as_bool).unwrap_or(true);
        let rid = result
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| Some(id.to_string()));
        Ok(TxResult { success, id: rid })
    }

    async fn remove_doc(
        &self,
        class: &str,
        space: &str,
        id: &str,
    ) -> Result<TxResult, ClientError> {
        let now = epoch_ms();
        let tx = json!({
            "_id": gen_tx_id(),
            "_class": "core:class:TxRemoveDoc",
            "space": "core:space:Tx",
            "objectId": id,
            "objectClass": class,
            "objectSpace": space,
            "modifiedBy": "core:account:System",
            "modifiedOn": now,
        });
        let result = self.raw_tx(tx).await?;
        let success = result.get("success").and_then(Value::as_bool).unwrap_or(true);
        Ok(TxResult { success, id: Some(id.to_string()) })
    }

    async fn add_collection(
        &self,
        class: &str,
        space: &str,
        attached_to: &str,
        attached_to_class: &str,
        collection: &str,
        attributes: Value,
    ) -> Result<String, ClientError> {
        let object_id = gen_tx_id();
        let now = epoch_ms();
        let tx = json!({
            "_id": gen_tx_id(),
            "_class": "core:class:TxCreateDoc",
            "space": "core:space:Tx",
            "objectId": object_id,
            "objectClass": class,
            "objectSpace": space,
            "modifiedBy": "core:account:System",
            "modifiedOn": now,
            "createdBy": "core:account:System",
            "attributes": attributes,
            "attachedTo": attached_to,
            "attachedToClass": attached_to_class,
            "collection": collection,
        });
        let _result = self.raw_tx(tx).await?;
        Ok(object_id)
    }

    async fn update_collection(
        &self,
        class: &str,
        space: &str,
        id: &str,
        attached_to: &str,
        attached_to_class: &str,
        collection: &str,
        operations: Value,
    ) -> Result<TxResult, ClientError> {
        let now = epoch_ms();
        let tx = json!({
            "_id": gen_tx_id(),
            "_class": "core:class:TxUpdateDoc",
            "space": "core:space:Tx",
            "objectId": id,
            "objectClass": class,
            "objectSpace": space,
            "modifiedBy": "core:account:System",
            "modifiedOn": now,
            "operations": operations,
            "attachedTo": attached_to,
            "attachedToClass": attached_to_class,
            "collection": collection,
        });
        let result = self.raw_tx(tx).await?;
        let success = result.get("success").and_then(Value::as_bool).unwrap_or(true);
        let rid = result
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| Some(id.to_string()));
        Ok(TxResult { success, id: rid })
    }

    async fn apply_if_tx(
        &self,
        scope: &str,
        matches: Vec<ApplyIfMatch>,
        not_matches: Vec<ApplyIfMatch>,
        txes: Vec<Value>,
    ) -> Result<ApplyIfResult, ClientError> {
        let now_ms = epoch_ms();
        let tx_id = gen_tx_id();
        let to_query_array = |xs: &[ApplyIfMatch]| -> Vec<Value> {
            xs.iter()
                .map(|m| json!({ "_class": m.class, "query": m.query }))
                .collect()
        };
        let mut tx = json!({
            "_id": tx_id,
            "_class": "core:class:TxApplyIf",
            "space": "core:space:Tx",
            "objectSpace": "core:space:Tx",
            "modifiedBy": "core:account:System",
            "modifiedOn": now_ms,
            "scope": scope,
            "match": to_query_array(&matches),
            "txes": txes,
        });
        if !not_matches.is_empty() {
            tx["notMatch"] = Value::Array(to_query_array(&not_matches));
        }
        let result = self.raw_tx(tx).await?;
        debug!(?result, "apply_if_tx response");
        let success = result.get("success").and_then(Value::as_bool).unwrap_or(false);
        let server_time = result
            .get("serverTime")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        Ok(ApplyIfResult { success, server_time })
    }
}

fn is_empty_object(v: &Value) -> bool {
    v.as_object().is_some_and(|o| o.is_empty())
}

fn normalize_query(mut query: Value) -> Value {
    let Some(obj) = query.as_object_mut() else {
        return query;
    };
    if let Some(id) = obj.get("_id")
        && let Some(s) = id.as_str()
    {
        let s = s.to_string();
        obj.insert("_id".into(), json!({ "$in": [s] }));
    }
    query
}

// Tiny URL-encoder used for the find-all query string. Avoids pulling in a
// new dep just for percent-encoding the `query` JSON.
mod urlencoding {
    pub(super) fn encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for b in s.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char);
                }
                _ => {
                    out.push('%');
                    out.push(hex(b >> 4));
                    out.push(hex(b & 0xf));
                }
            }
        }
        out
    }
    fn hex(n: u8) -> char {
        match n {
            0..=9 => (b'0' + n) as char,
            _ => (b'A' + n - 10) as char,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method as m_method, path as m_path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(uri: String) -> RestHulyClient {
        RestHulyClient::with_config(
            uri,
            "ws-uuid-1",
            "test-jwt",
            RestHulyConfig {
                max_429_retries: 2,
                default_retry_after: Duration::from_millis(10),
                max_retry_after: Duration::from_millis(20),
            },
        )
    }

    #[tokio::test]
    async fn find_all_sends_class_query_and_decodes_total_array() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/api/v1/find-all/ws-uuid-1"))
            .and(query_param("class", "tracker:class:Issue"))
            .and(header("authorization", "Bearer test-jwt"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "dataType": "TotalArray",
                "value": [
                    {"_id": "i1", "_class": "tracker:class:Issue"},
                    {"_id": "i2", "_class": "tracker:class:Issue"}
                ],
                "total": -1,
                "lookupMap": null,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let c = client(server.uri());
        let result = c
            .find_all("tracker:class:Issue", json!({}), None)
            .await
            .unwrap();
        assert_eq!(result.total, -1);
        assert_eq!(result.docs.len(), 2);
        assert!(result.lookup_map.is_none());
    }

    #[tokio::test]
    async fn find_all_lifts_status_error_from_2xx_body() {
        // Even on HTTP 200, Huly may reply with `{error: Status}` to signal
        // a domain failure. PR #11 ensured Status.params.message is captured
        // on WS — REST must do the same.
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/api/v1/find-all/ws-uuid-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "error": {
                    "code": "platform:status:UnknownError",
                    "params": {"message": "transactor blew up"}
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let c = client(server.uri());
        let err = c.find_all("cls", json!({}), None).await.unwrap_err();
        match err {
            ClientError::Rpc { code, message } => {
                assert_eq!(code, "platform:status:UnknownError");
                assert_eq!(message, "transactor blew up");
            }
            other => panic!("expected Rpc error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn find_all_auto_wraps_bare_string_id_query() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/api/v1/find-all/ws-uuid-1"))
            .and(query_param("query", r#"{"_id":{"$in":["doc-1"]}}"#))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "docs": [{"_id": "doc-1", "_class": "cls"}],
                "total": 1,
            })))
            .expect(1)
            .mount(&server)
            .await;
        let c = client(server.uri());
        let r = c
            .find_all("cls", json!({"_id": "doc-1"}), None)
            .await
            .unwrap();
        assert_eq!(r.total, 1);
    }

    #[tokio::test]
    async fn create_doc_posts_tx_create_doc_and_returns_object_id() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        Mock::given(m_method("POST"))
            .and(m_path("/api/v1/tx/ws-uuid-1"))
            .and(header("authorization", "Bearer test-jwt"))
            .and(body_partial_json(json!({
                "_class": "core:class:TxCreateDoc",
                "objectClass": "tracker:class:Issue",
                "objectSpace": "proj-1",
                "attributes": {"title": "T"}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let c = client(server.uri());
        let id = c
            .create_doc("tracker:class:Issue", "proj-1", json!({"title": "T"}))
            .await
            .unwrap();
        assert!(!id.is_empty(), "create_doc must return the new object id");
    }

    #[tokio::test]
    async fn upstream_4xx_decodes_status_error_body() {
        let server = MockServer::start().await;
        Mock::given(m_method("POST"))
            .and(m_path("/api/v1/tx/ws-uuid-1"))
            .respond_with(
                ResponseTemplate::new(400).set_body_json(json!({
                    "error": {
                        "code": "platform:status:Forbidden",
                        "message": "no",
                    }
                })),
            )
            .mount(&server)
            .await;
        let c = client(server.uri());
        let err = c
            .update_doc("cls", "sp", "id", json!({"$set": {"x": 1}}))
            .await
            .unwrap_err();
        match err {
            ClientError::Rpc { code, message } => {
                assert_eq!(code, "platform:status:Forbidden");
                assert_eq!(message, "no");
            }
            other => panic!("expected Rpc, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_429_retries_then_succeeds() {
        // First N-1 attempts get a 429; the last one succeeds. Our config
        // sets `max_429_retries=2`, so 2 retries + 1 success = 3 total tries.
        let server = MockServer::start().await;
        // First two: 429s with Retry-After-ms=5.
        Mock::given(m_method("GET"))
            .and(m_path("/api/v1/find-all/ws-uuid-1"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After-ms", "5")
                    .set_body_string("slow down"),
            )
            .up_to_n_times(2)
            .mount(&server)
            .await;
        // Then a 200.
        Mock::given(m_method("GET"))
            .and(m_path("/api/v1/find-all/ws-uuid-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"docs": [], "total": 0})))
            .mount(&server)
            .await;

        let c = client(server.uri());
        let r = c.find_all("cls", json!({}), None).await.unwrap();
        assert_eq!(r.total, 0);
    }

    #[tokio::test]
    async fn http_429_exhausts_retries_returns_rate_limited_error() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/api/v1/find-all/ws-uuid-1"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("Retry-After-ms", "5")
                    .set_body_string("nope"),
            )
            .mount(&server)
            .await;

        let c = client(server.uri());
        let err = c.find_all("cls", json!({}), None).await.unwrap_err();
        match err {
            ClientError::Rpc { code, message } => {
                assert_eq!(code, "rate_limited");
                assert!(message.contains("exhausted"), "msg: {message}");
            }
            other => panic!("expected Rpc, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_if_tx_serializes_envelope_and_decodes_success() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        Mock::given(m_method("POST"))
            .and(m_path("/api/v1/tx/ws-uuid-1"))
            .and(body_partial_json(json!({
                "_class": "core:class:TxApplyIf",
                "scope": "scope-1",
                "match": [{"_class": "C", "query": {"x": 1}}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "serverTime": 42
            })))
            .expect(1)
            .mount(&server)
            .await;

        let c = client(server.uri());
        let r = c
            .apply_if_tx(
                "scope-1",
                vec![ApplyIfMatch {
                    class: "C".into(),
                    query: json!({"x": 1}),
                }],
                vec![],
                vec![json!({"_class": "tx"})],
            )
            .await
            .unwrap();
        assert!(r.success);
        assert_eq!(r.server_time, 42);
    }

    #[tokio::test]
    async fn load_model_decodes_array_response() {
        let server = MockServer::start().await;
        Mock::given(m_method("GET"))
            .and(m_path("/api/v1/load-model/ws-uuid-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"_id": "tx1", "_class": "core:class:TxCreateDoc"}
            ])))
            .expect(1)
            .mount(&server)
            .await;
        let c = client(server.uri());
        let txs = c.load_model(false).await.unwrap();
        assert_eq!(txs.len(), 1);
    }

    #[test]
    fn lift_status_falls_back_to_params_message() {
        let v = json!({"code": "platform:status:UnknownError", "params": {"message": "boom"}});
        let (code, msg) = lift_status(&v);
        assert_eq!(code, "platform:status:UnknownError");
        assert_eq!(msg, "boom");
    }

    #[test]
    fn lift_status_prefers_top_level_message() {
        let v = json!({"code": "x", "message": "top", "params": {"message": "lower"}});
        let (_, msg) = lift_status(&v);
        assert_eq!(msg, "top");
    }

    /// `raw_tx` stamps `meta.request_id` from the surrounding
    /// [`with_request_id`] task scope. Verifies the wire body the
    /// transactor receives carries the audit correlator end-to-end.
    #[tokio::test]
    async fn raw_tx_stamps_request_id_into_meta_when_in_scope() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        Mock::given(m_method("POST"))
            .and(m_path("/api/v1/tx/ws-uuid-1"))
            .and(body_partial_json(json!({
                "_class": "core:class:TxCreateDoc",
                "meta": {"request_id": "01J0RID000000000000000000"},
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let c = client(server.uri());
        with_request_id("01J0RID000000000000000000".to_string(), async {
            c.create_doc("tracker:class:Issue", "proj-1", json!({"title": "T"}))
                .await
                .unwrap();
        })
        .await;
    }

    /// Outside of a [`with_request_id`] scope, no `meta.request_id`
    /// is added to the TX. The transactor receives the legacy shape.
    #[tokio::test]
    async fn raw_tx_does_not_stamp_meta_outside_request_id_scope() {
        let server = MockServer::start().await;
        Mock::given(m_method("POST"))
            .and(m_path("/api/v1/tx/ws-uuid-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(1)
            .mount(&server)
            .await;
        let c = client(server.uri());
        c.create_doc("tracker:class:Issue", "proj-1", json!({"title": "T"}))
            .await
            .unwrap();
        // Confirm the body did NOT carry a meta key. wiremock doesn't
        // expose negative body assertions directly, so inspect the
        // recorded request body.
        let received = server.received_requests().await.unwrap();
        let req = received
            .iter()
            .find(|r| r.url.path().ends_with("/tx/ws-uuid-1"))
            .unwrap();
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        assert!(
            body.get("meta").is_none(),
            "expected no meta when outside scope, got: {body}"
        );
    }
}
