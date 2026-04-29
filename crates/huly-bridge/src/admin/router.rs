use crate::admin::health::HealthState;
use crate::admin::platform_api::{self, MarkupState, PlatformClientHandle, PlatformState};
use crate::bridge::schema_resolver::SchemaHandle;
use crate::huly::collaborator::CollaboratorClient;
use crate::service::workspace_token::WorkspaceTokenCache;
use axum::{
    Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};
use metrics_exporter_prometheus::PrometheusHandle;
use secrecy::{ExposeSecret, SecretString};
use serde::Serialize;
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone)]
pub struct AppState {
    pub health: HealthState,
    pub metrics_handle: Arc<PrometheusHandle>,
    pub start_time: Instant,
    /// Hot-swappable platform client handle. Empty until the first successful
    /// Huly WS connect; handlers return 503 while empty.
    pub platform_client: PlatformClientHandle,
    pub api_token: Option<SecretString>,
    /// Collaborator client — `None` if `COLLABORATOR_URL` was not advertised by the server.
    pub collaborator_client: Option<CollaboratorClient>,
    /// Workspace-scoped token cache (populated on each successful WS login).
    pub workspace_token_cache: WorkspaceTokenCache,
    /// Per-workspace schema (MasterTags + Associations) resolver. Empty
    /// until the first successful WS connect populates it.
    pub schema_handle: SchemaHandle,
}

#[derive(Serialize)]
struct StatusResponse {
    uptime_secs: u64,
    huly_connected: bool,
    nats_connected: bool,
    ready: bool,
}

/// Bearer token authentication middleware.
/// Requires `api_token` to be configured. Returns 403 if not set, 401 if token is
/// missing or invalid.
async fn require_auth(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    match state.api_token {
        None => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "API token not configured"})),
        )
            .into_response(),
        Some(ref expected_token) => {
            let auth_header = request
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok());

            match auth_header {
                Some(header) if header.starts_with("Bearer ") => {
                    let provided = &header[7..];
                    if provided != expected_token.expose_secret() {
                        return (
                            StatusCode::UNAUTHORIZED,
                            Json(serde_json::json!({"error": "invalid token"})),
                        )
                            .into_response();
                    }
                }
                _ => {
                    return (
                        StatusCode::UNAUTHORIZED,
                        Json(
                            serde_json::json!({"error": "missing or invalid Authorization header"}),
                        ),
                    )
                        .into_response();
                }
            }
            next.run(request).await
        }
    }
}

pub fn create_router(state: AppState) -> Router {
    // Health probes and metrics are always unauthenticated (for load balancers / Prometheus)
    let public_routes = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/metrics", get(metrics))
        .with_state(state.clone());

    // Protected routes require api_token to be configured and valid Bearer header
    let mut protected_routes = Router::new()
        .route("/api/v1/status", get(status))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .with_state(state.clone());

    // Platform routes are always mounted; handlers return 503 until the first
    // successful WS connect populates the handle (and again after disconnect).
    {
        let platform_state = PlatformState {
            handle: state.platform_client.clone(),
            schema_handle: state.schema_handle.clone(),
        };
        let platform_routes = Router::new()
            .route("/api/v1/find", post(platform_api::find))
            .route("/api/v1/find-one", post(platform_api::find_one))
            .route("/api/v1/create", post(platform_api::create))
            .route("/api/v1/update", post(platform_api::update))
            .route("/api/v1/delete", post(platform_api::delete))
            .route(
                "/api/v1/add-collection",
                post(platform_api::add_collection),
            )
            .route(
                "/api/v1/update-collection",
                post(platform_api::update_collection),
            )
            .route("/api/v1/apply-if", post(platform_api::apply_if))
            .route_layer(middleware::from_fn_with_state(state.clone(), require_auth))
            .with_state(platform_state);
        protected_routes = protected_routes.merge(platform_routes);
    }

    // Markup routes are always registered (return 503 when collaborator
    // or workspace token is not yet available).
    {
        let markup_state = MarkupState {
            collaborator_client: state.collaborator_client.clone(),
            workspace_token_cache: state.workspace_token_cache.clone(),
        };
        let markup_routes = Router::new()
            .route("/api/v1/upload-markup", post(platform_api::upload_markup))
            .route("/api/v1/fetch-markup", post(platform_api::fetch_markup))
            .route_layer(middleware::from_fn_with_state(state.clone(), require_auth))
            .with_state(markup_state);
        protected_routes = protected_routes.merge(markup_routes);
    }

    public_routes.merge(protected_routes)
}

/// Liveness probe — always 200 if the process is running
async fn healthz() -> StatusCode {
    StatusCode::OK
}

/// Readiness probe — 200 only if Huly WS and NATS are connected
async fn readyz(State(state): State<AppState>) -> impl IntoResponse {
    if state.health.is_ready() {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}

/// Prometheus metrics endpoint
async fn metrics(State(state): State<AppState>) -> String {
    state.metrics_handle.render()
}

/// JSON status endpoint
async fn status(State(state): State<AppState>) -> Json<StatusResponse> {
    let health_status = state.health.status();
    Json(StatusResponse {
        uptime_secs: state.start_time.elapsed().as_secs(),
        huly_connected: health_status.huly_connected,
        nats_connected: health_status.nats_connected,
        ready: health_status.ready,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use metrics_exporter_prometheus::PrometheusBuilder;
    use tower::ServiceExt;

    fn test_state() -> AppState {
        let handle = PrometheusBuilder::new()
            .build_recorder()
            .handle();

        AppState {
            health: HealthState::new(),
            metrics_handle: Arc::new(handle),
            start_time: Instant::now(),
            platform_client: Arc::new(std::sync::RwLock::new(None)),
            api_token: None,
            collaborator_client: None,
            workspace_token_cache: WorkspaceTokenCache::new(),
            schema_handle: SchemaHandle::new(),
        }
    }

    #[tokio::test]
    async fn healthz_always_ok() {
        let app = create_router(test_state());
        let resp = app
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn readyz_unavailable_when_not_ready() {
        let app = create_router(test_state());
        let resp = app
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn readyz_ok_when_ready() {
        let state = test_state();
        state.health.set_huly_connected(true);
        state.health.set_nats_connected(true);

        let app = create_router(state);
        let resp = app
            .oneshot(Request::get("/readyz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn metrics_returns_prometheus_format() {
        let app = create_router(test_state());
        let resp = app
            .oneshot(Request::get("/metrics").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Prometheus output is text
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let _text = String::from_utf8(body.to_vec()).unwrap();
        // Just verify it's parseable text (may be empty if no metrics recorded)
    }

    fn test_state_with_token(token: &str) -> AppState {
        let handle = PrometheusBuilder::new()
            .build_recorder()
            .handle();

        AppState {
            health: HealthState::new(),
            metrics_handle: Arc::new(handle),
            start_time: Instant::now(),
            platform_client: Arc::new(std::sync::RwLock::new(None)),
            api_token: Some(SecretString::from(token)),
            collaborator_client: None,
            workspace_token_cache: WorkspaceTokenCache::new(),
            schema_handle: SchemaHandle::new(),
        }
    }

    #[tokio::test]
    async fn auth_required_returns_401_without_header() {
        let state = test_state_with_token("secret");
        let app = create_router(state);
        let resp = app
            .oneshot(Request::get("/api/v1/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn auth_passes_with_valid_token() {
        let state = test_state_with_token("secret");
        let app = create_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/v1/status")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn auth_rejects_wrong_token() {
        let state = test_state_with_token("secret");
        let app = create_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/v1/status")
                    .header("authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn healthz_bypasses_auth() {
        let state = test_state_with_token("secret");
        let app = create_router(state);
        let resp = app
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn api_returns_403_when_no_token_configured() {
        let state = test_state();
        let app = create_router(state);
        let resp = app
            .oneshot(Request::get("/api/v1/status").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn status_returns_json() {
        let state = test_state_with_token("secret");
        state.health.set_huly_connected(true);

        let app = create_router(state);
        let resp = app
            .oneshot(
                Request::get("/api/v1/status")
                    .header("authorization", "Bearer secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(json["huly_connected"].as_bool().unwrap());
        assert!(!json["nats_connected"].as_bool().unwrap());
        assert!(!json["ready"].as_bool().unwrap());
        assert!(json["uptime_secs"].as_u64().is_some());
    }

    #[tokio::test]
    async fn platform_routes_return_503_when_handle_empty() {
        // Routes are always mounted; an empty handle yields 503 instead of 404.
        let state = test_state_with_token("secret");
        let app = create_router(state);
        let resp = app
            .oneshot(
                Request::post("/api/v1/find")
                    .header("authorization", "Bearer secret")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"class":"tracker:class:Project","query":{}}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
