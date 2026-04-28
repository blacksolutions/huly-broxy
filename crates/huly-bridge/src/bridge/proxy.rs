use reqwest::Client;
use tracing::{debug, error};

pub use crate::bridge::rate_limit::RateLimitInfo;

pub struct RestProxy {
    client: Client,
    base_url: String,
    token: String,
}

/// Successful upstream response paired with parsed rate-limit metadata.
///
/// Returned by [`RestProxy::forward_with_meta`]. Callers that don't care
/// about the headers can keep using the legacy [`RestProxy::forward`] method,
/// which discards them.
#[derive(Debug, Clone)]
pub struct ProxyResponse {
    pub body: serde_json::Value,
    pub rate_limit: RateLimitInfo,
}

impl RestProxy {
    pub fn new(base_url: &str, token: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        }
    }

    /// Forward a request and return only the JSON body. Discards rate-limit
    /// metadata; use [`Self::forward_with_meta`] when callers need it.
    pub async fn forward(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, ProxyError> {
        self.forward_with_meta(method, path, body).await.map(|r| r.body)
    }

    /// Forward a request and return the JSON body plus parsed rate-limit
    /// headers. The `RateLimitInfo` is always populated (empty if the
    /// upstream emits none of the rate-limit headers).
    pub async fn forward_with_meta(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> Result<ProxyResponse, ProxyError> {
        let url = format!("{}{}", self.base_url, path);
        debug!(method, url, "proxying request");

        let mut req = match method.to_uppercase().as_str() {
            "GET" => self.client.get(&url),
            "POST" => self.client.post(&url),
            "PUT" => self.client.put(&url),
            "DELETE" => self.client.delete(&url),
            "PATCH" => self.client.patch(&url),
            _ => return Err(ProxyError::UnsupportedMethod(method.to_string())),
        };

        req = req.header("Authorization", format!("Bearer {}", self.token));

        if let Some(body) = body {
            req = req.json(&body);
        }

        let resp = req
            .send()
            .await
            .map_err(|e| ProxyError::Network(e.to_string()))?;

        let status = resp.status();
        let rate_limit = RateLimitInfo::from_headers(resp.headers());

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            error!(status = %status, body, "proxy request failed");
            return Err(ProxyError::Upstream {
                status: status.as_u16(),
                body,
            });
        }

        let body = resp
            .json()
            .await
            .map_err(|e| ProxyError::Format(e.to_string()))?;

        Ok(ProxyResponse { body, rate_limit })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("network error: {0}")]
    Network(String),

    #[error("upstream error: HTTP {status}: {body}")]
    Upstream { status: u16, body: String },

    #[error("response format error: {0}")]
    Format(String),

    #[error("unsupported HTTP method: {0}")]
    UnsupportedMethod(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, header, body_json};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn proxy_constructs_url_correctly() {
        let proxy = RestProxy::new("https://huly.example.com/", "tok");
        let url = format!("{}{}", proxy.base_url, "/api/v1/spaces");
        assert_eq!(url, "https://huly.example.com/api/v1/spaces");
    }

    #[test]
    fn proxy_strips_trailing_slash() {
        let proxy = RestProxy::new("https://huly.example.com/", "tok");
        assert_eq!(proxy.base_url, "https://huly.example.com");
    }

    #[tokio::test]
    async fn unsupported_method_returns_error() {
        let proxy = RestProxy::new("http://localhost:1", "tok");
        let result = proxy.forward("TRACE", "/", None).await;
        assert!(matches!(result.unwrap_err(), ProxyError::UnsupportedMethod(_)));
    }

    #[tokio::test]
    async fn network_error_on_unreachable_server() {
        let proxy = RestProxy::new("http://127.0.0.1:1", "tok");
        let result = proxy.forward("GET", "/test", None).await;
        assert!(matches!(result.unwrap_err(), ProxyError::Network(_)));
    }

    #[tokio::test]
    async fn get_request_forwards_and_returns_json() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/spaces"))
            .and(header("Authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;

        let proxy = RestProxy::new(&server.uri(), "test-token");
        let result = proxy.forward("GET", "/api/v1/spaces", None).await.unwrap();
        assert_eq!(result, serde_json::json!({"ok": true}));
    }

    #[tokio::test]
    async fn post_request_forwards_body() {
        let server = MockServer::start().await;
        let body = serde_json::json!({"name": "space1"});
        Mock::given(method("POST"))
            .and(path("/api/v1/spaces"))
            .and(header("Authorization", "Bearer tok"))
            .and(body_json(&body))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "123"})))
            .expect(1)
            .mount(&server)
            .await;

        let proxy = RestProxy::new(&server.uri(), "tok");
        let result = proxy.forward("POST", "/api/v1/spaces", Some(body)).await.unwrap();
        assert_eq!(result, serde_json::json!({"id": "123"}));
    }

    #[tokio::test]
    async fn put_request_forwards_body() {
        let server = MockServer::start().await;
        let body = serde_json::json!({"name": "updated"});
        Mock::given(method("PUT"))
            .and(path("/api/v1/spaces/1"))
            .and(body_json(&body))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;

        let proxy = RestProxy::new(&server.uri(), "tok");
        let result = proxy.forward("PUT", "/api/v1/spaces/1", Some(body)).await.unwrap();
        assert_eq!(result, serde_json::json!({"ok": true}));
    }

    #[tokio::test]
    async fn delete_request_forwards() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v1/spaces/1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"deleted": true})))
            .expect(1)
            .mount(&server)
            .await;

        let proxy = RestProxy::new(&server.uri(), "tok");
        let result = proxy.forward("DELETE", "/api/v1/spaces/1", None).await.unwrap();
        assert_eq!(result, serde_json::json!({"deleted": true}));
    }

    #[tokio::test]
    async fn patch_request_forwards_body() {
        let server = MockServer::start().await;
        let body = serde_json::json!({"status": "active"});
        Mock::given(method("PATCH"))
            .and(path("/api/v1/spaces/1"))
            .and(body_json(&body))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;

        let proxy = RestProxy::new(&server.uri(), "tok");
        let result = proxy.forward("PATCH", "/api/v1/spaces/1", Some(body)).await.unwrap();
        assert_eq!(result, serde_json::json!({"ok": true}));
    }

    #[tokio::test]
    async fn upstream_4xx_returns_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/forbidden"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;

        let proxy = RestProxy::new(&server.uri(), "tok");
        let err = proxy.forward("GET", "/forbidden", None).await.unwrap_err();
        match err {
            ProxyError::Upstream { status, body } => {
                assert_eq!(status, 403);
                assert_eq!(body, "forbidden");
            }
            _ => panic!("expected Upstream error, got {:?}", err),
        }
    }

    #[tokio::test]
    async fn upstream_5xx_returns_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/error"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let proxy = RestProxy::new(&server.uri(), "tok");
        let err = proxy.forward("GET", "/error", None).await.unwrap_err();
        match err {
            ProxyError::Upstream { status, .. } => assert_eq!(status, 500),
            _ => panic!("expected Upstream error, got {:?}", err),
        }
    }

    // --- RED/GREEN: rate-limit header parsing (0.7.19) ---

    #[tokio::test]
    async fn forward_with_meta_parses_rate_limit_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v1/spaces"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("X-RateLimit-Limit", "100")
                    .insert_header("X-RateLimit-Remaining", "37")
                    .insert_header("X-RateLimit-Reset", "1700000000000")
                    .insert_header("Retry-After-ms", "2500")
                    .set_body_json(serde_json::json!({"ok": true})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let proxy = RestProxy::new(&server.uri(), "tok");
        let resp = proxy
            .forward_with_meta("GET", "/api/v1/spaces", None)
            .await
            .unwrap();

        assert_eq!(resp.body, serde_json::json!({"ok": true}));
        assert_eq!(resp.rate_limit.limit, Some(100));
        assert_eq!(resp.rate_limit.remaining, Some(37));
        assert_eq!(resp.rate_limit.reset_ms, Some(1700000000000));
        assert_eq!(resp.rate_limit.retry_after_ms, Some(2500));
    }

    #[tokio::test]
    async fn forward_with_meta_falls_back_to_retry_after_seconds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/limited"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Retry-After", "3")
                    .set_body_json(serde_json::json!({"ok": true})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let proxy = RestProxy::new(&server.uri(), "tok");
        let resp = proxy.forward_with_meta("GET", "/limited", None).await.unwrap();
        // 3 seconds → 3000 ms.
        assert_eq!(resp.rate_limit.retry_after_ms, Some(3000));
    }

    #[tokio::test]
    async fn forward_with_meta_prefers_ms_header_when_both_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/both"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Retry-After-ms", "500")
                    .insert_header("Retry-After", "10")
                    .set_body_json(serde_json::json!({})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let proxy = RestProxy::new(&server.uri(), "tok");
        let resp = proxy.forward_with_meta("GET", "/both", None).await.unwrap();
        assert_eq!(resp.rate_limit.retry_after_ms, Some(500));
    }

    #[tokio::test]
    async fn forward_with_meta_no_rate_limit_headers_yields_empty_info() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/plain"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
            .expect(1)
            .mount(&server)
            .await;

        let proxy = RestProxy::new(&server.uri(), "tok");
        let resp = proxy.forward_with_meta("GET", "/plain", None).await.unwrap();
        assert!(resp.rate_limit.is_empty());
    }

    #[tokio::test]
    async fn rate_limit_info_parses_from_header_map() {
        // Direct unit test for the extractor — independent of the HTTP stack.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("X-RateLimit-Limit", "200".parse().unwrap());
        headers.insert("X-RateLimit-Remaining", "199".parse().unwrap());
        headers.insert("X-RateLimit-Reset", "1700000000000".parse().unwrap());
        headers.insert("Retry-After-ms", "750".parse().unwrap());

        let info = RateLimitInfo::from_headers(&headers);
        assert_eq!(info.limit, Some(200));
        assert_eq!(info.remaining, Some(199));
        assert_eq!(info.reset_ms, Some(1700000000000));
        assert_eq!(info.retry_after_ms, Some(750));
    }

    #[tokio::test]
    async fn rate_limit_info_ignores_garbage_header_values() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("X-RateLimit-Limit", "not-a-number".parse().unwrap());
        headers.insert("Retry-After", "also-bad".parse().unwrap());

        let info = RateLimitInfo::from_headers(&headers);
        assert!(info.limit.is_none());
        assert!(info.retry_after_ms.is_none());
    }

    #[tokio::test]
    async fn non_json_response_returns_format_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/text"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;

        let proxy = RestProxy::new(&server.uri(), "tok");
        let err = proxy.forward("GET", "/text", None).await.unwrap_err();
        assert!(matches!(err, ProxyError::Format(_)));
    }
}
