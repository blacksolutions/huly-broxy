use crate::huly::connection::{ConnectionError, HulyConnection};
use crate::huly::types::{Doc, FindOptions, FindResult, TxResult};
use async_trait::async_trait;
use huly_common::api::ApplyIfMatch;
use serde_json::{Value, json};
use std::sync::Arc;

#[async_trait]
#[cfg_attr(test, mockall::automock)]
pub trait PlatformClient: Send + Sync {
    async fn find_all(
        &self,
        class: &str,
        query: Value,
        options: Option<FindOptions>,
    ) -> Result<FindResult, ClientError>;

    async fn find_one(
        &self,
        class: &str,
        query: Value,
        options: Option<FindOptions>,
    ) -> Result<Option<Doc>, ClientError>;

    async fn create_doc(
        &self,
        class: &str,
        space: &str,
        attributes: Value,
    ) -> Result<String, ClientError>;

    async fn update_doc(
        &self,
        class: &str,
        space: &str,
        id: &str,
        operations: Value,
    ) -> Result<TxResult, ClientError>;

    async fn remove_doc(
        &self,
        class: &str,
        space: &str,
        id: &str,
    ) -> Result<TxResult, ClientError>;

    /// Create a document attached to a parent collection (Huly `addCollection`).
    ///
    /// This is the upstream `TxOperations.addCollection` wrapper. It is **not**
    /// server-atomic on its own (it is a `createDoc` + `TxCollectionCUD`).
    ///
    /// Wire payload:
    /// `{ method: "addCollection", params: [class, space, attachedTo, attachedToClass, collection, attributes] }`
    /// Returns the new document id (matches `createDoc`).
    async fn add_collection(
        &self,
        class: &str,
        space: &str,
        attached_to: &str,
        attached_to_class: &str,
        collection: &str,
        attributes: Value,
    ) -> Result<String, ClientError>;

    /// Update a document attached to a parent collection (Huly `updateCollection`).
    ///
    /// Wire payload:
    /// `{ method: "updateCollection", params: [class, space, id, attachedTo, attachedToClass, collection, operations] }`
    /// Returns a `TxResult` (matches `updateDoc`).
    #[allow(clippy::too_many_arguments)]
    async fn update_collection(
        &self,
        class: &str,
        space: &str,
        id: &str,
        attached_to: &str,
        attached_to_class: &str,
        collection: &str,
        operations: Value,
    ) -> Result<TxResult, ClientError>;

    /// Send a server-serialized `TxApplyIf` transaction.
    ///
    /// Wire payload: `{ method: "tx", params: [txApplyIfObject] }`.
    /// The server executes `txes` atomically iff every `matches` predicate
    /// finds at least one document, every `not_matches` predicate finds zero,
    /// and no concurrent operation with the same `scope` is in flight.
    ///
    /// `not_matches` mirrors upstream `TxApplyIf.notMatch` and is the basis
    /// for race-free "create-if-not-exists" patterns.
    ///
    /// Returns `ApplyIfResult { success, server_time }`.
    async fn apply_if_tx(
        &self,
        scope: &str,
        matches: Vec<ApplyIfMatch>,
        not_matches: Vec<ApplyIfMatch>,
        txes: Vec<Value>,
    ) -> Result<ApplyIfResult, ClientError>;
}

/// Result of a `TxApplyIf` transaction.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ApplyIfResult {
    pub success: bool,
    #[serde(rename = "serverTime", default)]
    pub server_time: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("connection error: {0}")]
    Connection(#[from] ConnectionError),

    #[error("rpc error: code={code}, message={message}")]
    Rpc { code: String, message: String },

    #[error("unexpected response format: {0}")]
    Format(String),
}

/// Current epoch milliseconds (for modifiedOn fields in sub-txes).
pub fn epoch_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Generate a simple unique-enough ID: `<epoch-hex>-<counter>`.
/// Mirrors the `generateId()` pattern from huly.core/packages/core/src/utils.ts.
/// No external dep — uses SystemTime + an atomic counter for local uniqueness.
pub fn gen_id(counter: u64) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTR: AtomicU64 = AtomicU64::new(0);
    let seq = CTR.fetch_add(1, Ordering::Relaxed) + counter;
    let t = epoch_ms() as u64;
    format!("{:x}-{:x}", t, seq)
}

pub struct HulyClient {
    conn: Arc<dyn HulyConnection>,
}

impl HulyClient {
    pub fn new(conn: Arc<dyn HulyConnection>) -> Self {
        Self { conn }
    }
}

#[async_trait]
impl PlatformClient for HulyClient {
    async fn find_all(
        &self,
        class: &str,
        query: Value,
        options: Option<FindOptions>,
    ) -> Result<FindResult, ClientError> {
        let mut params = vec![json!(class), query];
        if let Some(opts) = options {
            params.push(serde_json::to_value(opts).unwrap_or_default());
        }

        let resp = self.conn.send_request("findAll", params).await?;

        if let Some(err) = resp.error {
            return Err(ClientError::Rpc {
                code: err.code,
                message: err.message,
            });
        }

        let result = resp
            .result
            .ok_or_else(|| ClientError::Format("missing result in findAll response".into()))?;

        // Huly transactor speaks three shapes:
        //   1. plain `[doc, doc, …]`                         (legacy)
        //   2. `{ docs: [...], total, lookupMap }`           (legacy wrapper)
        //   3. `{ dataType: "TotalArray", value: [...],      (current 0.7.x)
        //         total, lookupMap }`
        // `total = -1` is the sentinel for "count not requested".
        if let Some(arr) = result.as_array() {
            return Ok(FindResult {
                total: arr.len() as i64,
                docs: serde_json::from_value(Value::Array(arr.clone()))
                    .map_err(|e| ClientError::Format(e.to_string()))?,
                lookup_map: None,
            });
        }

        if let Some(obj) = result.as_object() {
            let docs_field = if obj.contains_key("value") {
                "value"
            } else if obj.contains_key("docs") {
                "docs"
            } else {
                return Err(ClientError::Format(
                    "findAll response object has neither `value` nor `docs`".into(),
                ));
            };
            let docs_val = obj
                .get(docs_field)
                .cloned()
                .unwrap_or_else(|| Value::Array(vec![]));
            let docs: Vec<Doc> = serde_json::from_value(docs_val)
                .map_err(|e| ClientError::Format(e.to_string()))?;
            let total = obj
                .get("total")
                .and_then(|v| v.as_i64())
                .unwrap_or(docs.len() as i64);
            let lookup_map = obj.get("lookupMap").filter(|v| !v.is_null()).cloned();
            return Ok(FindResult {
                docs,
                total,
                lookup_map,
            });
        }

        Err(ClientError::Format(format!(
            "findAll result is neither array nor object: {result}"
        )))
    }

    async fn find_one(
        &self,
        class: &str,
        query: Value,
        options: Option<FindOptions>,
    ) -> Result<Option<Doc>, ClientError> {
        let mut opts = options.unwrap_or_default();
        opts.limit = Some(1);

        let result = self.find_all(class, query, Some(opts)).await?;
        Ok(result.docs.into_iter().next())
    }

    async fn create_doc(
        &self,
        class: &str,
        space: &str,
        attributes: Value,
    ) -> Result<String, ClientError> {
        let params = vec![json!(class), json!(space), attributes];
        let resp = self.conn.send_request("createDoc", params).await?;

        if let Some(err) = resp.error {
            return Err(ClientError::Rpc {
                code: err.code,
                message: err.message,
            });
        }

        let result = resp
            .result
            .ok_or_else(|| ClientError::Format("missing result in createDoc response".into()))?;

        result
            .as_str()
            .map(String::from)
            .ok_or_else(|| ClientError::Format("expected string id in createDoc response".into()))
    }

    async fn update_doc(
        &self,
        class: &str,
        space: &str,
        id: &str,
        operations: Value,
    ) -> Result<TxResult, ClientError> {
        let params = vec![json!(class), json!(space), json!(id), operations];
        let resp = self.conn.send_request("updateDoc", params).await?;

        if let Some(err) = resp.error {
            return Err(ClientError::Rpc {
                code: err.code,
                message: err.message,
            });
        }

        let result = resp
            .result
            .ok_or_else(|| ClientError::Format("missing result in updateDoc response".into()))?;

        serde_json::from_value(result).map_err(|e| ClientError::Format(e.to_string()))
    }

    async fn remove_doc(
        &self,
        class: &str,
        space: &str,
        id: &str,
    ) -> Result<TxResult, ClientError> {
        let params = vec![json!(class), json!(space), json!(id)];
        let resp = self.conn.send_request("removeDoc", params).await?;

        if let Some(err) = resp.error {
            return Err(ClientError::Rpc {
                code: err.code,
                message: err.message,
            });
        }

        let result = resp
            .result
            .ok_or_else(|| ClientError::Format("missing result in removeDoc response".into()))?;

        serde_json::from_value(result).map_err(|e| ClientError::Format(e.to_string()))
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
        let params = vec![
            json!(class),
            json!(space),
            json!(attached_to),
            json!(attached_to_class),
            json!(collection),
            attributes,
        ];
        let resp = self.conn.send_request("addCollection", params).await?;

        if let Some(err) = resp.error {
            return Err(ClientError::Rpc {
                code: err.code,
                message: err.message,
            });
        }

        let result = resp
            .result
            .ok_or_else(|| ClientError::Format("missing result in addCollection response".into()))?;

        result.as_str().map(String::from).ok_or_else(|| {
            ClientError::Format("expected string id in addCollection response".into())
        })
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
        let params = vec![
            json!(class),
            json!(space),
            json!(id),
            json!(attached_to),
            json!(attached_to_class),
            json!(collection),
            operations,
        ];
        let resp = self.conn.send_request("updateCollection", params).await?;

        if let Some(err) = resp.error {
            return Err(ClientError::Rpc {
                code: err.code,
                message: err.message,
            });
        }

        let result = resp.result.ok_or_else(|| {
            ClientError::Format("missing result in updateCollection response".into())
        })?;

        serde_json::from_value(result).map_err(|e| ClientError::Format(e.to_string()))
    }

    async fn apply_if_tx(
        &self,
        scope: &str,
        matches: Vec<ApplyIfMatch>,
        not_matches: Vec<ApplyIfMatch>,
        txes: Vec<Value>,
    ) -> Result<ApplyIfResult, ClientError> {
        let now_ms = epoch_ms();
        let tx_id = gen_id(0);

        // Serialize matches into DocumentClassQuery shape: { _class, query }
        let to_query_array = |xs: &[ApplyIfMatch]| -> Vec<Value> {
            xs.iter()
                .map(|m| json!({ "_class": m.class, "query": m.query }))
                .collect()
        };
        let match_json = to_query_array(&matches);

        let mut tx = json!({
            "_id": tx_id,
            "_class": "core:class:TxApplyIf",
            "space": "core:space:Tx",
            "objectSpace": "core:space:Tx",
            "modifiedBy": "core:account:System",
            "modifiedOn": now_ms,
            "scope": scope,
            "match": match_json,
            "txes": txes,
        });
        if !not_matches.is_empty() {
            tx["notMatch"] = Value::Array(to_query_array(&not_matches));
        }

        let resp = self.conn.send_request("tx", vec![tx]).await?;

        if let Some(err) = resp.error {
            return Err(ClientError::Rpc {
                code: err.code,
                message: err.message,
            });
        }

        let result = resp
            .result
            .ok_or_else(|| ClientError::Format("missing result in tx(TxApplyIf) response".into()))?;

        serde_json::from_value(result).map_err(|e| ClientError::Format(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::huly::connection::MockHulyConnection;
    use crate::huly::rpc::{RpcError, RpcResponse};
    use huly_common::api::ApplyIfMatch;

    fn mock_response(result: Value) -> RpcResponse {
        RpcResponse {
            id: 1,
            result: Some(result),
            error: None,
            chunk: None,
            rate_limit: None,
            terminate: None,
            bfst: None,
            queue: None,
        }
    }

    fn error_response(code: &str, message: &str) -> RpcResponse {
        RpcResponse {
            id: 1,
            result: None,
            error: Some(RpcError {
                code: code.to_string(),
                message: message.to_string(),
            }),
            chunk: None,
            rate_limit: None,
            terminate: None,
            bfst: None,
            queue: None,
        }
    }

    #[tokio::test]
    async fn find_all_returns_docs() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .withf(|method, params| {
                method == "findAll" && params[0] == "core:class:Issue"
            })
            .returning(|_, _| {
                Box::pin(async {
                    Ok(mock_response(json!({
                        "docs": [
                            {"_id": "i1", "_class": "core:class:Issue", "title": "Bug"},
                            {"_id": "i2", "_class": "core:class:Issue", "title": "Feature"}
                        ],
                        "total": 2
                    })))
                })
            });

        let client = HulyClient::new(Arc::new(mock));
        let result = client
            .find_all("core:class:Issue", json!({}), None)
            .await
            .unwrap();

        assert_eq!(result.total, 2);
        assert_eq!(result.docs.len(), 2);
        assert_eq!(result.docs[0].id, "i1");
        assert_eq!(result.docs[1].attributes["title"], "Feature");
    }

    #[tokio::test]
    async fn find_all_handles_array_response() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .returning(|_, _| {
                Box::pin(async {
                    Ok(mock_response(json!([
                        {"_id": "a", "_class": "cls"}
                    ])))
                })
            });

        let client = HulyClient::new(Arc::new(mock));
        let result = client.find_all("cls", json!({}), None).await.unwrap();
        assert_eq!(result.total, 1);
        assert_eq!(result.docs[0].id, "a");
    }

    /// Huly transactor 0.7.x emits `{dataType: "TotalArray", value: [...],
    /// total: -1, lookupMap: null}` instead of the legacy `{docs, total}`.
    /// Total = -1 means "count not requested"; the parser must accept it.
    #[tokio::test]
    async fn find_all_handles_total_array_response() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request().returning(|_, _| {
            Box::pin(async {
                Ok(mock_response(json!({
                    "dataType": "TotalArray",
                    "lookupMap": null,
                    "total": -1,
                    "value": [
                        {"_id": "p1", "_class": "tracker:class:Project", "modifiedOn": -1},
                        {"_id": "p2", "_class": "tracker:class:Project"},
                    ],
                })))
            })
        });

        let client = HulyClient::new(Arc::new(mock));
        let result = client
            .find_all("tracker:class:Project", json!({}), None)
            .await
            .unwrap();
        assert_eq!(result.total, -1);
        assert_eq!(result.docs.len(), 2);
        assert_eq!(result.docs[0].id, "p1");
        assert_eq!(result.docs[0].modified_on, -1);
        assert!(result.lookup_map.is_none());
    }

    #[tokio::test]
    async fn find_all_with_options() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .withf(|_, params| params.len() == 3 && params[2]["limit"] == 5)
            .returning(|_, _| {
                Box::pin(async {
                    Ok(mock_response(json!({"docs": [], "total": 0})))
                })
            });

        let client = HulyClient::new(Arc::new(mock));
        let opts = FindOptions {
            limit: Some(5),
            ..Default::default()
        };
        let result = client
            .find_all("cls", json!({}), Some(opts))
            .await
            .unwrap();
        assert_eq!(result.total, 0);
    }

    #[tokio::test]
    async fn find_one_returns_first_doc() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .returning(|_, _| {
                Box::pin(async {
                    Ok(mock_response(json!({
                        "docs": [{"_id": "only", "_class": "cls"}],
                        "total": 1
                    })))
                })
            });

        let client = HulyClient::new(Arc::new(mock));
        let doc = client
            .find_one("cls", json!({"_id": "only"}), None)
            .await
            .unwrap();
        assert_eq!(doc.unwrap().id, "only");
    }

    #[tokio::test]
    async fn find_one_returns_none_when_empty() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .returning(|_, _| {
                Box::pin(async {
                    Ok(mock_response(json!({"docs": [], "total": 0})))
                })
            });

        let client = HulyClient::new(Arc::new(mock));
        let doc = client.find_one("cls", json!({}), None).await.unwrap();
        assert!(doc.is_none());
    }

    #[tokio::test]
    async fn create_doc_returns_id() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .withf(|method, params| {
                method == "createDoc"
                    && params[0] == "core:class:Issue"
                    && params[1] == "space-1"
                    && params[2]["title"] == "New issue"
            })
            .returning(|_, _| {
                Box::pin(async {
                    Ok(mock_response(json!("new-doc-id")))
                })
            });

        let client = HulyClient::new(Arc::new(mock));
        let id = client
            .create_doc("core:class:Issue", "space-1", json!({"title": "New issue"}))
            .await
            .unwrap();
        assert_eq!(id, "new-doc-id");
    }

    #[tokio::test]
    async fn update_doc_returns_tx_result() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .withf(|method, _| method == "updateDoc")
            .returning(|_, _| {
                Box::pin(async {
                    Ok(mock_response(json!({"success": true, "id": "d1"})))
                })
            });

        let client = HulyClient::new(Arc::new(mock));
        let result = client
            .update_doc("cls", "sp", "d1", json!({"title": "Updated"}))
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn remove_doc_returns_tx_result() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .withf(|method, params| method == "removeDoc" && params[2] == "d1")
            .returning(|_, _| {
                Box::pin(async {
                    Ok(mock_response(json!({"success": true})))
                })
            });

        let client = HulyClient::new(Arc::new(mock));
        let result = client.remove_doc("cls", "sp", "d1").await.unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn rpc_error_propagated() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .returning(|_, _| {
                Box::pin(async { Ok(error_response("403", "forbidden")) })
            });

        let client = HulyClient::new(Arc::new(mock));
        let err = client.find_all("cls", json!({}), None).await.unwrap_err();
        match err {
            ClientError::Rpc { code, message } => {
                assert_eq!(code, "403");
                assert_eq!(message, "forbidden");
            }
            _ => panic!("expected RPC error"),
        }
    }

    #[tokio::test]
    async fn update_doc_passes_through_inc_operator() {
        // Verifies that arbitrary update operators (in particular `$inc`) are
        // forwarded verbatim as the 4th param to the upstream `updateDoc` RPC.
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .withf(|method, params| {
                method == "updateDoc"
                    && params[0] == "tracker:class:Project"
                    && params[1] == "core:space:Space"
                    && params[2] == "proj-1"
                    && params[3]["$inc"]["sequence"] == 1
            })
            .returning(|_, _| {
                Box::pin(async {
                    Ok(mock_response(json!({"success": true, "id": "proj-1"})))
                })
            });

        let client = HulyClient::new(Arc::new(mock));
        let result = client
            .update_doc(
                "tracker:class:Project",
                "core:space:Space",
                "proj-1",
                json!({"$inc": {"sequence": 1}}),
            )
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn add_collection_returns_id_and_uses_six_params() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .withf(|method, params| {
                method == "addCollection"
                    && params.len() == 6
                    && params[0] == "tracker:class:Issue"
                    && params[1] == "proj-1"
                    && params[2] == "tracker:ids:NoParent"
                    && params[3] == "tracker:class:Issue"
                    && params[4] == "subIssues"
                    && params[5]["title"] == "First"
            })
            .returning(|_, _| {
                Box::pin(async { Ok(mock_response(json!("issue-new"))) })
            });

        let client = HulyClient::new(Arc::new(mock));
        let id = client
            .add_collection(
                "tracker:class:Issue",
                "proj-1",
                "tracker:ids:NoParent",
                "tracker:class:Issue",
                "subIssues",
                json!({"title": "First"}),
            )
            .await
            .unwrap();
        assert_eq!(id, "issue-new");
    }

    #[tokio::test]
    async fn add_collection_format_error_when_result_not_string() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .returning(|_, _| Box::pin(async { Ok(mock_response(json!({"id": "x"}))) }));

        let client = HulyClient::new(Arc::new(mock));
        let err = client
            .add_collection("c", "s", "p", "pc", "col", json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, ClientError::Format(_)));
    }

    #[tokio::test]
    async fn update_collection_returns_tx_result_and_uses_seven_params() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .withf(|method, params| {
                method == "updateCollection"
                    && params.len() == 7
                    && params[0] == "tracker:class:Issue"
                    && params[1] == "proj-1"
                    && params[2] == "issue-1"
                    && params[3] == "tracker:ids:NoParent"
                    && params[4] == "tracker:class:Issue"
                    && params[5] == "subIssues"
                    && params[6]["title"] == "renamed"
            })
            .returning(|_, _| {
                Box::pin(async { Ok(mock_response(json!({"success": true, "id": "issue-1"}))) })
            });

        let client = HulyClient::new(Arc::new(mock));
        let result = client
            .update_collection(
                "tracker:class:Issue",
                "proj-1",
                "issue-1",
                "tracker:ids:NoParent",
                "tracker:class:Issue",
                "subIssues",
                json!({"title": "renamed"}),
            )
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.id.as_deref(), Some("issue-1"));
    }

    #[tokio::test]
    async fn add_collection_propagates_rpc_error() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .returning(|_, _| Box::pin(async { Ok(error_response("403", "denied")) }));

        let client = HulyClient::new(Arc::new(mock));
        let err = client
            .add_collection("c", "s", "p", "pc", "col", json!({}))
            .await
            .unwrap_err();
        match err {
            ClientError::Rpc { code, .. } => assert_eq!(code, "403"),
            _ => panic!("expected Rpc error"),
        }
    }

    #[tokio::test]
    async fn update_collection_propagates_rpc_error() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .returning(|_, _| Box::pin(async { Ok(error_response("404", "missing")) }));

        let client = HulyClient::new(Arc::new(mock));
        let err = client
            .update_collection("c", "s", "i", "p", "pc", "col", json!({}))
            .await
            .unwrap_err();
        match err {
            ClientError::Rpc { code, .. } => assert_eq!(code, "404"),
            _ => panic!("expected Rpc error"),
        }
    }

    #[tokio::test]
    async fn apply_if_tx_sends_tx_method_with_apply_if_envelope() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .withf(|method, params| {
                method == "tx"
                    && params.len() == 1
                    && params[0]["_class"] == "core:class:TxApplyIf"
                    && params[0]["scope"] == "tracker:project:p1:issue-create"
                    && params[0]["match"][0]["_class"] == "tracker:class:Project"
                    && params[0]["match"][0]["query"]["sequence"] == 5
                    && params[0]["txes"].as_array().map(|a| a.len() == 2).unwrap_or(false)
            })
            .returning(|_, _| {
                Box::pin(async {
                    Ok(mock_response(json!({"success": true, "serverTime": 12345})))
                })
            });

        let client = HulyClient::new(Arc::new(mock));
        let matches = vec![ApplyIfMatch {
            class: "tracker:class:Project".into(),
            query: json!({"sequence": 5}),
        }];
        let txes = vec![
            json!({"_class": "core:class:TxUpdateDoc", "_id": "t1"}),
            json!({"_class": "core:class:TxCreateDoc", "_id": "t2"}),
        ];
        let result = client
            .apply_if_tx("tracker:project:p1:issue-create", matches, vec![], txes)
            .await
            .unwrap();
        assert!(result.success);
        assert_eq!(result.server_time, 12345);
    }

    #[tokio::test]
    async fn apply_if_tx_serializes_not_match_when_present() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .withf(|method, params| {
                method == "tx"
                    && params[0]["notMatch"].is_array()
                    && params[0]["notMatch"][0]["_class"] == "tracker:class:Component"
                    && params[0]["notMatch"][0]["query"]["label"] == "Frontend"
            })
            .returning(|_, _| {
                Box::pin(async {
                    Ok(mock_response(json!({"success": true, "serverTime": 1})))
                })
            });
        let client = HulyClient::new(Arc::new(mock));
        let not_matches = vec![ApplyIfMatch {
            class: "tracker:class:Component".into(),
            query: json!({"label": "Frontend"}),
        }];
        let result = client
            .apply_if_tx("scope", vec![], not_matches, vec![json!({"_class": "tx"})])
            .await
            .unwrap();
        assert!(result.success);
    }

    #[tokio::test]
    async fn apply_if_tx_omits_not_match_when_empty() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .withf(|_, params| params[0].get("notMatch").is_none())
            .returning(|_, _| {
                Box::pin(async {
                    Ok(mock_response(json!({"success": true, "serverTime": 1})))
                })
            });
        let client = HulyClient::new(Arc::new(mock));
        client
            .apply_if_tx("scope", vec![], vec![], vec![json!({"_class": "tx"})])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn apply_if_tx_propagates_rpc_error() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .returning(|_, _| Box::pin(async { Ok(error_response("403", "denied")) }));

        let client = HulyClient::new(Arc::new(mock));
        let err = client
            .apply_if_tx("scope", vec![], vec![], vec![json!({})])
            .await
            .unwrap_err();
        match err {
            ClientError::Rpc { code, .. } => assert_eq!(code, "403"),
            _ => panic!("expected Rpc error"),
        }
    }

    #[tokio::test]
    async fn apply_if_tx_returns_success_false_when_scope_contended() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .returning(|_, _| {
                Box::pin(async {
                    Ok(mock_response(json!({"success": false, "serverTime": 0})))
                })
            });

        let client = HulyClient::new(Arc::new(mock));
        let result = client
            .apply_if_tx("scope", vec![], vec![], vec![json!({})])
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn connection_error_propagated() {
        let mut mock = MockHulyConnection::new();
        mock.expect_send_request()
            .returning(|_, _| {
                Box::pin(async { Err(ConnectionError::NotConnected) })
            });

        let client = HulyClient::new(Arc::new(mock));
        let err = client.find_all("cls", json!({}), None).await.unwrap_err();
        assert!(matches!(err, ClientError::Connection(_)));
    }
}
