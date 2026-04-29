//! Domain-specific MCP tools (Phase 5).
//!
//! These tools wrap the bridge admin API with workspace + project resolution
//! and Huly-specific schemas (issues, components, cards, projects, relations).
//!
//! Workspace resolution rule: every tool accepts an optional `workspace`. If
//! omitted and exactly one workspace is registered, that workspace is used.
//! If omitted and zero or multiple workspaces are present, the tool returns
//! an error message.

use crate::bridge_client::BridgeHttpClient;
use crate::discovery::BridgeRegistry;
use crate::mcp::catalog::{
    IssueStatus, MODEL_SPACE, NO_PARENT, TASK_TYPE_ISSUE, priority_name, status_id,
};
use crate::mcp::schema_cache::SchemaCache;
use crate::txcud::{gen_tx_id, tx_collection_create, tx_create_doc, tx_update_doc};
use huly_common::api::ApplyIfMatch;
use huly_common::types::{Doc, FindOptions};
use serde_json::{Value, json};

/// Workspace resolution: explicit name OR sole-registered workspace.
pub async fn resolve_workspace(
    registry: &BridgeRegistry,
    requested: Option<&str>,
) -> Result<String, String> {
    if let Some(ws) = requested {
        return Ok(ws.to_string());
    }
    let all = registry.list_workspaces().await;
    match all.len() {
        0 => Err("No workspaces discovered. Use huly_list_workspaces to verify.".to_string()),
        1 => Ok(all[0].workspace.clone()),
        _ => {
            let names: Vec<&str> = all.iter().map(|a| a.workspace.as_str()).collect();
            Err(format!(
                "Multiple workspaces present ({}); pass workspace explicitly.",
                names.join(", ")
            ))
        }
    }
}

/// Resolve a Tracker project by user input (`_id` OR `identifier` like "MUH").
/// If `requested` is None, returns the only project in the workspace, or errors
/// if zero or many.
pub async fn resolve_project(
    http: &BridgeHttpClient,
    proxy_url: &str,
    requested: Option<&str>,
) -> Result<Doc, String> {
    let result = http
        .find(
            proxy_url,
            "tracker:class:Project",
            json!({}),
            Some(FindOptions {
                limit: Some(200),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("Bridge error fetching projects: {e}"))?;

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
            "Project '{}' not found (matched neither _id nor identifier).",
            needle
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

/// Render a card list as a compact text block.
pub fn render_card_summaries(cards: &[Value]) -> String {
    let serialized = serde_json::to_string_pretty(cards).unwrap_or_else(|e| format!("{e}"));
    format!("Found {} cards:\n{}", cards.len(), serialized)
}

/// Render an issue list as a compact text block.
pub fn render_issue_summaries(issues: &[Value]) -> String {
    let serialized = serde_json::to_string_pretty(issues).unwrap_or_else(|e| format!("{e}"));
    format!("Found {} issues:\n{}", issues.len(), serialized)
}

/// Build the Discover response payload.
pub async fn discover(
    http: &BridgeHttpClient,
    proxy_url: &str,
) -> Result<Value, String> {
    let (projects, components, statuses, card_types, associations, issues) = tokio::try_join!(
        find_all_simple(http, proxy_url, "tracker:class:Project", json!({}), 1000),
        find_all_simple(http, proxy_url, "tracker:class:Component", json!({}), 1000),
        find_all_simple(http, proxy_url, "tracker:class:IssueStatus", json!({}), 1000),
        find_all_simple(http, proxy_url, "card:class:MasterTag", json!({}), 1000),
        find_all_simple(http, proxy_url, "core:class:Association", json!({}), 1000),
        find_all_simple(http, proxy_url, "tracker:class:Issue", json!({}), 1000),
    )?;

    // Status counts.
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
    http: &BridgeHttpClient,
    proxy_url: &str,
    class: &str,
    query: Value,
    limit: u64,
) -> Result<Vec<Value>, String> {
    let res = http
        .find(
            proxy_url,
            class,
            query,
            Some(FindOptions {
                limit: Some(limit),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("Bridge error fetching {class}: {e}"))?;
    Ok(res
        .docs
        .into_iter()
        .map(|d| serde_json::to_value(d).unwrap_or_default())
        .collect())
}

/// Find cards of one or all types, optionally filtered by query substring.
///
/// `kind`:
/// - `Some(name)` — fetch cards of that MasterTag name. The bridge resolves
///   the name → workspace-local id before hitting the transactor; an
///   unknown name surfaces as a 422 from the bridge.
/// - `None` — enumerate all MasterTag names known in the workspace
///   schema cache and union the results. If the schema cache is empty
///   (workspace unknown / not yet fetched), returns an empty list rather
///   than guessing.
#[allow(clippy::too_many_arguments)]
pub async fn find_cards(
    http: &BridgeHttpClient,
    proxy_url: &str,
    schema: &SchemaCache,
    registry: &BridgeRegistry,
    workspace: &str,
    kind: Option<&str>,
    query: Option<&str>,
    limit: u64,
) -> Result<Vec<Value>, String> {
    let names: Vec<String> = match kind {
        Some(name) => vec![name.to_string()],
        None => schema
            .get(workspace, registry)
            .await
            .card_types
            .keys()
            .cloned()
            .collect(),
    };

    let mut cards = Vec::new();
    for name in names {
        let res = http
            .find(
                proxy_url,
                &name,
                json!({}),
                Some(FindOptions {
                    limit: Some(limit),
                    ..Default::default()
                }),
            )
            .await
            .map_err(|e| format!("Bridge error fetching cards of type {name}: {e}"))?;
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

/// Find issues with optional filters.
pub async fn find_issues(
    http: &BridgeHttpClient,
    proxy_url: &str,
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

    let res = http
        .find(
            proxy_url,
            "tracker:class:Issue",
            Value::Object(filter),
            Some(FindOptions {
                limit: Some(limit),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("Bridge error fetching issues: {e}"))?;

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
            // Tag with priority label for readability.
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

/// Get a single issue by identifier, plus relations.
pub async fn get_issue(
    http: &BridgeHttpClient,
    proxy_url: &str,
    identifier: &str,
) -> Result<Option<Value>, String> {
    let res = http
        .find(
            proxy_url,
            "tracker:class:Issue",
            json!({ "identifier": identifier }),
            Some(FindOptions {
                limit: Some(1),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("Bridge error fetching issue: {e}"))?;
    let issue = match res.docs.into_iter().next() {
        Some(i) => i,
        None => return Ok(None),
    };

    let issue_id = issue.id.clone();

    let (out, inc) = tokio::try_join!(
        find_all_simple(
            http,
            proxy_url,
            "core:class:Relation",
            json!({ "docA": issue_id }),
            500
        ),
        find_all_simple(
            http,
            proxy_url,
            "core:class:Relation",
            json!({ "docB": issue_id }),
            500
        ),
    )?;

    let mut relations = Vec::new();
    for r in out {
        relations.push(json!({
            "direction": "outgoing",
            "linkedDoc": r.get("docB").cloned().unwrap_or(Value::Null),
            "association": r.get("association").cloned().unwrap_or(Value::Null),
        }));
    }
    for r in inc {
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

/// Construct issue attributes object per upstream spec.
///
/// `description` should be `""` (empty placeholder) when the caller will
/// subsequently upload a markup blob and `updateDoc` the ref back.
/// Passing a non-empty plain string is only acceptable for
/// non-collaborator-enabled deployments.
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
    let identifier = format!("{}-{}", project_identifier, seq);
    let rank = format!("0|i{:05}:", seq);
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

/// Create an issue under the given project. Returns (issue_id, identifier).
///
/// Race-resistance (Tier B / QA #24):
/// Bundles the sequence increment and issue creation into a single server-serialized
/// `TxApplyIf` scope.  The `match` predicate pins the project at its current
/// sequence N; the server rejects the scope if any other caller has already landed
/// an increment.  On rejection (`success: false`) we re-read and retry up to
/// `MAX_RETRIES` times with exponential backoff, closing the contiguity gap
/// identified in the Tier B follow-up TODO.
///
/// If `description_markdown` is `Some`, the issue is created with an empty
/// `description` placeholder, then the markdown is uploaded via `/api/v1/upload-markup`
/// and the returned `MarkupBlobRef` is written back via `updateDoc`.  If markup
/// upload fails the issue still exists with an empty description — a warning is
/// logged with the issue ID so an operator can retry the upload manually.
#[allow(clippy::too_many_arguments)]
pub async fn create_issue_in_project(
    http: &BridgeHttpClient,
    proxy_url: &str,
    project: &Doc,
    title: &str,
    description_markdown: Option<&str>,
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
            let delay = BACKOFF_BASE_MS << (attempt - 1); // 10, 20, 40, 80 ms
            tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
        }

        // Read the current project to learn sequence N.
        let current = http
            .find_one(
                proxy_url,
                "tracker:class:Project",
                json!({ "_id": project.id }),
            )
            .await
            .map_err(|e| format!("Bridge error reading project: {e}"))?
            .ok_or_else(|| format!("Project '{}' not found.", project.id))?;

        let seq_n = current
            .attributes
            .get("sequence")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| "Project missing 'sequence' field.".to_string())?;

        let next_seq = seq_n + 1;
        let issue_id = gen_tx_id(); // pre-assign so we know the id on success
        // Pass empty description placeholder; markup blob is uploaded after creation.
        let attrs = build_issue_attrs(
            title,
            "",
            status,
            priority,
            next_seq,
            &project_ident,
            component,
        );

        // TxUpdateDoc: $inc sequence by 1
        let tx_inc = tx_update_doc(
            &project.id,
            "tracker:class:Project",
            &project.id,
            modified_by,
            json!({ "$inc": { "sequence": 1 } }),
        );

        // TxCreateDoc (collection CUD): create the Issue
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

        let result = http
            .apply_if(proxy_url, &scope, matches, vec![], vec![tx_inc, tx_create])
            .await
            .map_err(|e| format!("Bridge error in apply_if: {e}"))?;

        if result.success {
            let identifier = format!("{}-{}", project_ident, next_seq);
            // Upload markdown as ProseMirror markup blob and write ref back.
            if let Some(md) = description_markdown.filter(|md| !md.is_empty()) {
                match http
                    .upload_markup(proxy_url, "tracker:class:Issue", &issue_id, "description", md)
                    .await
                {
                    Ok(markup_ref) => {
                        // Write the MarkupBlobRef into the issue's description field.
                        if let Err(e) = http
                            .update(
                                proxy_url,
                                "tracker:class:Issue",
                                &project.id,
                                &issue_id,
                                json!({ "description": markup_ref }),
                            )
                            .await
                        {
                            tracing::warn!(
                                issue_id = %issue_id,
                                error = %e,
                                "markup ref update failed after upload; issue exists with empty description"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            issue_id = %issue_id,
                            error = %e,
                            "markup upload failed; issue exists with empty description — retry upload_markup manually"
                        );
                    }
                }
            }
            return Ok((issue_id, identifier));
        }
        // success: false — another caller won the scope race; retry
    }

    Err(format!(
        "Failed to create issue in project '{}' after {} attempts (sequence contention).",
        project.id, MAX_RETRIES
    ))
}

/// Update issue sparsely. Returns list of changed field names.
#[allow(clippy::too_many_arguments)]
pub async fn update_issue(
    http: &BridgeHttpClient,
    proxy_url: &str,
    identifier: &str,
    title: Option<&str>,
    description: Option<&str>,
    status: Option<IssueStatus>,
    priority: Option<u8>,
    component: Option<&str>,
) -> Result<Option<Vec<String>>, String> {
    let res = http
        .find(
            proxy_url,
            "tracker:class:Issue",
            json!({ "identifier": identifier }),
            Some(FindOptions {
                limit: Some(1),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("Bridge error fetching issue: {e}"))?;
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
    if description.is_some() {
        // Description is handled separately via markup upload after findOne resolves the issue id.
        changed.push("description".to_string());
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

    // Upload description markup if provided (before update_collection so the
    // ref is available to include in the updates map).
    if let Some(md) = description.filter(|md| !md.is_empty()) {
        match http
            .upload_markup(proxy_url, "tracker:class:Issue", &issue.id, "description", md)
            .await
        {
            Ok(markup_ref) => {
                updates.insert("description".into(), json!(markup_ref));
            }
            Err(e) => {
                tracing::warn!(
                    issue_id = %issue.id,
                    error = %e,
                    "markup upload failed for description update; skipping description change"
                );
                changed.retain(|f| f != "description");
            }
        }
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

    http.update_collection(
        proxy_url,
        "tracker:class:Issue",
        &space,
        &issue.id,
        &attached_to,
        &attached_to_class,
        &collection,
        Value::Object(updates),
    )
    .await
    .map_err(|e| format!("Bridge error updating issue: {e}"))?;
    Ok(Some(changed))
}

/// Create a component if not present.
///
/// Race-resistance: rather than the legacy find-then-create dance, this issues a
/// single `apply_if_tx` whose `notMatch` predicate asserts that no Component with
/// the same `(space, label)` exists.  If the precondition holds, the create
/// commits atomically; otherwise the server returns `success: false` and we
/// re-read to return the duplicate's id as `Existing`.
///
/// Closes the previous TOCTOU window where two concurrent callers could each
/// see "no dupe" and both create.
pub async fn create_component(
    http: &BridgeHttpClient,
    proxy_url: &str,
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

    let result = http
        .apply_if(proxy_url, &scope, vec![], not_matches, vec![tx_create])
        .await
        .map_err(|e| format!("Bridge error creating component: {e}"))?;

    if result.success {
        return Ok(ComponentResult::Created {
            id: component_id,
            label: label.to_string(),
        });
    }

    // success: false → another doc with the same label exists. Resolve its id.
    let dupe = http
        .find_one(
            proxy_url,
            "tracker:class:Component",
            json!({ "space": project.id, "label": label }),
        )
        .await
        .map_err(|e| format!("Bridge error resolving existing component: {e}"))?
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComponentResult {
    Existing { id: String, label: String },
    Created { id: String, label: String },
}

/// Link an issue to a card via a relation.
///
/// `relation` is a free-form Association name (e.g. "module"). We resolve
/// it to a workspace-local Association `_id` from the schema cache before
/// stamping it into the relation's `association` attribute. Unknown names
/// short-circuit before any tx so the caller gets a useful error.
///
/// Race-resistance: the dup-check + create are bundled into a single
/// `apply_if_tx` whose `notMatch` predicate asserts that no Relation already
/// links `(docA=issue, docB=card, association=rel)`. On rejection we resolve
/// the existing Relation's id and return `AlreadyLinked`.
#[allow(clippy::too_many_arguments)]
pub async fn link_issue_to_card(
    http: &BridgeHttpClient,
    proxy_url: &str,
    schema: &SchemaCache,
    registry: &BridgeRegistry,
    workspace: &str,
    issue_identifier: &str,
    card_id: &str,
    relation: &str,
    modified_by: &str,
) -> Result<LinkResult, String> {
    let assoc_id = schema
        .get(workspace, registry)
        .await
        .associations
        .get(relation)
        .cloned()
        .ok_or_else(|| {
            format!(
                "Unknown relation '{relation}' in workspace '{workspace}'. Use huly_discover to list known associations."
            )
        })?;

    let issue_res = http
        .find(
            proxy_url,
            "tracker:class:Issue",
            json!({ "identifier": issue_identifier }),
            Some(FindOptions {
                limit: Some(1),
                ..Default::default()
            }),
        )
        .await
        .map_err(|e| format!("Bridge error fetching issue: {e}"))?;
    let issue = match issue_res.docs.into_iter().next() {
        Some(i) => i,
        None => return Ok(LinkResult::IssueNotFound),
    };

    let scope = format!("core:relation:{}:{}:{}", issue.id, card_id, assoc_id);
    let rel_id = gen_tx_id();
    let attrs = json!({
        "docA": issue.id,
        "docB": card_id,
        "association": assoc_id,
    });
    let tx_create = tx_create_doc(&rel_id, "core:class:Relation", MODEL_SPACE, modified_by, attrs);
    let not_matches = vec![ApplyIfMatch {
        class: "core:class:Relation".into(),
        query: json!({
            "docA": issue.id,
            "docB": card_id,
            "association": assoc_id,
        }),
    }];

    let result = http
        .apply_if(proxy_url, &scope, vec![], not_matches, vec![tx_create])
        .await
        .map_err(|e| format!("Bridge error creating relation: {e}"))?;

    if result.success {
        return Ok(LinkResult::Created { id: rel_id });
    }

    let existing = http
        .find_one(
            proxy_url,
            "core:class:Relation",
            json!({ "docA": issue.id, "docB": card_id, "association": assoc_id }),
        )
        .await
        .map_err(|e| format!("Bridge error resolving existing relation: {e}"))?
        .ok_or_else(|| {
            format!(
                "Relation create rejected by notMatch but lookup found none for issue {} ↔ card {card_id}",
                issue.id
            )
        })?;
    Ok(LinkResult::AlreadyLinked { id: existing.id })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkResult {
    IssueNotFound,
    AlreadyLinked { id: String },
    Created { id: String },
}

/// Parse README content into (name, description).
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

/// Derive a 4-char identifier from a name (first letter of each significant word).
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

/// Create a new tracker project.
pub async fn create_project(
    http: &BridgeHttpClient,
    proxy_url: &str,
    name: &str,
    identifier: &str,
    description: &str,
) -> Result<String, String> {
    http.create(
        proxy_url,
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
    .map_err(|e| format!("Bridge error creating project: {e}"))
}

#[allow(dead_code)]
pub fn no_parent() -> &'static str {
    NO_PARENT
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
