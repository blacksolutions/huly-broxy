//! Sanity checks that the JSON fixtures under `tests/fixtures/` are shaped
//! exactly as the 0.7.19 transactor emits them.
//!
//! We can't import the bridge's `HelloResponse` / `RpcResponse` structs from
//! here — `huly-bridge` is binary-only, so its modules aren't reachable from
//! integration tests. Instead, this binary asserts the fixture *shape* via
//! `serde_json::Value`, which is sufficient to catch fixture drift; the
//! bridge crate's own `#[cfg(test)]` unit tests cover full struct decoding.

mod common;

use serde_json::Value;

#[test]
fn hello_response_fixture_has_v719_top_level_fields() {
    let v: Value = common::load_fixture_json("hello_response.json");

    // Envelope.
    assert_eq!(v["id"], -1);
    assert_eq!(v["binary"], true);
    assert_eq!(v["compression"], true);

    // 0.7.19 additions live at the top level, *not* under `result`.
    assert!(v["serverVersion"].is_string(), "serverVersion missing/non-string: {v}");
    assert!(v["lastTx"].is_string(), "lastTx missing/non-string: {v}");
    assert!(v["lastHash"].is_string(), "lastHash missing/non-string: {v}");
    assert!(v["account"].is_object(), "account missing/non-object: {v}");
    assert!(v["useCompression"].is_boolean(), "useCompression missing: {v}");

    // Account shape (Phase 1: minimal — uuid required, others optional).
    let account = &v["account"];
    assert!(account["uuid"].is_string(), "account.uuid missing: {account}");
}
