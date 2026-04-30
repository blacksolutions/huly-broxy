//! NATS subject taxonomy + wire payloads for MCP-side audit events (D3).
//!
//! The bridge publishes the canonical "what actually happened" stream
//! under [`crate::announcement::EVENT_SUBJECT_PREFIX`]; MCP publishes
//! the complementary "what the AI tried" stream under `huly.mcp.*`.
//! Subscribers correlate by `request_id` (a ULID minted at MCP tool
//! entry and threaded into the transactor's `meta.request_id`) to
//! reconstruct intent → outcome chains.
//!
//! ## Subjects
//!
//! | Subject | Trigger |
//! |---|---|
//! | `huly.mcp.tool.invoked` | Every tool call, on entry. |
//! | `huly.mcp.tool.completed` | Every tool call, on return (ok or err). |
//! | `huly.mcp.action.<class>.<op>` | Mutating tool calls only, after `tool.invoked`. |
//! | `huly.mcp.error` | Tool failures, with full transactor `Status`. |
//!
//! ## Stability
//!
//! These subjects + the field set on each payload are part of the public
//! audit contract. New optional fields may be added in beta; existing
//! fields will not be removed or renamed without a major version bump.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Subject for `tool.invoked` events. Emitted at the entry of every
/// `#[tool]` method on the MCP server.
pub const TOOL_INVOKED_SUBJECT: &str = "huly.mcp.tool.invoked";

/// Subject for `tool.completed` events. Emitted at tool exit, regardless
/// of success or failure — `result` discriminates.
pub const TOOL_COMPLETED_SUBJECT: &str = "huly.mcp.tool.completed";

/// Subject for `error` events. Emitted alongside `tool.completed` when
/// the tool returned `Err(_)` and the underlying transport surfaced a
/// transactor [`Status`]-shaped error.
pub const ERROR_SUBJECT: &str = "huly.mcp.error";

/// Build the `huly.mcp.action.<class>.<op>` subject for a mutating tool
/// invocation. `class` is a subsystem grouping (e.g. `tracker.issue`,
/// `card`, `tracker.component`) and `op` is the verb (`create`, `update`,
/// `delete`, `link`).
pub fn action_subject(class: &str, op: &str) -> String {
    format!("huly.mcp.action.{class}.{op}")
}

/// Wire shape for `huly.mcp.tool.invoked`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolInvokedPayload {
    /// Tool name as registered with rmcp (e.g. `huly_create_issue`).
    pub tool: String,
    /// Workspace slug — `None` for tools that don't take a workspace
    /// (`huly_sync_status` / `huly_sync_cards`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub workspace: Option<String>,
    /// MCP `agent_id` (operator-configured; the same string the JWT
    /// broker logs for attribution).
    pub agent_id: String,
    /// SHA-256 of the JSON request body, hex, truncated to 16 chars.
    pub params_digest: String,
    /// ULID minted at tool entry; identical to the value plumbed into
    /// the transactor's `meta.request_id`.
    pub request_id: String,
    /// Wall-clock epoch ms of the invocation start.
    pub timestamp_ms: u64,
}

/// Result discriminator for `huly.mcp.tool.completed`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase", tag = "result")]
pub enum ToolCompletedResult {
    /// Tool returned `Ok(_)`. `result_digest` is the SHA-256 (16-char
    /// hex) of the tool's JSON return body.
    Ok { result_digest: String },
    /// Tool returned `Err(_)`. `error` is the human-readable error
    /// message (the same string surfaced to the MCP client).
    Err { error: String },
}

/// Wire shape for `huly.mcp.tool.completed`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCompletedPayload {
    /// Mirrors the `request_id` from the matching `tool.invoked`.
    pub request_id: String,
    /// Tool name (denormalised so subscribers don't have to join).
    pub tool: String,
    #[serde(flatten)]
    pub result: ToolCompletedResult,
    /// Wall-clock duration between `tool.invoked` and this event.
    pub duration_ms: u64,
    pub timestamp_ms: u64,
}

/// Wire shape for `huly.mcp.action.<class>.<op>`. Emitted only for
/// mutating tools — reads use `tool.invoked` for audit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionPayload {
    pub workspace: String,
    pub agent_id: String,
    pub request_id: String,
    /// Workspace-local id of the target document, when known at action
    /// time (always `None` for `create`; populated for `update` /
    /// `delete` / `link`).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub target_id: Option<String>,
    /// Names of fields the tool intends to modify (best-effort; pulled
    /// from the request body). `None` for `create` and `delete`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub fields_changed: Option<Vec<String>>,
    pub timestamp_ms: u64,
}

/// Wire shape for `huly.mcp.error`. Carries the full transactor
/// [`Status`]-shaped error when one was decoded, so subscribers can
/// reconstruct the failure server-side.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ErrorPayload {
    pub request_id: String,
    pub tool: String,
    /// `Status.code` from the transactor (e.g. `platform:status:Forbidden`).
    pub code: String,
    /// `Status.params.message` when present, otherwise the raw error
    /// string.
    pub message: String,
    /// Echo of `Status.params` (or any extra structured fields the
    /// transport surfaced). `Value::Null` when the error wasn't
    /// Status-shaped.
    #[serde(default)]
    pub params: Value,
    /// Optional transactor-side request id (when the transport echoed
    /// one back; reserved for a future REST surface change).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub transactor_request_id: Option<String>,
    pub timestamp_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subject_constants_are_stable() {
        assert_eq!(TOOL_INVOKED_SUBJECT, "huly.mcp.tool.invoked");
        assert_eq!(TOOL_COMPLETED_SUBJECT, "huly.mcp.tool.completed");
        assert_eq!(ERROR_SUBJECT, "huly.mcp.error");
    }

    #[test]
    fn action_subject_formats_class_and_op() {
        assert_eq!(
            action_subject("tracker.issue", "create"),
            "huly.mcp.action.tracker.issue.create"
        );
        assert_eq!(
            action_subject("card", "update"),
            "huly.mcp.action.card.update"
        );
    }

    #[test]
    fn tool_invoked_round_trip() {
        let p = ToolInvokedPayload {
            tool: "huly_create_issue".into(),
            workspace: Some("ws-1".into()),
            agent_id: "agent-test".into(),
            params_digest: "abc1234567890def".into(),
            request_id: "01J0000000000000000000000".into(),
            timestamp_ms: 1_700_000_000_000,
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<ToolInvokedPayload>(&s).unwrap(), p);
    }

    #[test]
    fn tool_completed_ok_serializes_result_tag() {
        let p = ToolCompletedPayload {
            request_id: "rq".into(),
            tool: "huly_get".into(),
            result: ToolCompletedResult::Ok {
                result_digest: "deadbeef".into(),
            },
            duration_ms: 12,
            timestamp_ms: 1,
        };
        let v: Value = serde_json::to_value(&p).unwrap();
        assert_eq!(v["result"], "ok");
        assert_eq!(v["result_digest"], "deadbeef");
        let back: ToolCompletedPayload = serde_json::from_value(v).unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn tool_completed_err_serializes_result_tag() {
        let p = ToolCompletedPayload {
            request_id: "rq".into(),
            tool: "huly_get".into(),
            result: ToolCompletedResult::Err {
                error: "boom".into(),
            },
            duration_ms: 5,
            timestamp_ms: 2,
        };
        let v: Value = serde_json::to_value(&p).unwrap();
        assert_eq!(v["result"], "err");
        assert_eq!(v["error"], "boom");
    }

    #[test]
    fn action_omits_optional_fields_when_none() {
        let p = ActionPayload {
            workspace: "ws".into(),
            agent_id: "a".into(),
            request_id: "rq".into(),
            target_id: None,
            fields_changed: None,
            timestamp_ms: 0,
        };
        let v: Value = serde_json::to_value(&p).unwrap();
        assert!(v.get("target_id").is_none());
        assert!(v.get("fields_changed").is_none());
    }

    #[test]
    fn error_payload_round_trip() {
        let p = ErrorPayload {
            request_id: "rq".into(),
            tool: "huly_create".into(),
            code: "platform:status:Forbidden".into(),
            message: "denied".into(),
            params: json!({"reason": "scope"}),
            transactor_request_id: None,
            timestamp_ms: 0,
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<ErrorPayload>(&s).unwrap(), p);
    }
}
