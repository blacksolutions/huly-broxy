use serde::{Deserialize, Serialize};

/// NATS subject for bridge announcements
pub const ANNOUNCE_SUBJECT: &str = "huly.bridge.announce";

/// NATS request/reply subject for on-demand bridge discovery.
/// Late-starting MCP subscribers send a request here to seed their registry
/// without waiting up to `ANNOUNCE_INTERVAL_SECS` for the next periodic
/// publish.
pub const LOOKUP_SUBJECT: &str = "huly.bridge.lookup";

/// Interval between announcements in seconds
pub const ANNOUNCE_INTERVAL_SECS: u64 = 10;

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
        };

        let json = serde_json::to_string(&ann).unwrap();
        let parsed: BridgeAnnouncement = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.workspace, "ws1");
        assert_eq!(parsed.proxy_url, "http://localhost:9090");
        assert!(parsed.ready);
        assert_eq!(parsed.social_id.as_deref(), Some("soc-1"));
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
        };
        let json = serde_json::to_string(&ann).unwrap();
        assert!(!json.contains("social_id"));
    }
}
