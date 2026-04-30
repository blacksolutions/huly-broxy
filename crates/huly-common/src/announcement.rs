//! Shared workspace-schema type.
//!
//! Pre-P4 this module also held bridge announcement / lookup wire types.
//! P4 / D10 deletes the announce-and-discover subject set entirely; only
//! the workspace-schema type survives because the MCP factory caches it
//! per workspace (D9).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Subject-prefix constant for the post-P4 event-only NATS surface.
///
/// The bridge publishes transactor events on `{EVENT_SUBJECT_PREFIX}.<class>.…`.
/// MCP subscribes to filtered slices (e.g. `huly.event.tx.core.class.*`)
/// to invalidate its schema cache. P7 (consumer subscriber) wires this in;
/// keep the constant available so callers can reference one canonical
/// string.
pub const EVENT_SUBJECT_PREFIX: &str = "huly.event";

/// Workspace-local Huly schema: name → workspace-local `_id`.
///
/// Maps user-visible names (e.g. "Module Spec") to the workspace's
/// MasterTag / Association `_id`s (e.g. `69cba7dae4930c825a40f63f`).
/// IDs are workspace-local — different workspaces will have different IDs
/// for the same conceptual MasterTag, and may have entirely different sets.
///
/// `BTreeMap` for deterministic serialization (stable hashes if anyone
/// later wants to etag the body itself).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSchema {
    /// MasterTag ("card type") name → workspace-local `_id`.
    #[serde(default)]
    pub card_types: BTreeMap<String, String>,
    /// Association ("relation") label → workspace-local `_id`.
    #[serde(default)]
    pub associations: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_schema_round_trip() {
        let mut s = WorkspaceSchema::default();
        s.card_types.insert("Module Spec".into(), "abc123".into());
        s.associations.insert("module".into(), "rel-1".into());

        let json = serde_json::to_string(&s).unwrap();
        let parsed: WorkspaceSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, s);
    }

    #[test]
    fn event_subject_prefix_pinned() {
        // Pinned constant — bumping is a wire-format change; coordinate
        // with P7 before changing.
        assert_eq!(EVENT_SUBJECT_PREFIX, "huly.event");
    }
}
