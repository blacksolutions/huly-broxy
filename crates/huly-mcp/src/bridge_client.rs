use huly_common::api::{
    AddCollectionRequest, ApplyIfMatch, ApplyIfRequest, ApplyIfResponse, CreateRequest,
    DeleteRequest, FetchMarkupRequest, FetchMarkupResponse, FindRequest, UpdateCollectionRequest,
    UpdateRequest, UploadMarkupRequest, UploadMarkupResponse,
};
use huly_common::types::{Doc, FindResult, TxResult};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum BridgeClientError {
    #[error("HTTP request failed: {0}")]
    Http(String),

    #[error("bridge returned error: HTTP {status}: {body}")]
    BridgeError { status: u16, body: String },

    #[error("response parse error: {0}")]
    Parse(String),
}

#[derive(Clone, Debug)]
pub struct BridgeHttpClient {
    http: reqwest::Client,
    api_token: Option<SecretString>,
}

impl BridgeHttpClient {
    pub fn new(api_token: Option<SecretString>) -> Self {
        Self {
            http: reqwest::Client::new(),
            api_token,
        }
    }

    pub async fn find(
        &self,
        base_url: &str,
        class: &str,
        query: Value,
        options: Option<huly_common::types::FindOptions>,
    ) -> Result<FindResult, BridgeClientError> {
        let req = FindRequest {
            class: class.to_string(),
            query,
            options,
        };
        self.post(base_url, "/api/v1/find", &req).await
    }

    pub async fn find_one(
        &self,
        base_url: &str,
        class: &str,
        query: Value,
    ) -> Result<Option<Doc>, BridgeClientError> {
        let req = FindRequest {
            class: class.to_string(),
            query,
            options: None,
        };
        let value: Value = self.post(base_url, "/api/v1/find-one", &req).await?;
        if value.is_null() {
            Ok(None)
        } else {
            serde_json::from_value(value).map_err(|e| BridgeClientError::Parse(e.to_string()))
        }
    }

    pub async fn create(
        &self,
        base_url: &str,
        class: &str,
        space: &str,
        attributes: Value,
    ) -> Result<String, BridgeClientError> {
        let req = CreateRequest {
            class: class.to_string(),
            space: space.to_string(),
            attributes,
        };
        let result: serde_json::Value = self.post(base_url, "/api/v1/create", &req).await?;
        result["id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| BridgeClientError::Parse("missing id in create response".into()))
    }

    pub async fn update(
        &self,
        base_url: &str,
        class: &str,
        space: &str,
        id: &str,
        operations: Value,
    ) -> Result<TxResult, BridgeClientError> {
        let req = UpdateRequest {
            class: class.to_string(),
            space: space.to_string(),
            id: id.to_string(),
            operations,
        };
        self.post(base_url, "/api/v1/update", &req).await
    }

    /// POST /api/v1/add-collection. Returns the new doc id.
    #[allow(clippy::too_many_arguments)]
    pub async fn add_collection(
        &self,
        base_url: &str,
        class: &str,
        space: &str,
        attached_to: &str,
        attached_to_class: &str,
        collection: &str,
        attributes: Value,
    ) -> Result<String, BridgeClientError> {
        let req = AddCollectionRequest {
            class: class.to_string(),
            space: space.to_string(),
            attached_to: attached_to.to_string(),
            attached_to_class: attached_to_class.to_string(),
            collection: collection.to_string(),
            attributes,
        };
        let result: serde_json::Value = self.post(base_url, "/api/v1/add-collection", &req).await?;
        result["id"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| BridgeClientError::Parse("missing id in add-collection response".into()))
    }

    /// POST /api/v1/update-collection.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_collection(
        &self,
        base_url: &str,
        class: &str,
        space: &str,
        id: &str,
        attached_to: &str,
        attached_to_class: &str,
        collection: &str,
        operations: Value,
    ) -> Result<TxResult, BridgeClientError> {
        let req = UpdateCollectionRequest {
            class: class.to_string(),
            space: space.to_string(),
            id: id.to_string(),
            attached_to: attached_to.to_string(),
            attached_to_class: attached_to_class.to_string(),
            collection: collection.to_string(),
            operations,
        };
        self.post(base_url, "/api/v1/update-collection", &req).await
    }

    /// POST /api/v1/apply-if.  Executes a server-serialized `TxApplyIf` transaction.
    ///
    /// `not_matches` enables atomic "create-if-not-exists" patterns: the scope
    /// only commits if no document matches any of the negative predicates.
    /// Returns `ApplyIfResponse { success, server_time }`.
    pub async fn apply_if(
        &self,
        base_url: &str,
        scope: &str,
        matches: Vec<ApplyIfMatch>,
        not_matches: Vec<ApplyIfMatch>,
        txes: Vec<Value>,
    ) -> Result<ApplyIfResponse, BridgeClientError> {
        let req = ApplyIfRequest {
            scope: scope.to_string(),
            matches,
            not_matches,
            txes,
        };
        self.post(base_url, "/api/v1/apply-if", &req).await
    }

    pub async fn delete(
        &self,
        base_url: &str,
        class: &str,
        space: &str,
        id: &str,
    ) -> Result<TxResult, BridgeClientError> {
        let req = DeleteRequest {
            class: class.to_string(),
            space: space.to_string(),
            id: id.to_string(),
        };
        self.post(base_url, "/api/v1/delete", &req).await
    }

    /// `POST /api/v1/upload-markup` — convert markdown to ProseMirror and upload to
    /// the collaborator service. Returns the `MarkupBlobRef` string.
    pub async fn upload_markup(
        &self,
        base_url: &str,
        object_class: &str,
        object_id: &str,
        object_attr: &str,
        markdown: &str,
    ) -> Result<String, BridgeClientError> {
        let req = UploadMarkupRequest {
            object_class: object_class.to_string(),
            object_id: object_id.to_string(),
            object_attr: object_attr.to_string(),
            markdown: markdown.to_string(),
        };
        let resp: UploadMarkupResponse =
            self.post(base_url, "/api/v1/upload-markup", &req).await?;
        Ok(resp.markup_ref)
    }

    /// `POST /api/v1/fetch-markup` — fetch markup and return content in the requested format.
    pub async fn fetch_markup(
        &self,
        base_url: &str,
        object_class: &str,
        object_id: &str,
        object_attr: &str,
        source_ref: Option<&str>,
        format: &str,
    ) -> Result<FetchMarkupResponse, BridgeClientError> {
        let req = FetchMarkupRequest {
            object_class: object_class.to_string(),
            object_id: object_id.to_string(),
            object_attr: object_attr.to_string(),
            source_ref: source_ref.map(String::from),
            format: format.to_string(),
        };
        self.post(base_url, "/api/v1/fetch-markup", &req).await
    }

    async fn post<T: serde::Serialize, R: serde::de::DeserializeOwned>(
        &self,
        base_url: &str,
        path: &str,
        body: &T,
    ) -> Result<R, BridgeClientError> {
        let url = format!("{}{}", base_url.trim_end_matches('/'), path);
        let mut request = self.http.post(&url).json(body);
        if let Some(ref token) = self.api_token {
            request = request.bearer_auth(token.expose_secret());
        }
        let response = request
            .send()
            .await
            .map_err(|e| BridgeClientError::Http(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown".to_string());
            return Err(BridgeClientError::BridgeError {
                status: status.as_u16(),
                body,
            });
        }

        response
            .json()
            .await
            .map_err(|e| BridgeClientError::Parse(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn client_creates_with_defaults() {
        let _client = BridgeHttpClient::new(None);
    }

    #[tokio::test]
    async fn find_returns_parsed_result() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "docs": [{"_id": "d1", "_class": "core:class:Issue"}],
                    "total": 1
                })),
            )
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .find(&server.uri(), "core:class:Issue", serde_json::json!({}), None)
            .await
            .unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.docs.len(), 1);
    }

    #[tokio::test]
    async fn find_one_returns_none_for_null() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find-one"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::Value::Null))
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .find_one(&server.uri(), "cls", serde_json::json!({}))
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn find_one_returns_doc() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find-one"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"_id": "d1", "_class": "cls"})),
            )
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .find_one(&server.uri(), "cls", serde_json::json!({}))
            .await
            .unwrap();
        assert!(result.is_some());
    }

    #[tokio::test]
    async fn create_returns_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/create"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "new-123"})),
            )
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let id = client
            .create(&server.uri(), "cls", "sp", serde_json::json!({"title": "test"}))
            .await
            .unwrap();
        assert_eq!(id, "new-123");
    }

    #[tokio::test]
    async fn create_errors_on_missing_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/create"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .create(&server.uri(), "cls", "sp", serde_json::json!({}))
            .await;
        assert!(matches!(result, Err(BridgeClientError::Parse(_))));
    }

    #[tokio::test]
    async fn update_returns_tx_result() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/update"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"success": true, "id": "d1"})),
            )
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .update(&server.uri(), "cls", "sp", "d1", serde_json::json!({"title": "new"}))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn delete_returns_tx_result() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/delete"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"success": true})),
            )
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .delete(&server.uri(), "cls", "sp", "d1")
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn non_success_returns_bridge_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .find(&server.uri(), "cls", serde_json::json!({}), None)
            .await;
        match result {
            Err(BridgeClientError::BridgeError { status, body }) => {
                assert_eq!(status, 500);
                assert_eq!(body, "internal error");
            }
            other => panic!("expected BridgeError, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn add_collection_returns_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/add-collection"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "issue-new"})),
            )
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let id = client
            .add_collection(
                &server.uri(),
                "tracker:class:Issue",
                "proj-1",
                "tracker:ids:NoParent",
                "tracker:class:Issue",
                "subIssues",
                serde_json::json!({"title": "T"}),
            )
            .await
            .unwrap();
        assert_eq!(id, "issue-new");
    }

    #[tokio::test]
    async fn add_collection_errors_on_missing_id() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/add-collection"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .add_collection(
                &server.uri(),
                "c",
                "s",
                "p",
                "pc",
                "col",
                serde_json::json!({}),
            )
            .await;
        assert!(matches!(result, Err(BridgeClientError::Parse(_))));
    }

    #[tokio::test]
    async fn update_collection_returns_tx_result() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/update-collection"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"success": true, "id": "issue-1"})),
            )
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .update_collection(
                &server.uri(),
                "tracker:class:Issue",
                "proj-1",
                "issue-1",
                "tracker:ids:NoParent",
                "tracker:class:Issue",
                "subIssues",
                serde_json::json!({"title": "renamed"}),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.id.as_deref(), Some("issue-1"));
    }

    #[tokio::test]
    async fn apply_if_returns_success_result() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/apply-if"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"success": true, "serverTime": 42000})),
            )
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .apply_if(
                &server.uri(),
                "tracker:project:p1:issue-create",
                vec![],
                vec![],
                vec![serde_json::json!({"_class": "core:class:TxUpdateDoc"})],
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.server_time, 42000);
    }

    #[tokio::test]
    async fn apply_if_returns_failure_result() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/apply-if"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"success": false, "serverTime": 0})),
            )
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .apply_if(&server.uri(), "scope", vec![], vec![], vec![serde_json::json!({})])
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn apply_if_serializes_not_matches_in_request_body() {
        use wiremock::matchers::body_partial_json;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/apply-if"))
            .and(body_partial_json(serde_json::json!({
                "notMatches": [{"_class": "tracker:class:Component", "query": {"label": "Frontend"}}]
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"success": true, "serverTime": 1})),
            )
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .apply_if(
                &server.uri(),
                "tracker:component:create",
                vec![],
                vec![ApplyIfMatch {
                    class: "tracker:class:Component".into(),
                    query: serde_json::json!({"label": "Frontend"}),
                }],
                vec![serde_json::json!({"_class": "core:class:TxCreateDoc"})],
            )
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn connection_error_returns_http_error() {
        let client = BridgeHttpClient::new(None);
        let result = client
            .find("http://127.0.0.1:1", "cls", serde_json::json!({}), None)
            .await;
        assert!(matches!(result, Err(BridgeClientError::Http(_))));
    }

    #[tokio::test]
    async fn upload_markup_returns_blob_ref() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/upload-markup"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ref": "blob-ref-xyz"})),
            )
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .upload_markup(
                &server.uri(),
                "tracker:class:Issue",
                "obj-1",
                "description",
                "**hello**",
            )
            .await
            .unwrap();
        assert_eq!(result, "blob-ref-xyz");
    }

    #[tokio::test]
    async fn upload_markup_propagates_bridge_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/upload-markup"))
            .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .upload_markup(&server.uri(), "c", "id", "attr", "text")
            .await;
        assert!(matches!(result, Err(BridgeClientError::BridgeError { status: 503, .. })));
    }

    #[tokio::test]
    async fn fetch_markup_returns_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/fetch-markup"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "content": "**hello**",
                    "format": "markdown"
                })),
            )
            .mount(&server)
            .await;

        let client = BridgeHttpClient::new(None);
        let result = client
            .fetch_markup(
                &server.uri(),
                "tracker:class:Issue",
                "obj-1",
                "description",
                Some("blob-ref-abc"),
                "markdown",
            )
            .await
            .unwrap();
        assert_eq!(result.content, "**hello**");
        assert_eq!(result.format, "markdown");
    }
}
