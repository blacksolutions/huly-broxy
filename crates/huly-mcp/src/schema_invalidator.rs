//! Subscribe-first schema cache invalidation (D9).
//!
//! The factory already has a 5-minute TTL fallback. This module adds the
//! event-driven path: subscribe to the bridge's transactor-event NATS
//! subject and, on every TX whose `objectClass` is a schema-mutating
//! class (MasterTag / Association / Class / Attribute / Mixin), invalidate
//! the factory's per-workspace schema cache so the next tool call refetches
//! `loadModel`.
//!
//! ## Subject choice
//!
//! The bridge publishes all transactor events under
//! `{prefix}.event.tx` (single fan-out subject; payload contains the
//! `event` body). We subscribe to the wildcard
//! `{prefix}.event.>` so we pick up any future per-class subject splits
//! without code changes. The singular form is the canonical choice
//! (see P7 reconciliation + `EVENT_SUBJECT_PREFIX`).
//!
//! ## Workspace targeting
//!
//! The current event payload does not carry a workspace identifier, so
//! we invalidate **every cached workspace** on a matching TX. Schema
//! refresh is cheap (one `loadModel` per workspace, only on next access),
//! and false positives just yield an extra fetch.

use crate::huly_client_factory::HulyClientFactory;
use futures::StreamExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

/// `objectClass` values that imply a schema mutation. Anything outside
/// this set leaves the cache alone.
pub const SCHEMA_MUTATING_CLASSES: &[&str] = &[
    "card:class:MasterTag",
    "core:class:Association",
    "core:class:Class",
    "core:class:Mixin",
    "core:class:Attribute",
];

/// `_class` values that are mutating transactions. We accept create /
/// update / remove / mixin attribute changes — anything that can change
/// the schema's surface.
pub const MUTATING_TX_CLASSES: &[&str] = &[
    "core:class:TxCreateDoc",
    "core:class:TxUpdateDoc",
    "core:class:TxRemoveDoc",
    "core:class:TxMixin",
];

/// Decide whether a single TX payload should invalidate the schema cache.
/// Pure function on the JSON value so it can be unit-tested without NATS.
pub fn is_schema_mutating(payload: &Value) -> bool {
    // The bridge wraps each event as `{ event: "tx", ... }`; the TX itself
    // may live at the top level OR under a nested `tx` field depending on
    // the shape upstream chose. Inspect both.
    let tx = match payload.get("tx") {
        Some(t) => t,
        None => payload,
    };
    let tx_class = tx.get("_class").and_then(Value::as_str).unwrap_or("");

    // TxApplyIf bundles inner txes in a `txes: [...]` array. If any inner
    // tx is schema-mutating, treat the whole envelope as mutating.
    if tx_class == "core:class:TxApplyIf"
        && let Some(inner) = tx.get("txes").and_then(Value::as_array)
    {
        return inner.iter().any(is_schema_mutating);
    }

    if !MUTATING_TX_CLASSES.contains(&tx_class) {
        return false;
    }
    let obj_class = tx
        .get("objectClass")
        .and_then(Value::as_str)
        .unwrap_or("");
    SCHEMA_MUTATING_CLASSES.contains(&obj_class)
}

/// Subscribe to the bridge's event subject and invalidate the factory's
/// schema cache for every cached workspace on each schema-mutating TX.
///
/// Runs until `cancel` fires. Backed by a single NATS subscription; we
/// don't try to filter at the broker (the subjects don't encode the
/// information we need) — instead we accept every `tx` event and filter
/// in-process.
pub async fn run_schema_invalidator(
    nats: async_nats::Client,
    factory: HulyClientFactory,
    subject_prefix: &str,
    cancel: CancellationToken,
) {
    // Wildcard catches the current `huly.event.tx` and any future
    // per-class subject split (`huly.event.tx.core.class.*`).
    let subject = format!("{subject_prefix}.event.>");
    let mut sub = match nats.subscribe(subject.clone()).await {
        Ok(s) => s,
        Err(e) => {
            warn!(subject = %subject, error = %e, "schema invalidator: subscribe failed");
            return;
        }
    };
    info!(subject = %subject, "schema invalidator listening");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                debug!("schema invalidator stopping");
                return;
            }
            msg = sub.next() => {
                let Some(msg) = msg else {
                    warn!("schema invalidator: subscription closed");
                    return;
                };
                let payload: Value = match serde_json::from_slice(&msg.payload) {
                    Ok(v) => v,
                    Err(e) => {
                        debug!(subject = %msg.subject, error = %e, "schema invalidator: skip non-json event");
                        continue;
                    }
                };
                if !is_schema_mutating(&payload) {
                    continue;
                }
                // Invalidate every cached workspace; refresh is lazy on
                // the next tool call.
                let n = factory.invalidate_all_schemas().await;
                debug!(workspaces = n, "schema cache invalidated by tx event");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_master_tag_create_as_mutating() {
        let v = json!({
            "_class": "core:class:TxCreateDoc",
            "objectClass": "card:class:MasterTag",
        });
        assert!(is_schema_mutating(&v));
    }

    #[test]
    fn detects_association_update_as_mutating() {
        let v = json!({
            "_class": "core:class:TxUpdateDoc",
            "objectClass": "core:class:Association",
        });
        assert!(is_schema_mutating(&v));
    }

    #[test]
    fn nested_under_tx_field_is_recognised() {
        let v = json!({
            "event": "tx",
            "tx": {
                "_class": "core:class:TxRemoveDoc",
                "objectClass": "core:class:Class",
            }
        });
        assert!(is_schema_mutating(&v));
    }

    #[test]
    fn issue_create_is_not_mutating() {
        let v = json!({
            "_class": "core:class:TxCreateDoc",
            "objectClass": "tracker:class:Issue",
        });
        assert!(!is_schema_mutating(&v));
    }

    #[test]
    fn unrelated_event_shape_is_not_mutating() {
        assert!(!is_schema_mutating(&json!({"event": "notification"})));
        assert!(!is_schema_mutating(&json!(null)));
        assert!(!is_schema_mutating(&json!([])));
    }

    #[test]
    fn mutating_tx_classes_cover_create_update_remove_mixin() {
        assert!(MUTATING_TX_CLASSES.contains(&"core:class:TxCreateDoc"));
        assert!(MUTATING_TX_CLASSES.contains(&"core:class:TxUpdateDoc"));
        assert!(MUTATING_TX_CLASSES.contains(&"core:class:TxRemoveDoc"));
        assert!(MUTATING_TX_CLASSES.contains(&"core:class:TxMixin"));
    }

    #[test]
    fn schema_mutating_classes_cover_master_tag_assoc_class_attribute() {
        assert!(SCHEMA_MUTATING_CLASSES.contains(&"card:class:MasterTag"));
        assert!(SCHEMA_MUTATING_CLASSES.contains(&"core:class:Association"));
        assert!(SCHEMA_MUTATING_CLASSES.contains(&"core:class:Class"));
        assert!(SCHEMA_MUTATING_CLASSES.contains(&"core:class:Attribute"));
        assert!(SCHEMA_MUTATING_CLASSES.contains(&"core:class:Mixin"));
    }

    #[test]
    fn tx_mixin_on_master_tag_is_mutating() {
        let v = json!({
            "_class": "core:class:TxMixin",
            "objectClass": "card:class:MasterTag",
        });
        assert!(is_schema_mutating(&v));
    }

    #[test]
    fn unknown_tx_class_is_not_mutating() {
        let v = json!({
            "_class": "core:class:TxUnknown",
            "objectClass": "card:class:MasterTag",
        });
        assert!(!is_schema_mutating(&v));
    }

    #[test]
    fn tx_create_with_missing_object_class_is_not_mutating() {
        let v = json!({"_class": "core:class:TxCreateDoc"});
        assert!(!is_schema_mutating(&v));
    }

    #[test]
    fn tx_apply_if_with_empty_txes_is_not_mutating() {
        let v = json!({"_class": "core:class:TxApplyIf", "txes": []});
        assert!(!is_schema_mutating(&v));
    }

    #[test]
    fn nested_apply_if_with_outer_event_wrapper_is_recognised() {
        let v = json!({
            "event": "tx",
            "tx": {
                "_class": "core:class:TxApplyIf",
                "txes": [
                    {"_class": "core:class:TxCreateDoc", "objectClass": "core:class:Attribute"},
                ],
            }
        });
        assert!(is_schema_mutating(&v));
    }

    #[test]
    fn apply_if_bundling_inner_class_tx_is_mutating() {
        let v = json!({
            "_class": "core:class:TxApplyIf",
            "txes": [
                {"_class": "core:class:TxUpdateDoc", "objectClass": "tracker:class:Issue"},
                {"_class": "core:class:TxCreateDoc", "objectClass": "card:class:MasterTag"},
            ],
        });
        assert!(is_schema_mutating(&v));
    }

    #[test]
    fn apply_if_bundling_only_non_class_tx_is_not_mutating() {
        let v = json!({
            "_class": "core:class:TxApplyIf",
            "txes": [
                {"_class": "core:class:TxUpdateDoc", "objectClass": "tracker:class:Issue"},
            ],
        });
        assert!(!is_schema_mutating(&v));
    }

    /// Synthetic-NATS happy path: publish a class-mutation TX, observe the
    /// factory's cached schema timestamp clear. Skipped when no NATS server
    /// is reachable.
    #[tokio::test]
    async fn nats_class_mutation_tx_clears_schema_refreshed_at() {
        use crate::huly_client_factory::HulyClientFactory;
        use huly_client::rest_huly_client::RestHulyClient;
        use std::sync::Arc;
        use std::time::Duration;
        use std::time::Instant;

        let Ok(c) = async_nats::connect("nats://127.0.0.1:4222").await else {
            return;
        };
        let factory = HulyClientFactory::new(c.clone(), "agent");
        // Seed a cached entry so invalidate_all_schemas has something to
        // touch. We bypass the JWT broker by writing into the inner map.
        let entry_marker = factory
            .seed_test_entry(
                "ws-1",
                Arc::new(RestHulyClient::new("http://x", "u", "t")),
                Some(Instant::now()),
            )
            .await;
        assert!(entry_marker, "seeding the workspace entry must succeed");

        let cancel = CancellationToken::new();
        let cancel_for_task = cancel.clone();
        let factory_for_task = factory.clone();
        let publisher = c.clone();
        let task = tokio::spawn(async move {
            run_schema_invalidator(c, factory_for_task, "huly", cancel_for_task).await;
        });

        // Give the subscriber a moment to be ready before publishing.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let tx = json!({
            "_class": "core:class:TxCreateDoc",
            "objectClass": "card:class:MasterTag",
            "objectId": "synthetic-master-tag",
        });
        publisher
            .publish("huly.event.tx", serde_json::to_vec(&tx).unwrap().into())
            .await
            .unwrap();
        publisher.flush().await.unwrap();

        // Poll the factory until the timestamp clears, with a short cap.
        let mut cleared = false;
        for _ in 0..50 {
            if factory
                .schema_refreshed_at_for_test("ws-1")
                .await
                .map(|t| t.is_none())
                .unwrap_or(false)
            {
                cleared = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(cleared, "schema timestamp should have been invalidated");

        cancel.cancel();
        task.await.unwrap();
    }
}
