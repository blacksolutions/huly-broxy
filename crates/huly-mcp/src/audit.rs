//! MCP-side audit publisher (P7 / D3 mcp side).
//!
//! Wraps an `async_nats::Client` and emits `huly.mcp.*` events on every
//! tool invocation. Calls are fire-and-forget: a NATS publish failure
//! never blocks or fails the tool itself, it only shows up as a
//! `warn!` log line. This keeps the audit channel from becoming a
//! coupled dependency of the user-visible tool path.
//!
//! ## Why a dedicated module
//!
//! - Subject taxonomy + payload shapes live in `huly_common::mcp_subjects`
//!   so the bridge / consumers can decode without depending on `huly-mcp`.
//! - Digesting (SHA-256 → 16-char hex) is centralised so we never leak
//!   raw request / response bodies onto NATS — the audit channel is
//!   intentionally collision-tolerant.
//! - `request_id` minting (ULID) is exposed as a free function so the
//!   tool entry-points can mint, log, AND thread it into transactor
//!   `meta.request_id` from a single source.

use huly_common::mcp_subjects::{
    ActionPayload, ERROR_SUBJECT, ErrorPayload, TOOL_COMPLETED_SUBJECT,
    TOOL_INVOKED_SUBJECT, ToolCompletedPayload, ToolCompletedResult,
    ToolInvokedPayload, action_subject,
};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::warn;
use ulid::Ulid;

/// Mint a fresh request id. ULID is monotonic-by-default, sortable by
/// time, and 26 ASCII chars long — friendly for log filters.
pub fn new_request_id() -> String {
    Ulid::new().to_string()
}

/// Wall-clock ms since the unix epoch. The audit channel timestamps
/// every event in this format.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Compute the audit-channel digest of a serialisable value.
///
/// SHA-256, hex-encoded, truncated to 16 chars. Collisions in 16 hex
/// chars (64 bits) are vanishingly unlikely for the tool-call traffic
/// we expect, and the truncation keeps the payload compact while
/// removing any motivation to put PII on the wire.
pub fn digest_json<T: Serialize>(v: &T) -> String {
    let bytes = serde_json::to_vec(v).unwrap_or_default();
    digest_bytes(&bytes)
}

/// Same as [`digest_json`] but for a JSON [`Value`] reference (avoids
/// re-serialising the body when the caller already has it as `Value`).
pub fn digest_value(v: &Value) -> String {
    let bytes = serde_json::to_vec(v).unwrap_or_default();
    digest_bytes(&bytes)
}

/// Direct byte digest. Public so tool helpers that already own a
/// `&[u8]` (e.g. a markup blob) don't double-allocate.
pub fn digest_bytes(bytes: &[u8]) -> String {
    let h = Sha256::digest(bytes);
    let mut out = String::with_capacity(16);
    // 16 hex chars == 8 bytes of the digest.
    for b in h.iter().take(8) {
        // Manual hex to avoid dragging in another tiny crate at the
        // call-site; matches `hex::encode(&h[..8])`.
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Audit-channel publisher.
///
/// Cheap to clone (the inner `async_nats::Client` is `Arc`-backed).
/// Construct one per process; share by clone across the tool router.
#[derive(Clone)]
pub struct AuditPublisher {
    nats: async_nats::Client,
    agent_id: String,
}

impl AuditPublisher {
    pub fn new(nats: async_nats::Client, agent_id: impl Into<String>) -> Self {
        Self {
            nats,
            agent_id: agent_id.into(),
        }
    }

    pub fn agent_id(&self) -> &str {
        &self.agent_id
    }

    async fn publish<T: Serialize>(&self, subject: &str, payload: &T) {
        let bytes = match serde_json::to_vec(payload) {
            Ok(b) => b,
            Err(e) => {
                warn!(subject, error = %e, "audit publish: serialize failed");
                return;
            }
        };
        if let Err(e) = self
            .nats
            .publish(subject.to_string(), bytes.into())
            .await
        {
            warn!(subject, error = %e, "audit publish: nats publish failed");
        }
    }

    /// Emit `huly.mcp.tool.invoked`. Call at the entry of every tool.
    pub async fn tool_invoked(
        &self,
        tool: &str,
        workspace: Option<&str>,
        params_digest: &str,
        request_id: &str,
    ) {
        let payload = ToolInvokedPayload {
            tool: tool.to_string(),
            workspace: workspace.map(str::to_string),
            agent_id: self.agent_id.clone(),
            params_digest: params_digest.to_string(),
            request_id: request_id.to_string(),
            timestamp_ms: now_ms(),
        };
        self.publish(TOOL_INVOKED_SUBJECT, &payload).await;
    }

    /// Emit `huly.mcp.tool.completed`. Call at the exit of every tool,
    /// regardless of success — `result` discriminates.
    pub async fn tool_completed(
        &self,
        tool: &str,
        request_id: &str,
        result: ToolCompletedResult,
        duration_ms: u64,
    ) {
        let payload = ToolCompletedPayload {
            request_id: request_id.to_string(),
            tool: tool.to_string(),
            result,
            duration_ms,
            timestamp_ms: now_ms(),
        };
        self.publish(TOOL_COMPLETED_SUBJECT, &payload).await;
    }

    /// Emit `huly.mcp.action.<class>.<op>`. Mutating tools only —
    /// reads use `tool.invoked` for audit.
    pub async fn action(
        &self,
        class: &str,
        op: &str,
        workspace: &str,
        request_id: &str,
        target_id: Option<&str>,
        fields_changed: Option<Vec<String>>,
    ) {
        let payload = ActionPayload {
            workspace: workspace.to_string(),
            agent_id: self.agent_id.clone(),
            request_id: request_id.to_string(),
            target_id: target_id.map(str::to_string),
            fields_changed,
            timestamp_ms: now_ms(),
        };
        let subject = action_subject(class, op);
        self.publish(&subject, &payload).await;
    }

    /// Emit `huly.mcp.error`. Call alongside `tool.completed` whenever
    /// the tool returns `Err(_)` — `params` carries the transactor
    /// `Status.params` block when one was decoded.
    pub async fn error(
        &self,
        request_id: &str,
        tool: &str,
        code: &str,
        message: &str,
        params: Value,
    ) {
        let payload = ErrorPayload {
            request_id: request_id.to_string(),
            tool: tool.to_string(),
            code: code.to_string(),
            message: message.to_string(),
            params,
            transactor_request_id: None,
            timestamp_ms: now_ms(),
        };
        self.publish(ERROR_SUBJECT, &payload).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;
    use serde_json::json;

    #[test]
    fn new_request_id_is_ulid_shaped() {
        let id = new_request_id();
        // ULIDs are 26 ASCII chars in Crockford base32.
        assert_eq!(id.len(), 26);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn digest_json_is_deterministic_and_16_chars() {
        let a = digest_json(&json!({"k": "v", "n": 1}));
        let b = digest_json(&json!({"k": "v", "n": 1}));
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
        // Differs from a different body.
        let c = digest_json(&json!({"k": "v", "n": 2}));
        assert_ne!(a, c);
    }

    #[test]
    fn digest_bytes_matches_sha256_truncated() {
        // Known SHA-256 of the empty string starts with e3b0c44298fc1c14...
        let d = digest_bytes(b"");
        assert_eq!(d, "e3b0c44298fc1c14");
    }

    /// Round-trip the four publisher methods through a real NATS server
    /// when one is reachable. Skipped silently in environments without
    /// NATS (mirrors the rest of the test suite's policy).
    #[tokio::test]
    async fn publisher_emits_all_four_subjects_when_nats_available() {
        let Ok(c) = async_nats::connect("nats://127.0.0.1:4222").await else {
            return;
        };
        let mut sub = c.subscribe("huly.mcp.>".to_string()).await.unwrap();
        let publisher = AuditPublisher::new(c.clone(), "agent-test");
        let rid = new_request_id();

        publisher
            .tool_invoked("huly_create", Some("ws-1"), "deadbeefcafebabe", &rid)
            .await;
        publisher
            .action("tracker.issue", "create", "ws-1", &rid, None, None)
            .await;
        publisher
            .tool_completed(
                "huly_create",
                &rid,
                ToolCompletedResult::Ok {
                    result_digest: "1234567890abcdef".into(),
                },
                7,
            )
            .await;
        publisher
            .error(&rid, "huly_create", "code", "msg", json!({}))
            .await;
        c.flush().await.unwrap();

        // Drain four messages; assert subjects + minimal shape.
        let mut subjects = Vec::new();
        for _ in 0..4 {
            let m = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                sub.next(),
            )
            .await
            .ok()
            .flatten()
            .expect("expected a published message");
            subjects.push(m.subject.to_string());
        }
        assert!(subjects.contains(&"huly.mcp.tool.invoked".to_string()));
        assert!(subjects.contains(&"huly.mcp.tool.completed".to_string()));
        assert!(subjects.contains(&"huly.mcp.action.tracker.issue.create".to_string()));
        assert!(subjects.contains(&"huly.mcp.error".to_string()));
    }
}
