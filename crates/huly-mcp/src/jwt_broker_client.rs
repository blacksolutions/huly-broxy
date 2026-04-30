//! Client helper for the bridge's JWT broker (P3 wire — P4 wires it).
//!
//! Issues a [`MintRequest`] on `huly.bridge.mint` and decodes the response
//! into either [`MintResponse`] or a structured [`MintError`]. P4 will plug
//! this into the `HulyClient` factory so every tool invocation can fetch a
//! workspace JWT from the local bridge instead of carrying long-lived secrets
//! in MCP's process memory.

use huly_common::mint::{
    MINT_SUBJECT, MINT_TIMEOUT, MintError, MintReply, MintRequest, MintResponse, error_codes,
};

/// Outcome of a single mint round-trip. Mirrors the wire shape so callers
/// can pattern-match without a second decode step.
#[derive(Debug)]
pub enum MintOutcome {
    Ok(MintResponse),
    Failed(MintError),
}

#[derive(Debug, thiserror::Error)]
pub enum MintClientError {
    #[error("nats request failed: {0}")]
    Nats(String),
    #[error("malformed mint reply: {0}")]
    Decode(String),
}

/// Encode a [`MintRequest`] into the wire payload sent on `huly.bridge.mint`.
/// Pulled out as a free function so unit tests can exercise the request shape
/// without spinning up NATS.
pub fn encode_request(req: &MintRequest) -> Result<Vec<u8>, MintClientError> {
    serde_json::to_vec(req).map_err(|e| MintClientError::Decode(format!("encode request: {e}")))
}

/// Decode a reply payload received on the inbox. Mirrors `encode_request` —
/// makes the wire-format dependency testable in isolation.
pub fn decode_reply(payload: &[u8]) -> Result<MintOutcome, MintClientError> {
    let reply: MintReply = serde_json::from_slice(payload)
        .map_err(|e| MintClientError::Decode(format!("decode reply: {e}")))?;
    Ok(match reply {
        MintReply::Ok(r) => MintOutcome::Ok(r),
        MintReply::Err { error } => MintOutcome::Failed(error),
    })
}

/// Send `MintRequest{workspace, agent_id}` on `huly.bridge.mint` and wait
/// up to [`MINT_TIMEOUT`] for the bridge to respond. The `request_id` is
/// generated per call — callers should not need to correlate replies
/// manually because NATS req/reply uses an inbox under the hood.
pub async fn request_jwt(
    nats: &async_nats::Client,
    workspace: &str,
    agent_id: &str,
) -> Result<MintOutcome, MintClientError> {
    let req = MintRequest {
        workspace: workspace.to_string(),
        agent_id: agent_id.to_string(),
        request_id: new_request_id(),
    };
    let payload = encode_request(&req)?;
    // `async-nats` 0.39 dropped the explicit `request_with_timeout` helper;
    // wrap the default `request` in a `tokio::time::timeout` so we still
    // bail in MINT_TIMEOUT regardless of the client's global setting.
    let msg = tokio::time::timeout(
        MINT_TIMEOUT,
        nats.request(MINT_SUBJECT.to_string(), payload.into()),
    )
    .await
    .map_err(|_| MintClientError::Nats(format!("timed out after {MINT_TIMEOUT:?}")))?
    .map_err(|e| MintClientError::Nats(e.to_string()))?;
    decode_reply(&msg.payload)
}

/// Generate a request id using time-since-epoch + thread randomness. We
/// avoid pulling a UUID/ULID dep just for this — request_id is logged for
/// audit and never needs to be globally unique.
fn new_request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("mcp-{now}-{seq}")
}

/// Convenience: classify an error reply by its code so call-sites don't
/// need to match string literals by hand.
pub fn is_unknown_workspace(err: &MintError) -> bool {
    err.code == error_codes::UNKNOWN_WORKSPACE
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    /// Spin up an embedded NATS client pair via `async-nats`'s test harness.
    /// We mock the broker by hand-subscribing on the subject, then asserting
    /// `request_jwt` round-trips correctly.
    async fn connect_pair() -> (async_nats::Client, async_nats::Client) {
        // Use the public `nats.io` demo? No — hermetic. We rely on running
        // a real NATS server at NATS_TEST_URL or skip. Because the harness
        // doesn't guarantee a NATS server, this test connects to an
        // in-process server via `async-nats::ConnectOptions::custom`. The
        // simpler route is to use `nats-server` if present; instead we
        // gate this test behind an env var.
        let url = std::env::var("HULY_TEST_NATS_URL")
            .unwrap_or_else(|_| "nats://127.0.0.1:4222".to_string());
        let a = async_nats::connect(&url).await.expect("nats a");
        let b = async_nats::connect(&url).await.expect("nats b");
        (a, b)
    }

    #[tokio::test]
    #[ignore = "requires a running NATS server (set HULY_TEST_NATS_URL)"]
    async fn request_jwt_round_trips_through_nats() {
        let (client, broker) = connect_pair().await;
        // Mock broker.
        let mut sub = broker
            .subscribe(MINT_SUBJECT.to_string())
            .await
            .expect("subscribe");
        let broker_task = tokio::spawn(async move {
            let msg = sub.next().await.expect("msg");
            let reply_to = msg.reply.expect("reply-to");
            let req: MintRequest = serde_json::from_slice(&msg.payload).unwrap();
            assert_eq!(req.workspace, "ws-1");
            assert_eq!(req.agent_id, "agent-x");
            let reply = MintReply::Ok(MintResponse {
                jwt: "ws-jwt".into(),
                account_service_jwt: Some("acct-jwt".into()),
                expires_at_ms: 1,
                refresh_at_ms: 0,
                transactor_url: "wss://t".into(),
                rest_base_url: "https://r".into(),
                workspace_uuid: "uuid".into(),
                accounts_url: None,
            });
            broker
                .publish(reply_to, serde_json::to_vec(&reply).unwrap().into())
                .await
                .unwrap();
            broker.flush().await.unwrap();
        });

        let outcome = request_jwt(&client, "ws-1", "agent-x").await.unwrap();
        match outcome {
            MintOutcome::Ok(r) => {
                assert_eq!(r.jwt, "ws-jwt");
                assert_eq!(r.account_service_jwt.as_deref(), Some("acct-jwt"));
                assert_eq!(r.workspace_uuid, "uuid");
            }
            MintOutcome::Failed(e) => panic!("unexpected error: {e:?}"),
        }
        broker_task.await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires a running NATS server (set HULY_TEST_NATS_URL)"]
    async fn request_jwt_surfaces_structured_error() {
        let (client, broker) = connect_pair().await;
        let mut sub = broker
            .subscribe(MINT_SUBJECT.to_string())
            .await
            .expect("subscribe");
        let broker_task = tokio::spawn(async move {
            let msg = sub.next().await.expect("msg");
            let reply_to = msg.reply.expect("reply-to");
            let reply = MintReply::Err {
                error: MintError {
                    code: error_codes::UNKNOWN_WORKSPACE.into(),
                    message: "no such ws".into(),
                },
            };
            broker
                .publish(reply_to, serde_json::to_vec(&reply).unwrap().into())
                .await
                .unwrap();
            broker.flush().await.unwrap();
        });

        let outcome = request_jwt(&client, "missing", "agent-y").await.unwrap();
        match outcome {
            MintOutcome::Failed(e) => {
                assert!(is_unknown_workspace(&e));
                assert_eq!(e.message, "no such ws");
            }
            MintOutcome::Ok(_) => panic!("expected error"),
        }
        broker_task.await.unwrap();
    }

    #[test]
    fn encode_request_emits_expected_wire_shape() {
        let req = MintRequest {
            workspace: "muhasebot".into(),
            agent_id: "agent-7".into(),
            request_id: "req-1".into(),
        };
        let bytes = encode_request(&req).unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["workspace"], "muhasebot");
        assert_eq!(v["agent_id"], "agent-7");
        assert_eq!(v["request_id"], "req-1");
    }

    #[test]
    fn decode_reply_parses_success_payload() {
        let resp = MintResponse {
            jwt: "ws-jwt".into(),
            account_service_jwt: Some("acct-jwt".into()),
            expires_at_ms: 100,
            refresh_at_ms: 40,
            transactor_url: "wss://t".into(),
            rest_base_url: "https://r".into(),
            workspace_uuid: "uuid-x".into(),
            accounts_url: Some("https://r/accounts".into()),
        };
        let bytes = serde_json::to_vec(&MintReply::Ok(resp.clone())).unwrap();
        match decode_reply(&bytes).unwrap() {
            MintOutcome::Ok(r) => assert_eq!(r, resp),
            MintOutcome::Failed(e) => panic!("unexpected error: {e:?}"),
        }
    }

    #[test]
    fn decode_reply_parses_error_payload() {
        let bytes = br#"{"error":{"code":"unknown_workspace","message":"nope"}}"#;
        match decode_reply(bytes).unwrap() {
            MintOutcome::Failed(e) => {
                assert!(is_unknown_workspace(&e));
                assert_eq!(e.message, "nope");
            }
            MintOutcome::Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn decode_reply_rejects_garbage() {
        let err = decode_reply(b"not json").unwrap_err();
        assert!(matches!(err, MintClientError::Decode(_)));
    }

    #[test]
    fn request_id_is_unique_within_process() {
        let a = new_request_id();
        let b = new_request_id();
        assert_ne!(a, b);
        assert!(a.starts_with("mcp-"));
    }

    #[test]
    fn is_unknown_workspace_matches_canonical_code() {
        let e = MintError {
            code: error_codes::UNKNOWN_WORKSPACE.into(),
            message: "x".into(),
        };
        assert!(is_unknown_workspace(&e));
        let e2 = MintError {
            code: error_codes::ACCOUNTS_FAILURE.into(),
            message: "x".into(),
        };
        assert!(!is_unknown_workspace(&e2));
    }
}
