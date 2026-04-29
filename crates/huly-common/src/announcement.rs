use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// NATS subject for bridge announcements
pub const ANNOUNCE_SUBJECT: &str = "huly.bridge.announce";

/// NATS request/reply subject for on-demand bridge discovery.
/// Late-starting MCP subscribers send a request here to seed their registry
/// without waiting up to `ANNOUNCE_INTERVAL_SECS` for the next periodic
/// publish.
pub const LOOKUP_SUBJECT: &str = "huly.bridge.lookup";

/// NATS request/reply subject prefix for on-demand workspace-schema fetch.
/// Full subject: `{SCHEMA_FETCH_SUBJECT_PREFIX}.{workspace}`.
///
/// Bridges respond with a [`WorkspaceSchemaResponse`]. Used by huly-mcp
/// when its cached `schema_version` lags the version advertised in a
/// recent `BridgeAnnouncement`.
pub const SCHEMA_FETCH_SUBJECT_PREFIX: &str = "huly.bridge.schema";

/// Helper: build the per-workspace schema fetch subject.
pub fn schema_fetch_subject(workspace: &str) -> String {
    format!("{SCHEMA_FETCH_SUBJECT_PREFIX}.{workspace}")
}

/// Interval between announcements in seconds
pub const ANNOUNCE_INTERVAL_SECS: u64 = 10;

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

/// Reply payload on `{SCHEMA_FETCH_SUBJECT_PREFIX}.{workspace}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceSchemaResponse {
    pub workspace: String,
    pub schema_version: u64,
    pub schema: WorkspaceSchema,
}

/// Bridge announcement published periodically to NATS for discovery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BridgeAnnouncement {
    pub workspace: String,
    pub proxy_url: String,
    pub huly_connected: bool,
    pub nats_connected: bool,
    pub ready: bool,
    pub uptime_secs: u64,
    pub version: String,
    pub timestamp: u64,
    /// PersonId of the social identity the bridge's workspace token
    /// authenticates as. Consumers (e.g. huly-mcp) use this for the
    /// `modifiedBy` / `createdBy` fields of transactions they enqueue
    /// against the bridge — without it the transactor rejects writes
    /// with `platform:status:AccountMismatch`.
    /// `None` while the bridge has not yet connected to Huly, or when an
    /// older accounts server omits `socialId` from its responses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub social_id: Option<String>,
    /// Monotonic counter of the bridge's resolved workspace schema.
    /// Bumped each time the resolver re-reads MasterTags / Associations
    /// from the transactor and the result differs. `0` means "no schema
    /// resolved yet" — consumers should not cache anything against `0`.
    /// Optional on the wire so older bridges keep parsing.
    #[serde(default)]
    pub schema_version: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcement_serializes() {
        let ann = BridgeAnnouncement {
            workspace: "ws1".into(),
            proxy_url: "http://localhost:9090".into(),
            huly_connected: true,
            nats_connected: true,
            ready: true,
            uptime_secs: 3600,
            version: "0.1.0".into(),
            timestamp: 1700000000000,
            social_id: Some("soc-1".into()),
            schema_version: 7,
        };

        let json = serde_json::to_string(&ann).unwrap();
        let parsed: BridgeAnnouncement = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.workspace, "ws1");
        assert_eq!(parsed.proxy_url, "http://localhost:9090");
        assert!(parsed.ready);
        assert_eq!(parsed.social_id.as_deref(), Some("soc-1"));
        assert_eq!(parsed.schema_version, 7);
    }

    #[test]
    fn announcement_tolerates_missing_social_id_from_older_bridges() {
        // A bridge running an older version that doesn't yet emit
        // `socialId` must still parse — the field is optional on the wire.
        let json = r#"{
            "workspace": "ws1",
            "proxy_url": "http://h:9090",
            "huly_connected": true,
            "nats_connected": true,
            "ready": true,
            "uptime_secs": 0,
            "version": "0.0.1",
            "timestamp": 0
        }"#;
        let parsed: BridgeAnnouncement = serde_json::from_str(json).unwrap();
        assert!(parsed.social_id.is_none());
        assert_eq!(parsed.schema_version, 0);
    }

    #[test]
    fn announcement_omits_social_id_when_none() {
        // Round-trip with `None` must not include the field, so older
        // consumers that still parse with `deny_unknown_fields` (none
        // currently do, but defensive) aren't broken.
        let ann = BridgeAnnouncement {
            workspace: "ws".into(),
            proxy_url: "http://h:9090".into(),
            huly_connected: false,
            nats_connected: false,
            ready: false,
            uptime_secs: 0,
            version: "0.1.0".into(),
            timestamp: 0,
            social_id: None,
            schema_version: 0,
        };
        let json = serde_json::to_string(&ann).unwrap();
        assert!(!json.contains("social_id"));
    }

    #[test]
    fn schema_fetch_subject_is_per_workspace() {
        assert_eq!(
            schema_fetch_subject("muhasebot"),
            "huly.bridge.schema.muhasebot"
        );
    }

    #[test]
    fn workspace_schema_round_trip() {
        let mut s = WorkspaceSchema::default();
        s.card_types
            .insert("Module Spec".into(), "abc123".into());
        s.associations.insert("module".into(), "rel-1".into());

        let json = serde_json::to_string(&s).unwrap();
        let parsed: WorkspaceSchema = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, s);
    }

    #[test]
    fn workspace_schema_response_round_trip() {
        let mut s = WorkspaceSchema::default();
        s.card_types.insert("Module Spec".into(), "id-1".into());
        let resp = WorkspaceSchemaResponse {
            workspace: "ws1".into(),
            schema_version: 4,
            schema: s,
        };
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: WorkspaceSchemaResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.workspace, "ws1");
        assert_eq!(parsed.schema_version, 4);
        assert_eq!(
            parsed.schema.card_types.get("Module Spec").map(String::as_str),
            Some("id-1")
        );
    }
}
