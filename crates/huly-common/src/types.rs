use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Reference to a document by its ID
pub type Ref = String;

/// Reference to a class
pub type ClassRef = String;

/// Reference to a space
pub type SpaceRef = String;

/// Generic document from Huly
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Doc {
    #[serde(rename = "_id")]
    pub id: Ref,
    #[serde(rename = "_class")]
    pub class: ClassRef,
    #[serde(rename = "space", default, skip_serializing_if = "Option::is_none")]
    pub space: Option<SpaceRef>,
    #[serde(rename = "modifiedOn", default)]
    pub modified_on: u64,
    #[serde(rename = "modifiedBy", default, skip_serializing_if = "Option::is_none")]
    pub modified_by: Option<String>,
    /// All other fields
    #[serde(flatten)]
    pub attributes: Value,
}

/// Result of findAll
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FindResult {
    pub docs: Vec<Doc>,
    pub total: u64,
    #[serde(rename = "lookupMap", default, skip_serializing_if = "Option::is_none")]
    pub lookup_map: Option<Value>,
}

/// Options for find queries
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FindOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lookup: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub projection: Option<Value>,
}

/// Result of a transaction (create/update/remove)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TxResult {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Ref>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn doc_deserializes_from_json() {
        let json = json!({
            "_id": "doc-123",
            "_class": "core:class:Issue",
            "space": "project-1",
            "modifiedOn": 1700000000000u64,
            "modifiedBy": "user:john",
            "title": "Fix bug",
            "priority": 1
        });

        let doc: Doc = serde_json::from_value(json).unwrap();
        assert_eq!(doc.id, "doc-123");
        assert_eq!(doc.class, "core:class:Issue");
        assert_eq!(doc.space.as_deref(), Some("project-1"));
        assert_eq!(doc.modified_on, 1700000000000);
        assert_eq!(doc.attributes["title"], "Fix bug");
        assert_eq!(doc.attributes["priority"], 1);
    }

    #[test]
    fn doc_without_optional_fields() {
        let json = json!({
            "_id": "doc-1",
            "_class": "core:class:Space"
        });

        let doc: Doc = serde_json::from_value(json).unwrap();
        assert!(doc.space.is_none());
        assert!(doc.modified_by.is_none());
        assert_eq!(doc.modified_on, 0);
    }

    #[test]
    fn doc_serializes_back_to_json() {
        let doc = Doc {
            id: "d1".into(),
            class: "cls".into(),
            space: Some("sp".into()),
            modified_on: 100,
            modified_by: None,
            attributes: json!({"name": "test"}),
        };

        let json = serde_json::to_value(&doc).unwrap();
        assert_eq!(json["_id"], "d1");
        assert_eq!(json["_class"], "cls");
        assert_eq!(json["space"], "sp");
        assert_eq!(json["name"], "test");
    }

    #[test]
    fn find_result_with_docs() {
        let json = json!({
            "docs": [
                {"_id": "a", "_class": "cls"},
                {"_id": "b", "_class": "cls"}
            ],
            "total": 42
        });

        let result: FindResult = serde_json::from_value(json).unwrap();
        assert_eq!(result.docs.len(), 2);
        assert_eq!(result.total, 42);
        assert!(result.lookup_map.is_none());
    }

    #[test]
    fn find_options_skips_none_fields() {
        let opts = FindOptions {
            limit: Some(10),
            ..Default::default()
        };

        let json = serde_json::to_value(&opts).unwrap();
        assert_eq!(json["limit"], 10);
        assert!(json.get("sort").is_none());
        assert!(json.get("lookup").is_none());
    }

    #[test]
    fn tx_result_success() {
        let json = json!({"success": true, "id": "new-doc-1"});
        let result: TxResult = serde_json::from_value(json).unwrap();
        assert!(result.success);
        assert_eq!(result.id.as_deref(), Some("new-doc-1"));
    }

    #[test]
    fn tx_result_without_id() {
        let json = json!({"success": false});
        let result: TxResult = serde_json::from_value(json).unwrap();
        assert!(!result.success);
        assert!(result.id.is_none());
    }
}
