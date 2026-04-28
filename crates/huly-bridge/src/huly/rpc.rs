use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static REQUEST_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Reset the ID counter (for testing determinism)
#[cfg(test)]
pub fn reset_request_id() {
    REQUEST_ID_COUNTER.store(1, Ordering::SeqCst);
}

fn next_request_id() -> u64 {
    REQUEST_ID_COUNTER.fetch_add(1, Ordering::SeqCst)
}

fn current_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// --- Request ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RpcRequest {
    pub id: u64,
    pub method: String,
    pub params: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<Value>,
    pub time: u64,
}

impl RpcRequest {
    pub fn new(method: impl Into<String>, params: Vec<Value>) -> Self {
        Self {
            id: next_request_id(),
            method: method.into(),
            params,
            meta: None,
            time: current_timestamp(),
        }
    }
}

// --- Response ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RpcResponse {
    /// Defaults to `-1` (server-push sentinel) when missing — the transactor sends
    /// unsolicited error/event frames without an `id` field.
    #[serde(default = "default_response_id")]
    pub id: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk: Option<Value>,
    #[serde(rename = "rateLimit", default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimit>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
    /// 0.7.19+: server-reported time spent serving the request, in milliseconds.
    /// Informational only — not consumed by the bridge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bfst: Option<f64>,
    /// 0.7.19+: server-reported internal queue metric. Informational only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<f64>,
}

/// Huly transactor uses string platform-status identifiers (e.g.
/// `platform:status:Unauthorized`, `platform:status:NotFound`) — not numeric codes.
/// Accept both shapes during deserialize so legacy numeric responses still work.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl<'de> Deserialize<'de> for RpcError {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            code: Value,
            #[serde(default)]
            message: String,
        }
        let raw = Raw::deserialize(de)?;
        let code = match raw.code {
            Value::String(s) => s,
            Value::Number(n) => n.to_string(),
            other => other.to_string(),
        };
        Ok(RpcError { code, message: raw.message })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RateLimit {
    /// Milliseconds the client should wait before retrying.
    #[serde(rename = "retryAfter", default, skip_serializing_if = "Option::is_none")]
    pub retry_after: Option<u64>,
    /// 0.7.19+: current request count within the window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<u64>,
    /// 0.7.19+: epoch milliseconds when the limit window resets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<u64>,
}

fn default_response_id() -> i64 {
    -1
}

impl RpcResponse {
    pub fn is_server_push(&self) -> bool {
        self.id == -1
    }

    pub fn is_error(&self) -> bool {
        self.error.is_some()
    }

    pub fn is_rate_limited(&self) -> bool {
        self.rate_limit.is_some()
    }
}

// --- Hello handshake ---

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HelloRequest {
    pub method: String,
    pub params: Vec<Value>,
    pub id: i64,
    #[serde(default)]
    pub binary: bool,
    #[serde(default)]
    pub compression: bool,
}

impl HelloRequest {
    pub fn new(binary: bool, compression: bool) -> Self {
        Self {
            method: "hello".to_string(),
            params: vec![],
            id: -1,
            binary,
            compression,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HelloResponse {
    pub id: i64,
    #[serde(default)]
    pub binary: bool,
    #[serde(default)]
    pub compression: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
    /// 0.7.19+: transactor build version string (e.g. "0.7.19").
    #[serde(rename = "serverVersion", default, skip_serializing_if = "Option::is_none")]
    pub server_version: Option<String>,
    /// 0.7.19+: id of the most recent transaction the server is aware of.
    #[serde(rename = "lastTx", default, skip_serializing_if = "Option::is_none")]
    pub last_tx: Option<String>,
    /// 0.7.19+: hash describing the current model state.
    #[serde(rename = "lastHash", default, skip_serializing_if = "Option::is_none")]
    pub last_hash: Option<String>,
    /// 0.7.19+: account info for the authenticated session, embedded in the
    /// hello reply rather than fetched separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
    /// 0.7.19+: server confirms whether it will use compression for responses.
    /// Distinct from `compression`, which echoes the client's request.
    #[serde(rename = "useCompression", default, skip_serializing_if = "Option::is_none")]
    pub use_compression: Option<bool>,
}

/// Unified account view used by both the WS hello handshake and the REST
/// `GET /api/v1/account/{workspace}` endpoint.
///
/// The two wire shapes are a strict subset/superset relationship:
/// - The hello-handshake reply embeds a slim form (typically just `uuid`,
///   sometimes `role` / `primarySocialId` / `socialIds`).
/// - The REST endpoint returns the same fields plus `fullSocialIds`
///   (tagged identities).
///
/// All fields except `uuid` default to their empty value so both shapes
/// decode into the same struct without errors.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub uuid: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(rename = "primarySocialId", default, skip_serializing_if = "Option::is_none")]
    pub primary_social_id: Option<String>,
    /// Bare social-id strings. Empty when the wire payload omits the field.
    #[serde(rename = "socialIds", default, skip_serializing_if = "Vec::is_empty")]
    pub social_ids: Vec<String>,
    /// Tagged social identities, returned only by the REST endpoint.
    /// Empty for the hello handshake payload.
    #[serde(rename = "fullSocialIds", default, skip_serializing_if = "Vec::is_empty")]
    pub full_social_ids: Vec<SocialId>,
}

/// Tagged social identity. Wire field is `type` (a Rust keyword) so the
/// Rust field is `kind` with a serde rename.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SocialId {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: String,
}

// --- Serialization modes ---

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProtocolOptions {
    pub binary: bool,
    pub compression: bool,
}

impl Default for ProtocolOptions {
    fn default() -> Self {
        Self {
            binary: false, // msgpackr (bundleStrings/structuredClone) incompatible with rmp_serde
            compression: true,
        }
    }
}

/// Serialize a value according to protocol options (JSON or msgpack, optionally snappy-compressed)
pub fn serialize<T: Serialize>(value: &T, opts: ProtocolOptions) -> Result<Vec<u8>, SerializeError> {
    let bytes = if opts.binary {
        rmp_serde::to_vec_named(value)?
    } else {
        serde_json::to_vec(value)?
    };

    if opts.compression {
        let mut encoder = snap::raw::Encoder::new();
        Ok(encoder.compress_vec(&bytes)?)
    } else {
        Ok(bytes)
    }
}

/// Deserialize a value according to protocol options
pub fn deserialize<T: for<'de> Deserialize<'de>>(
    data: &[u8],
    opts: ProtocolOptions,
) -> Result<T, SerializeError> {
    let bytes = if opts.compression {
        let mut decoder = snap::raw::Decoder::new();
        decoder.decompress_vec(data)?
    } else {
        data.to_vec()
    };

    if opts.binary {
        Ok(rmp_serde::from_slice(&bytes)?)
    } else {
        Ok(serde_json::from_slice(&bytes)?)
    }
}

// --- Error type ---

#[derive(Debug, thiserror::Error)]
pub enum SerializeError {
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("msgpack encode error: {0}")]
    MsgpackEncode(#[from] rmp_serde::encode::Error),

    #[error("msgpack decode error: {0}")]
    MsgpackDecode(#[from] rmp_serde::decode::Error),

    #[error("snappy error: {0}")]
    Snappy(#[from] snap::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // --- RED/GREEN: RpcRequest ---

    #[test]
    fn request_serializes_to_json() {
        let mut req = RpcRequest::new("findAll", vec![json!("core:class:Doc"), json!({"space": "sp1"})]);
        req.time = 1700000000000; // fixed for determinism

        let json = serde_json::to_value(&req).unwrap();
        assert!(json["id"].as_u64().unwrap() > 0);
        assert_eq!(json["method"], "findAll");
        assert_eq!(json["params"][0], "core:class:Doc");
        assert_eq!(json["params"][1]["space"], "sp1");
        assert_eq!(json["time"], 1700000000000u64);
        assert!(json.get("meta").is_none());
    }

    #[test]
    fn request_with_meta() {
        reset_request_id();
        let mut req = RpcRequest::new("createDoc", vec![]);
        req.meta = Some(json!({"source": "bridge"}));
        req.time = 0;

        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["meta"]["source"], "bridge");
    }

    #[test]
    fn request_ids_increment() {
        reset_request_id();
        let r1 = RpcRequest::new("a", vec![]);
        let r2 = RpcRequest::new("b", vec![]);
        let r3 = RpcRequest::new("c", vec![]);
        assert!(r2.id > r1.id);
        assert!(r3.id > r2.id);
    }

    #[test]
    fn request_json_roundtrip() {
        reset_request_id();
        let mut req = RpcRequest::new("findOne", vec![json!("class"), json!({"x": 1})]);
        req.time = 12345;

        let serialized = serde_json::to_string(&req).unwrap();
        let deserialized: RpcRequest = serde_json::from_str(&serialized).unwrap();
        assert_eq!(req, deserialized);
    }

    // --- RED/GREEN: RpcResponse ---

    #[test]
    fn response_with_result() {
        let json_str = r#"{"id": 1, "result": {"_id": "doc1", "_class": "core:class:Doc"}}"#;
        let resp: RpcResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.id, 1);
        assert_eq!(resp.result.as_ref().unwrap()["_id"], "doc1");
        assert!(!resp.is_error());
        assert!(!resp.is_server_push());
    }

    #[test]
    fn response_with_error() {
        let json_str = r#"{"id": 2, "error": {"code": 404, "message": "not found"}}"#;
        let resp: RpcResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.is_error());
        let err = resp.error.unwrap();
        assert_eq!(err.code, "404");
        assert_eq!(err.message, "not found");
    }

    #[test]
    fn response_with_string_error_code() {
        let json_str = r#"{"id": 2, "error": {"code": "platform:status:Unauthorized", "message": "no"}}"#;
        let resp: RpcResponse = serde_json::from_str(json_str).unwrap();
        let err = resp.error.unwrap();
        assert_eq!(err.code, "platform:status:Unauthorized");
        assert_eq!(err.message, "no");
    }

    #[test]
    fn response_id_defaults_to_minus_one_when_missing() {
        // Transactor sends unsolicited error frames without `id` — must decode as
        // server-push (id=-1) rather than failing the parse.
        let json_str = r#"{"error": {"code": "platform:status:Unauthorized", "message": "no"}}"#;
        let resp: RpcResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.id, -1);
        assert!(resp.is_server_push());
        assert!(resp.is_error());
        assert_eq!(resp.error.unwrap().code, "platform:status:Unauthorized");
    }

    #[test]
    fn response_with_numeric_id_zero() {
        // Sanity: an explicit id=0 is preserved (not overridden by the default).
        let resp: RpcResponse = serde_json::from_str(r#"{"id": 0, "result": null}"#).unwrap();
        assert_eq!(resp.id, 0);
        assert!(!resp.is_server_push());
    }

    #[test]
    fn response_server_push() {
        let json_str = r#"{"id": -1, "result": {"event": "tx"}}"#;
        let resp: RpcResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.is_server_push());
    }

    #[test]
    fn response_rate_limited() {
        let json_str = r#"{"id": 3, "result": null, "rateLimit": {"retryAfter": 5000}}"#;
        let resp: RpcResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.is_rate_limited());
        assert_eq!(resp.rate_limit.unwrap().retry_after, Some(5000));
    }

    #[test]
    fn response_with_chunk() {
        let json_str = r#"{"id": 4, "chunk": [{"_id": "a"}, {"_id": "b"}]}"#;
        let resp: RpcResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.chunk.is_some());
        let chunk = resp.chunk.unwrap();
        assert_eq!(chunk.as_array().unwrap().len(), 2);
    }

    // --- RED/GREEN: HelloRequest/Response ---

    #[test]
    fn hello_request_serializes() {
        let hello = HelloRequest::new(true, true);
        let json = serde_json::to_value(&hello).unwrap();
        assert_eq!(json["method"], "hello");
        assert_eq!(json["id"], -1);
        assert!(json["binary"].as_bool().unwrap());
        assert!(json["compression"].as_bool().unwrap());
    }

    #[test]
    fn hello_response_deserializes() {
        let json_str = r#"{"id": -1, "binary": true, "compression": true}"#;
        let resp: HelloResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.id, -1);
        assert!(resp.binary);
        assert!(resp.compression);
    }

    #[test]
    fn hello_response_decodes_v719_top_level_fields() {
        // 0.7.19: serverVersion / lastTx / lastHash / account / useCompression
        // are siblings of `id`/`binary`/`compression`, not nested under `result`.
        let json_str = r#"{
            "id": -1,
            "binary": true,
            "compression": true,
            "useCompression": true,
            "serverVersion": "0.7.19",
            "lastTx": "tx:abc",
            "lastHash": "sha256:deadbeef",
            "account": {
                "uuid": "acc-uuid-1",
                "role": "OWNER",
                "primarySocialId": "social:1",
                "socialIds": ["social:1", "social:2"]
            }
        }"#;
        let resp: HelloResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.server_version.as_deref(), Some("0.7.19"));
        assert_eq!(resp.last_tx.as_deref(), Some("tx:abc"));
        assert_eq!(resp.last_hash.as_deref(), Some("sha256:deadbeef"));
        assert_eq!(resp.use_compression, Some(true));
        let account = resp.account.expect("account");
        assert_eq!(account.uuid, "acc-uuid-1");
        assert_eq!(account.role.as_deref(), Some("OWNER"));
        assert_eq!(account.primary_social_id.as_deref(), Some("social:1"));
        assert_eq!(account.social_ids.len(), 2);
    }

    #[test]
    fn hello_response_legacy_without_new_fields() {
        // Forward-compat: legacy servers omit the new fields entirely.
        let json_str = r#"{"id": -1, "binary": true, "compression": true}"#;
        let resp: HelloResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.server_version.is_none());
        assert!(resp.last_tx.is_none());
        assert!(resp.last_hash.is_none());
        assert!(resp.account.is_none());
        assert!(resp.use_compression.is_none());
    }

    #[test]
    fn hello_response_account_minimal() {
        // Account with only the required uuid field.
        let json_str = r#"{
            "id": -1,
            "binary": false,
            "compression": false,
            "account": {"uuid": "acc-1"}
        }"#;
        let resp: HelloResponse = serde_json::from_str(json_str).unwrap();
        let account = resp.account.expect("account");
        assert_eq!(account.uuid, "acc-1");
        assert!(account.role.is_none());
        assert!(account.primary_social_id.is_none());
        assert!(account.social_ids.is_empty());
        assert!(account.full_social_ids.is_empty());
    }

    // --- Unified Account: shape parity with REST `/api/v1/account/{ws}` ---

    #[test]
    fn account_decodes_lean_ws_hello_shape() {
        // The slim form embedded in hello replies.
        let json_str = r#"{
            "uuid": "acc-uuid-1",
            "role": "OWNER",
            "primarySocialId": "social:1",
            "socialIds": ["social:1", "social:2"]
        }"#;
        let acc: Account = serde_json::from_str(json_str).unwrap();
        assert_eq!(acc.uuid, "acc-uuid-1");
        assert_eq!(acc.role.as_deref(), Some("OWNER"));
        assert_eq!(acc.primary_social_id.as_deref(), Some("social:1"));
        assert_eq!(acc.social_ids, vec!["social:1".to_string(), "social:2".to_string()]);
        assert!(acc.full_social_ids.is_empty(), "lean shape has no fullSocialIds");
    }

    #[test]
    fn account_decodes_full_rest_shape() {
        // The richer form returned by `GET /api/v1/account/{workspace}`.
        let json_str = r#"{
            "uuid": "u-1",
            "role": "OWNER",
            "primarySocialId": "s-1",
            "socialIds": ["s-1", "s-2"],
            "fullSocialIds": [
                {"type": "github", "value": "alice"},
                {"type": "email",  "value": "alice@example.com"}
            ]
        }"#;
        let acc: Account = serde_json::from_str(json_str).unwrap();
        assert_eq!(acc.uuid, "u-1");
        assert_eq!(acc.role.as_deref(), Some("OWNER"));
        assert_eq!(acc.primary_social_id.as_deref(), Some("s-1"));
        assert_eq!(acc.social_ids, vec!["s-1".to_string(), "s-2".to_string()]);
        assert_eq!(acc.full_social_ids.len(), 2);
        assert_eq!(acc.full_social_ids[0].kind, "github");
        assert_eq!(acc.full_social_ids[0].value, "alice");
        assert_eq!(acc.full_social_ids[1].kind, "email");
        assert_eq!(acc.full_social_ids[1].value, "alice@example.com");
    }

    #[test]
    fn account_decodes_uuid_only() {
        let acc: Account = serde_json::from_str(r#"{"uuid": "x"}"#).unwrap();
        assert_eq!(acc.uuid, "x");
        assert!(acc.role.is_none());
        assert!(acc.primary_social_id.is_none());
        assert!(acc.social_ids.is_empty());
        assert!(acc.full_social_ids.is_empty());
    }

    #[test]
    fn account_serializes_skips_empty_collections() {
        // Lean form: empty vectors should not appear on the wire.
        let acc = Account {
            uuid: "x".to_string(),
            role: None,
            primary_social_id: None,
            social_ids: vec![],
            full_social_ids: vec![],
        };
        let v = serde_json::to_value(&acc).unwrap();
        assert_eq!(v, serde_json::json!({"uuid": "x"}));
    }

    #[test]
    fn rpc_response_ignores_v719_metric_fields() {
        // 0.7.19 adds informational `bfst` (server time) and `queue` metrics.
        // They must decode into the optional fields without breaking anything.
        let json_str = r#"{"id": 7, "result": null, "bfst": 1.25, "queue": 0.5}"#;
        let resp: RpcResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.id, 7);
        assert_eq!(resp.bfst, Some(1.25));
        assert_eq!(resp.queue, Some(0.5));
    }

    #[test]
    fn rpc_response_metric_fields_default_to_none() {
        let json_str = r#"{"id": 8, "result": null}"#;
        let resp: RpcResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.bfst.is_none());
        assert!(resp.queue.is_none());
    }

    #[test]
    fn rate_limit_decodes_v719_fields() {
        let json_str = r#"{"current": 42, "reset": 1700000000000, "retryAfter": 5000}"#;
        let rl: RateLimit = serde_json::from_str(json_str).unwrap();
        assert_eq!(rl.current, Some(42));
        assert_eq!(rl.reset, Some(1700000000000));
        assert_eq!(rl.retry_after, Some(5000));
    }

    #[test]
    fn rate_limit_legacy_only_retry_after() {
        let json_str = r#"{"retryAfter": 1000}"#;
        let rl: RateLimit = serde_json::from_str(json_str).unwrap();
        assert_eq!(rl.retry_after, Some(1000));
        assert!(rl.current.is_none());
        assert!(rl.reset.is_none());
    }

    #[test]
    fn rate_limit_all_fields_optional() {
        let json_str = "{}";
        let rl: RateLimit = serde_json::from_str(json_str).unwrap();
        assert!(rl.retry_after.is_none());
        assert!(rl.current.is_none());
        assert!(rl.reset.is_none());
    }

    #[test]
    fn hello_response_with_error() {
        let json_str = r#"{"id": -1, "binary": false, "compression": false, "error": {"code": 401, "message": "unauthorized"}}"#;
        let resp: HelloResponse = serde_json::from_str(json_str).unwrap();
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, "401");
    }

    // --- RED/GREEN: Serialization modes ---

    #[test]
    fn json_no_compression_roundtrip() {
        reset_request_id();
        let opts = ProtocolOptions {
            binary: false,
            compression: false,
        };
        let mut req = RpcRequest::new("test", vec![json!(42)]);
        req.time = 100;

        let bytes = serialize(&req, opts).unwrap();
        // Should be valid JSON
        let _: Value = serde_json::from_slice(&bytes).unwrap();

        let decoded: RpcRequest = deserialize(&bytes, opts).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn json_with_compression_roundtrip() {
        reset_request_id();
        let opts = ProtocolOptions {
            binary: false,
            compression: true,
        };
        let mut req = RpcRequest::new("test", vec![json!("data")]);
        req.time = 200;

        let bytes = serialize(&req, opts).unwrap();
        // Compressed bytes should NOT be valid JSON
        assert!(serde_json::from_slice::<Value>(&bytes).is_err());

        let decoded: RpcRequest = deserialize(&bytes, opts).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn msgpack_no_compression_roundtrip() {
        reset_request_id();
        let opts = ProtocolOptions {
            binary: true,
            compression: false,
        };
        let mut req = RpcRequest::new("findAll", vec![json!({"key": "value"})]);
        req.time = 300;

        let bytes = serialize(&req, opts).unwrap();
        let decoded: RpcRequest = deserialize(&bytes, opts).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn msgpack_with_compression_roundtrip() {
        reset_request_id();
        let opts = ProtocolOptions::default(); // binary=true, compression=true
        let mut req = RpcRequest::new("createDoc", vec![json!({"title": "Hello World"})]);
        req.time = 400;

        let bytes = serialize(&req, opts).unwrap();
        let decoded: RpcRequest = deserialize(&bytes, opts).unwrap();
        assert_eq!(req, decoded);
    }

    #[test]
    fn response_roundtrip_all_modes() {
        let resp = RpcResponse {
            id: 42,
            result: Some(json!({"docs": [1, 2, 3]})),
            error: None,
            chunk: None,
            rate_limit: None,
            terminate: None,
            bfst: None,
            queue: None,
        };

        for opts in [
            ProtocolOptions { binary: false, compression: false },
            ProtocolOptions { binary: false, compression: true },
            ProtocolOptions { binary: true, compression: false },
            ProtocolOptions { binary: true, compression: true },
        ] {
            let bytes = serialize(&resp, opts).unwrap();
            let decoded: RpcResponse = deserialize(&bytes, opts).unwrap();
            assert_eq!(resp, decoded, "failed for opts: {opts:?}");
        }
    }

    #[test]
    fn compression_reduces_size_for_large_payloads() {
        reset_request_id();
        let large_data: Vec<Value> = (0..100).map(|i| json!({"index": i, "data": "x".repeat(100)})).collect();
        let mut req = RpcRequest::new("bulk", vec![json!(large_data)]);
        req.time = 500;

        let uncompressed = serialize(&req, ProtocolOptions { binary: true, compression: false }).unwrap();
        let compressed = serialize(&req, ProtocolOptions { binary: true, compression: true }).unwrap();

        assert!(compressed.len() < uncompressed.len(),
            "compressed ({}) should be smaller than uncompressed ({})",
            compressed.len(), uncompressed.len());
    }

    #[test]
    fn deserialize_invalid_data_returns_error() {
        let garbage = b"not valid data";
        let opts = ProtocolOptions { binary: false, compression: false };
        let result: Result<RpcRequest, _> = deserialize(garbage, opts);
        assert!(result.is_err());
    }

    #[test]
    fn deserialize_invalid_compressed_data_returns_error() {
        let garbage = b"not snappy";
        let opts = ProtocolOptions { binary: false, compression: true };
        let result: Result<RpcRequest, _> = deserialize(garbage, opts);
        assert!(result.is_err());
    }
}
