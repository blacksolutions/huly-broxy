//! JWT broker — responds to `huly.bridge.mint` NATS requests.
//!
//! For every request the bridge:
//!   1. Looks up the workspace in the configured `[[workspace_credentials]]`
//!      table (fallback to the primary `[huly]` block when the table is empty
//!      and the slug matches — single-tenant deployments).
//!   2. Logs into the accounts service to obtain an account-scoped token.
//!   3. Calls `selectWorkspace` (slug → workspace UUID + endpoint + token).
//!   4. Returns a [`MintResponse`] containing both the workspace JWT and the
//!      account-scoped JWT. Per P1 spike findings: `workspace_uuid` is the
//!      REST URL key, and `account_service_jwt` is required for
//!      `huly_list_workspaces` (account-service path, not the transactor).
//!
//! The JWT body never appears in `tracing` output — only the slug, agent_id,
//! request_id, workspace_uuid and (success|error) outcome.

use crate::config::WorkspaceCredential;
use async_trait::async_trait;
use futures::StreamExt;
use huly_client::accounts::{AccountsClient, AccountsError, WorkspaceLoginInfo};
use huly_common::mint::{
    MINT_SUBJECT, MintError, MintReply, MintRequest, MintResponse, error_codes,
};
use secrecy::ExposeSecret;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Default workspace JWT TTL when the credential entry doesn't override it.
/// Conservative because the broker can't reliably introspect upstream JWT
/// `exp` across deploys (Huly Cloud and self-hosted issue different shapes).
/// MCP refreshes one minute before expiry — short cycles cost an extra
/// `selectWorkspace` round-trip, not correctness.
pub const DEFAULT_JWT_TTL_SECS: u64 = 3600;

/// Refresh leeway before expiry, per P3 spec.
pub const REFRESH_LEEWAY_MS: u64 = 60_000;

/// Abstraction over the accounts-service calls the broker makes. Trait so
/// tests can inject a hermetic stub without spinning up a wiremock per case.
#[async_trait]
pub trait AccountsLogin: Send + Sync {
    /// Exchange `(email, password)` for an account-scoped JWT.
    async fn login_password(&self, email: &str, password: &str) -> Result<String, AccountsError>;

    /// Resolve a workspace slug → `WorkspaceLoginInfo` (workspace UUID,
    /// transactor endpoint, workspace JWT) using the supplied account token.
    async fn select_workspace(
        &self,
        token: &str,
        workspace: &str,
    ) -> Result<WorkspaceLoginInfo, AccountsError>;
}

#[async_trait]
impl AccountsLogin for AccountsClient {
    async fn login_password(&self, email: &str, password: &str) -> Result<String, AccountsError> {
        AccountsClient::login_password(self, email, password).await
    }

    async fn select_workspace(
        &self,
        token: &str,
        workspace: &str,
    ) -> Result<WorkspaceLoginInfo, AccountsError> {
        AccountsClient::select_workspace(self, token, workspace).await
    }
}

/// Static configuration for the broker, keyed by workspace slug.
#[derive(Clone, Debug)]
pub struct MintBrokerConfig {
    /// REST base URL announced to MCP. Bridges typically derive this from
    /// `[huly].url + "/api/v1"` once at startup.
    pub rest_base_url: String,
    /// Accounts-service base URL announced to MCP — sourced from
    /// `[huly] accounts_url` so MCP doesn't have to guess. `None` means
    /// the operator did not configure one; downstream tools that need it
    /// (`huly_list_workspaces`) surface the gap clearly.
    pub accounts_url: Option<String>,
    /// Workspace slug → resolved credential.
    pub credentials: Arc<HashMap<String, ResolvedCredential>>,
}

/// Credential narrowed to what the broker actually uses — at most one of
/// `password` or `token`. Constructing this enforces the config invariant
/// once at startup so the hot path never has to re-check.
#[derive(Clone, Debug)]
pub struct ResolvedCredential {
    pub workspace: String,
    pub email: String,
    pub auth: ResolvedAuth,
    pub jwt_ttl_secs: u64,
}

#[derive(Clone, Debug)]
pub enum ResolvedAuth {
    Password(String),
    Token(String),
}

impl MintBrokerConfig {
    /// Build the broker map from `[[workspace_credentials]]`. Returns an
    /// error if any entry is malformed (should already have been caught by
    /// `BridgeConfig::validate_workspace_credentials`, but we re-check here
    /// because the broker is the only consumer and would silently misroute
    /// otherwise).
    pub fn from_credentials(
        rest_base_url: String,
        accounts_url: Option<String>,
        creds: &[WorkspaceCredential],
    ) -> anyhow::Result<Self> {
        let mut map = HashMap::with_capacity(creds.len());
        for c in creds {
            let auth = match (&c.password, &c.token) {
                (Some(p), None) => ResolvedAuth::Password(p.expose_secret().to_string()),
                (None, Some(t)) => ResolvedAuth::Token(t.expose_secret().to_string()),
                _ => anyhow::bail!(
                    "workspace_credentials[{}]: exactly one of password/token required",
                    c.workspace
                ),
            };
            map.insert(
                c.workspace.clone(),
                ResolvedCredential {
                    workspace: c.workspace.clone(),
                    email: c.email.clone(),
                    auth,
                    jwt_ttl_secs: c.jwt_ttl_secs.unwrap_or(DEFAULT_JWT_TTL_SECS),
                },
            );
        }
        Ok(Self {
            rest_base_url,
            accounts_url,
            credentials: Arc::new(map),
        })
    }
}

/// Mint a single response for `req`. Pure function on (config, accounts) so
/// it's trivially testable without NATS in the loop.
pub async fn handle_mint(
    cfg: &MintBrokerConfig,
    accounts: &dyn AccountsLogin,
    req: &MintRequest,
) -> MintReply {
    let Some(cred) = cfg.credentials.get(&req.workspace) else {
        warn!(
            workspace = %req.workspace,
            agent_id = %req.agent_id,
            request_id = %req.request_id,
            "mint denied: unknown workspace"
        );
        return MintReply::Err {
            error: MintError {
                code: error_codes::UNKNOWN_WORKSPACE.into(),
                message: format!("workspace `{}` is not provisioned on this bridge", req.workspace),
            },
        };
    };

    // Step 1: account-scoped token. For Token-auth credentials the operator
    // already pre-issued one; for Password-auth we log in.
    let account_token = match &cred.auth {
        ResolvedAuth::Token(t) => t.clone(),
        ResolvedAuth::Password(p) => match accounts.login_password(&cred.email, p).await {
            Ok(t) => t,
            Err(e) => {
                error!(
                    workspace = %req.workspace,
                    agent_id = %req.agent_id,
                    request_id = %req.request_id,
                    error = %e,
                    "mint failed: accounts login"
                );
                return MintReply::Err {
                    error: MintError {
                        code: error_codes::ACCOUNTS_FAILURE.into(),
                        message: format!("accounts login failed: {e}"),
                    },
                };
            }
        },
    };

    // Step 2: selectWorkspace → workspace UUID + transactor endpoint + ws JWT.
    let info = match accounts.select_workspace(&account_token, &req.workspace).await {
        Ok(info) => info,
        Err(e) => {
            error!(
                workspace = %req.workspace,
                agent_id = %req.agent_id,
                request_id = %req.request_id,
                error = %e,
                "mint failed: selectWorkspace"
            );
            return MintReply::Err {
                error: MintError {
                    code: error_codes::ACCOUNTS_FAILURE.into(),
                    message: format!("selectWorkspace failed: {e}"),
                },
            };
        }
    };

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let expires_at_ms = now_ms.saturating_add(cred.jwt_ttl_secs.saturating_mul(1000));
    let refresh_at_ms = expires_at_ms.saturating_sub(REFRESH_LEEWAY_MS);

    // workspace UUID is the field the official client calls `workspaceId`
    // — see api-client/src/rest/rest.ts:110,267. WorkspaceLoginInfo.workspace
    // already holds it on self-hosted Huly.
    let workspace_uuid = info.workspace.clone();

    info!(
        workspace = %req.workspace,
        workspace_uuid = %workspace_uuid,
        agent_id = %req.agent_id,
        request_id = %req.request_id,
        expires_at_ms,
        "mint ok"
    );

    MintReply::Ok(MintResponse {
        jwt: info.token,
        account_service_jwt: Some(account_token),
        expires_at_ms,
        refresh_at_ms,
        transactor_url: info.endpoint,
        rest_base_url: cfg.rest_base_url.clone(),
        workspace_uuid,
        accounts_url: cfg.accounts_url.clone(),
    })
}

/// Run the broker subscriber until `cancel` fires. Each request is handled
/// concurrently — `selectWorkspace` is network-bound and slow handlers
/// shouldn't head-of-line each other.
pub async fn run_mint_responder(
    nats: async_nats::Client,
    cfg: MintBrokerConfig,
    accounts: Arc<dyn AccountsLogin>,
    cancel: CancellationToken,
) {
    let mut subscriber = match nats.subscribe(MINT_SUBJECT.to_string()).await {
        Ok(sub) => sub,
        Err(e) => {
            error!(error = %e, "failed to subscribe to mint subject");
            return;
        }
    };
    info!(subject = MINT_SUBJECT, "JWT broker listening");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("mint responder stopping");
                return;
            }
            msg = subscriber.next() => {
                let Some(msg) = msg else {
                    warn!("mint subscription closed");
                    return;
                };
                let Some(reply_to) = msg.reply.clone() else {
                    debug!("mint request without reply-to — ignoring");
                    continue;
                };
                let nats = nats.clone();
                let cfg = cfg.clone();
                let accounts = accounts.clone();
                tokio::spawn(async move {
                    process_one(nats, cfg, accounts, msg.payload, reply_to).await;
                });
            }
        }
    }
}

async fn process_one(
    nats: async_nats::Client,
    cfg: MintBrokerConfig,
    accounts: Arc<dyn AccountsLogin>,
    payload: bytes::Bytes,
    reply_to: async_nats::Subject,
) {
    let req: MintRequest = match serde_json::from_slice(&payload) {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "mint request malformed");
            let reply = MintReply::Err {
                error: MintError {
                    code: error_codes::INVALID_REQUEST.into(),
                    message: format!("malformed request: {e}"),
                },
            };
            send_reply(&nats, reply_to, &reply).await;
            return;
        }
    };
    let reply = handle_mint(&cfg, accounts.as_ref(), &req).await;
    send_reply(&nats, reply_to, &reply).await;
}

async fn send_reply(nats: &async_nats::Client, reply_to: async_nats::Subject, reply: &MintReply) {
    match serde_json::to_vec(reply) {
        Ok(bytes) => {
            if let Err(e) = nats.publish(reply_to, bytes.into()).await {
                error!(error = %e, "failed to publish mint reply");
            }
        }
        Err(e) => error!(error = %e, "failed to serialize mint reply"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use huly_client::accounts::WorkspaceLoginInfo;
    use std::sync::Mutex;
    use tracing_test::traced_test;

    /// Hermetic stub — tests assert against captured calls.
    #[derive(Default)]
    struct StubAccounts {
        login_response: Mutex<Option<Result<String, AccountsError>>>,
        select_response: Mutex<Option<Result<WorkspaceLoginInfo, AccountsError>>>,
        login_calls: Mutex<Vec<(String, String)>>,
        select_calls: Mutex<Vec<(String, String)>>,
    }

    impl StubAccounts {
        fn with_login(self, r: Result<String, AccountsError>) -> Self {
            *self.login_response.lock().unwrap() = Some(r);
            self
        }
        fn with_select(self, r: Result<WorkspaceLoginInfo, AccountsError>) -> Self {
            *self.select_response.lock().unwrap() = Some(r);
            self
        }
    }

    #[async_trait]
    impl AccountsLogin for StubAccounts {
        async fn login_password(
            &self,
            email: &str,
            password: &str,
        ) -> Result<String, AccountsError> {
            self.login_calls
                .lock()
                .unwrap()
                .push((email.into(), password.into()));
            self.login_response
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| Ok("acct-token-stub".into()))
        }

        async fn select_workspace(
            &self,
            token: &str,
            workspace: &str,
        ) -> Result<WorkspaceLoginInfo, AccountsError> {
            self.select_calls
                .lock()
                .unwrap()
                .push((token.into(), workspace.into()));
            self.select_response
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_else(|| {
                    Ok(WorkspaceLoginInfo {
                        endpoint: "wss://huly.example/_t".into(),
                        token: "ws-jwt-stub".into(),
                        workspace: "uuid-default".into(),
                        social_id: None,
                    })
                })
        }
    }

    fn cfg_with(workspace: &str, auth: ResolvedAuth) -> MintBrokerConfig {
        let mut map = HashMap::new();
        map.insert(
            workspace.to_string(),
            ResolvedCredential {
                workspace: workspace.into(),
                email: format!("{workspace}@example.com"),
                auth,
                jwt_ttl_secs: 3600,
            },
        );
        MintBrokerConfig {
            rest_base_url: "https://huly.example/api/v1".into(),
            accounts_url: None,
            credentials: Arc::new(map),
        }
    }

    fn cfg_with_accounts(
        workspace: &str,
        auth: ResolvedAuth,
        accounts_url: &str,
    ) -> MintBrokerConfig {
        let mut c = cfg_with(workspace, auth);
        c.accounts_url = Some(accounts_url.to_string());
        c
    }

    fn req_for(workspace: &str) -> MintRequest {
        MintRequest {
            workspace: workspace.into(),
            agent_id: "agent-x".into(),
            request_id: "req-1".into(),
        }
    }

    #[tokio::test]
    async fn happy_path_returns_full_response() {
        let cfg = cfg_with("muhasebot", ResolvedAuth::Password("pw".into()));
        let stub = StubAccounts::default()
            .with_login(Ok("acct-jwt-123".into()))
            .with_select(Ok(WorkspaceLoginInfo {
                endpoint: "wss://huly.example/_transactor".into(),
                token: "ws-jwt-456".into(),
                workspace: "uuid-muhasebot".into(),
                social_id: Some("soc-1".into()),
            }));
        let reply = handle_mint(&cfg, &stub, &req_for("muhasebot")).await;

        let r = match reply {
            MintReply::Ok(r) => r,
            MintReply::Err { error } => panic!("unexpected error: {error:?}"),
        };
        assert_eq!(r.jwt, "ws-jwt-456");
        assert_eq!(r.account_service_jwt.as_deref(), Some("acct-jwt-123"));
        assert_eq!(r.transactor_url, "wss://huly.example/_transactor");
        assert_eq!(r.workspace_uuid, "uuid-muhasebot");
        assert_eq!(r.rest_base_url, "https://huly.example/api/v1");
        assert!(r.expires_at_ms > 0);
        assert_eq!(r.refresh_at_ms, r.expires_at_ms - REFRESH_LEEWAY_MS);
        // login + select were both called with right args.
        assert_eq!(stub.login_calls.lock().unwrap().len(), 1);
        let select = stub.select_calls.lock().unwrap();
        assert_eq!(select.len(), 1);
        assert_eq!(select[0].0, "acct-jwt-123");
        assert_eq!(select[0].1, "muhasebot");
    }

    #[tokio::test]
    async fn token_credential_skips_login() {
        let cfg = cfg_with("ws", ResolvedAuth::Token("pre-issued".into()));
        let stub = StubAccounts::default();
        let reply = handle_mint(&cfg, &stub, &req_for("ws")).await;
        match reply {
            MintReply::Ok(r) => {
                assert_eq!(r.account_service_jwt.as_deref(), Some("pre-issued"));
            }
            MintReply::Err { error } => panic!("{error:?}"),
        }
        assert!(stub.login_calls.lock().unwrap().is_empty());
        let select = stub.select_calls.lock().unwrap();
        assert_eq!(select[0].0, "pre-issued");
    }

    #[tokio::test]
    async fn unknown_workspace_returns_structured_error() {
        let cfg = cfg_with("known", ResolvedAuth::Password("p".into()));
        let stub = StubAccounts::default();
        let reply = handle_mint(&cfg, &stub, &req_for("does-not-exist")).await;
        match reply {
            MintReply::Err { error } => {
                assert_eq!(error.code, error_codes::UNKNOWN_WORKSPACE);
                assert!(error.message.contains("does-not-exist"));
            }
            MintReply::Ok(_) => panic!("expected error"),
        }
        // Should have made zero accounts calls.
        assert!(stub.login_calls.lock().unwrap().is_empty());
        assert!(stub.select_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn accounts_login_failure_returns_structured_error_no_panic() {
        let cfg = cfg_with("ws", ResolvedAuth::Password("p".into()));
        let stub = StubAccounts::default()
            .with_login(Err(AccountsError::Failed("rpc error -32000: bad creds".into())));
        let reply = handle_mint(&cfg, &stub, &req_for("ws")).await;
        match reply {
            MintReply::Err { error } => {
                assert_eq!(error.code, error_codes::ACCOUNTS_FAILURE);
                assert!(error.message.contains("bad creds"));
            }
            MintReply::Ok(_) => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn select_workspace_failure_returns_structured_error_no_panic() {
        let cfg = cfg_with("ws", ResolvedAuth::Token("t".into()));
        let stub = StubAccounts::default().with_select(Err(AccountsError::Failed(
            "rpc error -32001: workspace gone".into(),
        )));
        let reply = handle_mint(&cfg, &stub, &req_for("ws")).await;
        match reply {
            MintReply::Err { error } => {
                assert_eq!(error.code, error_codes::ACCOUNTS_FAILURE);
                assert!(error.message.contains("workspace gone"));
            }
            MintReply::Ok(_) => panic!("expected error"),
        }
    }

    #[tokio::test]
    #[traced_test]
    async fn jwt_body_never_appears_in_logs() {
        let cfg = cfg_with("ws", ResolvedAuth::Password("p".into()));
        let stub = StubAccounts::default()
            .with_login(Ok("acct-jwt-DO-NOT-LEAK-acc".into()))
            .with_select(Ok(WorkspaceLoginInfo {
                endpoint: "wss://huly.example/_t".into(),
                token: "ws-jwt-DO-NOT-LEAK-ws".into(),
                workspace: "uuid-w".into(),
                social_id: None,
            }));
        let reply = handle_mint(&cfg, &stub, &req_for("ws")).await;
        assert!(matches!(reply, MintReply::Ok(_)));
        // The audit log line was emitted (workspace_uuid and other metadata).
        // The JWT bodies must not appear anywhere in captured logs.
        assert!(!logs_contain("DO-NOT-LEAK-ws"));
        assert!(!logs_contain("DO-NOT-LEAK-acc"));
        // Sanity: a legitimate audit field does appear.
        assert!(logs_contain("uuid-w"));
    }

    #[test]
    fn from_credentials_rejects_both_password_and_token() {
        // Belt-and-suspenders against config-validation drift.
        let creds = vec![WorkspaceCredential {
            workspace: "ws".into(),
            email: "a@b".into(),
            password: Some(secrecy::SecretString::from("p")),
            token: Some(secrecy::SecretString::from("t")),
            jwt_ttl_secs: None,
        }];
        let err =
            MintBrokerConfig::from_credentials("https://x".into(), None, &creds).unwrap_err();
        assert!(err.to_string().contains("exactly one"));
    }

    #[tokio::test]
    async fn happy_path_propagates_configured_accounts_url() {
        let cfg = cfg_with_accounts(
            "ws",
            ResolvedAuth::Token("t".into()),
            "https://huly.example/_accounts",
        );
        let stub = StubAccounts::default().with_select(Ok(WorkspaceLoginInfo {
            endpoint: "wss://h/_t".into(),
            token: "ws-jwt".into(),
            workspace: "uuid-x".into(),
            social_id: None,
        }));
        let reply = handle_mint(&cfg, &stub, &req_for("ws")).await;
        match reply {
            MintReply::Ok(r) => {
                assert_eq!(
                    r.accounts_url.as_deref(),
                    Some("https://huly.example/_accounts")
                );
            }
            MintReply::Err { error } => panic!("unexpected error: {error:?}"),
        }
    }

    #[tokio::test]
    async fn happy_path_omits_accounts_url_when_unconfigured() {
        let cfg = cfg_with("ws", ResolvedAuth::Token("t".into()));
        let stub = StubAccounts::default().with_select(Ok(WorkspaceLoginInfo {
            endpoint: "wss://h/_t".into(),
            token: "ws-jwt".into(),
            workspace: "uuid-x".into(),
            social_id: None,
        }));
        let reply = handle_mint(&cfg, &stub, &req_for("ws")).await;
        match reply {
            MintReply::Ok(r) => assert!(r.accounts_url.is_none()),
            MintReply::Err { error } => panic!("unexpected error: {error:?}"),
        }
    }
}
