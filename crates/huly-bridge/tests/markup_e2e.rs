//! End-to-end integration test for the markup upload flow.
//!
//! Spins up a wiremock server that simulates the Huly collaborator service,
//! then drives the admin API's `/api/v1/upload-markup` endpoint end-to-end.
//! Asserts:
//!   - The collaborator mock receives a POST to a correctly path-encoded URL.
//!   - The request body contains valid ProseMirror JSON derived from the input markdown.
//!   - The Authorization header carries the workspace-scoped token.
//!   - The admin API response carries the `ref` returned by the collaborator.

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use http_body_util::BodyExt;
use huly_bridge::admin::platform_api::{MarkupState, upload_markup, fetch_markup};
use huly_client::collaborator::CollaboratorClient;
use huly_bridge::service::workspace_token::WorkspaceTokenCache;
use secrecy::SecretString;
use serde_json::json;
use tower::ServiceExt;
use wiremock::matchers::{header, method, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Build the markup-only axum router backed by a given collaborator URL and token.
fn make_markup_router(collab_url: &str, token: &str) -> Router {
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

fn json_post(path: &str, body: serde_json::Value) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_string(&body).unwrap()))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upload_markup_path_is_percent_encoded() {
    // Arrange: collaborator mock accepts any /rpc/... POST and returns a blob ref.
    let collab = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/rpc/.+$"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({
                "content": {"description": "blob-ref-e2e-001"}
            })),
        )
        .expect(1)
        .mount(&collab)
        .await;

    let app = make_markup_router(&collab.uri(), "e2e-token");

    // Act
    let resp = app
        .oneshot(json_post(
            "/api/v1/upload-markup",
            json!({
                "objectClass": "tracker:class:Issue",
                "objectId": "issue-e2e",
                "objectAttr": "description",
                "markdown": "**Bold text** in a paragraph."
            }),
        ))
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["ref"], "blob-ref-e2e-001", "expected blob ref in response");

    // Assert the collaborator received the correctly encoded URL.
    let requests = collab.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let url = requests[0].url.as_str();
    // Must start with /rpc/ and must NOT contain raw `|` characters.
    assert!(url.contains("/rpc/"), "URL must have /rpc/ prefix: {url}");
    assert!(!url.contains('|'), "pipes must be percent-encoded in URL: {url}");
}

#[tokio::test]
async fn upload_markup_sends_authorization_header() {
    let collab = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer ws-scoped-token"))
        .and(path_regex(r"^/rpc/.+$"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({
                "content": {"description": "blob-ref-auth"}
            })),
        )
        .expect(1)
        .mount(&collab)
        .await;

    let app = make_markup_router(&collab.uri(), "ws-scoped-token");

    let resp = app
        .oneshot(json_post(
            "/api/v1/upload-markup",
            json!({
                "objectClass": "tracker:class:Issue",
                "objectId": "obj-1",
                "objectAttr": "description",
                "markdown": "Hello world"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    // wiremock's expect(1) will verify the auth-header constraint
}

#[tokio::test]
async fn upload_markup_body_contains_prosemirror_doc() {
    let collab = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/rpc/.+$"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({
                "content": {"description": "blob-ref-pm"}
            })),
        )
        .mount(&collab)
        .await;

    let app = make_markup_router(&collab.uri(), "tok");

    let resp = app
        .oneshot(json_post(
            "/api/v1/upload-markup",
            json!({
                "objectClass": "tracker:class:Issue",
                "objectId": "obj-2",
                "objectAttr": "description",
                "markdown": "# Heading\n\n**bold** and _italic_"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    // Verify the ProseMirror JSON sent to the collaborator.
    let requests = collab.received_requests().await.unwrap();
    assert!(!requests.is_empty());
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).unwrap();

    assert_eq!(body["method"], "createContent");
    let content = &body["payload"]["content"]["description"];
    assert_eq!(content["type"], "doc", "root must be doc: {content}");
    assert!(
        content["content"].is_array(),
        "content must be array: {content}"
    );
    // First block should be a heading.
    let blocks = content["content"].as_array().unwrap();
    assert!(
        blocks.iter().any(|b| b["type"] == "heading"),
        "expected heading block in: {blocks:?}"
    );
}

#[tokio::test]
async fn fetch_markup_returns_prosemirror_format() {
    let collab = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/rpc/.+$"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({
                "content": {
                    "description": {
                        "type": "doc",
                        "content": [
                            {
                                "type": "paragraph",
                                "content": [{"type": "text", "text": "Hello"}]
                            }
                        ]
                    }
                }
            })),
        )
        .mount(&collab)
        .await;

    let app = make_markup_router(&collab.uri(), "tok");

    let resp = app
        .oneshot(json_post(
            "/api/v1/fetch-markup",
            json!({
                "objectClass": "tracker:class:Issue",
                "objectId": "obj-3",
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
    let pm: serde_json::Value = serde_json::from_str(v["content"].as_str().unwrap()).unwrap();
    assert_eq!(pm["type"], "doc");
}

#[tokio::test]
async fn fetch_markup_markdown_format_roundtrips() {
    let collab = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path_regex(r"^/rpc/.+$"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({
                "content": {
                    "description": {
                        "type": "doc",
                        "content": [
                            {
                                "type": "paragraph",
                                "content": [{"type": "text", "text": "Hello world"}]
                            }
                        ]
                    }
                }
            })),
        )
        .mount(&collab)
        .await;

    let app = make_markup_router(&collab.uri(), "tok");

    let resp = app
        .oneshot(json_post(
            "/api/v1/fetch-markup",
            json!({
                "objectClass": "tracker:class:Issue",
                "objectId": "obj-4",
                "objectAttr": "description",
                "format": "markdown"
            }),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["format"], "markdown");
    assert!(
        v["content"].as_str().unwrap().contains("Hello world"),
        "expected Hello world in markdown, got: {}",
        v["content"]
    );
}
