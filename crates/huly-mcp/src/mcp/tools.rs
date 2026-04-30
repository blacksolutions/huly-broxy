//! Domain-specific tracker / card helpers for the MCP tools.
//!
//! These functions wrap [`huly_client::PlatformClient`] with project
//! resolution, schema-based name → id mapping, and Huly-specific shapes
//! (issues, components, cards, projects, relations).
//!
//! Compared to the pre-P4 implementation in `crates/huly-mcp/src/mcp/tools.rs`
//! (deleted by `61a750a`), this version:
//!
//! - takes a `&dyn PlatformClient` rather than the bridge's
//!   `BridgeHttpClient` — every call goes through the JWT-broker-driven
//!   factory and hits the transactor REST surface directly;
//! - resolves MasterTag / Association names through the per-workspace
//!   [`SchemaHandle`] minted by the factory rather than through the
//!   bridge schema responder which P4 retired;
//! - keeps the `apply_if_tx` race-resistance pattern verbatim — the wire
//!   shape is unchanged because the transactor is the consumer.

use crate::mcp::catalog::{
    IssueStatus, MODEL_SPACE, NO_PARENT, TASK_TYPE_ISSUE, priority_name, status_id,
};
use crate::txcud::{
    SYSTEM_ACCOUNT, gen_tx_id, tx_collection_create, tx_create_doc, tx_update_doc,
};
use huly_client::client::PlatformClient;
use huly_client::collaborator::{CollaboratorClient, CollaboratorError};
use huly_client::markdown::{markdown_to_prosemirror_json, prosemirror_to_markdown};
use huly_client::schema_resolver::SchemaHandle;
use huly_common::api::ApplyIfMatch;
use huly_common::types::{Doc, FindOptions};
use secrecy::SecretString;
use serde_json::{Value, json};

/// Resolve a Tracker project by user input (`_id` OR `identifier` like "MUH").
/// If `requested` is `None`, returns the only project in the workspace, or
/// errors if zero or many.
pub async fn resolve_project(
    client: &dyn PlatformClient,
    requested: Option<&str>,
) -> Result<Doc, String> {
    let result = client
        .find_all(
            "tracker:class:Project",
            json!({}),
            Some(FindOptions {
                limit: Some(200),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("error fetching projects: {e}"))?;

    if let Some(needle) = requested {
        if let Some(found) = result.docs.iter().find(|d| {
            d.id == needle
                || d.attributes
                    .get("identifier")
                    .and_then(|v| v.as_str())
                    .map(|s| s == needle)
                    .unwrap_or(false)
        }) {
            return Ok(found.clone());
        }
        return Err(format!(
            "Project '{needle}' not found (matched neither _id nor identifier)."
        ));
    }

    match result.docs.len() {
        0 => Err("No tracker projects found in workspace.".to_string()),
        1 => Ok(result.docs[0].clone()),
        _ => {
            let names: Vec<String> = result
                .docs
                .iter()
                .filter_map(|d| {
                    d.attributes
                        .get("identifier")
                        .and_then(|v| v.as_str())
                        .map(String::from)
                })
                .collect();
            Err(format!(
                "Multiple projects present ({}); pass project explicitly.",
                names.join(", ")
            ))
        }
    }
}

/// Find issues with optional filters. Returns a slim issue summary array.
pub async fn find_issues(
    client: &dyn PlatformClient,
    component: Option<&str>,
    status: Option<IssueStatus>,
    query: Option<&str>,
    limit: u64,
) -> Result<Vec<Value>, String> {
    let mut filter = serde_json::Map::new();
    if let Some(c) = component {
        filter.insert("component".into(), json!(c));
    }
    if let Some(s) = status {
        filter.insert("status".into(), json!(status_id(s)));
    }

    let res = client
        .find_all(
            "tracker:class:Issue",
            Value::Object(filter),
            Some(FindOptions {
                limit: Some(limit),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("error fetching issues: {e}"))?;

    let mut issues: Vec<Value> = res
        .docs
        .into_iter()
        .filter_map(|d| {
            // Skip docs missing a number — matches upstream behaviour.
            d.attributes.get("number")?;
            let mut summary = json!({
                "identifier": d.attributes.get("identifier").cloned().unwrap_or(Value::Null),
                "title": d.attributes.get("title").cloned().unwrap_or(Value::Null),
                "status": d.attributes.get("status").cloned().unwrap_or(Value::Null),
                "component": d.attributes.get("component").cloned().unwrap_or(Value::Null),
                "priority": d.attributes.get("priority").cloned().unwrap_or(Value::Null),
                "number": d.attributes.get("number").cloned().unwrap_or(Value::Null),
            });
            if let Some(p) = summary
                .get("priority")
                .and_then(|v| v.as_u64())
                .and_then(|n| u8::try_from(n).ok())
                && let Some(obj) = summary.as_object_mut()
            {
                obj.insert("priorityName".into(), json!(priority_name(p)));
            }
            Some(summary)
        })
        .collect();

    if let Some(needle) = query.map(|s| s.to_lowercase()).filter(|s| !s.is_empty()) {
        issues.retain(|i| {
            i.get("title")
                .and_then(|v| v.as_str())
                .map(|t| t.to_lowercase().contains(&needle))
                .unwrap_or(false)
        });
    }

    issues.sort_by(|a, b| {
        let an = a.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
        let bn = b.get("number").and_then(|v| v.as_i64()).unwrap_or(0);
        an.cmp(&bn)
    });

    Ok(issues)
}

/// Get a single issue by identifier, plus any incoming/outgoing relations.
pub async fn get_issue(
    client: &dyn PlatformClient,
    identifier: &str,
) -> Result<Option<Value>, String> {
    let res = client
        .find_all(
            "tracker:class:Issue",
            json!({ "identifier": identifier }),
            Some(FindOptions {
                limit: Some(1),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("error fetching issue: {e}"))?;
    let issue = match res.docs.into_iter().next() {
        Some(i) => i,
        None => return Ok(None),
    };
    let issue_id = issue.id.clone();

    let (outgoing, incoming) = tokio::try_join!(
        find_relations(client, "docA", &issue_id),
        find_relations(client, "docB", &issue_id),
    )?;

    let mut relations = Vec::new();
    for r in outgoing {
        relations.push(json!({
            "direction": "outgoing",
            "linkedDoc": r.get("docB").cloned().unwrap_or(Value::Null),
            "association": r.get("association").cloned().unwrap_or(Value::Null),
        }));
    }
    for r in incoming {
        relations.push(json!({
            "direction": "incoming",
            "linkedDoc": r.get("docA").cloned().unwrap_or(Value::Null),
            "association": r.get("association").cloned().unwrap_or(Value::Null),
        }));
    }

    let mut value = serde_json::to_value(&issue).unwrap_or_default();
    if let Some(obj) = value.as_object_mut() {
        obj.insert("linkedRelations".into(), Value::Array(relations));
    }
    Ok(Some(value))
}

async fn find_relations(
    client: &dyn PlatformClient,
    field: &str,
    issue_id: &str,
) -> Result<Vec<Value>, String> {
    let res = client
        .find_all(
            "core:class:Relation",
            json!({ field: issue_id }),
            Some(FindOptions {
                limit: Some(500),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("error fetching {field} relations: {e}"))?;
    Ok(res
        .docs
        .into_iter()
        .map(|d| serde_json::to_value(d).unwrap_or_default())
        .collect())
}

/// Sparse update of an issue. `description_markup_ref`, when `Some`, is the
/// already-resolved MarkupBlobRef (caller is responsible for the upload via
/// the collaborator-service helpers). Returns the list of changed field
/// names.
#[allow(clippy::too_many_arguments)]
pub async fn update_issue(
    client: &dyn PlatformClient,
    identifier: &str,
    title: Option<&str>,
    description_markup_ref: Option<&str>,
    status: Option<IssueStatus>,
    priority: Option<u8>,
    component: Option<&str>,
) -> Result<Option<Vec<String>>, String> {
    let res = client
        .find_all(
            "tracker:class:Issue",
            json!({ "identifier": identifier }),
            Some(FindOptions {
                limit: Some(1),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("error fetching issue: {e}"))?;
    let issue = match res.docs.into_iter().next() {
        Some(i) => i,
        None => return Ok(None),
    };

    let mut updates = serde_json::Map::new();
    let mut changed = Vec::new();
    if let Some(t) = title {
        updates.insert("title".into(), json!(t));
        changed.push("title".to_string());
    }
    if let Some(s) = status {
        updates.insert("status".into(), json!(status_id(s)));
        changed.push("status".to_string());
    }
    if let Some(p) = priority {
        updates.insert("priority".into(), json!(p));
        changed.push("priority".to_string());
    }
    if let Some(c) = component {
        updates.insert("component".into(), json!(c));
        changed.push("component".to_string());
    }
    if let Some(blob) = description_markup_ref {
        updates.insert("description".into(), json!(blob));
        changed.push("description".to_string());
    }

    if updates.is_empty() {
        return Ok(Some(changed));
    }

    let space = issue.space.clone().unwrap_or_default();
    let attached_to = issue
        .attributes
        .get("attachedTo")
        .and_then(|v| v.as_str())
        .unwrap_or(NO_PARENT)
        .to_string();
    let attached_to_class = issue
        .attributes
        .get("attachedToClass")
        .and_then(|v| v.as_str())
        .unwrap_or("tracker:class:Issue")
        .to_string();
    let collection = issue
        .attributes
        .get("collection")
        .and_then(|v| v.as_str())
        .unwrap_or("subIssues")
        .to_string();

    client
        .update_collection(
            "tracker:class:Issue",
            &space,
            &issue.id,
            &attached_to,
            &attached_to_class,
            &collection,
            Value::Object(updates),
        )
        .await
        .map_err(|e| format!("error updating issue: {e}"))?;
    Ok(Some(changed))
}

/// Construct issue attributes object per upstream spec.
#[allow(clippy::too_many_arguments)]
pub fn build_issue_attrs(
    title: &str,
    description: &str,
    status: IssueStatus,
    priority: u8,
    seq: i64,
    project_identifier: &str,
    component: Option<&str>,
) -> Value {
    let identifier = format!("{project_identifier}-{seq}");
    let rank = format!("0|i{seq:05}:");
    json!({
        "title": title,
        "description": description,
        "status": status_id(status),
        "priority": priority,
        "kind": TASK_TYPE_ISSUE,
        "number": seq,
        "identifier": identifier,
        "rank": rank,
        "dueDate": Value::Null,
        "assignee": Value::Null,
        "milestone": Value::Null,
        "estimation": 0,
        "remainingTime": 0,
        "reportedTime": 0,
        "reports": 0,
        "relations": [],
        "parents": [],
        "childInfo": [],
        "subIssues": 0,
        "comments": 0,
        "component": component.map(Value::from).unwrap_or(Value::Null),
    })
}

/// Create an issue under the given project. Returns `(issue_id, identifier)`.
///
/// Race-resistance: bundles the sequence increment and create into one
/// `apply_if_tx` keyed on the project's current sequence. On rejection
/// (concurrent create) we re-read and retry.
#[allow(clippy::too_many_arguments)]
pub async fn create_issue_in_project(
    client: &dyn PlatformClient,
    project: &Doc,
    title: &str,
    description_markup_ref: Option<&str>,
    status: IssueStatus,
    priority: u8,
    component: Option<&str>,
    modified_by: &str,
) -> Result<(String, String), String> {
    const MAX_RETRIES: u32 = 5;
    const BACKOFF_BASE_MS: u64 = 10;

    let project_ident = project
        .attributes
        .get("identifier")
        .and_then(|v| v.as_str())
        .unwrap_or("PROJ")
        .to_string();
    let scope = format!("tracker:project:{}:issue-create", project.id);

    for attempt in 0..MAX_RETRIES {
        if attempt > 0 {
            let delay = BACKOFF_BASE_MS << (attempt - 1);
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }

        let current = client
            .find_one(
                "tracker:class:Project",
                json!({ "_id": project.id }),
                None,
            )
            .await
            .map_err(|e| format!("error reading project: {e}"))?
            .ok_or_else(|| format!("Project '{}' not found.", project.id))?;
        let seq_n = current
            .attributes
            .get("sequence")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| "Project missing 'sequence' field.".to_string())?;

        let next_seq = seq_n + 1;
        let issue_id = gen_tx_id();
        let description_field = description_markup_ref.unwrap_or("");
        let attrs = build_issue_attrs(
            title,
            description_field,
            status,
            priority,
            next_seq,
            &project_ident,
            component,
        );

        let tx_inc = tx_update_doc(
            &project.id,
            "tracker:class:Project",
            &project.id,
            modified_by,
            json!({ "$inc": { "sequence": 1 } }),
        );
        let tx_create = tx_collection_create(
            &issue_id,
            "tracker:class:Issue",
            &project.id,
            NO_PARENT,
            "tracker:class:Issue",
            "subIssues",
            modified_by,
            attrs,
        );

        let matches = vec![ApplyIfMatch {
            class: "tracker:class:Project".into(),
            query: json!({ "_id": project.id, "sequence": seq_n }),
        }];

        let result = client
            .apply_if_tx(&scope, matches, vec![], vec![tx_inc, tx_create])
            .await
            .map_err(|e| format!("error in apply_if_tx for issue create: {e}"))?;

        if result.success {
            let identifier = format!("{project_ident}-{next_seq}");
            return Ok((issue_id, identifier));
        }
    }

    Err(format!(
        "Failed to create issue in project '{}' after {} attempts (sequence contention).",
        project.id, MAX_RETRIES
    ))
}

/// Result of [`create_component`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentResult {
    Existing { id: String, label: String },
    Created { id: String, label: String },
}

/// Create a component if not already present (race-free via apply_if_tx
/// with `notMatch` predicate on `(space, label)`).
pub async fn create_component(
    client: &dyn PlatformClient,
    project: &Doc,
    label: &str,
    description: &str,
    modified_by: &str,
) -> Result<ComponentResult, String> {
    let scope = format!("tracker:project:{}:component-create:{}", project.id, label);
    let component_id = gen_tx_id();
    let attrs = json!({
        "label": label,
        "description": description,
        "lead": Value::Null,
    });
    let tx_create = tx_create_doc(
        &component_id,
        "tracker:class:Component",
        &project.id,
        modified_by,
        attrs,
    );
    let not_matches = vec![ApplyIfMatch {
        class: "tracker:class:Component".into(),
        query: json!({ "space": project.id, "label": label }),
    }];

    let result = client
        .apply_if_tx(&scope, vec![], not_matches, vec![tx_create])
        .await
        .map_err(|e| format!("error creating component: {e}"))?;

    if result.success {
        return Ok(ComponentResult::Created {
            id: component_id,
            label: label.to_string(),
        });
    }
    let dupe = client
        .find_one(
            "tracker:class:Component",
            json!({ "space": project.id, "label": label }),
            None,
        )
        .await
        .map_err(|e| format!("error resolving existing component: {e}"))?
        .ok_or_else(|| {
            format!(
                "Component create rejected by notMatch but lookup found no '{label}' in project {}",
                project.id
            )
        })?;
    Ok(ComponentResult::Existing {
        id: dupe.id,
        label: label.to_string(),
    })
}

/// Create a new tracker project.
pub async fn create_project(
    client: &dyn PlatformClient,
    name: &str,
    identifier: &str,
    description: &str,
) -> Result<String, String> {
    client
        .create_doc(
            "tracker:class:Project",
            MODEL_SPACE,
            json!({
                "name": name,
                "identifier": identifier,
                "description": description,
                "archived": false,
                "private": false,
                "members": [],
                "sequence": 0,
            }),
        )
        .await
        .map_err(|e| format!("error creating project: {e}"))
}

/// Find cards of one or all MasterTag types, optionally filtered by a title
/// substring. `kind` is a MasterTag *name* (resolved via the schema) or a
/// platform id; `None` enumerates all known MasterTag names.
pub async fn find_cards(
    client: &dyn PlatformClient,
    schema: &SchemaHandle,
    kind: Option<&str>,
    query: Option<&str>,
    limit: u64,
) -> Result<Vec<Value>, String> {
    let class_ids: Vec<(String, String)> = match kind {
        Some(k) => {
            let id = schema
                .resolve_class(k)
                .await
                .map_err(|e| format!("schema: {e}"))?;
            vec![(k.to_string(), id)]
        }
        None => {
            let (_, snap) = schema.resolved().await;
            snap.card_types
                .iter()
                .map(|(name, id)| (name.clone(), id.clone()))
                .collect()
        }
    };

    let mut cards = Vec::new();
    for (name, id) in class_ids {
        let res = client
            .find_all(
                &id,
                json!({}),
                Some(FindOptions {
                    limit: Some(limit),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| format!("error fetching cards of type {name}: {e}"))?;
        for doc in res.docs {
            let mut v = serde_json::to_value(&doc).unwrap_or_default();
            if let Some(obj) = v.as_object_mut() {
                obj.insert("_cardType".into(), json!(name.clone()));
            }
            cards.push(v);
        }
    }

    if let Some(needle) = query.map(|s| s.to_lowercase()).filter(|s| !s.is_empty()) {
        cards.retain(|c| {
            c.get("title")
                .and_then(|v| v.as_str())
                .map(|t| t.to_lowercase().contains(&needle))
                .unwrap_or(false)
        });
    }

    cards.sort_by(|a, b| {
        a.get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("title").and_then(|v| v.as_str()).unwrap_or(""))
    });
    Ok(cards)
}

/// Create a card of a given MasterTag (`kind`, resolved via schema).
pub async fn create_card(
    client: &dyn PlatformClient,
    schema: &SchemaHandle,
    kind: &str,
    space: &str,
    title: &str,
    extra_attrs: Value,
) -> Result<String, String> {
    let class = schema
        .resolve_class(kind)
        .await
        .map_err(|e| format!("schema: {e}"))?;

    let mut attrs = if let Value::Object(map) = extra_attrs {
        map
    } else {
        serde_json::Map::new()
    };
    attrs.insert("title".into(), json!(title));

    client
        .create_doc(&class, space, Value::Object(attrs))
        .await
        .map_err(|e| format!("error creating card: {e}"))
}

/// Result of [`link_issue_to_card`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkResult {
    IssueNotFound,
    AlreadyLinked { id: String },
    Created { id: String },
}

/// Link an issue to a card via a relation. `relation` is an Association
/// *name* (resolved via schema); `modified_by` is the bridge-announced
/// socialId — falls back to [`SYSTEM_ACCOUNT`] if not known.
#[allow(clippy::too_many_arguments)]
pub async fn link_issue_to_card(
    client: &dyn PlatformClient,
    schema: &SchemaHandle,
    issue_identifier: &str,
    card_id: &str,
    relation: &str,
    modified_by: Option<&str>,
) -> Result<LinkResult, String> {
    let assoc_id = schema
        .resolve_class(relation)
        .await
        .map_err(|e| format!("schema: {e}"))?;

    let issue_res = client
        .find_all(
            "tracker:class:Issue",
            json!({ "identifier": issue_identifier }),
            Some(FindOptions {
                limit: Some(1),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("error fetching issue: {e}"))?;
    let issue = match issue_res.docs.into_iter().next() {
        Some(i) => i,
        None => return Ok(LinkResult::IssueNotFound),
    };

    let scope = format!("core:relation:{}:{}:{}", issue.id, card_id, assoc_id);
    let rel_id = gen_tx_id();
    let modifier = modified_by.unwrap_or(SYSTEM_ACCOUNT);
    let attrs = json!({
        "docA": issue.id,
        "docB": card_id,
        "association": assoc_id,
    });
    let tx_create = tx_create_doc(
        &rel_id,
        "core:class:Relation",
        MODEL_SPACE,
        modifier,
        attrs,
    );
    let not_matches = vec![ApplyIfMatch {
        class: "core:class:Relation".into(),
        query: json!({
            "docA": issue.id,
            "docB": card_id,
            "association": assoc_id,
        }),
    }];

    let result = client
        .apply_if_tx(&scope, vec![], not_matches, vec![tx_create])
        .await
        .map_err(|e| format!("error creating relation: {e}"))?;

    if result.success {
        return Ok(LinkResult::Created { id: rel_id });
    }
    let existing = client
        .find_one(
            "core:class:Relation",
            json!({ "docA": issue.id, "docB": card_id, "association": assoc_id }),
            None,
        )
        .await
        .map_err(|e| format!("error resolving existing relation: {e}"))?
        .ok_or_else(|| {
            format!(
                "Relation create rejected by notMatch but lookup found none for issue {} ↔ card {card_id}",
                issue.id
            )
        })?;
    Ok(LinkResult::AlreadyLinked { id: existing.id })
}

/// Build the Discover response payload — a one-shot snapshot of the most
/// useful workspace-introspection lists.
pub async fn discover(client: &dyn PlatformClient) -> Result<Value, String> {
    let (projects, components, statuses, card_types, associations, issues) = tokio::try_join!(
        find_all_simple(client, "tracker:class:Project", json!({}), 1000),
        find_all_simple(client, "tracker:class:Component", json!({}), 1000),
        find_all_simple(client, "tracker:class:IssueStatus", json!({}), 1000),
        find_all_simple(client, "card:class:MasterTag", json!({}), 1000),
        find_all_simple(client, "core:class:Association", json!({}), 1000),
        find_all_simple(client, "tracker:class:Issue", json!({}), 1000),
    )?;

    let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    for issue in &issues {
        if let Some(s) = issue.get("status").and_then(|v| v.as_str()) {
            *counts.entry(s.to_string()).or_insert(0) += 1;
        }
    }
    let by_status: serde_json::Map<String, Value> =
        counts.into_iter().map(|(k, v)| (k, json!(v))).collect();

    Ok(json!({
        "projects": projects,
        "components": components,
        "statuses": statuses,
        "cardTypes": card_types,
        "associations": associations,
        "issueSummary": {
            "total": issues.len(),
            "byStatus": by_status,
        },
    }))
}

async fn find_all_simple(
    client: &dyn PlatformClient,
    class: &str,
    query: Value,
    limit: u64,
) -> Result<Vec<Value>, String> {
    let res = client
        .find_all(
            class,
            query,
            Some(FindOptions {
                limit: Some(limit),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("error fetching {class}: {e}"))?;
    Ok(res
        .docs
        .into_iter()
        .map(|d| serde_json::to_value(d).unwrap_or_default())
        .collect())
}

/// Parse README content into `(name, description)` (heading + first non-code paragraph).
pub fn parse_readme(content: &str) -> (String, String) {
    let mut name = String::from("Untitled");
    let mut name_seen = false;
    let mut description = String::new();
    let mut in_code = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        if !name_seen {
            if let Some(rest) = trimmed.strip_prefix("# ") {
                name = rest.trim().to_string();
                name_seen = true;
            }
            continue;
        }
        if description.is_empty() && !trimmed.is_empty() && !trimmed.starts_with('#') {
            description = trimmed.to_string();
            break;
        }
    }
    (name, description)
}

/// Derive a 4-char identifier from a project name (initials).
pub fn derive_identifier(name: &str) -> String {
    let mut letters: Vec<char> = name
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .filter(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if letters.is_empty() {
        return "PROJ".to_string();
    }
    if letters.len() > 4 {
        letters.truncate(4);
    }
    letters.into_iter().collect()
}

/// Upload `markdown_text` as a ProseMirror markup blob for
/// `(object_class, object_id, object_attr)` and return the resulting
/// `MarkupBlobRef` string, suitable for stamping into the doc's
/// description / text field via `update_doc` / `update_collection`.
#[allow(clippy::too_many_arguments)]
pub async fn upload_markup(
    collaborator: &CollaboratorClient,
    token: &SecretString,
    workspace_uuid: &str,
    object_class: &str,
    object_id: &str,
    object_attr: &str,
    markdown_text: &str,
) -> Result<String, String> {
    let pm_json = markdown_to_prosemirror_json(markdown_text);
    collaborator
        .create_markup(
            token,
            workspace_uuid,
            object_class,
            object_id,
            object_attr,
            &pm_json,
        )
        .await
        .map_err(format_collab_error)
}

/// Fetch a markup blob and return it as markdown.
#[allow(clippy::too_many_arguments)]
pub async fn fetch_markup(
    collaborator: &CollaboratorClient,
    token: &SecretString,
    workspace_uuid: &str,
    object_class: &str,
    object_id: &str,
    object_attr: &str,
    source_ref: Option<&str>,
) -> Result<String, String> {
    let pm_json = collaborator
        .get_markup(
            token,
            workspace_uuid,
            object_class,
            object_id,
            object_attr,
            source_ref,
        )
        .await
        .map_err(format_collab_error)?;
    Ok(prosemirror_to_markdown(&pm_json))
}

fn format_collab_error(e: CollaboratorError) -> String {
    format!("collaborator error: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use huly_client::client::MockPlatformClient;
    use huly_common::types::FindResult;

    fn doc(id: &str, attrs: Value) -> Doc {
        Doc {
            id: id.into(),
            class: "x".into(),
            space: Some("sp".into()),
            modified_on: 0,
            modified_by: None,
            attributes: attrs,
        }
    }

    fn find_result(docs: Vec<Doc>) -> FindResult {
        let total = docs.len() as i64;
        FindResult {
            docs,
            total,
            lookup_map: None,
        }
    }

    #[test]
    fn parse_readme_extracts_name_and_description() {
        let md = "# My Project\n\nA helpful product.\n\n## Details\n";
        let (name, desc) = parse_readme(md);
        assert_eq!(name, "My Project");
        assert_eq!(desc, "A helpful product.");
    }

    #[test]
    fn parse_readme_skips_code_blocks_for_description() {
        let md = "# Title\n\n```\ncode\n```\n\nReal description.\n";
        let (name, desc) = parse_readme(md);
        assert_eq!(name, "Title");
        assert_eq!(desc, "Real description.");
    }

    #[test]
    fn parse_readme_defaults_when_no_heading() {
        let (name, desc) = parse_readme("just text");
        assert_eq!(name, "Untitled");
        assert_eq!(desc, "");
    }

    #[test]
    fn derive_identifier_uses_initials() {
        assert_eq!(derive_identifier("Muhasebot Project"), "MP");
        assert_eq!(derive_identifier("Alpha Beta Gamma Delta Epsilon"), "ABGD");
        assert_eq!(derive_identifier(""), "PROJ");
        assert_eq!(derive_identifier("singleword"), "S");
    }

    #[test]
    fn build_issue_attrs_uses_seq_and_identifier() {
        let v = build_issue_attrs(
            "Hello",
            "desc",
            IssueStatus::Todo,
            3,
            7,
            "MUH",
            Some("comp-1"),
        );
        assert_eq!(v["title"], "Hello");
        assert_eq!(v["status"], "tracker:status:Todo");
        assert_eq!(v["number"], 7);
        assert_eq!(v["identifier"], "MUH-7");
        assert_eq!(v["rank"], "0|i00007:");
        assert_eq!(v["component"], "comp-1");
        assert_eq!(v["priority"], 3);
        assert_eq!(v["kind"], TASK_TYPE_ISSUE);
        assert_eq!(v["estimation"], 0);
    }

    #[test]
    fn build_issue_attrs_null_component_when_absent() {
        let v = build_issue_attrs("h", "", IssueStatus::Backlog, 0, 1, "X", None);
        assert!(v["component"].is_null());
    }

    #[tokio::test]
    async fn resolve_project_returns_only_project_when_unique() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all().returning(|_, _, _| {
            Box::pin(async {
                Ok(find_result(vec![doc(
                    "p1",
                    json!({"identifier": "MUH"}),
                )]))
            })
        });
        let p = resolve_project(&mock, None).await.unwrap();
        assert_eq!(p.id, "p1");
    }

    #[tokio::test]
    async fn resolve_project_errors_on_multiple_without_arg() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all().returning(|_, _, _| {
            Box::pin(async {
                Ok(find_result(vec![
                    doc("p1", json!({"identifier": "MUH"})),
                    doc("p2", json!({"identifier": "OPS"})),
                ]))
            })
        });
        let err = resolve_project(&mock, None).await.unwrap_err();
        assert!(err.contains("Multiple projects"));
    }

    #[tokio::test]
    async fn resolve_project_matches_by_identifier_when_provided() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all().returning(|_, _, _| {
            Box::pin(async {
                Ok(find_result(vec![
                    doc("p1", json!({"identifier": "MUH"})),
                    doc("p2", json!({"identifier": "OPS"})),
                ]))
            })
        });
        let p = resolve_project(&mock, Some("OPS")).await.unwrap();
        assert_eq!(p.id, "p2");
    }

    #[tokio::test]
    async fn resolve_project_errors_when_none_present() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all()
            .returning(|_, _, _| Box::pin(async { Ok(find_result(vec![])) }));
        let err = resolve_project(&mock, None).await.unwrap_err();
        assert!(err.contains("No tracker projects"));
    }

    #[tokio::test]
    async fn find_issues_filters_and_sorts_by_number() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all().returning(|_, _, _| {
            Box::pin(async {
                Ok(find_result(vec![
                    doc(
                        "i2",
                        json!({"number": 2, "title": "T2", "identifier": "X-2", "status": "tracker:status:Todo", "priority": 1}),
                    ),
                    doc(
                        "i1",
                        json!({"number": 1, "title": "T1", "identifier": "X-1", "status": "tracker:status:Todo"}),
                    ),
                    // Doc without `number` is dropped.
                    doc("i3", json!({"title": "ghost"})),
                ]))
            })
        });
        let out = find_issues(&mock, None, None, None, 100).await.unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0]["identifier"], "X-1");
        assert_eq!(out[1]["identifier"], "X-2");
        assert_eq!(out[1]["priorityName"], "Urgent");
    }

    #[tokio::test]
    async fn find_issues_propagates_client_error() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all().returning(|_, _, _| {
            Box::pin(async {
                Err(huly_client::client::ClientError::Rpc {
                    code: "401".into(),
                    message: "denied".into(),
                })
            })
        });
        let err = find_issues(&mock, None, None, None, 100).await.unwrap_err();
        assert!(err.contains("denied"), "msg: {err}");
    }

    #[tokio::test]
    async fn get_issue_returns_none_when_missing() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all()
            .returning(|_, _, _| Box::pin(async { Ok(find_result(vec![])) }));
        let r = get_issue(&mock, "X-1").await.unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn update_issue_returns_none_when_missing() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all()
            .returning(|_, _, _| Box::pin(async { Ok(find_result(vec![])) }));
        let r = update_issue(&mock, "X-1", Some("t"), None, None, None, None)
            .await
            .unwrap();
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn update_issue_calls_update_collection_with_changes() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all().returning(|_, _, _| {
            Box::pin(async {
                Ok(find_result(vec![doc(
                    "issue-1",
                    json!({"identifier": "X-1", "number": 1}),
                )]))
            })
        });
        mock.expect_update_collection()
            .withf(|class, _space, id, _at, _atc, _col, ops| {
                class == "tracker:class:Issue"
                    && id == "issue-1"
                    && ops["title"] == "renamed"
                    && ops["priority"] == 2
            })
            .returning(|_, _, _, _, _, _, _| {
                Box::pin(async {
                    Ok(huly_common::types::TxResult {
                        success: true,
                        id: None,
                    })
                })
            });
        let changed = update_issue(
            &mock,
            "X-1",
            Some("renamed"),
            None,
            None,
            Some(2),
            None,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(changed.contains(&"title".to_string()));
        assert!(changed.contains(&"priority".to_string()));
    }

    #[tokio::test]
    async fn create_component_returns_created_on_success() {
        let mut mock = MockPlatformClient::new();
        mock.expect_apply_if_tx().returning(|_, _, _, _| {
            Box::pin(async {
                Ok(huly_client::client::ApplyIfResult {
                    success: true,
                    server_time: 1,
                })
            })
        });
        let p = doc(
            "proj-1",
            json!({"identifier": "MUH", "sequence": 0}),
        );
        let r = create_component(&mock, &p, "Frontend", "ui", "soc-1")
            .await
            .unwrap();
        match r {
            ComponentResult::Created { label, .. } => assert_eq!(label, "Frontend"),
            other => panic!("expected Created, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_component_returns_existing_on_dup() {
        let mut mock = MockPlatformClient::new();
        mock.expect_apply_if_tx().returning(|_, _, _, _| {
            Box::pin(async {
                Ok(huly_client::client::ApplyIfResult {
                    success: false,
                    server_time: 0,
                })
            })
        });
        // The find_one fall-back should resolve the dupe.
        mock.expect_find_one().returning(|_, _, _| {
            Box::pin(async {
                Ok(Some(doc(
                    "comp-existing",
                    json!({"label": "Frontend"}),
                )))
            })
        });
        let p = doc("proj-1", json!({}));
        let r = create_component(&mock, &p, "Frontend", "ui", "soc-1")
            .await
            .unwrap();
        match r {
            ComponentResult::Existing { id, .. } => assert_eq!(id, "comp-existing"),
            other => panic!("expected Existing, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn link_issue_to_card_unknown_relation_errors_with_schema_msg() {
        let mock = MockPlatformClient::new();
        let schema = SchemaHandle::new();
        let err =
            link_issue_to_card(&mock, &schema, "X-1", "card-1", "module", None)
                .await
                .unwrap_err();
        assert!(err.to_lowercase().contains("schema"), "msg: {err}");
    }

    #[tokio::test]
    async fn link_issue_to_card_returns_issue_not_found() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all()
            .returning(|_, _, _| Box::pin(async { Ok(find_result(vec![])) }));
        // Pre-seed the schema with the relation name → id.
        let schema = SchemaHandle::with_card_type_names_for_tests(&[]);
        // We need an Association map populated; install_for_tests routes.
        let mut ws = huly_common::announcement::WorkspaceSchema::default();
        ws.associations
            .insert("module".into(), "core:assoc:module".into());
        let schema = SchemaHandle::install_for_tests(ws);
        let r = link_issue_to_card(&mock, &schema, "X-1", "card-1", "module", None)
            .await
            .unwrap();
        assert!(matches!(r, LinkResult::IssueNotFound));
    }

    #[tokio::test]
    async fn create_project_returns_id() {
        let mut mock = MockPlatformClient::new();
        mock.expect_create_doc()
            .withf(|class, space, attrs| {
                class == "tracker:class:Project"
                    && space == MODEL_SPACE
                    && attrs["identifier"] == "MUH"
                    && attrs["sequence"] == 0
            })
            .returning(|_, _, _| Box::pin(async { Ok("proj-new".into()) }));
        let id = create_project(&mock, "Muhasebot", "MUH", "ledger")
            .await
            .unwrap();
        assert_eq!(id, "proj-new");
    }

    #[tokio::test]
    async fn create_project_propagates_client_error() {
        let mut mock = MockPlatformClient::new();
        mock.expect_create_doc().returning(|_, _, _| {
            Box::pin(async {
                Err(huly_client::client::ClientError::Rpc {
                    code: "403".into(),
                    message: "denied".into(),
                })
            })
        });
        let err = create_project(&mock, "n", "I", "")
            .await
            .unwrap_err();
        assert!(err.contains("denied"), "msg: {err}");
    }

    #[tokio::test]
    async fn create_card_resolves_kind_and_creates_doc() {
        let mut mock = MockPlatformClient::new();
        mock.expect_create_doc()
            .withf(|class, space, attrs| {
                class == "card:tag:Module" && space == "sp" && attrs["title"] == "Hello"
            })
            .returning(|_, _, _| Box::pin(async { Ok("card-new".into()) }));
        let mut ws = huly_common::announcement::WorkspaceSchema::default();
        ws.card_types
            .insert("Module Spec".into(), "card:tag:Module".into());
        let schema = SchemaHandle::install_for_tests(ws);
        let id = create_card(&mock, &schema, "Module Spec", "sp", "Hello", json!({}))
            .await
            .unwrap();
        assert_eq!(id, "card-new");
    }

    #[tokio::test]
    async fn create_card_unknown_kind_returns_schema_error() {
        let mock = MockPlatformClient::new();
        let schema = SchemaHandle::new();
        let err = create_card(&mock, &schema, "Module Spec", "sp", "h", json!({}))
            .await
            .unwrap_err();
        assert!(err.to_lowercase().contains("schema"), "msg: {err}");
    }

    #[tokio::test]
    async fn upload_markup_round_trip_against_wiremock() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/rpc/.+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": { "description": "blob-ref-1" }
            })))
            .mount(&server)
            .await;
        let collab = CollaboratorClient::new(&server.uri());
        let token = SecretString::from("jwt");
        let r = upload_markup(
            &collab,
            &token,
            "uuid-x",
            "tracker:class:Issue",
            "issue-1",
            "description",
            "# Hello",
        )
        .await
        .unwrap();
        assert_eq!(r, "blob-ref-1");
    }

    #[tokio::test]
    async fn upload_markup_propagates_collaborator_error() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path_regex(r"^/rpc/.+"))
            .respond_with(ResponseTemplate::new(403).set_body_string("denied"))
            .mount(&server)
            .await;
        let collab = CollaboratorClient::new(&server.uri());
        let token = SecretString::from("jwt");
        let err = upload_markup(
            &collab,
            &token,
            "uuid-x",
            "tracker:class:Issue",
            "issue-1",
            "description",
            "x",
        )
        .await
        .unwrap_err();
        assert!(err.to_lowercase().contains("collaborator"), "msg: {err}");
    }

    #[tokio::test]
    async fn fetch_markup_returns_markdown_round_trip() {
        use wiremock::matchers::{method, path_regex};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        // ProseMirror JSON for "Hello world\n".
        let pm = serde_json::json!({
            "type": "doc",
            "content": [
                {
                    "type": "paragraph",
                    "content": [{ "type": "text", "text": "Hello world" }]
                }
            ]
        });
        Mock::given(method("POST"))
            .and(path_regex(r"^/rpc/.+"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "content": { "description": pm }
            })))
            .mount(&server)
            .await;
        let collab = CollaboratorClient::new(&server.uri());
        let token = SecretString::from("jwt");
        let md = fetch_markup(
            &collab,
            &token,
            "uuid-x",
            "tracker:class:Issue",
            "issue-1",
            "description",
            None,
        )
        .await
        .unwrap();
        assert!(md.contains("Hello world"), "got: {md}");
    }

    #[tokio::test]
    async fn discover_aggregates_six_lists() {
        let mut mock = MockPlatformClient::new();
        mock.expect_find_all().returning(|class, _, _| {
            let class = class.to_string();
            Box::pin(async move {
                if class == "tracker:class:Issue" {
                    Ok(find_result(vec![doc(
                        "i1",
                        json!({"status": "tracker:status:Todo"}),
                    )]))
                } else {
                    Ok(find_result(vec![]))
                }
            })
        });
        let v = discover(&mock).await.unwrap();
        assert_eq!(v["projects"], json!([]));
        assert_eq!(v["issueSummary"]["total"], 1);
        assert_eq!(v["issueSummary"]["byStatus"]["tracker:status:Todo"], 1);
    }
}
