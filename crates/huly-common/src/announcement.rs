use serde::{Deserialize, Serialize};

/// NATS subject for bridge announcements
pub const ANNOUNCE_SUBJECT: &str = "huly.bridge.announce";

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
        };

        let json = serde_json::to_string(&ann).unwrap();
        let parsed: BridgeAnnouncement = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.workspace, "ws1");
        assert_eq!(parsed.proxy_url, "http://localhost:9090");
        assert!(parsed.ready);
    }
}
