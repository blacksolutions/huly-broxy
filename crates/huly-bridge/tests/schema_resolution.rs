//! Two-workspace integration test for the schema resolver.
//!
//! Demonstrates the core multi-workspace property: two bridges with
//! different schemas, the same user-visible name (`"Module Spec"`)
//! sent to each, each resolves to a different workspace-local `_class`
//! before the transactor call. This is the regression we built the
//! resolver for — hardcoded IDs in source meant the same name had to
//! mean the same id everywhere.
//!
//! Uses the platform-api router with a fake `PlatformClient` that
//! records the class string each handler forwards. No network, no NATS.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::Router;
use huly_bridge::admin::platform_api::{self, PlatformClientHandle, PlatformState};
use huly_client::schema_resolver::SchemaHandle;
use huly_client::client::{ApplyIfResult, ClientError, PlatformClient};
use huly_common::announcement::WorkspaceSchema;
use huly_common::api::ApplyIfMatch;
use huly_common::types::{Doc, FindOptions, FindResult, TxResult};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, RwLock};
use tower::ServiceExt;

/// Records the `class` string of every call so the test can assert
/// the handler resolved it before invoking the platform client.
struct RecordingClient {
    last_class: Mutex<Option<String>>,
}

impl RecordingClient {
    fn new() -> Self {
        Self {
            last_class: Mutex::new(None),
        }
    }

    fn last(&self) -> Option<String> {
        self.last_class.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl PlatformClient for RecordingClient {
    async fn find_all(
        &self,
        class: &str,
        _query: Value,
        _options: Option<FindOptions>,
    ) -> Result<FindResult, ClientError> {
        *self.last_class.lock().unwrap() = Some(class.to_string());
        Ok(FindResult {
            docs: vec![],
            total: 0,
            lookup_map: None,
        })
    }
    async fn find_one(
        &self,
        _: &str,
        _: Value,
        _: Option<FindOptions>,
    ) -> Result<Option<Doc>, ClientError> {
        Ok(None)
    }
    async fn create_doc(&self, _: &str, _: &str, _: Value) -> Result<String, ClientError> {
        Ok(String::new())
    }
    async fn update_doc(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: Value,
    ) -> Result<TxResult, ClientError> {
        Ok(TxResult { success: true, id: None })
    }
    async fn remove_doc(&self, _: &str, _: &str, _: &str) -> Result<TxResult, ClientError> {
        Ok(TxResult { success: true, id: None })
    }
    async fn add_collection(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: Value,
    ) -> Result<String, ClientError> {
        Ok(String::new())
    }
    async fn update_collection(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: Value,
    ) -> Result<TxResult, ClientError> {
        Ok(TxResult { success: true, id: None })
    }
    async fn apply_if_tx(
        &self,
        _: &str,
        _: Vec<ApplyIfMatch>,
        _: Vec<ApplyIfMatch>,
        _: Vec<Value>,
    ) -> Result<ApplyIfResult, ClientError> {
        Ok(ApplyIfResult {
            success: true,
            server_time: 0,
        })
    }
}

fn build_app(client: Arc<RecordingClient>, schema: SchemaHandle) -> Router {
    let handle: PlatformClientHandle =
        Arc::new(RwLock::new(Some(client as Arc<dyn PlatformClient>)));
    let state = PlatformState {
        handle,
        schema_handle: schema,
    };
    Router::new()
        .route("/api/v1/find", post(platform_api::find))
        .with_state(state)
}

fn json_post(path: &str, body: Value) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Two bridges, two schemas, same user-visible name → distinct
/// `_class` strings reach the per-bridge transactor stand-in.
#[tokio::test]
async fn two_bridges_resolve_same_name_to_different_local_ids() {
    // Workspace A: "Module Spec" lives under id-A.
    let client_a = Arc::new(RecordingClient::new());
    let schema_a = build_schema_with_card_type("Module Spec", "id-A-workspace-local");
    let app_a = build_app(client_a.clone(), schema_a);

    // Workspace B: same name, different id.
    let client_b = Arc::new(RecordingClient::new());
    let schema_b = build_schema_with_card_type("Module Spec", "id-B-workspace-local");
    let app_b = build_app(client_b.clone(), schema_b);

    // MCP-side caller speaks names. Both requests carry the *same*
    // `class: "Module Spec"`, with no workspace-id in the body — the
    // routing decision is the choice of bridge URL (here, which app
    // we call).
    let body = json!({"class": "Module Spec", "query": {}});

    let resp_a = app_a.oneshot(json_post("/api/v1/find", body.clone())).await.unwrap();
    assert_eq!(resp_a.status(), StatusCode::OK);

    let resp_b = app_b.oneshot(json_post("/api/v1/find", body)).await.unwrap();
    assert_eq!(resp_b.status(), StatusCode::OK);

    let class_seen_by_a = client_a.last().expect("bridge A received a call");
    let class_seen_by_b = client_b.last().expect("bridge B received a call");

    assert_eq!(class_seen_by_a, "id-A-workspace-local");
    assert_eq!(class_seen_by_b, "id-B-workspace-local");
    assert_ne!(
        class_seen_by_a, class_seen_by_b,
        "same user-visible name must resolve to *different* workspace-local ids"
    );
}

/// Platform-shaped IDs (`x:y:Z`) bypass resolution unchanged — operators
/// can still address tracker/account/core classes by their stable IDs
/// without the schema cache having to know about them.
#[tokio::test]
async fn platform_ids_pass_through_unchanged() {
    let client = Arc::new(RecordingClient::new());
    // Empty schema. Platform-id-shaped class still resolves (passthrough).
    let schema = SchemaHandle::new();
    let app = build_app(client.clone(), schema);

    let body = json!({"class": "tracker:class:Issue", "query": {}});
    let resp = app.oneshot(json_post("/api/v1/find", body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(client.last().as_deref(), Some("tracker:class:Issue"));
}

/// Unknown name → 400 (better than letting a garbage class hit the
/// transactor and getting back a useless error).
#[tokio::test]
async fn unknown_name_rejected_at_bridge_edge() {
    let client = Arc::new(RecordingClient::new());
    let schema = SchemaHandle::new(); // empty
    let app = build_app(client.clone(), schema);

    let body = json!({"class": "Module Spec", "query": {}});
    let resp = app.oneshot(json_post("/api/v1/find", body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    // The fake client must not have been called.
    assert!(
        client.last().is_none(),
        "unknown class must not reach the platform client"
    );
}

/// Build a SchemaHandle with a single card-type entry mapped to a custom id.
fn build_schema_with_card_type(name: &str, id: &str) -> SchemaHandle {
    let mut schema = WorkspaceSchema::default();
    schema.card_types.insert(name.to_string(), id.to_string());
    SchemaHandle::install_for_tests(schema)
}
