//! Helpers for constructing explicit Huly TxCUD sub-transaction objects.
//!
//! These are needed when wrapping operations inside a `TxApplyIf` envelope, because
//! `TxApplyIf` requires pre-built transaction objects rather than the simplified
//! method-call form used by `updateDoc` / `addCollection`.
//!
//! Wire shapes follow `huly.core/packages/core/src/tx.ts`.

use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Current epoch milliseconds.
fn epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Generate a unique-enough ID: `<epoch-hex>-<monotonic-counter>`.
/// Mirrors upstream `generateId()` without an external dependency.
pub fn gen_tx_id() -> String {
    static CTR: AtomicU64 = AtomicU64::new(0);
    let seq = CTR.fetch_add(1, Ordering::Relaxed);
    let t = epoch_ms() as u64;
    format!("{:x}-{:x}", t, seq)
}

/// Build a `TxUpdateDoc` sub-transaction.
///
/// Corresponds to `TxFactory.createTxUpdateDoc` in huly.core.
pub fn tx_update_doc(
    object_id: &str,
    object_class: &str,
    object_space: &str,
    operations: Value,
) -> Value {
    json!({
        "_id": gen_tx_id(),
        "_class": "core:class:TxUpdateDoc",
        "space": "core:space:Tx",
        "objectId": object_id,
        "objectClass": object_class,
        "objectSpace": object_space,
        "modifiedBy": "core:account:System",
        "modifiedOn": epoch_ms(),
        "operations": operations,
    })
}

/// Build a plain `TxCreateDoc` sub-transaction (not attached to a collection).
///
/// Use this for top-level docs (Component, Relation, Project, …) that are
/// created via the `createDoc` RPC rather than `addCollection`.
pub fn tx_create_doc(
    object_id: &str,
    object_class: &str,
    object_space: &str,
    attributes: Value,
) -> Value {
    json!({
        "_id": gen_tx_id(),
        "_class": "core:class:TxCreateDoc",
        "space": "core:space:Tx",
        "objectId": object_id,
        "objectClass": object_class,
        "objectSpace": object_space,
        "modifiedBy": "core:account:System",
        "modifiedOn": epoch_ms(),
        "createdBy": "core:account:System",
        "attributes": attributes,
    })
}

/// Build a `TxCreateDoc` + `TxCollectionCUD` spread sub-transaction.
///
/// This mirrors `TxFactory.createTxCollectionCUD(attachedToClass, attachedTo, space, collection,
/// createTxCreateDoc(class, space, attributes, objectId))` from huly.core.
///
/// The result has `_class: "core:class:TxCreateDoc"` with the collection CUD fields
/// (`collection`, `attachedTo`, `attachedToClass`) merged in.
pub fn tx_collection_create(
    object_id: &str,
    object_class: &str,
    object_space: &str,
    attached_to: &str,
    attached_to_class: &str,
    collection: &str,
    attributes: Value,
) -> Value {
    json!({
        "_id": gen_tx_id(),
        "_class": "core:class:TxCreateDoc",
        "space": "core:space:Tx",
        "objectId": object_id,
        "objectClass": object_class,
        "objectSpace": object_space,
        "modifiedBy": "core:account:System",
        "modifiedOn": epoch_ms(),
        "createdBy": "core:account:System",
        "attributes": attributes,
        // collection CUD fields (spread from createTxCollectionCUD)
        "collection": collection,
        "attachedTo": attached_to,
        "attachedToClass": attached_to_class,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tx_update_doc_has_required_fields() {
        let tx = tx_update_doc("proj-1", "tracker:class:Project", "proj-1", json!({"$inc": {"sequence": 1}}));
        assert_eq!(tx["_class"], "core:class:TxUpdateDoc");
        assert_eq!(tx["space"], "core:space:Tx");
        assert_eq!(tx["objectId"], "proj-1");
        assert_eq!(tx["objectClass"], "tracker:class:Project");
        assert_eq!(tx["objectSpace"], "proj-1");
        assert_eq!(tx["operations"]["$inc"]["sequence"], 1);
        assert_eq!(tx["modifiedBy"], "core:account:System");
        assert!(tx["_id"].as_str().map(|s| !s.is_empty()).unwrap_or(false));
        assert!(tx["modifiedOn"].as_i64().unwrap_or(0) > 0);
    }

    #[test]
    fn tx_collection_create_has_required_fields() {
        let tx = tx_collection_create(
            "issue-new",
            "tracker:class:Issue",
            "proj-1",
            "tracker:ids:NoParent",
            "tracker:class:Issue",
            "subIssues",
            json!({"title": "T", "number": 6}),
        );
        assert_eq!(tx["_class"], "core:class:TxCreateDoc");
        assert_eq!(tx["space"], "core:space:Tx");
        assert_eq!(tx["objectId"], "issue-new");
        assert_eq!(tx["objectClass"], "tracker:class:Issue");
        assert_eq!(tx["objectSpace"], "proj-1");
        assert_eq!(tx["collection"], "subIssues");
        assert_eq!(tx["attachedTo"], "tracker:ids:NoParent");
        assert_eq!(tx["attachedToClass"], "tracker:class:Issue");
        assert_eq!(tx["attributes"]["title"], "T");
        assert_eq!(tx["attributes"]["number"], 6);
    }

    #[test]
    fn tx_create_doc_has_required_fields_and_no_collection_keys() {
        let tx = tx_create_doc(
            "comp-1",
            "tracker:class:Component",
            "proj-1",
            json!({"label": "Frontend", "description": "ui"}),
        );
        assert_eq!(tx["_class"], "core:class:TxCreateDoc");
        assert_eq!(tx["space"], "core:space:Tx");
        assert_eq!(tx["objectId"], "comp-1");
        assert_eq!(tx["objectClass"], "tracker:class:Component");
        assert_eq!(tx["objectSpace"], "proj-1");
        assert_eq!(tx["attributes"]["label"], "Frontend");
        assert_eq!(tx["createdBy"], "core:account:System");
        // Must NOT carry collection-CUD fields — those are exclusively for
        // attached docs (e.g. issue subIssues).
        assert!(tx.get("collection").is_none());
        assert!(tx.get("attachedTo").is_none());
        assert!(tx.get("attachedToClass").is_none());
    }

    #[test]
    fn gen_tx_id_produces_unique_ids() {
        let ids: std::collections::HashSet<String> = (0..20).map(|_| gen_tx_id()).collect();
        assert_eq!(ids.len(), 20, "all IDs must be distinct");
    }
}
