use huly_client::schema_resolver::{ResolveError, SchemaHandle};
use huly_client::client::{ApplyIfResult, ClientError, PlatformClient};
use huly_client::collaborator::{CollaboratorClient, CollaboratorError};
use huly_client::markdown::{markdown_to_prosemirror_json, prosemirror_to_markdown};
use crate::service::workspace_token::WorkspaceTokenCache;
use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use huly_common::api::{
    AddCollectionRequest, ApplyIfRequest, CreateRequest, DeleteRequest, FetchMarkupRequest,
    FetchMarkupResponse, FindRequest, UpdateCollectionRequest, UpdateRequest, UploadMarkupRequest,
    UploadMarkupResponse,
};
use huly_common::types::{FindResult, TxResult};
use std::sync::{Arc, RwLock};

/// Hot-swappable handle to the platform client. Empty until the first WS connect
/// completes; swapped back to empty on disconnect so handlers return 503.
pub type PlatformClientHandle = Arc<RwLock<Option<Arc<dyn PlatformClient>>>>;

#[derive(Clone)]
pub struct PlatformState {
    pub handle: PlatformClientHandle,
    pub schema_handle: SchemaHandle,
}

impl PlatformState {
    fn client(&self) -> Result<Arc<dyn PlatformClient>, ApiError> {
        self.handle
            .read()
            .expect("platform client handle poisoned")
            .clone()
            .ok_or_else(|| ApiError::ServiceUnavailable("platform client not yet available".into()))
    }

    /// Resolve `class`, treating platform-id-shaped values as opaque
    /// pass-throughs and resolving names against the workspace schema.
    /// Returns 422 for unknown / ambiguous names so the caller can react
    /// instead of silently sending a garbage class to the transactor.
    async fn resolve_class(&self, class: &str) -> Result<String, ApiError> {
        match self.schema_handle.resolve_class(class).await {
            Ok(id) => Ok(id),
            Err(ResolveError::Unknown(name)) => Err(ApiError::Validation(format!(
                "unknown class '{name}' — pass a platform id (e.g. tracker:class:Issue) or a known MasterTag/Association name"
            ))),
            Err(e @ ResolveError::Ambiguous { .. }) => Err(ApiError::Validation(e.to_string())),
        }
    }
}

/// State for markup endpoints (collaborator service).
#[derive(Clone)]
pub struct MarkupState {
    /// Collaborator client — `None` until `COLLABORATOR_URL` is known.
    pub collaborator_client: Option<CollaboratorClient>,
    /// Workspace-scoped token — `None` while bridge is still connecting.
    pub workspace_token_cache: WorkspaceTokenCache,
}

pub async fn find(
    State(state): State<PlatformState>,
    Json(req): Json<FindRequest>,
) -> Result<Json<FindResult>, ApiError> {
    validate_non_empty("class", &req.class)?;
    let class = state.resolve_class(&req.class).await?;
    let client = state.client()?;
    let result = client.find_all(&class, req.query, req.options).await?;
    Ok(Json(result))
}

pub async fn find_one(
    State(state): State<PlatformState>,
    Json(req): Json<FindRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    validate_non_empty("class", &req.class)?;
    let class = state.resolve_class(&req.class).await?;
    let client = state.client()?;
    let result = client.find_one(&class, req.query, req.options).await?;
    match result {
        Some(doc) => Ok(Json(serde_json::to_value(doc).unwrap_or_default())),
        None => Ok(Json(serde_json::Value::Null)),
    }
}

pub async fn create(
    State(state): State<PlatformState>,
    Json(req): Json<CreateRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    validate_non_empty("class", &req.class)?;
    validate_non_empty("space", &req.space)?;
    let class = state.resolve_class(&req.class).await?;
    let client = state.client()?;
    let id = client
        .create_doc(&class, &req.space, req.attributes)
        .await?;
    Ok(Json(serde_json::json!({ "id": id })))
}

pub async fn update(
    State(state): State<PlatformState>,
    Json(req): Json<UpdateRequest>,
) -> Result<Json<TxResult>, ApiError> {
    validate_non_empty("class", &req.class)?;
    validate_non_empty("space", &req.space)?;
    validate_non_empty("id", &req.id)?;
    let class = state.resolve_class(&req.class).await?;
    let client = state.client()?;
    let result = client
        .update_doc(&class, &req.space, &req.id, req.operations)
        .await?;
    Ok(Json(result))
}

pub async fn add_collection(
    State(state): State<PlatformState>,
    Json(req): Json<AddCollectionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    validate_non_empty("class", &req.class)?;
    validate_non_empty("space", &req.space)?;
    validate_non_empty("attachedTo", &req.attached_to)?;
    validate_non_empty("attachedToClass", &req.attached_to_class)?;
    validate_non_empty("collection", &req.collection)?;
    let class = state.resolve_class(&req.class).await?;
    let attached_to_class = state.resolve_class(&req.attached_to_class).await?;
    let client = state.client()?;
    let id = client
        .add_collection(
            &class,
            &req.space,
            &req.attached_to,
            &attached_to_class,
            &req.collection,
            req.attributes,
        )
        .await?;
    Ok(Json(serde_json::json!({ "id": id })))
}

pub async fn update_collection(
    State(state): State<PlatformState>,
    Json(req): Json<UpdateCollectionRequest>,
) -> Result<Json<TxResult>, ApiError> {
    validate_non_empty("class", &req.class)?;
    validate_non_empty("space", &req.space)?;
    validate_non_empty("id", &req.id)?;
    validate_non_empty("attachedTo", &req.attached_to)?;
    validate_non_empty("attachedToClass", &req.attached_to_class)?;
    validate_non_empty("collection", &req.collection)?;
    let class = state.resolve_class(&req.class).await?;
    let attached_to_class = state.resolve_class(&req.attached_to_class).await?;
    let client = state.client()?;
    let result = client
        .update_collection(
            &class,
            &req.space,
            &req.id,
            &req.attached_to,
            &attached_to_class,
            &req.collection,
            req.operations,
        )
        .await?;
    Ok(Json(result))
}

pub async fn apply_if(
    State(state): State<PlatformState>,
    Json(req): Json<ApplyIfRequest>,
) -> Result<Json<ApplyIfResult>, ApiError> {
    validate_non_empty("scope", &req.scope)?;
    if req.txes.is_empty() {
        return Err(ApiError::Validation("'txes' must not be empty".into()));
    }
    let client = state.client()?;
    let result = client
        .apply_if_tx(&req.scope, req.matches, req.not_matches, req.txes)
        .await?;
    Ok(Json(result))
}

pub async fn delete(
    State(state): State<PlatformState>,
    Json(req): Json<DeleteRequest>,
) -> Result<Json<TxResult>, ApiError> {
    validate_non_empty("class", &req.class)?;
    validate_non_empty("space", &req.space)?;
    validate_non_empty("id", &req.id)?;
    let class = state.resolve_class(&req.class).await?;
    let client = state.client()?;
    let result = client.remove_doc(&class, &req.space, &req.id).await?;
    Ok(Json(result))
}

/// `POST /api/v1/upload-markup` — convert markdown to ProseMirror and upload
/// to the collaborator service. Returns `{ "ref": "<MarkupBlobRef>" }`.
///
/// Returns `503` if the collaborator client or workspace token is not yet available.
pub async fn upload_markup(
    State(state): State<MarkupState>,
    Json(req): Json<UploadMarkupRequest>,
) -> Result<Json<UploadMarkupResponse>, ApiError> {
    let client = state
        .collaborator_client
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("collaborator URL not available".into()))?;
    let token = state
        .workspace_token_cache
        .get()
        .ok_or_else(|| ApiError::ServiceUnavailable("workspace token not yet available".into()))?;

    validate_non_empty("objectClass", &req.object_class)?;
    validate_non_empty("objectId", &req.object_id)?;
    validate_non_empty("objectAttr", &req.object_attr)?;

    // Workspace UUID is encoded in the collaborator key; use object_class space
    // convention here. Callers must pass the workspace UUID as object_id prefix or
    // we derive it from the token subject — for now we use the object_id directly as
    // documented: the collaborator key is `{workspaceUuid}|{class}|{id}|{attr}`.
    // The MCP layer passes workspace_uuid explicitly via a dedicated field; for the
    // admin HTTP endpoint we accept it as part of the objectId convention and let
    // the collaborator service validate authz.
    //
    // Practical note: `object_id` here is the Huly doc ID (e.g. `"issue-abc"`).
    // The workspace UUID is extracted from the token by the collaborator service
    // itself — the URL key is just a routing hint. We pass `""` as workspace_uuid
    // and rely on the collaborator service header-based auth.
    let pm_json = markdown_to_prosemirror_json(&req.markdown);

    let markup_ref = client
        .create_markup(
            &token,
            "", // workspace_uuid derived server-side from auth token
            &req.object_class,
            &req.object_id,
            &req.object_attr,
            &pm_json,
        )
        .await
        .map_err(ApiError::Collaborator)?;

    Ok(Json(UploadMarkupResponse { markup_ref }))
}

/// `POST /api/v1/fetch-markup` — fetch markup from collaborator and optionally
/// convert to markdown.
///
/// Returns `503` if the collaborator client or workspace token is not yet available.
/// Returns `400` for unknown `format` values.
pub async fn fetch_markup(
    State(state): State<MarkupState>,
    Json(req): Json<FetchMarkupRequest>,
) -> Result<Json<FetchMarkupResponse>, ApiError> {
    let client = state
        .collaborator_client
        .as_ref()
        .ok_or_else(|| ApiError::ServiceUnavailable("collaborator URL not available".into()))?;
    let token = state
        .workspace_token_cache
        .get()
        .ok_or_else(|| ApiError::ServiceUnavailable("workspace token not yet available".into()))?;

    validate_non_empty("objectClass", &req.object_class)?;
    validate_non_empty("objectId", &req.object_id)?;
    validate_non_empty("objectAttr", &req.object_attr)?;

    if req.format != "markdown" && req.format != "prosemirror" {
        return Err(ApiError::Validation(format!(
            "'format' must be 'markdown' or 'prosemirror', got '{}'",
            req.format
        )));
    }

    let pm_json = client
        .get_markup(
            &token,
            "",
            &req.object_class,
            &req.object_id,
            &req.object_attr,
            req.source_ref.as_deref(),
        )
        .await
        .map_err(ApiError::Collaborator)?;

    let (content, format) = if req.format == "markdown" {
        (prosemirror_to_markdown(&pm_json), "markdown".to_string())
    } else {
        (pm_json, "prosemirror".to_string())
    };

    Ok(Json(FetchMarkupResponse { content, format }))
}

/// Maps errors to HTTP responses
pub enum ApiError {
    Client(ClientError),
    Validation(String),
    ServiceUnavailable(String),
    Collaborator(CollaboratorError),
}

impl From<ClientError> for ApiError {
    fn from(err: ClientError) -> Self {
        Self::Client(err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::Validation(msg) => (StatusCode::BAD_REQUEST, msg),
            ApiError::ServiceUnavailable(msg) => (StatusCode::SERVICE_UNAVAILABLE, msg),
            ApiError::Collaborator(err) => match &err {
                CollaboratorError::Http(_) => (StatusCode::BAD_GATEWAY, err.to_string()),
                CollaboratorError::Status { status, body } => {
                    let http_status = StatusCode::from_u16(*status)
                        .unwrap_or(StatusCode::UNPROCESSABLE_ENTITY);
                    (http_status, body.clone())
                }
                CollaboratorError::Parse(_) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            },
            ApiError::Client(err) => match &err {
                ClientError::Connection(_) => (StatusCode::BAD_GATEWAY, err.to_string()),
                ClientError::Rpc { code, message } => {
                    let status = match code.as_str() {
                        "404" => StatusCode::NOT_FOUND,
                        "403" => StatusCode::FORBIDDEN,
                        c if c.contains("NotFound") => StatusCode::NOT_FOUND,
                        c if c.contains("Forbidden") => StatusCode::FORBIDDEN,
                        c if c.contains("Unauthorized") => StatusCode::UNAUTHORIZED,
                        _ => StatusCode::UNPROCESSABLE_ENTITY,
                    };
                    // Huly transactor frequently emits errors with a populated
                    // `code` and an empty `message`. Falling through with the
                    // empty string strands callers with a useless
                    // `{"error":""}`; surface the code instead so they can act
                    // on it (e.g. retry on `AccountMismatch`).
                    let body = if message.trim().is_empty() {
                        code.clone()
                    } else {
                        format!("{code}: {message}")
                    };
                    (status, body)
                }
                ClientError::Format(_) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()),
            },
        };

        let body = serde_json::json!({ "error": message });
        (status, Json(body)).into_response()
    }
}

fn validate_non_empty(field: &str, value: &str) -> Result<(), ApiError> {
    if value.trim().is_empty() {
        return Err(ApiError::Validation(format!("'{}' must not be empty", field)));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use huly_client::schema_resolver::SchemaHandle;
    use huly_client::client::{ClientError, MockPlatformClient};
    use huly_client::connection::ConnectionError;

    fn test_schema_handle_with_names(names: &[&str]) -> SchemaHandle {
        SchemaHandle::with_card_type_names_for_tests(names)
    }
    use axum::body::Body;
    use axum::http::Request;
    use axum::Router;
    use axum::routing::post;
    use http_body_util::BodyExt;
    use serde_json::json;
    use tower::ServiceExt;

    fn test_app(client: MockPlatformClient) -> Router {
        // Schema with a few identity-mapped names so tests can use either
        // platform-id-shaped values (`tracker:class:Issue` — passthrough)
        // or short names (`cls`, `c` — pre-mapped here). Real resolution
        // is exercised in dedicated `class_resolution_*` tests below.
        let handle: PlatformClientHandle = Arc::new(RwLock::new(Some(
            Arc::new(client) as Arc<dyn PlatformClient>,
        )));
        let schema_handle = test_schema_handle_with_names(&["cls", "c", "pc"]);
        let state = PlatformState {
            handle,
            schema_handle,
        };
        Router::new()
            .route("/api/v1/find", post(find))
            .route("/api/v1/find-one", post(find_one))
            .route("/api/v1/create", post(create))
            .route("/api/v1/update", post(update))
            .route("/api/v1/delete", post(delete))
            .route("/api/v1/add-collection", post(add_collection))
            .route("/api/v1/update-collection", post(update_collection))
            .route("/api/v1/apply-if", post(apply_if))
            .with_state(state)
    }

    fn json_request(path: &str, body: serde_json::Value) -> Request<Body> {
        Request::post(path)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_string(&body).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn platform_handlers_return_503_when_handle_empty() {
        let handle: PlatformClientHandle = Arc::new(RwLock::new(None));
        let state = PlatformState {
            handle,
            schema_handle: SchemaHandle::new(),
        };
        let app = Router::new()
            .route("/api/v1/find", post(find))
            .route("/api/v1/apply-if", post(apply_if))
            .with_state(state);

        let resp = app
            .clone()
            .oneshot(json_request(
                "/api/v1/find",
                json!({"class": "tracker:class:Project", "query": {}}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let resp = app
            .oneshot(json_request(
                "/api/v1/apply-if",
                json!({
                    "scope": "test:scope",
                    "matches": [],
                    "notMatches": [],
                    "txes": [{"_id": "x", "_class": "c", "space": "s", "objectId": "x", "objectClass": "c", "objectSpace": "s", "operations": {}}],
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn platform_handlers_recover_after_swap() {
        // Empty handle → 503; populate → 200; swap empty → 503 again.
        let handle: PlatformClientHandle = Arc::new(RwLock::new(None));
        let state = PlatformState {
            handle: handle.clone(),
            schema_handle: SchemaHandle::new(),
        };
        let app = Router::new()
            .route("/api/v1/find", post(find))
            .with_state(state);

        let req = || {
            json_request(
                "/api/v1/find",
                json!({"class": "tracker:class:Project", "query": {}}),
            )
        };

        let resp = app.clone().oneshot(req()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let mut mock = MockPlatformClient::new();
        mock.expect_find_all().returning(|_, _, _| {
            Box::pin(async {
                Ok(FindResult {
                    docs: vec![],
                    total: 0,
                    lookup_map: None,
                })
            })
        });
        *handle.write().unwrap() = Some(Arc::new(mock) as Arc<dyn PlatformClient>);
        let resp = app.clone().oneshot(req()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        *handle.write().unwrap() = None;
        let resp = app.oneshot(req()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn find_returns_results() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all()
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(FindResult {
                        docs: vec![],
                        total: 0,
                        lookup_map: None,
                    })
                })
            });

        let app = test_app(mock);
        let resp = app
            .oneshot(json_request("/api/v1/find", json!({"class": "cls", "query": {}})))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["total"], 0);
    }

    #[tokio::test]
    async fn find_one_returns_null_when_not_found() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_one()
            .returning(|_, _, _| Box::pin(async { Ok(None) }));

        let app = test_app(mock);
        let resp = app
            .oneshot(json_request("/api/v1/find-one", json!({"class": "cls", "query": {}})))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json.is_null());
    }

    #[tokio::test]
    async fn create_returns_id() {
        let mut mock = MockPlatformClient::new();
        mock.expect_create_doc()
            .returning(|_, _, _| Box::pin(async { Ok("new-id".to_string()) }));

        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/create",
                json!({"class": "cls", "space": "sp", "attributes": {"title": "test"}}),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["id"], "new-id");
    }

    #[tokio::test]
    async fn update_returns_tx_result() {
        let mut mock = MockPlatformClient::new();
        mock.expect_update_doc()
            .returning(|_, _, _, _| {
                Box::pin(async {
                    Ok(TxResult {
                        success: true,
                        id: Some("d1".to_string()),
                    })
                })
            });

        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/update",
                json!({"class": "cls", "space": "sp", "id": "d1", "operations": {"title": "updated"}}),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["success"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn delete_returns_tx_result() {
        let mut mock = MockPlatformClient::new();
        mock.expect_remove_doc()
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(TxResult {
                        success: true,
                        id: None,
                    })
                })
            });

        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/delete",
                json!({"class": "cls", "space": "sp", "id": "d1"}),
            ))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn connection_error_returns_502() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all()
            .returning(|_, _, _| {
                Box::pin(async {
                    Err(ClientError::Connection(ConnectionError::NotConnected))
                })
            });

        let app = test_app(mock);
        let resp = app
            .oneshot(json_request("/api/v1/find", json!({"class": "cls", "query": {}})))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    #[tokio::test]
    async fn find_rejects_empty_class() {
        let mock = MockPlatformClient::new();
        let app = test_app(mock);
        let resp = app
            .oneshot(json_request("/api/v1/find", json!({"class": "", "query": {}})))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_rejects_empty_space() {
        let mock = MockPlatformClient::new();
        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/create",
                json!({"class": "cls", "space": "", "attributes": {}}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_rejects_empty_id() {
        let mock = MockPlatformClient::new();
        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/update",
                json!({"class": "cls", "space": "sp", "id": "", "operations": {}}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn delete_rejects_whitespace_only_fields() {
        let mock = MockPlatformClient::new();
        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/delete",
                json!({"class": "   ", "space": "sp", "id": "d1"}),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn rpc_404_returns_not_found() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all()
            .returning(|_, _, _| {
                Box::pin(async {
                    Err(ClientError::Rpc {
                        code: "404".into(),
                        message: "not found".into(),
                    })
                })
            });

        let app = test_app(mock);
        let resp = app
            .oneshot(json_request("/api/v1/find", json!({"class": "cls", "query": {}})))
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    async fn assert_rpc_code_maps_to(code: &'static str, expected: StatusCode) {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all().returning(move |_, _, _| {
            Box::pin(async move {
                Err(ClientError::Rpc {
                    code: code.into(),
                    message: "x".into(),
                })
            })
        });
        let app = test_app(mock);
        let resp = app
            .oneshot(json_request("/api/v1/find", json!({"class": "cls", "query": {}})))
            .await
            .unwrap();
        assert_eq!(resp.status(), expected, "code {code}");
    }

    #[tokio::test]
    async fn platform_status_unauthorized_maps_to_401() {
        assert_rpc_code_maps_to("platform:status:Unauthorized", StatusCode::UNAUTHORIZED).await;
    }

    #[tokio::test]
    async fn platform_status_not_found_maps_to_404() {
        assert_rpc_code_maps_to("platform:status:NotFound", StatusCode::NOT_FOUND).await;
    }

    #[tokio::test]
    async fn platform_status_forbidden_maps_to_403() {
        assert_rpc_code_maps_to("platform:status:Forbidden", StatusCode::FORBIDDEN).await;
    }

    #[tokio::test]
    async fn unknown_string_code_maps_to_422() {
        assert_rpc_code_maps_to("platform:status:WeirdThing", StatusCode::UNPROCESSABLE_ENTITY).await;
    }

    #[tokio::test]
    async fn legacy_numeric_403_still_maps_to_forbidden() {
        assert_rpc_code_maps_to("403", StatusCode::FORBIDDEN).await;
    }

    #[tokio::test]
    async fn add_collection_returns_id() {
        let mut mock = MockPlatformClient::new();
        mock.expect_add_collection()
            .withf(|class, space, attached, attached_class, collection, _attrs| {
                class == "tracker:class:Issue"
                    && space == "proj-1"
                    && attached == "tracker:ids:NoParent"
                    && attached_class == "tracker:class:Issue"
                    && collection == "subIssues"
            })
            .returning(|_, _, _, _, _, _| Box::pin(async { Ok("issue-1".to_string()) }));

        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/add-collection",
                json!({
                    "class": "tracker:class:Issue",
                    "space": "proj-1",
                    "attachedTo": "tracker:ids:NoParent",
                    "attachedToClass": "tracker:class:Issue",
                    "collection": "subIssues",
                    "attributes": {"title": "T"},
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["id"], "issue-1");
    }

    #[tokio::test]
    async fn add_collection_rejects_empty_collection() {
        let mock = MockPlatformClient::new();
        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/add-collection",
                json!({
                    "class": "c",
                    "space": "s",
                    "attachedTo": "p",
                    "attachedToClass": "pc",
                    "collection": "",
                    "attributes": {},
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn update_collection_returns_tx_result() {
        let mut mock = MockPlatformClient::new();
        mock.expect_update_collection()
            .withf(|class, space, id, attached, attached_class, collection, _ops| {
                class == "tracker:class:Issue"
                    && space == "proj-1"
                    && id == "issue-1"
                    && attached == "tracker:ids:NoParent"
                    && attached_class == "tracker:class:Issue"
                    && collection == "subIssues"
            })
            .returning(|_, _, _, _, _, _, _| {
                Box::pin(async {
                    Ok(TxResult {
                        success: true,
                        id: Some("issue-1".to_string()),
                    })
                })
            });

        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/update-collection",
                json!({
                    "class": "tracker:class:Issue",
                    "space": "proj-1",
                    "id": "issue-1",
                    "attachedTo": "tracker:ids:NoParent",
                    "attachedToClass": "tracker:class:Issue",
                    "collection": "subIssues",
                    "operations": {"title": "renamed"},
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["success"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn update_collection_rejects_empty_id() {
        let mock = MockPlatformClient::new();
        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/update-collection",
                json!({
                    "class": "c",
                    "space": "s",
                    "id": "",
                    "attachedTo": "p",
                    "attachedToClass": "pc",
                    "collection": "col",
                    "operations": {},
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn apply_if_returns_result() {
        use huly_client::client::ApplyIfResult;
        let mut mock = MockPlatformClient::new();
        mock.expect_apply_if_tx()
            .withf(|scope, _matches, _not_matches, txes| {
                scope == "tracker:project:p1:issue-create" && txes.len() == 1
            })
            .returning(|_, _, _, _| {
                Box::pin(async {
                    Ok(ApplyIfResult {
                        success: true,
                        server_time: 99999,
                    })
                })
            });

        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/apply-if",
                json!({
                    "scope": "tracker:project:p1:issue-create",
                    "matches": [],
                    "txes": [{"_class": "core:class:TxUpdateDoc"}],
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(v["success"].as_bool().unwrap());
        assert_eq!(v["serverTime"], 99999);
    }

    #[tokio::test]
    async fn apply_if_forwards_not_matches() {
        use huly_client::client::ApplyIfResult;
        let mut mock = MockPlatformClient::new();
        mock.expect_apply_if_tx()
            .withf(|_scope, _matches, not_matches, _txes| {
                not_matches.len() == 1
                    && not_matches[0].class == "tracker:class:Component"
                    && not_matches[0].query["label"] == "Frontend"
            })
            .returning(|_, _, _, _| {
                Box::pin(async {
                    Ok(ApplyIfResult { success: true, server_time: 1 })
                })
            });

        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/apply-if",
                json!({
                    "scope": "tracker:component:create",
                    "matches": [],
                    "notMatches": [{"_class": "tracker:class:Component", "query": {"label": "Frontend"}}],
                    "txes": [{"_class": "core:class:TxCreateDoc"}],
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn apply_if_rejects_empty_txes() {
        let mock = MockPlatformClient::new();
        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/apply-if",
                json!({
                    "scope": "some:scope",
                    "txes": [],
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn apply_if_rejects_empty_scope() {
        let mock = MockPlatformClient::new();
        let app = test_app(mock);
        let resp = app
            .oneshot(json_request(
                "/api/v1/apply-if",
                json!({
                    "scope": "",
                    "txes": [{"_class": "x"}],
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    // ---- markup endpoint tests ----

    fn markup_app_no_collaborator() -> Router {
        let state = MarkupState {
            collaborator_client: None,
            workspace_token_cache: WorkspaceTokenCache::new(),
        };
        Router::new()
            .route("/api/v1/upload-markup", post(upload_markup))
            .route("/api/v1/fetch-markup", post(fetch_markup))
            .with_state(state)
    }

    fn markup_app_with_collab(collab_url: &str, token: &str) -> Router {
        use huly_client::collaborator::CollaboratorClient;
        use secrecy::SecretString;
        let cache = WorkspaceTokenCache::new();
        cache.set(SecretString::from(token));
        let state = MarkupState {
            collaborator_client: Some(CollaboratorClient::new(collab_url)),
            workspace_token_cache: cache,
        };
        Router::new()
            .route("/api/v1/upload-markup", post(upload_markup))
            .route("/api/v1/fetch-markup", post(fetch_markup))
            .with_state(state)
    }

    #[tokio::test]
    async fn upload_markup_returns_503_when_no_collaborator() {
        let app = markup_app_no_collaborator();
        let resp = app
            .oneshot(json_request(
                "/api/v1/upload-markup",
                json!({
                    "objectClass": "tracker:class:Issue",
                    "objectId": "obj-1",
                    "objectAttr": "description",
                    "markdown": "**hello**"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn upload_markup_returns_503_when_no_workspace_token() {
        use huly_client::collaborator::CollaboratorClient;
        let state = MarkupState {
            collaborator_client: Some(CollaboratorClient::new("http://collab.example")),
            workspace_token_cache: WorkspaceTokenCache::new(), // empty — no token yet
        };
        let app = Router::new()
            .route("/api/v1/upload-markup", post(upload_markup))
            .with_state(state);

        let resp = app
            .oneshot(json_request(
                "/api/v1/upload-markup",
                json!({
                    "objectClass": "c",
                    "objectId": "id",
                    "objectAttr": "description",
                    "markdown": "hello"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn upload_markup_calls_collaborator_and_returns_ref() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/rpc/.+$"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({
                    "content": {"description": "blob-ref-xyz"}
                })),
            )
            .mount(&server)
            .await;

        let app = markup_app_with_collab(&server.uri(), "tok");
        let resp = app
            .oneshot(json_request(
                "/api/v1/upload-markup",
                json!({
                    "objectClass": "tracker:class:Issue",
                    "objectId": "obj-1",
                    "objectAttr": "description",
                    "markdown": "**hello world**"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["ref"], "blob-ref-xyz");
    }

    #[tokio::test]
    async fn fetch_markup_returns_503_when_no_collaborator() {
        let app = markup_app_no_collaborator();
        let resp = app
            .oneshot(json_request(
                "/api/v1/fetch-markup",
                json!({
                    "objectClass": "c",
                    "objectId": "id",
                    "objectAttr": "description"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn fetch_markup_prosemirror_format_returns_json_string() {
        use wiremock::{Mock, MockServer, ResponseTemplate};
        use wiremock::matchers::{method, path_regex};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/rpc/.+$"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({
                    "content": {"description": {"type": "doc", "content": []}}
                })),
            )
            .mount(&server)
            .await;

        let app = markup_app_with_collab(&server.uri(), "tok");
        let resp = app
            .oneshot(json_request(
                "/api/v1/fetch-markup",
                json!({
                    "objectClass": "c",
                    "objectId": "id",
                    "objectAttr": "description",
                    "format": "prosemirror"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["format"], "prosemirror");
        // Content should be a JSON string of the ProseMirror doc
        let content_str = v["content"].as_str().unwrap();
        let pm: serde_json::Value = serde_json::from_str(content_str).unwrap();
        assert_eq!(pm["type"], "doc");
    }

    #[tokio::test]
    async fn fetch_markup_invalid_format_returns_400() {
        use huly_client::collaborator::CollaboratorClient;
        use secrecy::SecretString;
        let cache = WorkspaceTokenCache::new();
        cache.set(SecretString::from("tok"));
        let state = MarkupState {
            collaborator_client: Some(CollaboratorClient::new("http://collab.example")),
            workspace_token_cache: cache,
        };
        let app = Router::new()
            .route("/api/v1/fetch-markup", post(fetch_markup))
            .with_state(state);
        let resp = app
            .oneshot(json_request(
                "/api/v1/fetch-markup",
                json!({
                    "objectClass": "c",
                    "objectId": "id",
                    "objectAttr": "description",
                    "format": "html"
                }),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
