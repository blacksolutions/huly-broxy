//! Per-workspace schema resolver.
//!
//! Reads MasterTag (card type) and Association (relation) instance docs
//! from the bound transactor and exposes a name → workspace-local `_id`
//! map. Names are workspace-stable (humans rarely rename), IDs are
//! workspace-local — same conceptual MasterTag has different `_id`s in
//! different Huly workspaces. Only the bridge holds a workspace's
//! transactor connection, so only the bridge can resolve names.
//!
//! Stable platform classes (e.g. `tracker:class:Issue`,
//! `core:class:Association`) pass through verbatim; only MasterTag and
//! Association *instance* IDs are workspace-local. See [`is_platform_id`].

use crate::client::{ClientError, PlatformClient};
use huly_common::announcement::WorkspaceSchema;
use huly_common::types::Doc;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Wire classes the bridge queries to populate the schema. Constants of the
/// stable Huly platform model — same on every workspace.
const MASTER_TAG_CLASS: &str = "card:class:MasterTag";
const ASSOCIATION_CLASS: &str = "core:class:Association";

/// Hot-swappable schema cache for a single workspace.
///
/// Exposed via [`SchemaHandle::resolved`] so the announcer can publish
/// `schema_version`, the lookup-responder can deliver the full map, and
/// HTTP handlers can resolve `class` names → workspace-local IDs.
#[derive(Clone, Debug)]
pub struct SchemaHandle {
    inner: Arc<RwLock<SchemaState>>,
}

#[derive(Debug, Default, Clone)]
struct SchemaState {
    version: u64,
    schema: WorkspaceSchema,
    /// Names that resolved to multiple `_id`s on the last refresh — the
    /// resolver returns [`ResolveError::Ambiguous`] for these instead of
    /// silently picking one.
    ambiguous_card_types: BTreeMap<String, Vec<String>>,
    ambiguous_associations: BTreeMap<String, Vec<String>>,
}

impl Default for SchemaHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl SchemaHandle {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(SchemaState::default())),
        }
    }

    /// Snapshot of the resolved schema. Used by the MCP factory's per-workspace
    /// schema cache (D9). The legacy `huly.bridge.schema.<workspace>` NATS
    /// subject was retired in P4.
    pub async fn resolved(&self) -> (u64, WorkspaceSchema) {
        let g = self.inner.read().await;
        (g.version, g.schema.clone())
    }

    pub async fn version(&self) -> u64 {
        self.inner.read().await.version
    }

    /// Resolve a user-supplied `class` to a workspace-local `_id`.
    ///
    /// Resolution rules:
    /// - Empty input is rejected by the caller's validator (we don't
    ///   reach here).
    /// - Platform-stable IDs (the `x:y:Z` shape) pass through unchanged.
    /// - Otherwise the value is treated as a name and looked up in the
    ///   card-type or association map. We try card_types first, then
    ///   associations, because tools call into both.
    /// - Names that hit a collision return [`ResolveError::Ambiguous`].
    /// - Unknown names return [`ResolveError::Unknown`].
    pub async fn resolve_class(&self, class: &str) -> Result<String, ResolveError> {
        if is_platform_id(class) {
            return Ok(class.to_string());
        }

        let g = self.inner.read().await;

        if let Some(ids) = g.ambiguous_card_types.get(class) {
            return Err(ResolveError::Ambiguous {
                name: class.to_string(),
                kind: SchemaKind::CardType,
                matches: ids.clone(),
            });
        }
        if let Some(id) = g.schema.card_types.get(class) {
            return Ok(id.clone());
        }

        if let Some(ids) = g.ambiguous_associations.get(class) {
            return Err(ResolveError::Ambiguous {
                name: class.to_string(),
                kind: SchemaKind::Association,
                matches: ids.clone(),
            });
        }
        if let Some(id) = g.schema.associations.get(class) {
            return Ok(id.clone());
        }

        Err(ResolveError::Unknown(class.to_string()))
    }

    /// Test-only: build a schema cache with pre-baked card-type entries
    /// (each name mapped to itself) so handler tests can pass short
    /// strings (e.g. `"cls"`) through `resolve_class` without hitting
    /// `Unknown`. Real resolution is exercised in [`tests`].
    #[doc(hidden)]
    pub fn with_card_type_names_for_tests(names: &[&str]) -> Self {
        let mut schema = WorkspaceSchema::default();
        for n in names {
            schema.card_types.insert((*n).to_string(), (*n).to_string());
        }
        let inner = Arc::new(RwLock::new(SchemaState {
            version: 1,
            schema,
            ambiguous_card_types: BTreeMap::new(),
            ambiguous_associations: BTreeMap::new(),
        }));
        Self { inner }
    }

    /// Test-only: install a fully-formed schema (arbitrary name → id
    /// mappings) so integration tests can exercise the resolver with
    /// distinct workspace-local IDs across two bridge stand-ins.
    #[doc(hidden)]
    pub fn install_for_tests(schema: WorkspaceSchema) -> Self {
        let inner = Arc::new(RwLock::new(SchemaState {
            version: 1,
            schema,
            ambiguous_card_types: BTreeMap::new(),
            ambiguous_associations: BTreeMap::new(),
        }));
        Self { inner }
    }

    /// Replace the cached schema. Bumps `version` iff the new schema differs.
    /// Returns the new version.
    async fn install(&self, new_schema: WorkspaceSchema, ambig: AmbiguityState) -> u64 {
        let mut g = self.inner.write().await;
        let unchanged = g.schema == new_schema
            && g.ambiguous_card_types == ambig.card_types
            && g.ambiguous_associations == ambig.associations;
        if unchanged && g.version != 0 {
            return g.version;
        }
        g.version = g.version.saturating_add(1);
        g.schema = new_schema;
        g.ambiguous_card_types = ambig.card_types;
        g.ambiguous_associations = ambig.associations;
        g.version
    }
}

/// Whether a string already has Huly platform-id shape (`prefix:kind:Name`).
/// Such IDs are workspace-stable and must not be remapped.
pub fn is_platform_id(s: &str) -> bool {
    s.split(':').count() >= 3 && s.contains(':')
}

#[derive(Debug, Default, Clone)]
struct AmbiguityState {
    card_types: BTreeMap<String, Vec<String>>,
    associations: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResolveError {
    #[error("unknown class name '{0}' — not a platform id and not a known MasterTag/Association")]
    Unknown(String),

    #[error(
        "ambiguous {kind:?} name '{name}' — matches {} workspace docs ({}); disambiguate by passing the workspace-local id directly",
        matches.len(),
        matches.join(", ")
    )]
    Ambiguous {
        name: String,
        kind: SchemaKind,
        matches: Vec<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaKind {
    CardType,
    Association,
}

/// Refresh the cached schema by querying the transactor.
///
/// Pragmatically pulls user-visible names from each doc by trying a small
/// list of attribute keys. Falls back to `_id` if nothing matches — that
/// way the doc is at least addressable by id, and the missing field
/// surfaces in tracing rather than as silent data loss.
pub async fn refresh(
    client: &dyn PlatformClient,
    handle: &SchemaHandle,
) -> Result<u64, ClientError> {
    let card_docs = client
        .find_all(MASTER_TAG_CLASS, json!({}), None)
        .await?
        .docs;
    let assoc_docs = client
        .find_all(ASSOCIATION_CLASS, json!({}), None)
        .await?
        .docs;

    let (card_types, ambig_ct) = build_map(&card_docs, CARD_TYPE_NAME_KEYS);
    let (associations, ambig_a) = build_map(&assoc_docs, ASSOC_NAME_KEYS);

    let new_schema = WorkspaceSchema {
        card_types,
        associations,
    };
    let ambig = AmbiguityState {
        card_types: ambig_ct,
        associations: ambig_a,
    };

    let version = handle.install(new_schema, ambig).await;
    debug!(
        version,
        card_types = card_docs.len(),
        associations = assoc_docs.len(),
        "schema resolver refreshed"
    );
    Ok(version)
}

/// Attribute keys to probe for the user-visible name of a MasterTag.
/// Order matters — first hit wins. `label` is the upstream Huly convention
/// for MasterTags; `name` is a common synonym.
const CARD_TYPE_NAME_KEYS: &[&str] = &["label", "name"];

/// Attribute keys to probe for the user-visible name of an Association.
/// `nameA` is the upstream Huly convention (one direction's label).
const ASSOC_NAME_KEYS: &[&str] = &["nameA", "name", "label"];

fn build_map(
    docs: &[Doc],
    name_keys: &[&str],
) -> (BTreeMap<String, String>, BTreeMap<String, Vec<String>>) {
    let mut acc: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for doc in docs {
        let name = pick_name(&doc.attributes, name_keys).unwrap_or_else(|| doc.id.clone());
        if name.is_empty() {
            warn!(id = %doc.id, "schema doc has empty name — skipping");
            continue;
        }
        acc.entry(name).or_default().push(doc.id.clone());
    }
    let mut unique = BTreeMap::new();
    let mut ambiguous = BTreeMap::new();
    for (name, mut ids) in acc {
        if ids.len() == 1 {
            unique.insert(name, ids.pop().unwrap());
        } else {
            warn!(name = %name, count = ids.len(), "schema name is ambiguous in workspace");
            ambiguous.insert(name, ids);
        }
    }
    (unique, ambiguous)
}

fn pick_name(attrs: &Value, keys: &[&str]) -> Option<String> {
    let obj = attrs.as_object()?;
    for k in keys {
        if let Some(s) = obj.get(*k).and_then(|v| v.as_str())
            && !s.is_empty()
        {
            return Some(s.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::MockPlatformClient;
    use huly_common::types::FindResult;

    fn mock_doc(id: &str, class: &str, attrs: Value) -> Doc {
        Doc {
            id: id.into(),
            class: class.into(),
            space: None,
            modified_on: 0,
            modified_by: None,
            attributes: attrs,
        }
    }

    #[test]
    fn platform_ids_pass_through() {
        assert!(is_platform_id("tracker:class:Issue"));
        assert!(is_platform_id("core:class:Association"));
        assert!(is_platform_id("card:class:MasterTag"));
        assert!(!is_platform_id("Module Spec"));
        assert!(!is_platform_id("69cba7dae4930c825a40f63f"));
        assert!(!is_platform_id(""));
    }

    #[tokio::test]
    async fn resolve_passes_through_platform_ids_without_schema() {
        let h = SchemaHandle::new();
        let r = h.resolve_class("tracker:class:Issue").await.unwrap();
        assert_eq!(r, "tracker:class:Issue");
    }

    #[tokio::test]
    async fn unknown_name_errors() {
        let h = SchemaHandle::new();
        let err = h.resolve_class("Module Spec").await.unwrap_err();
        assert!(matches!(err, ResolveError::Unknown(_)));
    }

    #[tokio::test]
    async fn refresh_populates_card_types_and_associations() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all()
            .withf(|class, _, _| class == MASTER_TAG_CLASS)
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(FindResult {
                        docs: vec![
                            mock_doc("id-mod", "card:class:MasterTag", json!({"label": "Module Spec"})),
                            mock_doc("id-de", "card:class:MasterTag", json!({"label": "Data Entity"})),
                        ],
                        total: 2,
                        lookup_map: None,
                    })
                })
            });
        mock.expect_find_all()
            .withf(|class, _, _| class == ASSOCIATION_CLASS)
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(FindResult {
                        docs: vec![mock_doc(
                            "id-rel-mod",
                            "core:class:Association",
                            json!({"nameA": "module"}),
                        )],
                        total: 1,
                        lookup_map: None,
                    })
                })
            });

        let h = SchemaHandle::new();
        let v1 = refresh(&mock, &h).await.unwrap();
        assert_eq!(v1, 1, "first refresh installs version 1");

        let id = h.resolve_class("Module Spec").await.unwrap();
        assert_eq!(id, "id-mod");
        let id = h.resolve_class("module").await.unwrap();
        assert_eq!(id, "id-rel-mod");
        let id = h.resolve_class("tracker:class:Issue").await.unwrap();
        assert_eq!(id, "tracker:class:Issue");
    }

    #[tokio::test]
    async fn refresh_does_not_bump_version_when_unchanged() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all()
            .withf(|class, _, _| class == MASTER_TAG_CLASS)
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(FindResult {
                        docs: vec![mock_doc(
                            "id-mod",
                            "card:class:MasterTag",
                            json!({"label": "Module Spec"}),
                        )],
                        total: 1,
                        lookup_map: None,
                    })
                })
            });
        mock.expect_find_all()
            .withf(|class, _, _| class == ASSOCIATION_CLASS)
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(FindResult {
                        docs: vec![],
                        total: 0,
                        lookup_map: None,
                    })
                })
            });

        let h = SchemaHandle::new();
        let v1 = refresh(&mock, &h).await.unwrap();
        let v2 = refresh(&mock, &h).await.unwrap();
        assert_eq!(v1, v2, "identical refresh keeps version stable");
    }

    #[tokio::test]
    async fn ambiguous_name_returns_ambiguous_error() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all()
            .withf(|class, _, _| class == MASTER_TAG_CLASS)
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(FindResult {
                        docs: vec![
                            mock_doc("id-a", "card:class:MasterTag", json!({"label": "Spec"})),
                            mock_doc("id-b", "card:class:MasterTag", json!({"label": "Spec"})),
                        ],
                        total: 2,
                        lookup_map: None,
                    })
                })
            });
        mock.expect_find_all()
            .withf(|class, _, _| class == ASSOCIATION_CLASS)
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(FindResult {
                        docs: vec![],
                        total: 0,
                        lookup_map: None,
                    })
                })
            });

        let h = SchemaHandle::new();
        refresh(&mock, &h).await.unwrap();
        let err = h.resolve_class("Spec").await.unwrap_err();
        match err {
            ResolveError::Ambiguous {
                name,
                kind,
                matches,
            } => {
                assert_eq!(name, "Spec");
                assert_eq!(kind, SchemaKind::CardType);
                assert_eq!(matches.len(), 2);
            }
            _ => panic!("expected Ambiguous"),
        }
    }

    #[tokio::test]
    async fn doc_without_known_name_falls_back_to_id() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all()
            .withf(|class, _, _| class == MASTER_TAG_CLASS)
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(FindResult {
                        docs: vec![mock_doc(
                            "id-orphan",
                            "card:class:MasterTag",
                            json!({"unrelated": "junk"}),
                        )],
                        total: 1,
                        lookup_map: None,
                    })
                })
            });
        mock.expect_find_all()
            .withf(|class, _, _| class == ASSOCIATION_CLASS)
            .returning(|_, _, _| {
                Box::pin(async {
                    Ok(FindResult {
                        docs: vec![],
                        total: 0,
                        lookup_map: None,
                    })
                })
            });

        let h = SchemaHandle::new();
        refresh(&mock, &h).await.unwrap();
        // Doc had no usable name field — registered under its own `_id`
        // so it stays addressable.
        let s = h.resolved().await.1;
        assert!(
            s.card_types.contains_key("id-orphan"),
            "fallback map should contain id-orphan as name"
        );
        let resolved = h.resolve_class("id-orphan").await.unwrap();
        assert_eq!(resolved, "id-orphan");
    }
}
