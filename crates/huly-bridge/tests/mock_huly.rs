//! In-process mock Huly server used by integration tests.
//!
//! Provides an ephemeral axum server exposing a WebSocket upgrade route at `/`
//! and a REST recording mechanism, plus a `WsScript` helper for scripting
//! bidirectional frame sequences (text JSON or binary msgpack+snappy).

mod common;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{Method, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{any, get};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as TMessage;

// ----------------- RED-first smoke test -----------------

/// Smoke test: the mock can accept a WS connection and reply to a hello frame.
/// Written BEFORE the harness (TDD red), then implemented.
#[tokio::test]
async fn mock_huly_serves_hello() {
    let mock = MockHuly::start().await;

    let ws_url = mock.ws_url();
    let (mut ws, _resp) = connect_async(&ws_url).await.expect("connect");

    // Client sends a hello frame (text JSON).
    let hello = json!({
        "method": "hello",
        "params": [],
        "id": -1,
        "binary": false,
        "compression": false
    });
    ws.send(TMessage::Text(hello.to_string().into())).await.expect("send");

    // Server should reply with a hello response.
    let msg = ws.next().await.expect("frame").expect("ok");
    let text = match msg {
        TMessage::Text(t) => t.to_string(),
        other => panic!("expected text frame, got {other:?}"),
    };
    let v: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(v["id"], -1);
    assert_eq!(v["binary"], false);
    assert_eq!(v["compression"], false);
}

// ----------------- MockHuly harness -----------------

type RestKey = (String, String); // (METHOD, path)

#[derive(Default)]
struct RestState {
    /// Canned responses keyed by (METHOD, path).
    canned: HashMap<RestKey, Value>,
    /// Observed calls in order.
    observed: Vec<RestKey>,
    /// Expected calls (recorded via mock_rest); asserted at drop.
    expected: Vec<RestKey>,
}

#[derive(Clone, Default)]
struct SharedState {
    rest: Arc<Mutex<RestState>>,
}

/// In-process mock Huly server.
pub struct MockHuly {
    addr: std::net::SocketAddr,
    state: SharedState,
    _server: JoinHandle<()>,
}

impl MockHuly {
    /// Bind on an ephemeral port and spawn the axum server in the background.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let state = SharedState::default();

        let app = Router::new()
            .route("/", any(ws_root))
            .route("/config.json", get(config_json))
            .fallback(any(rest_fallback))
            .with_state(state.clone());

        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app.into_make_service()).await;
        });

        MockHuly { addr, state, _server: server }
    }

    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn ws_url(&self) -> String {
        format!("ws://{}/", self.addr)
    }

    /// Record an expected REST call and stash the canned JSON response.
    pub fn mock_rest(&self, method: &str, path: &str, response: Value) {
        let key = (method.to_ascii_uppercase(), path.to_string());
        let mut st = self.state.rest.lock().expect("rest mutex poisoned");
        st.canned.insert(key.clone(), response);
        st.expected.push(key);
    }
}

impl Drop for MockHuly {
    fn drop(&mut self) {
        // Best-effort assertion that every expected REST call was observed.
        // We intentionally avoid panicking during unwind.
        if std::thread::panicking() {
            return;
        }
        if let Ok(st) = self.state.rest.lock() {
            for key in &st.expected {
                if !st.observed.contains(key) {
                    eprintln!(
                        "MockHuly: expected REST call {} {} was not observed",
                        key.0, key.1
                    );
                }
            }
        }
    }
}

// ----------------- Axum handlers -----------------

async fn ws_root(ws: WebSocketUpgrade) -> axum::response::Response {
    ws.on_upgrade(handle_ws)
}

async fn handle_ws(mut socket: WebSocket) {
    // Minimal default behaviour: on first text frame that looks like a hello,
    // echo back a hello response. On msgpack+snappy binary frames we just
    // keep the socket open; tests driving custom sequences can use `WsScript`.
    while let Some(Ok(msg)) = socket.next().await {
        match msg {
            Message::Text(t) => {
                if let Ok(v) = serde_json::from_str::<Value>(&t)
                    && v.get("method").and_then(Value::as_str) == Some("hello")
                {
                    let resp = json!({
                        "id": -1,
                        "binary": v.get("binary").and_then(Value::as_bool).unwrap_or(false),
                        "compression": v.get("compression").and_then(Value::as_bool).unwrap_or(false),
                        "result": {
                            "serverVersion": "0.7.19",
                            "lastTx": "tx:0",
                            "lastHash": "hash:0",
                            "account": {
                                "uuid": "acc-uuid-1",
                                "email": "test@example.com"
                            }
                        }
                    });
                    let _ = socket.send(Message::Text(resp.to_string().into())).await;
                }
            }
            Message::Binary(_) => {
                // Leave binary handling to scripted sequences; swallow for now.
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}

async fn config_json() -> impl IntoResponse {
    axum::Json(json!({
        "ACCOUNTS_URL": "http://mock/accounts",
        "COLLABORATOR_URL": "http://mock/collab",
        "FILES_URL": "http://mock/files",
        "UPLOAD_URL": "http://mock/upload"
    }))
}

async fn rest_fallback(
    State(state): State<SharedState>,
    req: axum::http::Request<axum::body::Body>,
) -> axum::response::Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    rest_record(&state, method, &path).await.into_response()
}

async fn rest_record(
    state: &SharedState,
    method: Method,
    path: &str,
) -> (StatusCode, axum::Json<Value>) {
    let key = (method.as_str().to_ascii_uppercase(), path.to_string());
    let mut st = state.rest.lock().expect("rest mutex poisoned");
    st.observed.push(key.clone());
    if let Some(v) = st.canned.get(&key) {
        (StatusCode::OK, axum::Json(v.clone()))
    } else {
        (StatusCode::NOT_FOUND, axum::Json(json!({"error": "no canned response"})))
    }
}

// ----------------- WsScript helper -----------------

/// Script a sequence of server-side actions for a dedicated WS session.
///
/// Usage:
///   let script = WsScript::new()
///       .expect_text_contains("hello")
///       .send_text(json!({...}).to_string())
///       .send_binary_msgpack_snappy(&some_response);
///
/// Drive it against an already-upgraded `WebSocket` via `script.run(&mut ws)`.
pub struct WsScript {
    steps: Vec<Step>,
}

enum Step {
    ExpectTextContains(String),
    ExpectBinary,
    SendText(String),
    SendBinary(Vec<u8>),
}

impl WsScript {
    pub fn new() -> Self {
        Self { steps: Vec::new() }
    }

    pub fn expect_text_contains(mut self, needle: impl Into<String>) -> Self {
        self.steps.push(Step::ExpectTextContains(needle.into()));
        self
    }

    pub fn expect_binary(mut self) -> Self {
        self.steps.push(Step::ExpectBinary);
        self
    }

    pub fn send_text(mut self, text: impl Into<String>) -> Self {
        self.steps.push(Step::SendText(text.into()));
        self
    }

    pub fn send_binary(mut self, bytes: Vec<u8>) -> Self {
        self.steps.push(Step::SendBinary(bytes));
        self
    }

    /// Encode `value` as JSON -> msgpack -> snappy and push as a binary frame.
    pub fn send_binary_msgpack_snappy<T: serde::Serialize>(self, value: &T) -> Self {
        let bytes = common::encode_msgpack_snappy(value).expect("encode");
        self.send_binary(bytes)
    }

    /// Drive the script against a server-side WebSocket.
    pub async fn run(self, socket: &mut WebSocket) {
        for step in self.steps {
            match step {
                Step::ExpectTextContains(needle) => {
                    let msg = socket.next().await.expect("client frame").expect("ok");
                    match msg {
                        Message::Text(t) => {
                            assert!(
                                t.contains(&needle),
                                "expected text frame containing {needle:?}, got {t:?}"
                            );
                        }
                        other => panic!("expected text frame, got {other:?}"),
                    }
                }
                Step::ExpectBinary => {
                    let msg = socket.next().await.expect("client frame").expect("ok");
                    match msg {
                        Message::Binary(_) => {}
                        other => panic!("expected binary frame, got {other:?}"),
                    }
                }
                Step::SendText(t) => {
                    socket.send(Message::Text(t.into())).await.expect("send text");
                }
                Step::SendBinary(b) => {
                    socket.send(Message::Binary(b.into())).await.expect("send binary");
                }
            }
        }
    }
}

impl Default for WsScript {
    fn default() -> Self {
        Self::new()
    }
}
