use huly_common::announcement::{extract_host, is_unspecified_host};
use secrecy::SecretString;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct BridgeConfig {
    pub huly: HulyConfig,
    pub nats: NatsConfig,
    #[serde(default)]
    pub admin: AdminConfig,
    #[serde(default)]
    pub log: LogConfig,
    /// Per-workspace credentials the JWT broker uses to mint workspace tokens
    /// for MCP clients. One entry per workspace this bridge is authoritative
    /// for. Requests for workspaces not listed here return `unknown_workspace`
    /// — the primary `[huly]` block is the bridge's own session for events
    /// and is **not** a fallback for the broker.
    #[serde(default, rename = "workspace_credentials")]
    pub workspace_credentials: Vec<WorkspaceCredential>,
}

/// One mintable workspace. `password` and `token` are mutually exclusive —
/// configs that supply neither (or both) are rejected at load time.
#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceCredential {
    /// Human-readable workspace slug. Matches `MintRequest.workspace`.
    pub workspace: String,
    /// Account email used to log into the accounts service.
    pub email: String,
    /// Account password (mutually exclusive with `token`).
    #[serde(default)]
    pub password: Option<SecretString>,
    /// Pre-issued account-scoped token (mutually exclusive with `password`).
    #[serde(default)]
    pub token: Option<SecretString>,
    /// Optional override of the default workspace JWT lifetime, in seconds.
    /// Brokers can't introspect upstream JWT `exp` reliably across deploys
    /// (Huly Cloud and self-hosted issue different shapes), so the broker
    /// declares a conservative lifetime to MCP via `expires_at_ms`. Default
    /// 3600s.
    #[serde(default)]
    pub jwt_ttl_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct HulyConfig {
    pub url: String,
    pub workspace: String,
    /// Explicit accounts service base URL. When unset, defaults to `{url}/api/v1/accounts`.
    /// Set this for deployments that path-route accounts elsewhere — e.g. Huly Cloud /
    /// self-hosted with `/_accounts`. Discoverable via `{url}/config.json -> ACCOUNTS_URL`.
    pub accounts_url: Option<String>,
    pub auth: AuthConfig,
    #[serde(default)]
    pub use_binary_protocol: bool,
    #[serde(default = "default_true")]
    pub use_compression: bool,
    #[serde(default = "default_reconnect_delay")]
    pub reconnect_delay_ms: u64,
    #[serde(default = "default_ping_interval")]
    pub ping_interval_secs: u64,
    /// Cap on in-flight RPC requests held in the pending map. Saturating returns a
    /// transient `PendingRequestsExceeded` error (callers should back off + retry)
    /// and increments `huly_bridge_pending_requests_dropped_total`. Default protects
    /// against unbounded memory growth when the transactor stalls (issue #13).
    #[serde(default = "default_max_pending_requests")]
    pub max_pending_requests: usize,
    /// Skip TLS certificate verification (for self-signed certs in development).
    #[serde(default)]
    pub tls_skip_verify: bool,
    /// Path to custom CA certificate file (PEM format).
    pub tls_ca_cert: Option<String>,
}

// `AuthConfig` was hoisted into `huly-client` so the auth helpers can live
// alongside the rest of the transactor protocol code. Re-exported here so
// existing call sites under `crate::config::AuthConfig` keep working.
pub use huly_client::auth::AuthConfig;

#[derive(Debug, Deserialize)]
pub struct NatsConfig {
    #[serde(default = "default_nats_url")]
    pub url: String,
    pub subject_prefix: Option<String>,
    pub credentials: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AdminConfig {
    #[serde(default = "default_admin_host")]
    pub host: String,
    #[serde(default = "default_admin_port")]
    pub port: u16,
    /// URL to advertise in NATS announcements. If not set, constructed from host:port.
    pub advertise_url: Option<String>,
    /// Bearer token for authenticating platform API requests. Required — if not set, /api/v1/* returns 403.
    pub api_token: Option<SecretString>,
}

#[derive(Debug, Deserialize)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
    #[serde(default)]
    pub json: bool,
}

// Defaults

fn default_true() -> bool {
    true
}

fn default_reconnect_delay() -> u64 {
    1000
}

fn default_ping_interval() -> u64 {
    10
}

fn default_max_pending_requests() -> usize {
    huly_client::connection::DEFAULT_MAX_PENDING_REQUESTS
}

fn default_nats_url() -> String {
    "nats://127.0.0.1:4222".to_string()
}

fn default_admin_host() -> String {
    "127.0.0.1".to_string()
}

fn default_admin_port() -> u16 {
    9095
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            host: default_admin_host(),
            port: default_admin_port(),
            advertise_url: None,
            api_token: None,
        }
    }
}

impl AdminConfig {
    /// Returns the URL to advertise in NATS announcements.
    pub fn proxy_url(&self) -> String {
        self.advertise_url
            .clone()
            .unwrap_or_else(|| format!("http://{}:{}", self.host, self.port))
    }

    /// Reject configurations that would announce an unroutable URL — bind
    /// wildcards (`0.0.0.0`, `::`) work for listening but cannot be dialed
    /// by MCP clients on other hosts.
    fn validate(&self) -> anyhow::Result<()> {
        match &self.advertise_url {
            Some(url) => {
                if is_unspecified_host(extract_host(url)) {
                    anyhow::bail!(
                        "[admin] advertise_url ({url}) uses an unspecified host; \
                         set it to a routable address (e.g. the bridge host's LAN IP)"
                    );
                }
            }
            None => {
                if is_unspecified_host(&self.host) {
                    anyhow::bail!(
                        "[admin] host is a wildcard ({}) and advertise_url is not set; \
                         set advertise_url to a routable URL \
                         (e.g. \"http://<lan-ip>:{}\") so MCP clients on other hosts \
                         can reach this bridge",
                        self.host,
                        self.port,
                    );
                }
            }
        }
        Ok(())
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

impl BridgeConfig {
    pub fn from_file(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: BridgeConfig = toml::from_str(&content)?;
        config.admin.validate()?;
        config.validate_workspace_credentials()?;
        Ok(config)
    }

    /// Reject misconfigured credential entries early. Each entry must:
    /// - have exactly one of `password` / `token`, and
    /// - have a unique `workspace` slug across the array.
    ///
    /// Also enforces the P3 invariant: the workspace named in `[huly]`
    /// must be mintable. Either it appears explicitly in
    /// `workspace_credentials` or — for single-tenant deployments — the
    /// array is empty and the bridge will mint via `[huly].auth`. Any
    /// other shape (non-empty array that omits `[huly].workspace`) is a
    /// configuration bug because the bridge would advertise a workspace
    /// it cannot mint for.
    fn validate_workspace_credentials(&self) -> anyhow::Result<()> {
        use std::collections::HashSet;
        let mut seen: HashSet<&str> = HashSet::new();
        for entry in &self.workspace_credentials {
            if !seen.insert(entry.workspace.as_str()) {
                anyhow::bail!(
                    "[[workspace_credentials]] duplicate workspace slug: {}",
                    entry.workspace
                );
            }
            match (&entry.password, &entry.token) {
                (Some(_), Some(_)) => anyhow::bail!(
                    "[[workspace_credentials]] for {}: set exactly one of `password`/`token`, not both",
                    entry.workspace
                ),
                (None, None) => anyhow::bail!(
                    "[[workspace_credentials]] for {}: must set either `password` or `token`",
                    entry.workspace
                ),
                _ => {}
            }
        }
        if !self.workspace_credentials.is_empty()
            && !seen.contains(self.huly.workspace.as_str())
        {
            anyhow::bail!(
                "[[workspace_credentials]] is set but does not include the bridge's primary workspace `{}` — \
                 add an entry for it or remove the array entirely (single-tenant fallback)",
                self.huly.workspace
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    #[test]
    fn parse_full_config_with_token_auth() {
        let toml = r#"
            [huly]
            url = "https://huly.example.com"
            workspace = "my-workspace"

            [huly.auth]
            method = "token"
            token = "abc123"

            [nats]
            url = "nats://localhost:4222"
            subject_prefix = "huly"
        "#;

        let config: BridgeConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.huly.url, "https://huly.example.com");
        assert_eq!(config.huly.workspace, "my-workspace");
        assert!(matches!(config.huly.auth, AuthConfig::Token { .. }));
        assert!(!config.huly.use_binary_protocol); // default false — msgpackr incompatible
        assert!(config.huly.use_compression);
        assert_eq!(config.huly.reconnect_delay_ms, 1000);
        assert_eq!(config.huly.ping_interval_secs, 10);
        assert_eq!(config.huly.max_pending_requests, 10_000);
        assert_eq!(config.nats.url, "nats://localhost:4222");
        assert_eq!(config.nats.subject_prefix.as_deref(), Some("huly"));
        assert_eq!(config.admin.port, 9095);
        assert_eq!(config.log.level, "info");
    }

    #[test]
    fn parse_config_with_password_auth() {
        let toml = r#"
            [huly]
            url = "https://huly.example.com"
            workspace = "ws1"

            [huly.auth]
            method = "password"
            email = "user@example.com"
            password = "secret"

            [nats]
        "#;

        let config: BridgeConfig = toml::from_str(toml).unwrap();
        match &config.huly.auth {
            AuthConfig::Password { email, password } => {
                assert_eq!(email, "user@example.com");
                assert_eq!(password.expose_secret(), "secret");
            }
            _ => panic!("expected password auth"),
        }
        assert_eq!(config.nats.url, "nats://127.0.0.1:4222");
    }

    #[test]
    fn parse_config_with_custom_admin() {
        let toml = r#"
            [huly]
            url = "https://huly.example.com"
            workspace = "ws1"
            use_binary_protocol = false
            use_compression = false
            reconnect_delay_ms = 500
            ping_interval_secs = 5

            [huly.auth]
            method = "token"
            token = "tok"

            [nats]
            url = "nats://nats:4222"

            [admin]
            host = "0.0.0.0"
            port = 8080

            [log]
            level = "debug"
            json = true
        "#;

        let config: BridgeConfig = toml::from_str(toml).unwrap();
        assert!(!config.huly.use_binary_protocol);
        assert!(!config.huly.use_compression);
        assert_eq!(config.huly.reconnect_delay_ms, 500);
        assert_eq!(config.huly.ping_interval_secs, 5);
        assert_eq!(config.admin.host, "0.0.0.0");
        assert_eq!(config.admin.port, 8080);
        assert_eq!(config.log.level, "debug");
        assert!(config.log.json);
    }

    #[test]
    fn parse_config_with_tls_options() {
        let toml = r#"
            [huly]
            url = "https://huly.example.com"
            workspace = "ws1"
            tls_skip_verify = true
            tls_ca_cert = "/etc/ssl/custom-ca.pem"

            [huly.auth]
            method = "token"
            token = "tok"

            [nats]
            credentials = "/etc/nats/creds"

            [admin]
            api_token = "my-secret-token"
        "#;

        let config: BridgeConfig = toml::from_str(toml).unwrap();
        assert!(config.huly.tls_skip_verify);
        assert_eq!(config.huly.tls_ca_cert.as_deref(), Some("/etc/ssl/custom-ca.pem"));
        assert_eq!(config.nats.credentials.as_deref(), Some("/etc/nats/creds"));
        assert_eq!(config.admin.api_token.as_ref().map(|t| t.expose_secret()), Some("my-secret-token"));
    }

    #[test]
    fn tls_defaults_to_verify() {
        let toml = r#"
            [huly]
            url = "https://huly.example.com"
            workspace = "ws1"

            [huly.auth]
            method = "token"
            token = "tok"

            [nats]
        "#;

        let config: BridgeConfig = toml::from_str(toml).unwrap();
        assert!(!config.huly.tls_skip_verify);
        assert!(config.huly.tls_ca_cert.is_none());
        assert!(config.nats.credentials.is_none());
        assert!(config.admin.api_token.is_none());
    }

    #[test]
    fn invalid_toml_syntax_fails() {
        let toml = r#"
            [huly
            url = "https://huly.example.com"
        "#;
        let result: Result<BridgeConfig, _> = toml::from_str(toml);
        assert!(result.is_err());
    }

    #[test]
    fn from_file_missing_file_fails() {
        let result = BridgeConfig::from_file(std::path::Path::new("/nonexistent/config.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn missing_huly_section_fails() {
        let toml = r#"
            [nats]
            url = "nats://localhost:4222"
        "#;

        let result: Result<BridgeConfig, _> = toml::from_str(toml);
        assert!(result.is_err());
    }

    #[test]
    fn validate_rejects_wildcard_bind_without_advertise_url() {
        let admin = AdminConfig {
            host: "0.0.0.0".into(),
            port: 9095,
            advertise_url: None,
            api_token: None,
        };
        let err = admin.validate().unwrap_err().to_string();
        assert!(err.contains("advertise_url"), "error should name the missing field: {err}");
    }

    #[test]
    fn validate_rejects_ipv6_wildcard_bind_without_advertise_url() {
        let admin = AdminConfig {
            host: "::".into(),
            port: 9095,
            advertise_url: None,
            api_token: None,
        };
        assert!(admin.validate().is_err());
    }

    #[test]
    fn validate_accepts_wildcard_bind_with_routable_advertise_url() {
        let admin = AdminConfig {
            host: "0.0.0.0".into(),
            port: 9095,
            advertise_url: Some("http://192.168.0.10:9095".into()),
            api_token: None,
        };
        admin.validate().expect("routable advertise_url should pass");
    }

    #[test]
    fn validate_rejects_wildcard_advertise_url() {
        let admin = AdminConfig {
            host: "0.0.0.0".into(),
            port: 9095,
            advertise_url: Some("http://0.0.0.0:9095".into()),
            api_token: None,
        };
        assert!(admin.validate().is_err());
    }

    #[test]
    fn validate_rejects_ipv6_wildcard_advertise_url() {
        let admin = AdminConfig {
            host: "0.0.0.0".into(),
            port: 9095,
            advertise_url: Some("http://[::]:9095/".into()),
            api_token: None,
        };
        assert!(admin.validate().is_err());
    }

    #[test]
    fn validate_accepts_specific_bind_host() {
        let admin = AdminConfig {
            host: "127.0.0.1".into(),
            port: 9095,
            advertise_url: None,
            api_token: None,
        };
        admin.validate().expect("loopback bind should pass");
    }

    #[test]
    fn validate_accepts_hostname_advertise_url() {
        let admin = AdminConfig {
            host: "0.0.0.0".into(),
            port: 9095,
            advertise_url: Some("http://bridge.internal:9095".into()),
            api_token: None,
        };
        admin.validate().expect("hostname advertise_url should pass");
    }

    #[test]
    fn parse_workspace_credentials_array() {
        let toml = r#"
            [huly]
            url = "https://huly.example.com"
            workspace = "primary"

            [huly.auth]
            method = "token"
            token = "tok"

            [nats]

            [[workspace_credentials]]
            workspace = "primary"
            email = "p@example.com"
            password = "p1"

            [[workspace_credentials]]
            workspace = "secondary"
            email = "s@example.com"
            token = "acct-tok"
            jwt_ttl_secs = 7200
        "#;
        let config: BridgeConfig = toml::from_str(toml).unwrap();
        config.validate_workspace_credentials().unwrap();
        assert_eq!(config.workspace_credentials.len(), 2);
        assert_eq!(config.workspace_credentials[0].workspace, "primary");
        assert!(config.workspace_credentials[0].password.is_some());
        assert_eq!(config.workspace_credentials[1].jwt_ttl_secs, Some(7200));
    }

    #[test]
    fn workspace_credentials_default_to_empty() {
        let toml = r#"
            [huly]
            url = "https://huly.example.com"
            workspace = "ws1"
            [huly.auth]
            method = "token"
            token = "tok"
            [nats]
        "#;
        let config: BridgeConfig = toml::from_str(toml).unwrap();
        assert!(config.workspace_credentials.is_empty());
        config.validate_workspace_credentials().unwrap();
    }

    #[test]
    fn workspace_credentials_reject_both_password_and_token() {
        let toml = r#"
            [huly]
            url = "https://huly.example.com"
            workspace = "primary"
            [huly.auth]
            method = "token"
            token = "tok"
            [nats]
            [[workspace_credentials]]
            workspace = "primary"
            email = "p@example.com"
            password = "p1"
            token = "t1"
        "#;
        let config: BridgeConfig = toml::from_str(toml).unwrap();
        let err = config.validate_workspace_credentials().unwrap_err().to_string();
        assert!(err.contains("exactly one"), "got: {err}");
    }

    #[test]
    fn workspace_credentials_reject_neither_password_nor_token() {
        let toml = r#"
            [huly]
            url = "https://huly.example.com"
            workspace = "primary"
            [huly.auth]
            method = "token"
            token = "tok"
            [nats]
            [[workspace_credentials]]
            workspace = "primary"
            email = "p@example.com"
        "#;
        let config: BridgeConfig = toml::from_str(toml).unwrap();
        let err = config.validate_workspace_credentials().unwrap_err().to_string();
        assert!(err.contains("must set either"), "got: {err}");
    }

    #[test]
    fn workspace_credentials_reject_duplicate_slugs() {
        let toml = r#"
            [huly]
            url = "https://huly.example.com"
            workspace = "primary"
            [huly.auth]
            method = "token"
            token = "tok"
            [nats]
            [[workspace_credentials]]
            workspace = "primary"
            email = "a@example.com"
            password = "p"
            [[workspace_credentials]]
            workspace = "primary"
            email = "b@example.com"
            password = "q"
        "#;
        let config: BridgeConfig = toml::from_str(toml).unwrap();
        let err = config.validate_workspace_credentials().unwrap_err().to_string();
        assert!(err.contains("duplicate"), "got: {err}");
    }

    #[test]
    fn workspace_credentials_must_include_primary_workspace_when_set() {
        let toml = r#"
            [huly]
            url = "https://huly.example.com"
            workspace = "primary"
            [huly.auth]
            method = "token"
            token = "tok"
            [nats]
            [[workspace_credentials]]
            workspace = "secondary"
            email = "s@example.com"
            password = "s"
        "#;
        let config: BridgeConfig = toml::from_str(toml).unwrap();
        let err = config.validate_workspace_credentials().unwrap_err().to_string();
        assert!(err.contains("primary"), "got: {err}");
    }

    #[test]
    fn missing_workspace_fails() {
        let toml = r#"
            [huly]
            url = "https://huly.example.com"

            [huly.auth]
            method = "token"
            token = "tok"

            [nats]
        "#;

        let result: Result<BridgeConfig, _> = toml::from_str(toml);
        assert!(result.is_err());
    }
}
