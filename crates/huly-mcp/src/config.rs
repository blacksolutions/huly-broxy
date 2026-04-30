use serde::Deserialize;
use std::path::{Path, PathBuf};
use toml::Value;

#[derive(Debug, Deserialize)]
pub struct McpConfig {
    pub nats: NatsConfig,
    #[serde(default)]
    pub mcp: McpSettings,
    #[serde(default)]
    pub log: LogConfig,
}

#[derive(Debug, Deserialize)]
pub struct NatsConfig {
    #[serde(default = "default_nats_url")]
    pub url: String,
    pub credentials: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct McpSettings {
    /// Identifier of the calling agent. Required (D8): the bridge JWT broker
    /// logs this for audit and per-agent rate-limit attribution. Failure to
    /// set it (and the absence of an rmcp clientInfo override) is a fatal
    /// startup error rather than a silent fallback.
    #[serde(default)]
    pub agent_id: Option<String>,
    /// Selected MCP transport. Per the P1 spike, "rest" is the only
    /// implemented variant in P4; "ws" is reserved for a future variant
    /// that talks to the transactor over WebSocket directly.
    #[serde(default = "default_transport")]
    pub transport: String,
    /// Optional configuration for the Node.js sync pipeline subprocess used
    /// by `huly_sync_status` and `huly_sync_cards`. If unset, those tools
    /// return a helpful error explaining how to configure them.
    #[serde(default)]
    pub sync: Option<SyncConfig>,
}

fn default_transport() -> String {
    "rest".to_string()
}

/// Configuration for shelling out to the upstream Node.js sync pipeline.
///
/// Upstream lives at `huly-api/packages/sync/dist/index.js`. Set `script_path`
/// to that file's absolute path on disk. `working_dir` controls where the
/// sync runs (affects `.huly-sync-state.json` and `docs/` resolution).
#[derive(Debug, Clone, Deserialize)]
pub struct SyncConfig {
    /// Absolute path to the compiled sync entrypoint (e.g.
    /// `/path/to/huly-api/packages/sync/dist/index.js`). Required.
    pub script_path: PathBuf,
    /// Node binary to invoke. Defaults to `node` (found on PATH).
    #[serde(default = "default_node_binary")]
    pub node_binary: String,
    /// Working directory for the subprocess. The sync tool resolves
    /// `.huly-sync-state.json` and `docs/` relative to this dir. Defaults to
    /// the current process working directory (".").
    #[serde(default = "default_working_dir")]
    pub working_dir: PathBuf,
    /// Hard timeout for the subprocess; killed if exceeded. Defaults to 300s.
    #[serde(default = "default_sync_timeout")]
    pub timeout_secs: u64,
}

fn default_node_binary() -> String {
    "node".to_string()
}

fn default_working_dir() -> PathBuf {
    PathBuf::from(".")
}

fn default_sync_timeout() -> u64 {
    300
}

#[derive(Debug, Deserialize)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default)]
    pub json: bool,
}

fn default_nats_url() -> String {
    "nats://127.0.0.1:4222".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            agent_id: None,
            transport: default_transport(),
            sync: None,
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            json: false,
        }
    }
}

impl McpConfig {
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        reject_legacy_catalog(&content)?;
        let config: McpConfig = toml::from_str(&content)?;
        Ok(config)
    }
}

/// Hard-error if the operator still has a `[mcp.catalog]` (or sub-table)
/// section in their TOML. Card-type and association IDs are now resolved
/// per-workspace at runtime via the bridge — silently ignoring the
/// override is what got workspace-local IDs hardcoded into source in
/// the first place. Loud migration is the safer default.
fn reject_legacy_catalog(content: &str) -> anyhow::Result<()> {
    let parsed: Value = toml::from_str(content)?;
    let mcp = match parsed.get("mcp").and_then(|v| v.as_table()) {
        Some(t) => t,
        None => return Ok(()),
    };
    if mcp.contains_key("catalog") {
        return Err(anyhow::anyhow!(
            "[mcp.catalog] is no longer supported. Card type and relation IDs are now \
             resolved per-workspace at runtime by the bridge. Remove the [mcp.catalog] \
             (and any sub-tables like [mcp.catalog.card_types] / [mcp.catalog.relations]) \
             section from your config."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_toml_syntax_fails() {
        let toml = r#"
            [nats
            url = "nats://localhost:4222"
        "#;
        let result: Result<McpConfig, _> = toml::from_str(toml);
        assert!(result.is_err());
    }

    #[test]
    fn from_file_missing_file_fails() {
        let result = McpConfig::from_file(std::path::Path::new("/nonexistent/config.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_minimal_config() {
        let toml = r#"
            [nats]
            url = "nats://localhost:4222"
        "#;

        let config: McpConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.nats.url, "nats://localhost:4222");
        assert_eq!(config.mcp.transport, "rest");
        assert!(config.mcp.agent_id.is_none());
        assert_eq!(config.log.level, "info");
    }

    #[test]
    fn parse_agent_id_from_mcp_section() {
        let toml = r#"
            [nats]
            url = "nats://localhost:4222"

            [mcp]
            agent_id = "claude-code-murat-001"
            transport = "rest"
        "#;
        let config: McpConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.mcp.agent_id.as_deref(), Some("claude-code-murat-001"));
        assert_eq!(config.mcp.transport, "rest");
    }

    #[test]
    fn legacy_catalog_section_rejected_loudly() {
        // Operators upgrading must remove [mcp.catalog] explicitly — silently
        // ignoring it is what let workspace-local IDs hardcode into source
        // last time around.
        let toml = r#"
            [nats]
            url = "nats://localhost:4222"

            [mcp.catalog.card_types]
            "Module Spec" = "stale-id"
        "#;
        let err = reject_legacy_catalog(toml).expect_err("must reject legacy catalog");
        let msg = err.to_string();
        assert!(msg.contains("[mcp.catalog]"), "msg: {msg}");
        assert!(msg.contains("no longer supported"), "msg: {msg}");
    }

    #[test]
    fn config_without_legacy_catalog_passes() {
        let toml = r#"
            [nats]
            url = "nats://localhost:4222"
        "#;
        reject_legacy_catalog(toml).unwrap();
    }

    #[test]
    fn parse_sync_config_minimal() {
        let toml = r#"
            [nats]
            url = "nats://localhost:4222"

            [mcp.sync]
            script_path = "/opt/huly/sync/dist/index.js"
        "#;

        let config: McpConfig = toml::from_str(toml).unwrap();
        let sync = config.mcp.sync.expect("sync should parse");
        assert_eq!(
            sync.script_path,
            std::path::PathBuf::from("/opt/huly/sync/dist/index.js")
        );
        assert_eq!(sync.node_binary, "node");
        assert_eq!(sync.working_dir, std::path::PathBuf::from("."));
        assert_eq!(sync.timeout_secs, 300);
    }

    #[test]
    fn parse_sync_config_full_overrides() {
        let toml = r#"
            [nats]
            url = "nats://localhost:4222"

            [mcp.sync]
            script_path = "/opt/sync.js"
            node_binary = "/usr/local/bin/node"
            working_dir = "/var/huly"
            timeout_secs = 60
        "#;

        let config: McpConfig = toml::from_str(toml).unwrap();
        let sync = config.mcp.sync.unwrap();
        assert_eq!(sync.node_binary, "/usr/local/bin/node");
        assert_eq!(sync.working_dir, std::path::PathBuf::from("/var/huly"));
        assert_eq!(sync.timeout_secs, 60);
    }

    #[test]
    fn parse_full_config() {
        let toml = r#"
            [nats]
            url = "nats://nats:4222"
            credentials = "/etc/nats/creds"

            [mcp]
            agent_id = "claude-code-murat-001"

            [log]
            level = "debug"
            json = true
        "#;

        let config: McpConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.mcp.agent_id.as_deref(), Some("claude-code-murat-001"));
        assert_eq!(config.log.level, "debug");
        assert!(config.log.json);
    }
}
