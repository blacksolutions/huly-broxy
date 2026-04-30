use crate::huly::rpc::{
    HelloRequest, ProtocolOptions, RpcRequest, RpcResponse, SerializeError,
    deserialize, serialize,
};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

/// Certificate verifier that accepts all certificates (for tls_skip_verify).
#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn rand_session_id() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    t ^ (std::process::id() as u128) << 64
}

/// Events pushed by the Huly server (id == -1)
#[derive(Debug, Clone)]
pub struct HulyEvent {
    pub result: Option<Value>,
}

/// Trait for testability
#[async_trait]
#[cfg_attr(test, mockall::automock)]
pub trait HulyConnection: Send + Sync {
    async fn send_request(
        &self,
        method: &str,
        params: Vec<Value>,
    ) -> Result<RpcResponse, ConnectionError>;
    fn is_connected(&self) -> bool;
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("not connected")]
    NotConnected,

    #[error("websocket error: {0}")]
    WebSocket(String),

    #[error("hello handshake failed: {0}")]
    HandshakeFailed(String),

    #[error("serialization error: {0}")]
    Serialization(#[from] SerializeError),

    #[error("request timed out: id={0}")]
    Timeout(u64),

    #[error("connection closed")]
    Closed,

    #[error("send failed: {0}")]
    SendFailed(String),

    /// Pending-requests map saturated. Treated as transient — caller should back off
    /// and retry; the cap protects against unbounded memory growth when the transactor
    /// stalls but the WebSocket stays open (issue #13).
    #[error("pending requests exceeded cap of {cap}")]
    PendingRequestsExceeded { cap: usize },
}

/// Default cap for the in-flight (pending) requests map.
///
/// Sized for normal Huly workloads: each pending entry is a `oneshot::Sender` plus a
/// `u64` key (~64 bytes), so 10k entries ≈ a few hundred KiB. We cap rather than
/// expose a dial because the value is operationally boring — saturating it means the
/// transactor is stuck or the bridge is being abused, not that an operator picked a
/// bad number. The config knob (`huly.max_pending_requests`) exists so a deployment
/// can lower it (tests, constrained sidecar, hostile multi-tenant) without code
/// changes; raising it past the default is almost always the wrong fix.
pub const DEFAULT_MAX_PENDING_REQUESTS: usize = 10_000;

type PendingRequests = Arc<Mutex<HashMap<u64, oneshot::Sender<RpcResponse>>>>;
type HandshakeTx = Arc<Mutex<Option<oneshot::Sender<RpcResponse>>>>;

pub struct WsConnection {
    write_tx: mpsc::Sender<Message>,
    pending: PendingRequests,
    max_pending: usize,
    protocol: ProtocolOptions,
    connected: Arc<std::sync::atomic::AtomicBool>,
    request_timeout: std::time::Duration,
    read_handle: tokio::task::JoinHandle<()>,
    write_handle: tokio::task::JoinHandle<()>,
    ping_handle: Option<tokio::task::JoinHandle<()>>,
}

impl std::fmt::Debug for WsConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsConnection")
            .field("connected", &self.is_connected())
            .field("protocol", &self.protocol)
            .finish()
    }
}

impl WsConnection {
    /// Connect to Huly WebSocket and perform hello handshake.
    pub async fn connect(
        ws_url: &str,
        token: &str,
        protocol: ProtocolOptions,
    ) -> Result<(Self, mpsc::Receiver<HulyEvent>), ConnectionError> {
        Self::connect_with_tls(
            ws_url,
            token,
            protocol,
            false,
            None,
            10,
            DEFAULT_MAX_PENDING_REQUESTS,
        )
        .await
    }

    /// Connect with custom TLS options.
    pub async fn connect_with_tls(
        ws_url: &str,
        token: &str,
        protocol: ProtocolOptions,
        tls_skip_verify: bool,
        tls_ca_cert: Option<&str>,
        ping_interval_secs: u64,
        max_pending: usize,
    ) -> Result<(Self, mpsc::Receiver<HulyEvent>), ConnectionError> {
        // Huly WS URL: `{endpoint}/{token}?sessionId={id}`.
        // The JWT goes in the path (not query), matching the official TS client
        // (`concatLink(endpoint, '/' + token)` + `?sessionId=`).
        let session_id = format!("{:024x}", rand_session_id());
        let url = format!(
            "{}/{}?sessionId={}",
            ws_url.trim_end_matches('/'),
            token,
            session_id,
        );

        let (ws_stream, _) = if tls_skip_verify || tls_ca_cert.is_some() {
            let tls_config = if tls_skip_verify {
                warn!("TLS certificate verification is DISABLED (tls_skip_verify=true) — insecure, use only in development");
                rustls::ClientConfig::builder()
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(NoVerifier))
                    .with_no_client_auth()
            } else {
                let mut root_store = rustls::RootCertStore::empty();
                if let Some(ca_path) = tls_ca_cert {
                    let ca_pem = std::fs::read(ca_path)
                        .map_err(|e| ConnectionError::WebSocket(format!("failed to read CA cert: {e}")))?;
                    let certs = rustls_pemfile::certs(&mut &ca_pem[..])
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| ConnectionError::WebSocket(format!("invalid CA cert: {e}")))?;
                    for cert in certs {
                        root_store.add(cert)
                            .map_err(|e| ConnectionError::WebSocket(format!("failed to add CA cert: {e}")))?;
                    }
                }
                rustls::ClientConfig::builder()
                    .with_root_certificates(root_store)
                    .with_no_client_auth()
            };
            let connector = tokio_tungstenite::Connector::Rustls(Arc::new(tls_config));
            tokio_tungstenite::connect_async_tls_with_config(
                &url,
                None,
                false,
                Some(connector),
            )
            .await
            .map_err(|e| ConnectionError::WebSocket(e.to_string()))?
        } else {
            tokio_tungstenite::connect_async(&url)
                .await
                .map_err(|e| ConnectionError::WebSocket(e.to_string()))?
        };

        let (ws_write, ws_read) = futures::StreamExt::split(ws_stream);

        let (write_tx, write_rx) = mpsc::channel::<Message>(256);
        let (event_tx, event_rx) = mpsc::channel::<HulyEvent>(1024);
        let pending: PendingRequests = Arc::new(Mutex::new(HashMap::new()));
        let connected = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let (handshake_tx, handshake_rx) = oneshot::channel::<RpcResponse>();
        let handshake_tx: HandshakeTx = Arc::new(Mutex::new(Some(handshake_tx)));

        // Spawn write task
        let write_connected = connected.clone();
        let write_handle = tokio::spawn(async move {
            use futures::SinkExt;
            let mut ws_write = ws_write;
            let mut write_rx = write_rx;
            while let Some(msg) = write_rx.recv().await {
                if ws_write.send(msg).await.is_err() {
                    write_connected.store(false, std::sync::atomic::Ordering::SeqCst);
                    break;
                }
            }
        });

        // Spawn read task
        let read_pending = pending.clone();
        let read_connected = connected.clone();
        let read_handshake_tx = handshake_tx.clone();
        let read_handle = tokio::spawn(async move {
            use futures::StreamExt;
            let mut ws_read = ws_read;
            while let Some(msg_result) = ws_read.next().await {
                let msg = match msg_result {
                    Ok(m) => m,
                    Err(e) => {
                        error!("ws read error: {e}");
                        read_connected.store(false, std::sync::atomic::Ordering::SeqCst);
                        break;
                    }
                };

                // Dispatch decode by frame type, not by trial-and-error.
                // Text frames are always JSON; Binary frames use the negotiated protocol
                // (msgpack [+ snappy]). Trial-and-error fallback misinterprets text as
                // binary the moment any JSON shape mismatch occurs.
                let response: RpcResponse = match &msg {
                    Message::Text(t) => {
                        // Application-level keepalive frames travel as **bare
                        // strings**, not JSON. The transactor sends `"pong!"`
                        // in reply to our outbound `"ping"` keepalive (see
                        // `huly.core/packages/client/src/index.ts:82-83`:
                        // `pingConst = 'ping'`, `pongConst = 'pong!'`). Short-
                        // circuit before serde_json so the read loop doesn't
                        // log a deserialize warning every 10s and so the
                        // server-initiated `"ping"` case (rare but specified)
                        // is treated as keepalive rather than malformed JSON.
                        let s = t.as_str();
                        if s == "ping" || s == "pong!" {
                            debug!(payload = %s, "ws keepalive frame received");
                            continue;
                        }
                        match serde_json::from_str(s) {
                            Ok(r) => r,
                            Err(e) => {
                                warn!(error = %e, "failed to deserialize text ws message");
                                debug!(payload = %text_prefix(t, 256), "raw text payload");
                                continue;
                            }
                        }
                    }
                    Message::Binary(b) => match deserialize::<RpcResponse>(b, protocol) {
                        Ok(r) => r,
                        Err(_) => match serde_json::from_slice::<RpcResponse>(b) {
                            Ok(r) => r,
                            Err(e) => {
                                warn!(error = %e, "failed to deserialize binary ws message");
                                debug!(payload_hex = %hex_prefix(b, 64), "raw binary payload prefix");
                                continue;
                            }
                        },
                    },
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Close(_) => {
                        info!("ws closed by server");
                        read_connected.store(false, std::sync::atomic::Ordering::SeqCst);
                        break;
                    }
                    _ => continue,
                };

                // Surface unsolicited server-side errors — these arrive without a
                // request id and would otherwise be silently dropped into the event
                // channel, hiding auth/protocol failures behind a hello timeout.
                if let Some(ref err) = response.error {
                    let params_repr = err
                        .params
                        .as_ref()
                        .map(|p| p.to_string())
                        .unwrap_or_default();
                    warn!(
                        code = %err.code,
                        message = %err.message,
                        params = %params_repr,
                        id = response.id,
                        "transactor error"
                    );
                }

                // Filter the application-level ping echo. The transactor
                // replies to each Text-frame `"ping"` keepalive with
                // `{"result":"ping"}` and no matching id; without this
                // guard it would surface as an unsolicited server-push
                // event every cycle and pollute the event channel.
                if response.id == -1
                    && response.error.is_none()
                    && response.result.as_ref().and_then(Value::as_str) == Some("ping")
                {
                    debug!("ws ping echo received");
                    continue;
                }

                if response.id == -1 {
                    // id=-1: either the hello handshake reply or a server push
                    let mut hs = read_handshake_tx.lock().await;
                    if let Some(tx) = hs.take() {
                        // First id=-1 response is the hello handshake reply
                        let _ = tx.send(response);
                    } else {
                        // Handshake already completed — genuine server push
                        forward_event_or_drop(
                            &event_tx,
                            HulyEvent {
                                result: response.result,
                            },
                        )
                        .await;
                    }
                } else {
                    let id = response.id as u64;
                    let mut pending = read_pending.lock().await;
                    if let Some(sender) = pending.remove(&id) {
                        let _ = sender.send(response);
                    } else {
                        debug!("received response for unknown id: {id}");
                    }
                }
            }
        });

        // Spawn ping task to keep the WebSocket alive.
        //
        // The keepalive is a **Text data frame** carrying the bare string
        // `"ping"` — matching the official Huly TS client (see
        // `huly.core/packages/client-resources/src/connection.ts`). WS Ping
        // *control* frames look correct on the wire but are silently
        // ignored by several L7 proxies (AWS ALB, some nginx configs)
        // when they decide whether to reset the idle timer; the bridge
        // would then get cleanly closed by the proxy at its idle limit
        // (~60s in the wild) despite a healthy 10s ping cadence. A data
        // frame counts as activity everywhere.
        let ping_handle = if ping_interval_secs > 0 {
            let ping_tx = write_tx.clone();
            let ping_connected = connected.clone();
            let interval = std::time::Duration::from_secs(ping_interval_secs);
            Some(tokio::spawn(async move {
                let mut ticker = tokio::time::interval(interval);
                ticker.tick().await; // skip immediate first tick
                loop {
                    ticker.tick().await;
                    if !ping_connected.load(std::sync::atomic::Ordering::SeqCst) {
                        break;
                    }
                    if ping_tx.send(Message::Text("ping".into())).await.is_err() {
                        break;
                    }
                    debug!("ws ping sent");
                }
            }))
        } else {
            None
        };

        let conn = Self {
            write_tx,
            pending,
            max_pending,
            protocol,
            connected,
            request_timeout: std::time::Duration::from_secs(30),
            read_handle,
            write_handle,
            ping_handle,
        };

        // Perform hello handshake
        conn.handshake(protocol, handshake_rx).await?;

        Ok((conn, event_rx))
    }

    async fn handshake(
        &self,
        protocol: ProtocolOptions,
        handshake_rx: oneshot::Receiver<RpcResponse>,
    ) -> Result<(), ConnectionError> {
        let hello = HelloRequest::new(protocol.binary, protocol.compression);

        // Hello is always sent as JSON
        let json = serde_json::to_string(&hello)
            .map_err(|e| ConnectionError::Serialization(e.into()))?;

        self.write_tx
            .send(Message::Text(json.into()))
            .await
            .map_err(|e| ConnectionError::SendFailed(e.to_string()))?;

        let resp = match tokio::time::timeout(std::time::Duration::from_secs(10), handshake_rx)
            .await
        {
            Ok(Ok(resp)) => resp,
            Ok(Err(_)) => return Err(ConnectionError::Closed),
            Err(_) => {
                return Err(ConnectionError::HandshakeFailed(
                    "hello handshake timed out after 10s".to_string(),
                ));
            }
        };

        if resp.is_error() {
            let err = resp.error.unwrap();
            return Err(ConnectionError::HandshakeFailed(format!(
                "code={}, message={}",
                err.code, err.message
            )));
        }

        info!(binary = protocol.binary, compression = protocol.compression, "hello handshake complete");
        Ok(())
    }

    async fn send_raw(&self, request: &RpcRequest) -> Result<RpcResponse, ConnectionError> {
        // The negotiated `compression` flag controls **response** compression
        // (server→client). Outgoing JSON requests always travel uncompressed
        // inside a Text frame; only `binary=true` (msgpack) sends Binary, in
        // which case `serialize` applies snappy when compression is also on.
        let msg = if self.protocol.binary {
            Message::Binary(serialize(request, self.protocol)?.into())
        } else {
            let json = serde_json::to_string(request)
                .map_err(|e| ConnectionError::Serialization(SerializeError::Json(e)))?;
            Message::Text(json.into())
        };

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(request.id, tx);
        }

        if let Err(e) = self.write_tx.send(msg).await {
            // Clean up orphaned pending entry on send failure
            self.pending.lock().await.remove(&request.id);
            return Err(ConnectionError::SendFailed(e.to_string()));
        }

        match tokio::time::timeout(self.request_timeout, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => Err(ConnectionError::Closed),
            Err(_) => {
                // Clean up orphaned pending entry on timeout
                self.pending.lock().await.remove(&request.id);
                Err(ConnectionError::Timeout(request.id))
            }
        }
    }

    /// Abort WebSocket tasks and mark as disconnected.
    pub async fn shutdown(&self) {
        self.connected.store(false, std::sync::atomic::Ordering::SeqCst);
        self.read_handle.abort();
        self.write_handle.abort();
        if let Some(ref h) = self.ping_handle {
            h.abort();
        }
    }

    #[cfg(test)]
    pub fn set_request_timeout(&mut self, timeout: std::time::Duration) {
        self.request_timeout = timeout;
    }

    #[cfg(test)]
    pub fn set_max_pending(&mut self, max_pending: usize) {
        self.max_pending = max_pending;
    }
}

impl Drop for WsConnection {
    fn drop(&mut self) {
        self.read_handle.abort();
        self.write_handle.abort();
        if let Some(ref h) = self.ping_handle {
            h.abort();
        }
    }
}

#[async_trait]
impl HulyConnection for WsConnection {
    async fn send_request(
        &self,
        method: &str,
        params: Vec<Value>,
    ) -> Result<RpcResponse, ConnectionError> {
        if !self.is_connected() {
            return Err(ConnectionError::NotConnected);
        }
        // Cap check before consuming an RPC id (issue #13). The check is best-effort
        // under concurrent senders — a brief burst can overshoot by the number of
        // racing callers — but the map cannot grow unboundedly, which is the
        // resource-hardening goal. Counter increments so ops can alert on saturation.
        {
            let pending = self.pending.lock().await;
            if pending.len() >= self.max_pending {
                drop(pending);
                crate::admin::metrics::record_pending_request_dropped();
                return Err(ConnectionError::PendingRequestsExceeded {
                    cap: self.max_pending,
                });
            }
        }
        let request = RpcRequest::new(method, params);
        self.send_raw(&request).await
    }

    fn is_connected(&self) -> bool {
        self.connected.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Forward a server-push event to the bridge consumer, or drop + meter on overflow.
///
/// Uses `try_send` so a full channel drops the event rather than blocking the read
/// loop (which would stall every other RPC response sharing the socket). Both
/// "channel full" and "receiver closed" increment
/// `huly_bridge_events_dropped_total` (issue #14) so ops can alert on bridge
/// backpressure. Extracted so the drop path is unit-testable without spinning up
/// the read task.
async fn forward_event_or_drop(event_tx: &mpsc::Sender<HulyEvent>, event: HulyEvent) {
    use tokio::sync::mpsc::error::TrySendError;
    match event_tx.try_send(event) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            warn!("event channel full, dropping event");
            crate::admin::metrics::record_event_dropped();
        }
        Err(TrySendError::Closed(_)) => {
            warn!("event channel closed, dropping event");
            crate::admin::metrics::record_event_dropped();
        }
    }
}

fn text_prefix(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

fn hex_prefix(b: &[u8], max_bytes: usize) -> String {
    let mut out = String::with_capacity(max_bytes * 2);
    for byte in b.iter().take(max_bytes) {
        use std::fmt::Write;
        let _ = write!(out, "{:02x}", byte);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use std::net::SocketAddr;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    /// Start a mock WS server that responds to hello and echoes requests
    async fn start_mock_server() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let handle = tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let ws_stream = tokio_tungstenite::accept_async(stream).await.unwrap();
                let (mut write, mut read) = ws_stream.split();

                while let Some(Ok(msg)) = read.next().await {
                    match msg {
                        Message::Text(text) => {
                            // Parse and respond
                            if let Ok(req) = serde_json::from_str::<serde_json::Value>(&text) {
                                let id = req["id"].as_i64().unwrap_or(0);
                                let method = req["method"].as_str().unwrap_or("");

                                let response = if method == "hello" {
                                    serde_json::json!({
                                        "id": id,
                                        "binary": req["binary"].as_bool().unwrap_or(false),
                                        "compression": req["compression"].as_bool().unwrap_or(false),
                                        "result": "ok"
                                    })
                                } else {
                                    serde_json::json!({
                                        "id": id,
                                        "result": {"echo": method}
                                    })
                                };

                                let _ = write
                                    .send(Message::Text(response.to_string().into()))
                                    .await;
                            }
                        }
                        Message::Binary(data) => {
                            // For binary protocol, try to deserialize, echo back
                            if let Ok(req) = serde_json::from_slice::<serde_json::Value>(&data) {
                                let id = req["id"].as_i64().unwrap_or(0);
                                let response = serde_json::json!({
                                    "id": id,
                                    "result": {"binary_echo": true}
                                });
                                let _ = write
                                    .send(Message::Text(response.to_string().into()))
                                    .await;
                            } else {
                                // Try all protocol options for deserialization
                                for try_opts in [
                                    ProtocolOptions { binary: true, compression: true },
                                    ProtocolOptions { binary: true, compression: false },
                                ] {
                                    if let Ok(req) = deserialize::<serde_json::Value>(&data, try_opts) {
                                        let id = req["id"].as_i64().unwrap_or(0);
                                        let response = serde_json::json!({
                                            "id": id,
                                            "result": {"binary_echo": true}
                                        });
                                        let resp_bytes = serialize(&response, try_opts).unwrap();
                                        let _ = write.send(Message::Binary(resp_bytes.into())).await;
                                        break;
                                    }
                                }
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
            }
        });

        (addr, handle)
    }

    #[tokio::test]
    async fn connect_and_handshake_json_mode() {
        let (addr, _handle) = start_mock_server().await;
        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions {
            binary: false,
            compression: false,
        };

        let (conn, _events) = WsConnection::connect(&ws_url, "test-token", opts)
            .await
            .unwrap();
        assert!(conn.is_connected());
    }

    #[tokio::test]
    async fn send_request_receives_response() {
        let (addr, _handle) = start_mock_server().await;
        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions {
            binary: false,
            compression: false,
        };

        let (conn, _events) = WsConnection::connect(&ws_url, "test-token", opts)
            .await
            .unwrap();

        let resp = conn.send_request("findAll", vec![]).await.unwrap();
        assert!(!resp.is_error());
        assert_eq!(resp.result.unwrap()["echo"], "findAll");
    }

    #[tokio::test]
    async fn connect_to_invalid_url_fails() {
        let opts = ProtocolOptions::default();
        let result = WsConnection::connect("ws://127.0.0.1:1", "token", opts).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn mock_connection_trait() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .returning(|method, _| {
                let method = method.to_string();
                Box::pin(async move {
                    Ok(RpcResponse {
                        id: 1,
                        result: Some(serde_json::json!({"method": method})),
                        error: None,
                        chunk: None,
                        rate_limit: None,
                        terminate: None,
                        bfst: None,
                        queue: None,
                    })
                })
            });
        mock.expect_is_connected().returning(|| true);

        assert!(mock.is_connected());
        let resp = mock
            .send_request("test", vec![])
            .await
            .unwrap();
        assert_eq!(resp.result.unwrap()["method"], "test");
    }

    #[tokio::test]
    async fn server_push_events_received() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server that sends hello response then a push event
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws_stream = tokio_tungstenite::accept_async(stream).await.unwrap();
            let (mut write, mut read) = ws_stream.split();

            // Wait for hello
            if let Some(Ok(Message::Text(text))) = read.next().await {
                let req: serde_json::Value = serde_json::from_str(&text).unwrap();
                let id = req["id"].as_i64().unwrap();
                let hello_resp = serde_json::json!({"id": id, "binary": false, "compression": false});
                write
                    .send(Message::Text(hello_resp.to_string().into()))
                    .await
                    .unwrap();

                // Send a server push event
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                let push = serde_json::json!({"id": -1, "result": {"event": "tx", "data": "test"}});
                write
                    .send(Message::Text(push.to_string().into()))
                    .await
                    .unwrap();
            }
        });

        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions {
            binary: false,
            compression: false,
        };

        let (_conn, mut events) = WsConnection::connect(&ws_url, "tok", opts)
            .await
            .unwrap();

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(event.result.unwrap()["event"], "tx");
    }

    #[tokio::test]
    async fn handshake_failure_returns_error() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server that rejects hello with an error
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws_stream = tokio_tungstenite::accept_async(stream).await.unwrap();
            let (mut write, mut read) = ws_stream.split();

            if let Some(Ok(Message::Text(text))) = read.next().await {
                let req: serde_json::Value = serde_json::from_str(&text).unwrap();
                let id = req["id"].as_i64().unwrap();
                let error_resp = serde_json::json!({
                    "id": id,
                    "error": {"code": 401, "message": "unauthorized"}
                });
                write
                    .send(Message::Text(error_resp.to_string().into()))
                    .await
                    .unwrap();
            }
        });

        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions::default();

        let result = WsConnection::connect(&ws_url, "bad-token", opts).await;
        assert!(matches!(result.unwrap_err(), ConnectionError::HandshakeFailed(_)));
    }

    #[tokio::test]
    async fn disconnection_detected_after_server_closes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server that completes hello then immediately closes
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws_stream = tokio_tungstenite::accept_async(stream).await.unwrap();
            let (mut write, mut read) = ws_stream.split();

            if let Some(Ok(Message::Text(text))) = read.next().await {
                let req: serde_json::Value = serde_json::from_str(&text).unwrap();
                let id = req["id"].as_i64().unwrap();
                let hello_resp = serde_json::json!({"id": id, "binary": false, "compression": false});
                write
                    .send(Message::Text(hello_resp.to_string().into()))
                    .await
                    .unwrap();

                // Close the connection
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                write.send(Message::Close(None)).await.unwrap();
            }
        });

        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions::default();

        let (conn, _events) = WsConnection::connect(&ws_url, "tok", opts)
            .await
            .unwrap();

        // Wait for server close to be processed
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(!conn.is_connected());
    }

    #[tokio::test]
    async fn timeout_cleans_up_pending_entry() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server that responds to hello but ignores all other requests
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws_stream = tokio_tungstenite::accept_async(stream).await.unwrap();
            let (mut write, mut read) = ws_stream.split();

            while let Some(Ok(msg)) = read.next().await {
                if let Message::Text(text) = msg
                    && let Ok(req) = serde_json::from_str::<serde_json::Value>(&text) {
                        let method = req["method"].as_str().unwrap_or("");
                        if method == "hello" {
                            let id = req["id"].as_i64().unwrap();
                            let resp = serde_json::json!({"id": id, "binary": false, "compression": false});
                            let _ = write.send(Message::Text(resp.to_string().into())).await;
                        }
                        // Non-hello requests: deliberately no response -> triggers timeout
                    }
            }
        });

        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions { binary: false, compression: false };

        let (mut conn, _events) = WsConnection::connect(&ws_url, "tok", opts).await.unwrap();
        conn.set_request_timeout(std::time::Duration::from_millis(200));

        let result = conn.send_request("neverRespond", vec![]).await;
        assert!(matches!(result.unwrap_err(), ConnectionError::Timeout(_)));

        // After timeout, the pending map should be empty (orphaned entry cleaned up)
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let pending = conn.pending.lock().await;
        assert_eq!(pending.len(), 0, "pending map should be empty after timeout cleanup");
    }

    #[tokio::test]
    async fn send_failed_cleans_up_pending_entry() {
        let (addr, _handle) = start_mock_server().await;
        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions { binary: false, compression: false };

        let (mut conn, _events) = WsConnection::connect(&ws_url, "test-token", opts)
            .await
            .unwrap();
        conn.set_request_timeout(std::time::Duration::from_millis(100));

        // Drop the write_tx sender to force SendFailed on next send_raw
        drop(conn.write_tx.clone());
        // Replace write_tx with a closed channel
        let (tx, rx) = mpsc::channel::<Message>(1);
        drop(rx); // immediately close receiver
        conn.write_tx = tx;

        let result = conn.send_raw(&RpcRequest::new("test", vec![])).await;
        assert!(matches!(result.unwrap_err(), ConnectionError::SendFailed(_)));

        // Pending map should be empty (entry cleaned up on SendFailed)
        let pending = conn.pending.lock().await;
        assert_eq!(pending.len(), 0, "pending map should be empty after SendFailed cleanup");
    }

    #[tokio::test]
    async fn close_aborts_tasks_and_marks_disconnected() {
        let (addr, _handle) = start_mock_server().await;
        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions { binary: false, compression: false };

        let (conn, _events) = WsConnection::connect(&ws_url, "test-token", opts)
            .await
            .unwrap();
        assert!(conn.is_connected());

        conn.shutdown().await;
        assert!(!conn.is_connected());
    }

    #[tokio::test]
    async fn request_after_disconnect_fails() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server completes hello then closes
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws_stream = tokio_tungstenite::accept_async(stream).await.unwrap();
            let (mut write, mut read) = ws_stream.split();

            if let Some(Ok(Message::Text(text))) = read.next().await {
                let req: serde_json::Value = serde_json::from_str(&text).unwrap();
                let id = req["id"].as_i64().unwrap();
                let hello_resp = serde_json::json!({"id": id, "binary": false, "compression": false});
                write
                    .send(Message::Text(hello_resp.to_string().into()))
                    .await
                    .unwrap();

                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                write.send(Message::Close(None)).await.unwrap();
            }
        });

        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions::default();

        let (conn, _events) = WsConnection::connect(&ws_url, "tok", opts)
            .await
            .unwrap();

        // Wait for disconnect
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let result = conn.send_request("findAll", vec![]).await;
        assert!(matches!(result.unwrap_err(), ConnectionError::NotConnected));
    }

    /// Negotiate binary protocol but reply over Text JSON frames — bridge must dispatch
    /// by frame type and accept the JSON response, not run msgpack on `{`.
    #[tokio::test]
    async fn text_response_dispatched_as_json_under_binary_protocol() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let (mut write, mut read) = ws.split();
            while let Some(Ok(msg)) = read.next().await {
                if let Message::Text(text) = msg {
                    let req: serde_json::Value = serde_json::from_str(&text).unwrap();
                    let id = req["id"].as_i64().unwrap();
                    let method = req["method"].as_str().unwrap_or("");
                    let resp = if method == "hello" {
                        serde_json::json!({"id": id, "binary": true, "compression": false, "result": "ok"})
                    } else {
                        serde_json::json!({"id": id, "result": {"echo_text": method}})
                    };
                    write.send(Message::Text(resp.to_string().into())).await.unwrap();
                } else if let Message::Binary(b) = msg {
                    let req: serde_json::Value = deserialize(&b, ProtocolOptions { binary: true, compression: false }).unwrap();
                    let id = req["id"].as_i64().unwrap();
                    // Reply as TEXT JSON even though binary was negotiated.
                    let resp = serde_json::json!({"id": id, "result": {"echo_text": req["method"]}});
                    write.send(Message::Text(resp.to_string().into())).await.unwrap();
                }
            }
        });

        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions { binary: true, compression: false };
        let (conn, _events) = WsConnection::connect(&ws_url, "tok", opts).await.unwrap();

        let resp = conn.send_request("findAll", vec![]).await.unwrap();
        assert_eq!(resp.result.unwrap()["echo_text"], "findAll");
    }

    /// A text frame with valid JSON but missing required fields must NOT fall through to
    /// msgpack — it must be dropped with a warn, leaving the request pending.
    #[tokio::test]
    async fn invalid_text_does_not_fall_through_to_msgpack() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let (mut write, mut read) = ws.split();
            while let Some(Ok(msg)) = read.next().await {
                if let Message::Text(text) = msg {
                    let req: serde_json::Value = serde_json::from_str(&text).unwrap();
                    let id = req["id"].as_i64().unwrap();
                    let method = req["method"].as_str().unwrap_or("");
                    if method == "hello" {
                        let resp = serde_json::json!({"id": id, "binary": false, "compression": false, "result": "ok"});
                        write.send(Message::Text(resp.to_string().into())).await.unwrap();
                    } else {
                        // Send a malformed frame (no id) — should be dropped, not fed to msgpack.
                        let bad = serde_json::json!({"unexpected": "shape"});
                        write.send(Message::Text(bad.to_string().into())).await.unwrap();
                    }
                }
            }
        });

        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions { binary: false, compression: false };
        let (conn, _events) = WsConnection::connect(&ws_url, "tok", opts).await.unwrap();

        // Override request_timeout via send_request: we expect Timeout, not a panic / msgpack error.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            conn.send_request("findAll", vec![]),
        )
        .await;
        // The reply was malformed → request stays pending → our outer 2s timeout fires.
        assert!(result.is_err(), "expected outer timeout, got {:?}", result);
    }

    /// Issue #14: drop path must execute when the bounded event channel is full.
    /// Receiver is held but never drains; capacity-1 channel + first send saturates,
    /// second send hits TrySendError::Full and increments the dropped counter.
    #[tokio::test]
    async fn forward_event_drops_when_channel_full() {
        let (tx, _rx) = mpsc::channel::<HulyEvent>(1);
        forward_event_or_drop(&tx, HulyEvent { result: None }).await;
        // Channel is now full (capacity 1, receiver not draining).
        assert_eq!(tx.capacity(), 0, "first send should saturate");
        // Second event must drop, not block.
        let drop_call = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            forward_event_or_drop(&tx, HulyEvent { result: None }),
        )
        .await;
        assert!(drop_call.is_ok(), "drop path must return quickly, not block");
        // Channel still saturated → drop occurred.
        assert_eq!(tx.capacity(), 0);
    }

    #[tokio::test]
    async fn forward_event_drops_when_receiver_closed() {
        let (tx, rx) = mpsc::channel::<HulyEvent>(8);
        drop(rx);
        let drop_call = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            forward_event_or_drop(&tx, HulyEvent { result: None }),
        )
        .await;
        assert!(drop_call.is_ok(), "closed-channel drop must return quickly");
    }

    /// Issue #13: cap on pending-requests map. Two slots, two manual reservations,
    /// the third send_request must fail with PendingRequestsExceeded WITHOUT growing
    /// the map past the cap and WITHOUT consuming an RPC id.
    #[tokio::test]
    async fn pending_requests_cap_rejects_overflow() {
        let (addr, _handle) = start_mock_server().await;
        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions { binary: false, compression: false };

        let (mut conn, _events) = WsConnection::connect(&ws_url, "tok", opts).await.unwrap();
        conn.set_max_pending(2);

        // Pre-populate the map with two stub entries so it's saturated.
        {
            let mut pending = conn.pending.lock().await;
            let (tx1, _rx1) = oneshot::channel();
            let (tx2, _rx2) = oneshot::channel();
            pending.insert(9_000_001, tx1);
            pending.insert(9_000_002, tx2);
            assert_eq!(pending.len(), 2);
        }

        let result = conn.send_request("findAll", vec![]).await;
        match result {
            Err(ConnectionError::PendingRequestsExceeded { cap }) => assert_eq!(cap, 2),
            other => panic!("expected PendingRequestsExceeded, got {other:?}"),
        }

        // Map size unchanged — overflow drop did NOT insert.
        let pending = conn.pending.lock().await;
        assert_eq!(pending.len(), 2, "cap rejection must not grow the map");
    }

    #[test]
    fn hex_prefix_truncates_and_formats() {
        assert_eq!(hex_prefix(&[0x7b, 0x22, 0x69], 8), "7b2269");
        assert_eq!(hex_prefix(&[0xff; 100], 4), "ffffffff");
        assert_eq!(hex_prefix(&[], 4), "");
    }

    #[test]
    fn text_prefix_truncates_by_chars_not_bytes() {
        assert_eq!(text_prefix("héllo", 3), "hél");
        assert_eq!(text_prefix("short", 100), "short");
    }

    /// Regression: outgoing JSON requests must travel as **Text frames** even
    /// when `compression=true` is negotiated. The `compression` flag only
    /// applies to server→client response payloads — the client never compresses
    /// outgoing requests. A prior version snappy-encoded the request and
    /// either failed `String::from_utf8` (-> 502 immediately) or, after a
    /// well-meant "fix", shipped snappy bytes in a Binary frame that the Huly
    /// transactor silently dropped (-> 60s request timeout).
    #[tokio::test]
    async fn send_raw_uses_text_frame_with_uncompressed_json_when_not_binary() {
        use tokio::sync::oneshot;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (frame_tx, frame_rx) = oneshot::channel::<Message>();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws_stream = tokio_tungstenite::accept_async(stream).await.unwrap();
            let (mut write, mut read) = ws_stream.split();

            // Hello (always Text).
            if let Some(Ok(Message::Text(text))) = read.next().await {
                let req: serde_json::Value = serde_json::from_str(&text).unwrap();
                let id = req["id"].as_i64().unwrap();
                let hello_resp =
                    serde_json::json!({"id": id, "binary": false, "compression": true});
                write
                    .send(Message::Text(hello_resp.to_string().into()))
                    .await
                    .unwrap();
            }

            // Capture the next frame (the request under test) so the test can
            // assert on its variant + payload.
            if let Some(Ok(msg)) = read.next().await {
                let _ = frame_tx.send(msg);
            }
        });

        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions {
            binary: false,
            compression: true,
        };
        let (mut conn, _events) = WsConnection::connect(&ws_url, "tok", opts).await.unwrap();
        conn.set_request_timeout(std::time::Duration::from_millis(200));

        let _ = conn.send_request("findAll", vec![]).await;

        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), frame_rx)
            .await
            .expect("frame not received before timeout")
            .expect("frame channel closed");
        match frame {
            Message::Text(t) => {
                let v: serde_json::Value = serde_json::from_str(&t).unwrap();
                assert_eq!(v["method"], "findAll", "request body parsed as JSON");
            }
            other => panic!("non-binary mode must emit a Text frame, got {other:?}"),
        }
    }

    /// Regression: the WebSocket keepalive must be a **Text data frame**
    /// containing the bare string `"ping"`, not a WebSocket Ping control
    /// frame. Several L7 proxies (AWS ALB, some nginx configs) only count
    /// data frames as activity — control frames don't reset the idle
    /// timer, so a control-frame keepalive lets the proxy close the
    /// connection at its idle timeout (~60s in the wild) mid-session.
    /// The official Huly TS client uses this same Text-frame heartbeat.
    /// The transactor's `{"result":"ping"}` echo must be filtered, not
    /// surfaced as a server-push event.
    #[tokio::test]
    async fn keepalive_uses_text_data_frame_and_filters_echo() {
        use tokio::sync::oneshot;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (frame_tx, frame_rx) = oneshot::channel::<Message>();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let ws_stream = tokio_tungstenite::accept_async(stream).await.unwrap();
            let (mut write, mut read) = ws_stream.split();

            // Hello.
            if let Some(Ok(Message::Text(text))) = read.next().await {
                let req: serde_json::Value = serde_json::from_str(&text).unwrap();
                let id = req["id"].as_i64().unwrap();
                let hello_resp =
                    serde_json::json!({"id": id, "binary": false, "compression": false});
                write
                    .send(Message::Text(hello_resp.to_string().into()))
                    .await
                    .unwrap();
            }

            // Capture the next frame (the keepalive under test) and echo
            // BOTH wire formats the transactor uses for keepalive
            // responses so the bridge's read loop must exercise both
            // filter paths:
            //   - bare-text `pong!` (huly.core client/src/index.ts:83) —
            //     the actual production wire format
            //   - JSON `{"result":"ping"}` — the alternate shape used
            //     when the server initiates a keepalive round
            if let Some(Ok(msg)) = read.next().await {
                write
                    .send(Message::Text("pong!".into()))
                    .await
                    .unwrap();
                write
                    .send(Message::Text(r#"{"result":"ping"}"#.into()))
                    .await
                    .unwrap();
                let _ = frame_tx.send(msg);
            }

            // Drain so the connection stays open for the duration of the test.
            while read.next().await.is_some() {}
        });

        let ws_url = format!("ws://127.0.0.1:{}/ws", addr.port());
        let opts = ProtocolOptions {
            binary: false,
            compression: false,
        };
        let (_conn, mut events) = WsConnection::connect_with_tls(
            &ws_url,
            "tok",
            opts,
            false,
            None,
            1,
            DEFAULT_MAX_PENDING_REQUESTS,
        )
        .await
        .unwrap();

        let frame = tokio::time::timeout(std::time::Duration::from_secs(3), frame_rx)
            .await
            .expect("keepalive frame not sent within 3s")
            .expect("frame channel closed");

        match frame {
            Message::Text(t) => assert_eq!(
                t.as_str(),
                "ping",
                "keepalive must be the bare-string `ping`, not JSON-wrapped"
            ),
            Message::Ping(_) => panic!(
                "WS Ping control frames are silently dropped by L7 proxies; \
                 keepalive must be a Text data frame"
            ),
            other => panic!("unexpected keepalive frame: {other:?}"),
        }

        // The transactor's `{result:"ping"}` echo must NOT surface as a
        // server-push event — otherwise every keepalive cycle would
        // pollute the event channel and downstream consumers.
        let drained =
            tokio::time::timeout(std::time::Duration::from_millis(200), events.recv()).await;
        assert!(
            drained.is_err(),
            "ping echo must be filtered, not forwarded as event"
        );
    }
}
