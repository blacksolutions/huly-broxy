use crate::config::AuthConfig;
use crate::huly::accounts::{AccountsClient, AccountsError};
use secrecy::ExposeSecret;

/// Authenticate with Huly and return an account-scoped session token.
///
/// `accounts_url` overrides the default `{base_url}/api/v1/accounts` derivation.
pub async fn authenticate(
    base_url: &str,
    accounts_url: Option<&str>,
    auth: &AuthConfig,
) -> Result<String, AuthError> {
    match auth {
        AuthConfig::Token { token } => Ok(token.expose_secret().to_string()),
        AuthConfig::Password { email, password } => {
            let client = AccountsClient::from_config(base_url, accounts_url);
            client
                .login_password(email, password.expose_secret())
                .await
                .map_err(AuthError::from)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("auth failed: {0}")]
    Failed(String),

    #[error("network error: {0}")]
    Network(String),
}

impl From<AccountsError> for AuthError {
    fn from(e: AccountsError) -> Self {
        match e {
            AccountsError::Network(m) => AuthError::Network(m),
            AccountsError::Failed(m) => AuthError::Failed(m),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthConfig;
    use secrecy::SecretString;
    use serde_json::json;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn token_auth_returns_token_directly() {
        let auth = AuthConfig::Token {
            token: SecretString::from("my-token-123"),
        };
        let result = authenticate("https://huly.example.com", None, &auth).await;
        assert_eq!(result.unwrap(), "my-token-123");
    }

    #[tokio::test]
    async fn password_auth_fails_on_unreachable_server() {
        let auth = AuthConfig::Password {
            email: "user@test.com".into(),
            password: SecretString::from("pass"),
        };
        let result = authenticate("http://127.0.0.1:1", None, &auth).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), AuthError::Network(_)));
    }

    #[tokio::test]
    async fn password_login_succeeds_via_jsonrpc() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_accounts"))
            .and(body_json(json!({
                "method": "login",
                "params": {"email": "alice@example.com", "password": "hunter2"},
                "id": 1,
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "result": {"token": "session-abc"}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let accounts = format!("{}/_accounts", server.uri());
        let auth = AuthConfig::Password {
            email: "alice@example.com".into(),
            password: SecretString::from("hunter2"),
        };
        let token = authenticate(&server.uri(), Some(&accounts), &auth)
            .await
            .unwrap();
        assert_eq!(token, "session-abc");
    }

    #[tokio::test]
    async fn password_login_401_returns_failed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_string("invalid credentials"))
            .mount(&server)
            .await;

        let auth = AuthConfig::Password {
            email: "user@test.com".into(),
            password: SecretString::from("wrong"),
        };
        let err = authenticate(&server.uri(), Some(&server.uri()), &auth)
            .await
            .unwrap_err();
        match err {
            AuthError::Failed(msg) => assert!(msg.contains("401"), "expected 401 in message: {msg}"),
            _ => panic!("expected Failed error, got {:?}", err),
        }
    }

    #[tokio::test]
    async fn password_login_default_path_is_legacy() {
        // When accounts_url is None, derives `{base}/api/v1/accounts` (legacy deployments).
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/accounts"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": 1,
                "result": {"token": "tok"}
            })))
            .expect(1)
            .mount(&server)
            .await;

        let auth = AuthConfig::Password {
            email: "u@e.com".into(),
            password: SecretString::from("p"),
        };
        let tok = authenticate(&server.uri(), None, &auth).await.unwrap();
        assert_eq!(tok, "tok");
    }
}
