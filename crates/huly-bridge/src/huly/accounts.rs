use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Clone)]
pub struct AccountsClient {
    base_url: String,
    http: reqwest::Client,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceLoginInfo {
    pub endpoint: String,
    pub token: String,
    #[serde(default)]
    pub workspace: String,
    /// PersonId of the social identity the workspace token authenticates as.
    /// Upstream `WorkspaceLoginInfo extends LoginInfo` (see
    /// `huly.core/packages/account-client/src/types.ts:58`), so this field
    /// is always present in real responses but typed `Option<>` for tolerance
    /// against older accounts servers and test fixtures.
    #[serde(default)]
    pub social_id: Option<String>,
}

/// Result of `loginOtp`: indicates whether the OTP was sent and when retry is allowed.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtpInfo {
    pub sent: bool,
    pub retry_on: u64,
}

/// Result of `validateOtp` (and other login flows that yield an account session).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginInfo {
    pub account: String,
    pub name: Option<String>,
    pub social_id: Option<String>,
    pub token: Option<String>,
}

/// One entry returned by `getSocialIds`. Matches the upstream `SocialId`
/// shape (huly.core/packages/account-client/src/types.ts) — only the
/// fields the bridge needs are deserialized.
#[derive(Debug, Clone, Deserialize)]
pub struct SocialId {
    #[serde(rename = "_id")]
    pub id: String,
    /// `huly`, `email`, `google`, `github`, `oidc`. The transactor stamps
    /// `modifiedBy` with the `huly`-type id when present (see
    /// `pickPrimarySocialId` in huly.core/packages/core/src/utils.ts).
    pub r#type: String,
    #[serde(default)]
    pub is_deleted: bool,
}

/// Pick the canonical `_id` to stamp on transactions, mirroring upstream
/// `pickPrimarySocialId` in huly.core/packages/core/src/utils.ts: drop
/// deleted entries, prefer the `huly`-type if present, otherwise return
/// the first active entry. Returns `None` when no active entries remain.
pub fn pick_primary_social_id(ids: &[SocialId]) -> Option<&SocialId> {
    let active: Vec<&SocialId> = ids.iter().filter(|s| !s.is_deleted).collect();
    if active.is_empty() {
        return None;
    }
    active
        .iter()
        .find(|s| s.r#type == "huly")
        .copied()
        .or(active.first().copied())
}

#[derive(Debug, Deserialize)]
struct LoginTokenResponse {
    token: String,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse<T> {
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AccountsError {
    #[error("network error: {0}")]
    Network(String),
    #[error("accounts error: {0}")]
    Failed(String),
}

impl AccountsClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    /// Construct from config: explicit `accounts_url` if set, else `{huly_url}/api/v1/accounts`.
    pub fn from_config(huly_url: &str, accounts_url: Option<&str>) -> Self {
        let base = accounts_url
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("{}/api/v1/accounts", huly_url.trim_end_matches('/')));
        Self::new(base)
    }

    /// JSON-RPC `login({email, password})` → account token.
    /// Huly v0.7.382+ expects named params (object), not positional (array).
    pub async fn login_password(
        &self,
        email: &str,
        password: &str,
    ) -> Result<String, AccountsError> {
        let body = json!({
            "method": "login",
            "params": {"email": email, "password": password},
            "id": 1,
        });
        let result: LoginTokenResponse = self.call(&body, None).await?;
        Ok(result.token)
    }

    /// JSON-RPC `loginOtp({email})` → `{sent, retryOn}`.
    /// Triggers Huly to email a one-time code to the address. The returned `retry_on`
    /// is an epoch-ms timestamp before which a re-send will be rate-limited.
    pub async fn login_otp(&self, email: &str) -> Result<OtpInfo, AccountsError> {
        let body = json!({
            "method": "loginOtp",
            "params": {"email": email},
            "id": 1,
        });
        self.call(&body, None).await
    }

    /// JSON-RPC `validateOtp({email, code, password: null, action: null})` → `LoginInfo`.
    /// Exchanges an emailed OTP for an account-scoped session.
    pub async fn validate_otp(&self, email: &str, code: &str) -> Result<LoginInfo, AccountsError> {
        let body = json!({
            "method": "validateOtp",
            "params": {
                "email": email,
                "code": code,
                "password": null,
                "action": null,
            },
            "id": 1,
        });
        self.call(&body, None).await
    }

    /// JSON-RPC `selectWorkspace(workspace, "external")` → `{endpoint, token, workspace}`.
    pub async fn select_workspace(
        &self,
        token: &str,
        workspace: &str,
    ) -> Result<WorkspaceLoginInfo, AccountsError> {
        let body = json!({
            "method": "selectWorkspace",
            "params": [workspace, "external"],
            "id": 1,
        });
        self.call(&body, Some(token)).await
    }

    /// JSON-RPC `getLoginInfoByToken()` → `{endpoint, token, workspace}` for the
    /// workspace encoded in the supplied JWT. Use this for token auth — the JWT IS
    /// the workspace identity, so this avoids `selectWorkspace`'s slug lookup which
    /// can return a different workspace when the user has multiple.
    pub async fn get_login_info_by_token(
        &self,
        token: &str,
    ) -> Result<WorkspaceLoginInfo, AccountsError> {
        let body = json!({
            "method": "getLoginInfoByToken",
            "params": [],
            "id": 1,
        });
        self.call(&body, Some(token)).await
    }

    /// JSON-RPC `getLoginInfoByToken()` parsed as `LoginInfo` rather than
    /// `WorkspaceLoginInfo`. Useful when the supplied token is account-scoped
    /// (no workspace claim) — the server returns `{account, name, socialId,
    /// token}` without `endpoint`/`workspace`, which the wider
    /// `WorkspaceLoginInfo` deserializer rejects.
    ///
    /// Self-hosted Huly omits `socialId` from `selectWorkspace` responses but
    /// includes it here. **Note:** the `socialId` returned by this method is
    /// the *login* identity (often `email`-type), not the canonical PersonId
    /// the transactor expects on `modifiedBy`. Prefer `get_social_ids` +
    /// `pick_primary_social_id` for tx attribution.
    pub async fn get_login_info(
        &self,
        token: &str,
    ) -> Result<LoginInfo, AccountsError> {
        let body = json!({
            "method": "getLoginInfoByToken",
            "params": [],
            "id": 1,
        });
        self.call(&body, Some(token)).await
    }

    /// JSON-RPC `getSocialIds({includeDeleted})` → list of all social
    /// identities for the authenticated account. The transactor stamps
    /// `modifiedBy` with the `huly`-type id from this list (see
    /// `pickPrimarySocialId` in huly.core/packages/core/src/utils.ts);
    /// the email-type id from `getLoginInfoByToken` is the login
    /// identity, not the PersonId for tx attribution.
    pub async fn get_social_ids(
        &self,
        token: &str,
        include_deleted: bool,
    ) -> Result<Vec<SocialId>, AccountsError> {
        let body = json!({
            "method": "getSocialIds",
            "params": {"includeDeleted": include_deleted},
            "id": 1,
        });
        self.call(&body, Some(token)).await
    }

    async fn call<T: for<'de> Deserialize<'de>>(
        &self,
        body: &serde_json::Value,
        bearer: Option<&str>,
    ) -> Result<T, AccountsError> {
        let mut req = self.http.post(&self.base_url).json(body);
        if let Some(tok) = bearer {
            req = req.bearer_auth(tok);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| AccountsError::Network(e.to_string()))?;

        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| AccountsError::Network(e.to_string()))?;

        if !status.is_success() {
            return Err(AccountsError::Failed(format!(
                "HTTP {}: {}",
                status.as_u16(),
                text
            )));
        }

        let parsed: JsonRpcResponse<T> = serde_json::from_str(&text)
            .map_err(|e| AccountsError::Failed(format!("invalid response: {e}: {text}")))?;

        if let Some(err) = parsed.error {
            return Err(AccountsError::Failed(format!(
                "rpc error {}: {}",
                err.code, err.message
            )));
        }
        parsed
            .result
            .ok_or_else(|| AccountsError::Failed("missing result field".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn from_config_defaults_to_legacy_path() {
        let c = AccountsClient::from_config("https://huly.example.com/", None);
        assert_eq!(c.base_url, "https://huly.example.com/api/v1/accounts");
    }

    #[test]
    fn from_config_uses_explicit_url() {
        let c = AccountsClient::from_config(
            "https://huly.example.com",
            Some("https://huly.example.com/_accounts"),
        );
        assert_eq!(c.base_url, "https://huly.example.com/_accounts");
    }

    #[tokio::test]
    async fn select_workspace_returns_endpoint_and_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_json(json!({
                "method": "selectWorkspace",
                "params": ["test-workspace", "external"],
                "id": 1,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "result": {
                    "endpoint": "wss://huly.example.com/_transactor",
                    "token": "ws-scoped-token",
                    "workspace": "uuid-1234",
                    "socialId": "soc-abc",
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let info = c.select_workspace("acct-token", "test-workspace").await.unwrap();
        assert_eq!(info.endpoint, "wss://huly.example.com/_transactor");
        assert_eq!(info.token, "ws-scoped-token");
        assert_eq!(info.workspace, "uuid-1234");
        assert_eq!(info.social_id.as_deref(), Some("soc-abc"));
    }

    #[tokio::test]
    async fn select_workspace_tolerates_missing_social_id() {
        // Older accounts servers may omit the `socialId` field entirely;
        // deserialize must accept the legacy shape.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "result": {
                    "endpoint": "wss://huly.example.com/_transactor",
                    "token": "ws-tok",
                    "workspace": "ws-1",
                }
            })))
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let info = c.select_workspace("t", "ws").await.unwrap();
        assert!(info.social_id.is_none());
    }

    #[tokio::test]
    async fn select_workspace_propagates_rpc_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "error": {"code": -32000, "message": "workspace not found"}
            })))
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let err = c.select_workspace("t", "missing").await.unwrap_err();
        assert!(matches!(err, AccountsError::Failed(m) if m.contains("workspace not found")));
    }

    #[tokio::test]
    async fn select_workspace_propagates_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let err = c.select_workspace("t", "ws").await.unwrap_err();
        match err {
            AccountsError::Failed(m) => assert!(m.contains("401"), "got: {m}"),
            _ => panic!("expected Failed"),
        }
    }

    #[tokio::test]
    async fn select_workspace_network_error_unreachable() {
        let c = AccountsClient::new("http://127.0.0.1:1");
        let err = c.select_workspace("t", "ws").await.unwrap_err();
        assert!(matches!(err, AccountsError::Network(_)));
    }

    #[tokio::test]
    async fn login_password_returns_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_json(json!({
                "method": "login",
                "params": {"email": "alice@example.com", "password": "hunter2"},
                "id": 1,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "result": {"token": "acct-token-xyz"}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let token = c.login_password("alice@example.com", "hunter2").await.unwrap();
        assert_eq!(token, "acct-token-xyz");
    }

    #[tokio::test]
    async fn get_login_info_by_token_returns_endpoint_and_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_json(json!({
                "method": "getLoginInfoByToken",
                "params": [],
                "id": 1,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "result": {
                    "endpoint": "wss://huly.example.com/_transactor",
                    "token": "ws-scoped-token",
                    "workspace": "uuid-test-workspace",
                    "workspaceUrl": "test-workspace",
                    "role": "OWNER",
                    "socialId": "soc-token-flow",
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let info = c.get_login_info_by_token("acct-token").await.unwrap();
        assert_eq!(info.endpoint, "wss://huly.example.com/_transactor");
        assert_eq!(info.token, "ws-scoped-token");
        assert_eq!(info.workspace, "uuid-test-workspace");
        assert_eq!(info.social_id.as_deref(), Some("soc-token-flow"));
    }

    #[test]
    fn pick_primary_social_id_prefers_huly_type() {
        let ids = vec![
            SocialId {
                id: "email-id".into(),
                r#type: "email".into(),
                is_deleted: false,
            },
            SocialId {
                id: "huly-id".into(),
                r#type: "huly".into(),
                is_deleted: false,
            },
        ];
        assert_eq!(pick_primary_social_id(&ids).unwrap().id, "huly-id");
    }

    #[test]
    fn pick_primary_social_id_skips_deleted_entries() {
        let ids = vec![
            SocialId {
                id: "huly-deleted".into(),
                r#type: "huly".into(),
                is_deleted: true,
            },
            SocialId {
                id: "email-active".into(),
                r#type: "email".into(),
                is_deleted: false,
            },
        ];
        assert_eq!(pick_primary_social_id(&ids).unwrap().id, "email-active");
    }

    #[test]
    fn pick_primary_social_id_returns_none_when_all_deleted() {
        let ids = vec![SocialId {
            id: "x".into(),
            r#type: "huly".into(),
            is_deleted: true,
        }];
        assert!(pick_primary_social_id(&ids).is_none());
    }

    #[test]
    fn pick_primary_social_id_falls_back_to_first_active_when_no_huly_type() {
        let ids = vec![
            SocialId {
                id: "github-id".into(),
                r#type: "github".into(),
                is_deleted: false,
            },
            SocialId {
                id: "email-id".into(),
                r#type: "email".into(),
                is_deleted: false,
            },
        ];
        assert_eq!(pick_primary_social_id(&ids).unwrap().id, "github-id");
    }

    #[tokio::test]
    async fn get_social_ids_parses_response_array() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_json(json!({
                "method": "getSocialIds",
                "params": {"includeDeleted": false},
                "id": 1,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "result": [
                    {"_id": "1167144386167767041", "type": "email", "isDeleted": false,
                     "key": "email:user@example.com", "value": "user@example.com"},
                    {"_id": "1167144386200764417", "type": "huly", "isDeleted": false,
                     "key": "huly:uuid", "value": "uuid"}
                ]
            })))
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let ids = c.get_social_ids("acct-token", false).await.unwrap();
        assert_eq!(ids.len(), 2);
        let primary = pick_primary_social_id(&ids).unwrap();
        assert_eq!(primary.id, "1167144386200764417");
        assert_eq!(primary.r#type, "huly");
    }

    #[tokio::test]
    async fn get_login_info_returns_social_id_for_account_token() {
        // Self-hosted Huly returns LoginInfo (not WorkspaceLoginInfo) when
        // `getLoginInfoByToken` is called with an account-scoped token.
        // The bridge needs this path to capture socialId, since
        // selectWorkspace omits it on this deployment.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_json(json!({
                "method": "getLoginInfoByToken",
                "params": [],
                "id": 1,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "result": {
                    "account": "uuid-acct-1",
                    "name": "Murat",
                    "socialId": "1167144386167767041",
                    "token": "acct-token",
                }
            })))
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let info = c.get_login_info("acct-token").await.unwrap();
        assert_eq!(info.account, "uuid-acct-1");
        assert_eq!(info.social_id.as_deref(), Some("1167144386167767041"));
    }

    #[tokio::test]
    async fn get_login_info_by_token_propagates_rpc_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "error": {"code": -32001, "message": "token expired"}
            })))
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let err = c.get_login_info_by_token("t").await.unwrap_err();
        assert!(matches!(err, AccountsError::Failed(m) if m.contains("token expired")));
    }

    #[tokio::test]
    async fn login_otp_sends_named_params_and_parses_camel_case() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_json(json!({
                "method": "loginOtp",
                "params": {"email": "alice@example.com"},
                "id": 1,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "result": {"sent": true, "retryOn": 1_700_000_000_000u64}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let info = c.login_otp("alice@example.com").await.unwrap();
        assert!(info.sent);
        assert_eq!(info.retry_on, 1_700_000_000_000);
    }

    #[tokio::test]
    async fn login_otp_propagates_rpc_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "error": {"code": -32010, "message": "rate limited"}
            })))
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let err = c.login_otp("a@b.c").await.unwrap_err();
        assert!(matches!(err, AccountsError::Failed(m) if m.contains("rate limited")));
    }

    #[tokio::test]
    async fn validate_otp_sends_full_params_and_parses_login_info() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .and(body_json(json!({
                "method": "validateOtp",
                "params": {
                    "email": "alice@example.com",
                    "code": "123456",
                    "password": null,
                    "action": null,
                },
                "id": 1,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "result": {
                    "account": "uuid-acct-1",
                    "name": "Alice",
                    "socialId": "soc-42",
                    "token": "session-xyz",
                }
            })))
            .expect(1)
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let info = c
            .validate_otp("alice@example.com", "123456")
            .await
            .unwrap();
        assert_eq!(info.account, "uuid-acct-1");
        assert_eq!(info.name.as_deref(), Some("Alice"));
        assert_eq!(info.social_id.as_deref(), Some("soc-42"));
        assert_eq!(info.token.as_deref(), Some("session-xyz"));
    }

    #[tokio::test]
    async fn validate_otp_accepts_null_optional_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "result": {
                    "account": "uuid-acct-2",
                    "name": null,
                    "socialId": null,
                    "token": null,
                }
            })))
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let info = c.validate_otp("a@b.c", "000000").await.unwrap();
        assert_eq!(info.account, "uuid-acct-2");
        assert!(info.name.is_none());
        assert!(info.social_id.is_none());
        assert!(info.token.is_none());
    }

    #[tokio::test]
    async fn validate_otp_propagates_rpc_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "error": {"code": -32011, "message": "invalid code"}
            })))
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let err = c.validate_otp("a@b.c", "999999").await.unwrap_err();
        assert!(matches!(err, AccountsError::Failed(m) if m.contains("invalid code")));
    }

    #[tokio::test]
    async fn invalid_json_returns_failed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let c = AccountsClient::new(server.uri());
        let err = c.select_workspace("t", "ws").await.unwrap_err();
        assert!(matches!(err, AccountsError::Failed(m) if m.contains("invalid response")));
    }
}
