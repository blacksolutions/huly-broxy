//! Catalog of Huly class IDs, status IDs, and relation IDs.
//!
//! Defaults match the Muhasebot deployment. Production deployments will have
//! different IDs — override via `[mcp.catalog]` config section. Operators can
//! discover the correct IDs in their workspace via the `huly_discover` tool
//! (it returns `cardTypes[]` and `associations[]`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Card type enum mirrored from the upstream MCP server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum CardType {
    #[serde(rename = "Module Spec")]
    ModuleSpec,
    #[serde(rename = "Data Entity")]
    DataEntity,
    #[serde(rename = "Business Flow")]
    BusinessFlow,
    #[serde(rename = "Compliance Item")]
    ComplianceItem,
    #[serde(rename = "Product Decision")]
    ProductDecision,
    #[serde(rename = "Jurisdiction")]
    Jurisdiction,
}

impl CardType {
    pub fn name(self) -> &'static str {
        match self {
            CardType::ModuleSpec => "Module Spec",
            CardType::DataEntity => "Data Entity",
            CardType::BusinessFlow => "Business Flow",
            CardType::ComplianceItem => "Compliance Item",
            CardType::ProductDecision => "Product Decision",
            CardType::Jurisdiction => "Jurisdiction",
        }
    }

    pub fn all() -> &'static [CardType] {
        &[
            CardType::ModuleSpec,
            CardType::DataEntity,
            CardType::BusinessFlow,
            CardType::ComplianceItem,
            CardType::ProductDecision,
            CardType::Jurisdiction,
        ]
    }

    #[allow(dead_code)]
    pub fn from_name(name: &str) -> Option<CardType> {
        match name {
            "Module Spec" => Some(CardType::ModuleSpec),
            "Data Entity" => Some(CardType::DataEntity),
            "Business Flow" => Some(CardType::BusinessFlow),
            "Compliance Item" => Some(CardType::ComplianceItem),
            "Product Decision" => Some(CardType::ProductDecision),
            "Jurisdiction" => Some(CardType::Jurisdiction),
            _ => None,
        }
    }
}

/// Issue status enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum IssueStatus {
    #[serde(rename = "backlog")]
    Backlog,
    #[serde(rename = "todo")]
    Todo,
    #[serde(rename = "inProgress")]
    InProgress,
    #[serde(rename = "done")]
    Done,
    #[serde(rename = "canceled")]
    Canceled,
}

impl IssueStatus {
    #[allow(dead_code)]
    pub fn name(self) -> &'static str {
        match self {
            IssueStatus::Backlog => "backlog",
            IssueStatus::Todo => "todo",
            IssueStatus::InProgress => "inProgress",
            IssueStatus::Done => "done",
            IssueStatus::Canceled => "canceled",
        }
    }

    #[allow(dead_code)]
    pub fn all() -> &'static [IssueStatus] {
        &[
            IssueStatus::Backlog,
            IssueStatus::Todo,
            IssueStatus::InProgress,
            IssueStatus::Done,
            IssueStatus::Canceled,
        ]
    }
}

/// Relation type enum (issue -> card linkage).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum RelationType {
    #[serde(rename = "module")]
    Module,
    #[serde(rename = "entity")]
    Entity,
    #[serde(rename = "flow")]
    Flow,
    #[serde(rename = "compliance")]
    Compliance,
    #[serde(rename = "decision")]
    Decision,
}

impl RelationType {
    pub fn name(self) -> &'static str {
        match self {
            RelationType::Module => "module",
            RelationType::Entity => "entity",
            RelationType::Flow => "flow",
            RelationType::Compliance => "compliance",
            RelationType::Decision => "decision",
        }
    }

    pub fn all() -> &'static [RelationType] {
        &[
            RelationType::Module,
            RelationType::Entity,
            RelationType::Flow,
            RelationType::Compliance,
            RelationType::Decision,
        ]
    }
}

pub const NO_PARENT: &str = "tracker:ids:NoParent";
pub const MODEL_SPACE: &str = "core:space:Model";
pub const TASK_TYPE_ISSUE: &str = "tracker:taskTypes:Issue";

// Default IDs (Muhasebot deployment).
const DEFAULT_CARD_MODULE_SPEC: &str = "69cba7dae4930c825a40f63f";
const DEFAULT_CARD_DATA_ENTITY: &str = "69cba7dae4930c825a40f63b";
const DEFAULT_CARD_BUSINESS_FLOW: &str = "69cba7d9e4930c825a40f637";
const DEFAULT_CARD_COMPLIANCE_ITEM: &str = "69cba7d9e4930c825a40f639";
const DEFAULT_CARD_PRODUCT_DECISION: &str = "69cba7dbe4930c825a40f641";
const DEFAULT_CARD_JURISDICTION: &str = "69cba7dae4930c825a40f63d";

const DEFAULT_REL_MODULE: &str = "69cd0eab1ee97351f1e74832";
const DEFAULT_REL_ENTITY: &str = "69cd11c92f89db58b7dff5db";
const DEFAULT_REL_FLOW: &str = "69cd11c92f89db58b7dff5dd";
const DEFAULT_REL_COMPLIANCE: &str = "69cd11c92f89db58b7dff5df";
const DEFAULT_REL_DECISION: &str = "69cd11c92f89db58b7dff5e1";

/// Status IDs are stable Huly model references — same on every deployment.
pub fn status_id(status: IssueStatus) -> &'static str {
    match status {
        IssueStatus::Backlog => "tracker:status:Backlog",
        IssueStatus::Todo => "tracker:status:Todo",
        IssueStatus::InProgress => "tracker:status:InProgress",
        IssueStatus::Done => "tracker:status:Done",
        IssueStatus::Canceled => "tracker:status:Canceled",
    }
}

/// Priority label (informational; numeric values pass through).
pub fn priority_name(p: u8) -> &'static str {
    match p {
        0 => "NoPriority",
        1 => "Urgent",
        2 => "High",
        3 => "Medium",
        4 => "Low",
        _ => "Unknown",
    }
}

/// Override map for catalog IDs. All fields are optional; missing values fall
/// back to Muhasebot defaults.
#[derive(Debug, Default, Clone, Deserialize)]
pub struct CatalogOverrides {
    #[serde(default)]
    pub card_types: HashMap<String, String>,
    #[serde(default)]
    pub relations: HashMap<String, String>,
}

impl CatalogOverrides {
    /// Return override keys that do not map to any known `CardType` / `RelationType`.
    /// Callers should emit a startup warning so operators notice typos (the override
    /// would otherwise be silently ignored and the Muhasebot default would apply).
    pub fn unknown_keys(&self) -> Vec<String> {
        let known_ct: HashSet<&str> = CardType::all().iter().map(|c| c.name()).collect();
        let known_rt: HashSet<&str> = RelationType::all().iter().map(|r| r.name()).collect();
        let mut out = Vec::new();
        for k in self.card_types.keys() {
            if !known_ct.contains(k.as_str()) {
                out.push(format!("card_types.\"{k}\""));
            }
        }
        for k in self.relations.keys() {
            if !known_rt.contains(k.as_str()) {
                out.push(format!("relations.\"{k}\""));
            }
        }
        out.sort();
        out
    }
}

/// Resolved catalog: card type IDs and relation IDs keyed by enum.
#[derive(Debug, Clone)]
pub struct Catalog {
    card_types: HashMap<CardType, String>,
    relations: HashMap<RelationType, String>,
}

impl Default for Catalog {
    fn default() -> Self {
        Catalog::new(&CatalogOverrides::default())
    }
}

impl Catalog {
    pub fn new(overrides: &CatalogOverrides) -> Self {
        let mut card_types = HashMap::new();
        for ct in CardType::all() {
            let id = overrides
                .card_types
                .get(ct.name())
                .cloned()
                .unwrap_or_else(|| default_card_id(*ct).to_string());
            card_types.insert(*ct, id);
        }

        let mut relations = HashMap::new();
        for rt in RelationType::all() {
            let id = overrides
                .relations
                .get(rt.name())
                .cloned()
                .unwrap_or_else(|| default_relation_id(*rt).to_string());
            relations.insert(*rt, id);
        }

        Self { card_types, relations }
    }

    pub fn card_type_id(&self, ct: CardType) -> &str {
        self.card_types
            .get(&ct)
            .expect("catalog initialized for all CardType variants")
    }

    pub fn relation_id(&self, rt: RelationType) -> &str {
        self.relations
            .get(&rt)
            .expect("catalog initialized for all RelationType variants")
    }

    #[allow(dead_code)]
    pub fn card_type_by_id(&self, id: &str) -> Option<CardType> {
        self.card_types
            .iter()
            .find(|(_, v)| v.as_str() == id)
            .map(|(k, _)| *k)
    }

    #[allow(dead_code)]
    pub fn relation_by_id(&self, id: &str) -> Option<RelationType> {
        self.relations
            .iter()
            .find(|(_, v)| v.as_str() == id)
            .map(|(k, _)| *k)
    }

    /// All card type IDs (for `huly_find_cards` when no specific type given).
    pub fn all_card_type_ids(&self) -> Vec<(CardType, String)> {
        CardType::all()
            .iter()
            .map(|ct| (*ct, self.card_type_id(*ct).to_string()))
            .collect()
    }
}

fn default_card_id(ct: CardType) -> &'static str {
    match ct {
        CardType::ModuleSpec => DEFAULT_CARD_MODULE_SPEC,
        CardType::DataEntity => DEFAULT_CARD_DATA_ENTITY,
        CardType::BusinessFlow => DEFAULT_CARD_BUSINESS_FLOW,
        CardType::ComplianceItem => DEFAULT_CARD_COMPLIANCE_ITEM,
        CardType::ProductDecision => DEFAULT_CARD_PRODUCT_DECISION,
        CardType::Jurisdiction => DEFAULT_CARD_JURISDICTION,
    }
}

fn default_relation_id(rt: RelationType) -> &'static str {
    match rt {
        RelationType::Module => DEFAULT_REL_MODULE,
        RelationType::Entity => DEFAULT_REL_ENTITY,
        RelationType::Flow => DEFAULT_REL_FLOW,
        RelationType::Compliance => DEFAULT_REL_COMPLIANCE,
        RelationType::Decision => DEFAULT_REL_DECISION,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_type_name_round_trip() {
        for ct in CardType::all() {
            let name = ct.name();
            assert_eq!(CardType::from_name(name), Some(*ct), "round-trip {}", name);
        }
    }

    #[test]
    fn default_catalog_has_all_card_types() {
        let cat = Catalog::default();
        for ct in CardType::all() {
            let id = cat.card_type_id(*ct);
            assert!(!id.is_empty(), "missing default for {:?}", ct);
            assert_eq!(cat.card_type_by_id(id), Some(*ct), "lookup-by-id {:?}", ct);
        }
    }

    #[test]
    fn default_catalog_has_all_relations() {
        let cat = Catalog::default();
        for rt in RelationType::all() {
            let id = cat.relation_id(*rt);
            assert!(!id.is_empty(), "missing default for {:?}", rt);
            assert_eq!(cat.relation_by_id(id), Some(*rt));
        }
    }

    #[test]
    fn overrides_replace_defaults() {
        let mut o = CatalogOverrides::default();
        o.card_types.insert("Module Spec".into(), "custom-id".into());
        o.relations.insert("module".into(), "custom-rel".into());
        let cat = Catalog::new(&o);
        assert_eq!(cat.card_type_id(CardType::ModuleSpec), "custom-id");
        assert_eq!(cat.relation_id(RelationType::Module), "custom-rel");
        // Unaltered defaults remain.
        assert_eq!(
            cat.card_type_id(CardType::DataEntity),
            DEFAULT_CARD_DATA_ENTITY
        );
    }

    #[test]
    fn status_ids_match_huly_model_refs() {
        assert_eq!(status_id(IssueStatus::Backlog), "tracker:status:Backlog");
        assert_eq!(status_id(IssueStatus::Todo), "tracker:status:Todo");
        assert_eq!(
            status_id(IssueStatus::InProgress),
            "tracker:status:InProgress"
        );
        assert_eq!(status_id(IssueStatus::Done), "tracker:status:Done");
        assert_eq!(status_id(IssueStatus::Canceled), "tracker:status:Canceled");
    }

    #[test]
    fn issue_status_serde_round_trip() {
        for st in IssueStatus::all() {
            let json = serde_json::to_value(st).unwrap();
            let back: IssueStatus = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(*st, back, "json: {json}");
        }
    }

    #[test]
    fn relation_type_serde_round_trip() {
        for rt in RelationType::all() {
            let json = serde_json::to_value(rt).unwrap();
            let back: RelationType = serde_json::from_value(json).unwrap();
            assert_eq!(*rt, back);
        }
    }

    #[test]
    fn card_type_serde_round_trip() {
        for ct in CardType::all() {
            let json = serde_json::to_value(ct).unwrap();
            let back: CardType = serde_json::from_value(json).unwrap();
            assert_eq!(*ct, back);
        }
    }

    #[test]
    fn unknown_keys_empty_when_all_keys_valid() {
        let mut o = CatalogOverrides::default();
        o.card_types.insert("Module Spec".into(), "x".into());
        o.relations.insert("module".into(), "y".into());
        assert!(o.unknown_keys().is_empty());
    }

    #[test]
    fn unknown_keys_flags_typos_in_overrides() {
        let mut o = CatalogOverrides::default();
        o.card_types.insert("Module Spec".into(), "x".into()); // valid
        o.card_types.insert("Modul Spec".into(), "x".into()); // typo
        o.relations.insert("moduel".into(), "y".into()); // typo
        let unknown = o.unknown_keys();
        assert_eq!(unknown.len(), 2);
        assert!(unknown.iter().any(|k| k.contains("Modul Spec")));
        assert!(unknown.iter().any(|k| k.contains("moduel")));
    }

    #[test]
    fn priority_name_known_values() {
        assert_eq!(priority_name(0), "NoPriority");
        assert_eq!(priority_name(1), "Urgent");
        assert_eq!(priority_name(2), "High");
        assert_eq!(priority_name(3), "Medium");
        assert_eq!(priority_name(4), "Low");
        assert_eq!(priority_name(99), "Unknown");
    }
}
