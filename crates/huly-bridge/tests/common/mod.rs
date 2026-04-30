//! Shared helpers for huly-bridge integration tests.
//!
//! Allow dead-code: each integration test binary pulls this module in via
//! `mod common;`, and not every binary uses every helper.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::Value;
use tokio::net::TcpListener;
use tokio::process::{Child, Command};

/// Resolve `crates/huly-bridge/tests/fixtures/{name}` relative to the crate manifest.
pub fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Read a fixture as raw bytes. Panics on IO error (acceptable in tests).
pub fn load_fixture_bytes(name: &str) -> Vec<u8> {
    let path = fixture_path(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()))
}

/// Read a fixture as a parsed `serde_json::Value`.
pub fn load_fixture_json(name: &str) -> Value {
    let bytes = load_fixture_bytes(name);
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("parse fixture {name} as json: {e}"))
}

/// Bind a TCP listener on port 0, capture the assigned port, then drop the
/// listener so the caller can rebind. There is a tiny TOCTOU window here, but
/// it is acceptable for test orchestration on localhost.
pub async fn ephemeral_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind ephemeral");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    port
}

/// Encode `value` as msgpack (named fields) then snappy-compress. Mirrors the
/// `huly_client::rpc::serialize` pipeline for binary+compression mode.
/// Duplicated here because the crate is binary-only and has no library target
/// to import from; future phases can refactor into a shared lib if needed.
pub fn encode_msgpack_snappy<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, String> {
    let mp = rmp_serde::to_vec_named(value).map_err(|e| format!("msgpack: {e}"))?;
    let mut enc = snap::raw::Encoder::new();
    enc.compress_vec(&mp).map_err(|e| format!("snappy: {e}"))
}

/// Decode snappy-compressed msgpack back into `T`. Inverse of
/// `encode_msgpack_snappy`.
pub fn decode_msgpack_snappy<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    let mut dec = snap::raw::Decoder::new();
    let mp = dec.decompress_vec(bytes).map_err(|e| format!("snappy: {e}"))?;
    rmp_serde::from_slice(&mp).map_err(|e| format!("msgpack: {e}"))
}

/// Handle for an externally-spawned `nats-server` test process. Killed on drop.
pub struct NatsHandle {
    child: Child,
}

impl Drop for NatsHandle {
    fn drop(&mut self) {
        // start_kill is non-blocking; the process reaper handles the rest.
        let _ = self.child.start_kill();
    }
}

/// Spawn an ephemeral `nats-server` if the binary is on PATH.
///
/// Returns `Some((handle, url))` on success, or `None` (with an `eprintln!`
/// warning) when `nats-server` is not installed. Tests that need NATS should
/// early-return on `None` rather than fail — this keeps the dev loop usable
/// on machines without a NATS install.
pub async fn ephemeral_nats() -> Option<(NatsHandle, String)> {
    if which_nats_server().is_none() {
        eprintln!("ephemeral_nats: `nats-server` not found on PATH; skipping NATS-backed test");
        return None;
    }

    let port = ephemeral_port().await;
    let child = Command::new("nats-server")
        .args(["-a", "127.0.0.1", "-p", &port.to_string(), "-DV"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;

    // Give the server a brief moment to bind. Polling the TCP port with a
    // tight loop avoids a fixed sleep if it comes up faster.
    let url = format!("nats://127.0.0.1:{port}");
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return Some((NatsHandle { child }, url));
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    eprintln!("ephemeral_nats: nats-server failed to bind on port {port}");
    None
}

fn which_nats_server() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("nats-server");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
