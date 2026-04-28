use crate::types::FindOptions;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Request body for find / find-one endpoints
#[derive(Debug, Serialize, Deserialize)]
pub struct FindRequest {
    pub class: String,
    pub query: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<FindOptions>,
}

/// Request body for create endpoint
#[derive(Debug, Serialize, Deserialize)]
pub struct CreateRequest {
    pub class: String,
    pub space: String,
    pub attributes: Value,
}

/// Request body for update endpoint
#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateRequest {
    pub class: String,
    pub space: String,
    pub id: String,
    pub operations: Value,
}

/// Request body for delete endpoint
#[derive(Debug, Serialize, Deserialize)]
pub struct DeleteRequest {
    pub class: String,
    pub space: String,
    pub id: String,
}

/// Request body for addCollection endpoint
#[derive(Debug, Serialize, Deserialize)]
pub struct AddCollectionRequest {
    pub class: String,
    pub space: String,
    #[serde(rename = "attachedTo")]
    pub attached_to: String,
    #[serde(rename = "attachedToClass")]
    pub attached_to_class: String,
    pub collection: String,
    pub attributes: Value,
}

/// One match predicate inside an ApplyIfRequest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyIfMatch {
    #[serde(rename = "_class")]
    pub class: String,
    pub query: serde_json::Value,
}

/// Request body for the `/api/v1/apply-if` endpoint.
///
/// The bridge forwards this as a `TxApplyIf` envelope to the Huly transactor.
/// Sub-txes must be pre-constructed by the caller (see `huly-mcp::txcud`).
#[derive(Debug, Serialize, Deserialize)]
pub struct ApplyIfRequest {
    pub scope: String,
    #[serde(default)]
    pub matches: Vec<ApplyIfMatch>,
    /// Negative match predicates: the scope succeeds only if **no** documents
    /// match any of these queries. Mirrors upstream `TxApplyIf.notMatch`.
    /// Used to atomically express "create-if-not-exists" without a TOCTOU race.
    #[serde(default, rename = "notMatches", skip_serializing_if = "Vec::is_empty")]
    pub not_matches: Vec<ApplyIfMatch>,
    /// Pre-constructed TxCUD objects (TxUpdateDoc, TxCollectionCUD/TxCreateDoc, …).
    pub txes: Vec<serde_json::Value>,
}

/// Response from the `/api/v1/apply-if` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyIfResponse {
    pub success: bool,
    #[serde(rename = "serverTime")]
    pub server_time: i64,
}

/// Request body for updateCollection endpoint
#[derive(Debug, Serialize, Deserialize)]
pub struct UpdateCollectionRequest {
    pub class: String,
    pub space: String,
    pub id: String,
    #[serde(rename = "attachedTo")]
    pub attached_to: String,
    #[serde(rename = "attachedToClass")]
    pub attached_to_class: String,
    pub collection: String,
    pub operations: Value,
}

/// Request body for `POST /api/v1/upload-markup`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UploadMarkupRequest {
    pub object_class: String,
    pub object_id: String,
    pub object_attr: String,
    pub markdown: String,
}

/// Response from `POST /api/v1/upload-markup`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UploadMarkupResponse {
    /// The MarkupBlobRef returned by the collaborator service.
    #[serde(rename = "ref")]
    pub markup_ref: String,
}

/// Request body for `POST /api/v1/fetch-markup`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct FetchMarkupRequest {
    pub object_class: String,
    pub object_id: String,
    pub object_attr: String,
    /// Optional existing blob reference to fetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    /// Desired output format: `"markdown"` (default) or `"prosemirror"`.
    #[serde(default = "default_fetch_format")]
    pub format: String,
}

fn default_fetch_format() -> String {
    "markdown".to_string()
}

/// Response from `POST /api/v1/fetch-markup`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FetchMarkupResponse {
    /// Markup content in the requested format.
    pub content: String,
    /// Format of the returned content (`"markdown"` or `"prosemirror"`).
    pub format: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn find_request_serializes() {
        let req = FindRequest {
            class: "core:class:Issue".into(),
            query: json!({"space": "s1"}),
            options: Some(FindOptions {
                limit: Some(10),
                ..Default::default()
            }),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["class"], "core:class:Issue");
        assert_eq!(json["options"]["limit"], 10);
    }

    #[test]
    fn add_collection_request_roundtrips() {
        let req = AddCollectionRequest {
            class: "tracker:class:Issue".into(),
            space: "proj-1".into(),
            attached_to: "tracker:ids:NoParent".into(),
            attached_to_class: "tracker:class:Issue".into(),
            collection: "subIssues".into(),
            attributes: json!({"title": "T"}),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["attachedTo"], "tracker:ids:NoParent");
        assert_eq!(v["attachedToClass"], "tracker:class:Issue");
        assert_eq!(v["collection"], "subIssues");
        let back: AddCollectionRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back.collection, "subIssues");
    }

    #[test]
    fn update_collection_request_roundtrips() {
        let req = UpdateCollectionRequest {
            class: "tracker:class:Issue".into(),
            space: "proj-1".into(),
            id: "issue-1".into(),
            attached_to: "tracker:ids:NoParent".into(),
            attached_to_class: "tracker:class:Issue".into(),
            collection: "subIssues".into(),
            operations: json!({"title": "renamed"}),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["id"], "issue-1");
        assert_eq!(v["attachedToClass"], "tracker:class:Issue");
        let back: UpdateCollectionRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back.id, "issue-1");
    }

    #[test]
    fn apply_if_request_roundtrips() {
        let req = ApplyIfRequest {
            scope: "tracker:project:proj-1:issue-create".into(),
            matches: vec![ApplyIfMatch {
                class: "tracker:class:Project".into(),
                query: json!({"_id": "proj-1", "sequence": 5}),
            }],
            not_matches: vec![],
            txes: vec![
                json!({"_class": "core:class:TxUpdateDoc", "_id": "tx-1"}),
                json!({"_class": "core:class:TxCreateDoc", "_id": "tx-2"}),
            ],
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["scope"], "tracker:project:proj-1:issue-create");
        assert_eq!(v["matches"][0]["_class"], "tracker:class:Project");
        assert_eq!(v["matches"][0]["query"]["sequence"], 5);
        assert_eq!(v["txes"].as_array().unwrap().len(), 2);
        // notMatches must be omitted from the wire when empty.
        assert!(v.get("notMatches").is_none());
        let back: ApplyIfRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back.scope, "tracker:project:proj-1:issue-create");
        assert_eq!(back.matches.len(), 1);
        assert!(back.not_matches.is_empty());
        assert_eq!(back.txes.len(), 2);
    }

    #[test]
    fn apply_if_request_default_empty_matches() {
        let v = serde_json::json!({
            "scope": "some:scope",
            "txes": []
        });
        let req: ApplyIfRequest = serde_json::from_value(v).unwrap();
        assert!(req.matches.is_empty());
        assert!(req.not_matches.is_empty());
    }

    #[test]
    fn apply_if_request_serializes_not_matches_when_present() {
        let req = ApplyIfRequest {
            scope: "tracker:component:create".into(),
            matches: vec![],
            not_matches: vec![ApplyIfMatch {
                class: "tracker:class:Component".into(),
                query: json!({"space": "proj-1", "label": "Frontend"}),
            }],
            txes: vec![json!({"_class": "core:class:TxCreateDoc"})],
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["notMatches"][0]["_class"], "tracker:class:Component");
        assert_eq!(v["notMatches"][0]["query"]["label"], "Frontend");
        let back: ApplyIfRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back.not_matches.len(), 1);
        assert_eq!(back.not_matches[0].class, "tracker:class:Component");
    }

    #[test]
    fn apply_if_response_roundtrips() {
        let resp = ApplyIfResponse { success: true, server_time: 1700000000000 };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["success"], true);
        assert_eq!(v["serverTime"], 1700000000000i64);
        let back: ApplyIfResponse = serde_json::from_value(v).unwrap();
        assert!(back.success);
        assert_eq!(back.server_time, 1700000000000);
    }

    #[test]
    fn create_request_serializes() {
        let req = CreateRequest {
            class: "core:class:Issue".into(),
            space: "sp1".into(),
            attributes: json!({"title": "Bug"}),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["space"], "sp1");
        assert_eq!(json["attributes"]["title"], "Bug");
    }

    #[test]
    fn upload_markup_request_roundtrips() {
        let req = UploadMarkupRequest {
            object_class: "tracker:class:Issue".into(),
            object_id: "obj-1".into(),
            object_attr: "description".into(),
            markdown: "**hello**".into(),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["objectClass"], "tracker:class:Issue");
        assert_eq!(v["objectId"], "obj-1");
        assert_eq!(v["objectAttr"], "description");
        assert_eq!(v["markdown"], "**hello**");
        let back: UploadMarkupRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn upload_markup_response_serializes_ref_keyword() {
        let resp = UploadMarkupResponse { markup_ref: "blob-ref-xyz".into() };
        let v = serde_json::to_value(&resp).unwrap();
        // "ref" is a Rust keyword — field must be serialised as "ref"
        assert_eq!(v["ref"], "blob-ref-xyz");
        let back: UploadMarkupResponse = serde_json::from_value(v).unwrap();
        assert_eq!(back.markup_ref, "blob-ref-xyz");
    }

    #[test]
    fn fetch_markup_request_roundtrips_with_defaults() {
        let req = FetchMarkupRequest {
            object_class: "tracker:class:Issue".into(),
            object_id: "obj-1".into(),
            object_attr: "description".into(),
            source_ref: Some("blob-ref-abc".into()),
            format: "prosemirror".into(),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["objectClass"], "tracker:class:Issue");
        assert_eq!(v["sourceRef"], "blob-ref-abc");
        assert_eq!(v["format"], "prosemirror");
        let back: FetchMarkupRequest = serde_json::from_value(v).unwrap();
        assert_eq!(back, req);
    }

    #[test]
    fn fetch_markup_request_default_format_is_markdown() {
        let v = json!({
            "objectClass": "c",
            "objectId": "id",
            "objectAttr": "description"
        });
        let req: FetchMarkupRequest = serde_json::from_value(v).unwrap();
        assert_eq!(req.format, "markdown");
        assert!(req.source_ref.is_none());
    }

    #[test]
    fn fetch_markup_response_roundtrips() {
        let resp = FetchMarkupResponse {
            content: "**hello**".into(),
            format: "markdown".into(),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["content"], "**hello**");
        assert_eq!(v["format"], "markdown");
        let back: FetchMarkupResponse = serde_json::from_value(v).unwrap();
        assert_eq!(back, resp);
    }
}
