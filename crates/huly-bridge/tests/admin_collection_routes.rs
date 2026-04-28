//! End-to-end integration test for the new addCollection / updateCollection
//! admin routes. Exercises the full admin router (auth middleware + handler +
//! PlatformClient trait) using a hand-rolled fake PlatformClient.
//!
//! `MockPlatformClient` (mockall) is only generated under `#[cfg(test)]` for
//! the lib crate, so it is not visible from integration test binaries. A small
//! hand-rolled fake that records calls is more than sufficient here.

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use huly_bridge::admin::health::HealthState;
use huly_bridge::admin::router::{AppState, create_router};
use huly_bridge::huly::client::{ApplyIfResult, ClientError, PlatformClient};
use huly_common::api::ApplyIfMatch;
use huly_common::types::{Doc, FindOptions, FindResult, TxResult};
use metrics_exporter_prometheus::PrometheusBuilder;
use secrecy::SecretString;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tower::ServiceExt;

type AddCollectionCall = (String, String, String, String, String, Value);
type UpdateCollectionCall = (String, String, String, String, String, String, Value);

#[derive(Default)]
struct FakeClient {
    last_add_collection: Mutex<Option<AddCollectionCall>>,
    last_update_collection: Mutex<Option<UpdateCollectionCall>>,
}

#[async_trait]
impl PlatformClient for FakeClient {
    async fn find_all(
        &self,
        _class: &str,
        _query: Value,
        _options: Option<FindOptions>,
    ) -> Result<FindResult, ClientError> {
        Ok(FindResult { docs: vec![], total: 0, lookup_map: None })
    }
    async fn find_one(
        &self,
        _class: &str,
        _query: Value,
        _options: Option<FindOptions>,
    ) -> Result<Option<Doc>, ClientError> {
        Ok(None)
    }
    async fn create_doc(
        &self,
        _class: &str,
        _space: &str,
        _attributes: Value,
    ) -> Result<String, ClientError> {
        Ok("ignored".into())
    }
    async fn update_doc(
        &self,
        _class: &str,
        _space: &str,
        _id: &str,
        _operations: Value,
    ) -> Result<TxResult, ClientError> {
        Ok(TxResult { success: true, id: None })
    }
    async fn remove_doc(
        &self,
        _class: &str,
        _space: &str,
        _id: &str,
    ) -> Result<TxResult, ClientError> {
        Ok(TxResult { success: true, id: None })
    }
    async fn add_collection(
        &self,
        class: &str,
        space: &str,
        attached_to: &str,
        attached_to_class: &str,
        collection: &str,
        attributes: Value,
    ) -> Result<String, ClientError> {
        *self.last_add_collection.lock().unwrap() = Some((
            class.into(),
            space.into(),
            attached_to.into(),
            attached_to_class.into(),
            collection.into(),
            attributes,
        ));
        Ok("issue-new".into())
    }
    async fn update_collection(
        &self,
        class: &str,
        space: &str,
        id: &str,
        attached_to: &str,
        attached_to_class: &str,
        collection: &str,
        operations: Value,
    ) -> Result<TxResult, ClientError> {
        *self.last_update_collection.lock().unwrap() = Some((
            class.into(),
            space.into(),
            id.into(),
            attached_to.into(),
            attached_to_class.into(),
            collection.into(),
            operations,
        ));
        Ok(TxResult { success: true, id: Some(id.to_string()) })
    }

    async fn apply_if_tx(
        &self,
        _scope: &str,
        _matches: Vec<ApplyIfMatch>,
        _not_matches: Vec<ApplyIfMatch>,
        _txes: Vec<Value>,
    ) -> Result<ApplyIfResult, ClientError> {
        Ok(ApplyIfResult { success: true, server_time: 0 })
    }
}

fn build_state(client: Arc<FakeClient>) -> AppState {
    use huly_bridge::admin::platform_api::PlatformClientHandle;
    use huly_bridge::huly::client::PlatformClient;
    use huly_bridge::service::workspace_token::WorkspaceTokenCache;
    use std::sync::RwLock;
    let metrics = PrometheusBuilder::new().build_recorder().handle();
    let platform_handle: PlatformClientHandle = Arc::new(RwLock::new(Some(
        client as Arc<dyn PlatformClient>,
    )));
    AppState {
        health: HealthState::new(),
        metrics_handle: Arc::new(metrics),
        start_time: Instant::now(),
        platform_client: platform_handle,
        api_token: Some(SecretString::from("test-token")),
        collaborator_client: None,
        workspace_token_cache: WorkspaceTokenCache::new(),
    }
}

fn json_post(path: &str, body: serde_json::Value) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .header("authorization", "Bearer test-token")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn add_collection_route_returns_id_for_authenticated_caller() {
    let fake = Arc::new(FakeClient::default());
    let app = create_router(build_state(fake.clone()));
    let resp = app
        .oneshot(json_post(
            "/api/v1/add-collection",
            json!({
                "class": "tracker:class:Issue",
                "space": "proj-1",
                "attachedTo": "tracker:ids:NoParent",
                "attachedToClass": "tracker:class:Issue",
                "collection": "subIssues",
                "attributes": {"title": "First"},
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["id"], "issue-new");

    let captured = fake.last_add_collection.lock().unwrap().clone().unwrap();
    assert_eq!(captured.0, "tracker:class:Issue");
    assert_eq!(captured.1, "proj-1");
    assert_eq!(captured.2, "tracker:ids:NoParent");
    assert_eq!(captured.3, "tracker:class:Issue");
    assert_eq!(captured.4, "subIssues");
    assert_eq!(captured.5["title"], "First");
}

#[tokio::test]
async fn add_collection_route_requires_auth() {
    let fake = Arc::new(FakeClient::default());
    let app = create_router(build_state(fake));
    let resp = app
        .oneshot(
            Request::post("/api/v1/add-collection")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "class": "c",
                        "space": "s",
                        "attachedTo": "p",
                        "attachedToClass": "pc",
                        "collection": "col",
                        "attributes": {},
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn update_collection_route_returns_tx_result() {
    let fake = Arc::new(FakeClient::default());
    let app = create_router(build_state(fake.clone()));
    let resp = app
        .oneshot(json_post(
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

    let captured = fake.last_update_collection.lock().unwrap().clone().unwrap();
    assert_eq!(captured.2, "issue-1");
    assert_eq!(captured.5, "subIssues");
    assert_eq!(captured.6["title"], "renamed");
}
