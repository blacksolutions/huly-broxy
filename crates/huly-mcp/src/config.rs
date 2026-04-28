use crate::mcp::catalog::CatalogOverrides;
use secrecy::SecretString;
use serde::Deserialize;
use std::path::{Path, PathBuf};

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
    #[serde(default = "default_stale_timeout")]
    pub stale_timeout_secs: u64,
    /// Bearer token for authenticating with bridge `/api/v1/*` endpoints.
    /// Must match the bridge's `admin.api_token` value.
    pub bridge_api_token: Option<SecretString>,
    /// Override card type / relation IDs. Defaults match Muhasebot deployment.
    /// Use `[mcp.catalog.card_types]` and `[mcp.catalog.relations]` TOML
    /// sub-tables to override per-name.
    #[serde(default)]
    pub catalog: CatalogOverrides,
    /// Optional configuration for the Node.js sync pipeline subprocess used
    /// by `huly_sync_status` and `huly_sync_cards`. If unset, those tools
    /// return a helpful error explaining how to configure them.
    #[serde(default)]
    pub sync: Option<SyncConfig>,
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

fn default_stale_timeout() -> u64 {
    30
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            stale_timeout_secs: default_stale_timeout(),
            bridge_api_token: None,
            catalog: CatalogOverrides::default(),
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
        let config: McpConfig = toml::from_str(&content)?;
        Ok(config)
    }
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
        assert_eq!(config.mcp.stale_timeout_secs, 30);
        assert_eq!(config.log.level, "info");
    }

    #[test]
    fn parse_catalog_overrides() {
        let toml = r#"
            [nats]
            url = "nats://localhost:4222"

            [mcp.catalog.card_types]
            "Module Spec" = "custom-module-id"

            [mcp.catalog.relations]
            "module" = "custom-rel-id"
        "#;

        let config: McpConfig = toml::from_str(toml).unwrap();
        assert_eq!(
            config.mcp.catalog.card_types.get("Module Spec"),
            Some(&"custom-module-id".to_string())
        );
        assert_eq!(
            config.mcp.catalog.relations.get("module"),
            Some(&"custom-rel-id".to_string())
        );
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
            stale_timeout_secs = 60

            [log]
            level = "debug"
            json = true
        "#;

        let config: McpConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.mcp.stale_timeout_secs, 60);
        assert_eq!(config.log.level, "debug");
        assert!(config.log.json);
    }
}
