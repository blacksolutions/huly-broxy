//! JWT broker wire types.
//!
//! The bridge mints workspace JWTs on demand for MCP over a NATS request/reply
//! exchange. MCP issues a [`MintRequest`] on [`MINT_SUBJECT`] and expects a
//! response payload that deserializes either as [`MintResponse`] (success) or
//! [`MintError`] (structured failure).
//!
//! ## Wire format
//!
//! Subject: `huly.bridge.mint`
//!
//! Request:
//! ```json
//! { "workspace": "muhasebot",
//!   "agent_id": "claude-code-murat-001",
//!   "request_id": "01HXG..." }
//! ```
//!
//! Response (success):
//! ```json
//! { "jwt": "eyJ...",
//!   "account_service_jwt": "eyJ...",
//!   "expires_at_ms": 1730000000000,
//!   "refresh_at_ms": 1729996400000,
//!   "transactor_url": "wss://huly.black.solutions/...",
//!   "rest_base_url": "https://huly.black.solutions/api/v1",
//!   "workspace_uuid": "0192abcd-..." }
//! ```
//!
//! Response (error):
//! ```json
//! { "error": { "code": "unknown_workspace", "message": "..." } }
//! ```
//!
//! ## Critical fields (per P1 spike)
//!
//! - `workspace_uuid` is the **REST URL key** — `POST /api/v1/tx/{uuid}`, never
//!   the human-readable slug. Source: `huly.core/packages/api-client/src/rest/rest.ts:110,267`.
//! - `account_service_jwt` is the **account-scoped** token, distinct from the
//!   workspace `jwt`. Required for `huly_list_workspaces` (served by the
//!   account service, not the transactor).
//! - `refresh_at_ms = expires_at_ms - 60_000` (1 minute leeway).

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// NATS subject the bridge subscribes to for JWT mint requests.
pub const MINT_SUBJECT: &str = "huly.bridge.mint";

/// How long MCP waits on a `request_with_timeout` before giving up.
pub const MINT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MintRequest {
    /// Human-readable workspace slug (matches `[[workspace_credentials]].workspace`
    /// in the bridge config).
    pub workspace: String,
    /// Identifier of the calling agent. Logged by the broker for audit.
    pub agent_id: String,
    /// Caller-supplied correlation id (ULID/UUID). Echoed in logs only —
    /// not in the response payload (NATS req/reply correlates via inbox).
    pub request_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MintResponse {
    /// Workspace-scoped JWT — used by MCP to talk to the transactor RPC and
    /// REST endpoints.
    pub jwt: String,

    /// Optional account-service JWT — used for cross-workspace endpoints
    /// (`huly_list_workspaces`, etc.). Brokers that cannot obtain one return
    /// `None`; callers that need it must surface the gap clearly rather than
    /// substituting the workspace `jwt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_service_jwt: Option<String>,

    /// Wall-clock epoch milliseconds when the workspace JWT becomes invalid.
    pub expires_at_ms: u64,

    /// Wall-clock epoch milliseconds when callers should refresh proactively
    /// (= `expires_at_ms - 60_000`).
    pub refresh_at_ms: u64,

    /// WebSocket URL of the transactor that owns this workspace.
    pub transactor_url: String,

    /// Base URL for REST endpoints (`{base}/api/v1/tx/{workspace_uuid}`).
    pub rest_base_url: String,

    /// Workspace UUID — REST URL key, NOT the human-readable slug.
    pub workspace_uuid: String,
}

/// Structured error reply. Wire shape is `{"error": {"code": ..., "message": ...}}`
/// so successful and failed responses can share one subject.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MintError {
    pub code: String,
    pub message: String,
}

/// Wire wrapper distinguishing success from error on the same subject.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MintReply {
    Ok(MintResponse),
    Err {
        error: MintError,
    },
}

/// Canonical error codes emitted by the broker. Stringly-typed because they
/// cross a NATS boundary and may be extended without forcing a lockstep
/// upgrade of MCP clients.
pub mod error_codes {
    pub const UNKNOWN_WORKSPACE: &str = "unknown_workspace";
    pub const ACCOUNTS_FAILURE: &str = "accounts_failure";
    pub const INVALID_REQUEST: &str = "invalid_request";
    pub const INTERNAL: &str = "internal";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips() {
        let req = MintRequest {
            workspace: "muhasebot".into(),
            agent_id: "claude-code-001".into(),
            request_id: "01HXG".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: MintRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn response_round_trips_with_account_service_jwt() {
        let resp = MintResponse {
            jwt: "ws-jwt".into(),
            account_service_jwt: Some("acct-jwt".into()),
            expires_at_ms: 1_730_000_000_000,
            refresh_at_ms: 1_729_999_940_000,
            transactor_url: "wss://h.example/_transactor".into(),
            rest_base_url: "https://h.example/api/v1".into(),
            workspace_uuid: "uuid-1".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        let back: MintResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn response_round_trips_without_account_service_jwt() {
        let resp = MintResponse {
            jwt: "ws-jwt".into(),
            account_service_jwt: None,
            expires_at_ms: 1_730_000_000_000,
            refresh_at_ms: 1_729_999_940_000,
            transactor_url: "wss://h/_t".into(),
            rest_base_url: "https://h/api/v1".into(),
            workspace_uuid: "uuid-2".into(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        // Field is skipped when None, but deserialize defaults it back.
        assert!(!json.contains("account_service_jwt"));
        let back: MintResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn reply_decodes_success_and_error_on_same_subject() {
        let ok_json = serde_json::json!({
            "jwt": "j",
            "expires_at_ms": 1u64,
            "refresh_at_ms": 0u64,
            "transactor_url": "wss://x",
            "rest_base_url": "https://x",
            "workspace_uuid": "u",
        })
        .to_string();
        let err_json = serde_json::json!({
            "error": { "code": "unknown_workspace", "message": "no such ws" }
        })
        .to_string();

        match serde_json::from_str::<MintReply>(&ok_json).unwrap() {
            MintReply::Ok(r) => assert_eq!(r.jwt, "j"),
            MintReply::Err { .. } => panic!("expected ok"),
        }
        match serde_json::from_str::<MintReply>(&err_json).unwrap() {
            MintReply::Err { error } => {
                assert_eq!(error.code, error_codes::UNKNOWN_WORKSPACE);
                assert_eq!(error.message, "no such ws");
            }
            MintReply::Ok(_) => panic!("expected err"),
        }
    }

    #[test]
    fn subject_and_timeout_constants_exposed() {
        assert_eq!(MINT_SUBJECT, "huly.bridge.mint");
        assert_eq!(MINT_TIMEOUT, Duration::from_secs(5));
    }
}
