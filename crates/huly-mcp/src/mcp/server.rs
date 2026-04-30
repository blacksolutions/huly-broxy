//! MCP server.
//!
//! Post-P4 every tool call resolves a per-workspace [`PlatformClient`] via
//! [`HulyClientFactory`] (which talks to the bridge JWT broker on
//! `huly.bridge.mint`). The bridge HTTP gateway and bridge-discovery NATS
//! subjects are gone — this file no longer touches them.
//!
//! Only the basic CRUD tools (`huly_find`, `huly_get`, `huly_create`,
//! `huly_update`, `huly_delete`) plus `huly_list_workspaces` and
//! `huly_status` are wired through the new path. The richer tracker /
//! markup / sync tools that depended on the bridge's `/api/v1/*` surface
//! return a structured TODO error pending P5 — they will be re-implemented
//! against `huly_client::collaborator` and the new schema cache.

use crate::audit::{AuditPublisher, digest_json, new_request_id};
use crate::huly_client_factory::{FactoryError, HulyClientFactory};
use crate::mcp::catalog::IssueStatus;
use crate::mcp::tools;
use crate::sync::SyncRunner;
use huly_client::accounts::AccountsClient;
use huly_client::client::{ClientError, PlatformClient};
use huly_common::mcp_subjects::ToolCompletedResult;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListWorkspacesParams {
    /// Optional: a workspace slug whose minted account-service JWT is used
    /// to query the account service. Omit to require operator config (single
    /// tenant) or to error when none is registered yet.
    pub workspace: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StatusParams {
    pub workspace: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FindParams {
    pub workspace: String,
    pub class: String,
    #[serde(default)]
    pub query: serde_json::Value,
    pub limit: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetParams {
    pub workspace: String,
    pub class: String,
    #[serde(default)]
    pub query: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateParams {
    pub workspace: String,
    pub class: String,
    pub space: String,
    pub attributes: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateParams {
    pub workspace: String,
    pub class: String,
    pub space: String,
    pub id: String,
    pub operations: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeleteParams {
    pub workspace: String,
    pub class: String,
    pub space: String,
    pub id: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FindIssuesParams {
    pub workspace: String,
    #[serde(default)]
    pub component: Option<String>,
    #[serde(default)]
    pub status: Option<IssueStatus>,
    /// Title substring (case-insensitive).
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetIssueParams {
    pub workspace: String,
    /// Project-prefixed identifier, e.g. `MUH-42`.
    pub identifier: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateIssueParams {
    pub workspace: String,
    pub identifier: String,
    #[serde(default)]
    pub title: Option<String>,
    /// Already-resolved MarkupBlobRef (call `huly_upload_markup` first).
    #[serde(default)]
    pub description_ref: Option<String>,
    #[serde(default)]
    pub status: Option<IssueStatus>,
    #[serde(default)]
    pub priority: Option<u8>,
    #[serde(default)]
    pub component: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateIssueParams {
    pub workspace: String,
    /// Project _id OR identifier (e.g. "MUH"). Optional only when the
    /// workspace has exactly one project.
    #[serde(default)]
    pub project: Option<String>,
    pub title: String,
    /// Already-resolved MarkupBlobRef (call `huly_upload_markup` first).
    #[serde(default)]
    pub description_ref: Option<String>,
    #[serde(default)]
    pub status: Option<IssueStatus>,
    #[serde(default)]
    pub priority: Option<u8>,
    #[serde(default)]
    pub component: Option<String>,
    /// Optional override for the `modifiedBy` PersonId. Defaults to
    /// `core:account:System` (only valid against bridges that haven't
    /// upgraded; production should pass the announced socialId).
    #[serde(default)]
    pub modified_by: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateComponentParams {
    pub workspace: String,
    #[serde(default)]
    pub project: Option<String>,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub modified_by: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateProjectParams {
    pub workspace: String,
    pub name: String,
    /// 1–4 letter project prefix (e.g. "MUH"). When omitted, derived from
    /// the name's initials.
    #[serde(default)]
    pub identifier: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FindCardsParams {
    pub workspace: String,
    /// MasterTag *name* (resolved via the per-workspace schema). Omit to
    /// enumerate all known card types.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub query: Option<String>,
    #[serde(default)]
    pub limit: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateCardParams {
    pub workspace: String,
    /// MasterTag *name* (resolved via the per-workspace schema).
    pub kind: String,
    pub space: String,
    pub title: String,
    /// Extra attributes merged into the create payload (must be a JSON object).
    #[serde(default)]
    pub attributes: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LinkIssueToCardParams {
    pub workspace: String,
    pub issue_identifier: String,
    pub card_id: String,
    /// Association *name* (e.g. "module"). Resolved via the per-workspace schema.
    pub relation: String,
    #[serde(default)]
    pub modified_by: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DiscoverParams {
    pub workspace: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UploadMarkupParams {
    pub workspace: String,
    pub object_class: String,
    pub object_id: String,
    pub object_attr: String,
    /// Plain markdown text. Converted to ProseMirror JSON before upload.
    pub markdown: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FetchMarkupParams {
    pub workspace: String,
    pub object_class: String,
    pub object_id: String,
    pub object_attr: String,
    /// Optional `source` parameter — pass when retrieving a specific
    /// historical revision.
    #[serde(default)]
    pub source_ref: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SyncStatusParams {}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SyncCardsParams {
    pub workspace: Option<String>,
    #[serde(default, alias = "dryRun")]
    pub dry_run: bool,
}

#[derive(Clone)]
pub struct HulyMcpServer {
    factory: HulyClientFactory,
    #[allow(dead_code)]
    nats: async_nats::Client,
    /// Echoed in error messages for operator clarity.
    #[allow(dead_code)]
    agent_id: String,
    audit: AuditPublisher,
    sync_runner: Option<Arc<SyncRunner>>,
    tool_router: ToolRouter<Self>,
}

impl HulyMcpServer {
    pub fn new(
        factory: HulyClientFactory,
        nats: async_nats::Client,
        agent_id: impl Into<String>,
    ) -> Self {
        let agent_id = agent_id.into();
        let audit = AuditPublisher::new(nats.clone(), agent_id.clone());
        Self {
            factory,
            nats,
            agent_id,
            audit,
            sync_runner: None,
            tool_router: Self::tool_router(),
        }
    }

    pub fn with_sync_runner(mut self, runner: Option<SyncRunner>) -> Self {
        self.sync_runner = runner.map(Arc::new);
        self
    }

    /// Wrap a tool body with `tool.invoked` / `tool.completed` /
    /// `huly.mcp.error` publishes. The body receives the freshly-minted
    /// `request_id` so it can plumb it into transactor TX `meta` (P7
    /// commit 4) or into action subjects (commit 4).
    ///
    /// `params_digest` is supplied by the caller (via [`digest_json`])
    /// so the wrapper can take ownership of `params` into the body
    /// closure without re-borrowing.
    ///
    /// The body returns `Result<String, AuditedError>`; the wrapper
    /// translates that to `Result<String, String>` for rmcp and emits
    /// the structured audit/error events. Publish errors never fail
    /// the tool — they are warn-logged inside [`AuditPublisher`].
    async fn record_tool<F, Fut>(
        &self,
        tool: &str,
        workspace: Option<String>,
        params_digest: String,
        body: F,
    ) -> Result<String, String>
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = Result<String, AuditedError>>,
    {
        let request_id = new_request_id();
        let started = Instant::now();
        self.audit
            .tool_invoked(tool, workspace.as_deref(), &params_digest, &request_id)
            .await;

        let outcome = body(request_id.clone()).await;
        let duration_ms = started.elapsed().as_millis() as u64;

        match outcome {
            Ok(s) => {
                let result_digest = crate::audit::digest_bytes(s.as_bytes());
                self.audit
                    .tool_completed(
                        tool,
                        &request_id,
                        ToolCompletedResult::Ok { result_digest },
                        duration_ms,
                    )
                    .await;
                Ok(s)
            }
            Err(e) => {
                let display = e.display.clone();
                self.audit
                    .tool_completed(
                        tool,
                        &request_id,
                        ToolCompletedResult::Err {
                            error: display.clone(),
                        },
                        duration_ms,
                    )
                    .await;
                self.audit
                    .error(
                        &request_id,
                        tool,
                        e.code.as_deref().unwrap_or("mcp:tool:error"),
                        e.message.as_deref().unwrap_or(&display),
                        e.params.unwrap_or(Value::Null),
                    )
                    .await;
                Err(display)
            }
        }
    }

    async fn client_for(
        &self,
        workspace: &str,
    ) -> Result<Arc<dyn PlatformClient>, String> {
        self.factory
            .for_workspace(workspace)
            .await
            .map_err(|e| format_factory_error(&e))
    }

    async fn collaborator_for(
        &self,
        workspace: &str,
    ) -> Result<huly_client::collaborator::CollaboratorClient, String> {
        let url = self
            .factory
            .collaborator_url(workspace)
            .await
            .map_err(|e| format_factory_error(&e))?
            .ok_or_else(|| {
                "bridge JWT broker did not advertise a collaborator_url for this workspace; \
                 the markup tools are unavailable until the bridge can resolve \
                 COLLABORATOR_URL via /config.json."
                    .to_string()
            })?;
        Ok(huly_client::collaborator::CollaboratorClient::new(&url))
    }
}

/// Best-effort extraction of field names from a Huly update-operations
/// object. Recognises `$set` / `$unset` / `$inc` / `$push` / `$pull`
/// keys; anything else is treated as a top-level field name. Returns
/// `None` when nothing is parseable.
fn changed_fields(ops: &Value) -> Option<Vec<String>> {
    let obj = ops.as_object()?;
    let mut out: Vec<String> = Vec::new();
    for (k, v) in obj {
        if k.starts_with('$') {
            if let Some(inner) = v.as_object() {
                for ik in inner.keys() {
                    out.push(ik.clone());
                }
            }
        } else {
            out.push(k.clone());
        }
    }
    if out.is_empty() {
        None
    } else {
        out.sort();
        out.dedup();
        Some(out)
    }
}

/// Map a Huly class name (`tracker:class:Issue`) to the audit-channel
/// class token (`tracker.issue`). Drops the `:class:` infix so subjects
/// like `huly.mcp.action.tracker.issue.create` stay readable in
/// `nats sub` / shell tooling. Falls back to a `:`→`.` substitution
/// when the input doesn't fit the `<plugin>:class:<Name>` mould.
fn audit_class(class: &str) -> String {
    if let Some((plugin, rest)) = class.split_once(':') {
        // `core:class:Foo` → `core.foo`, `tracker:class:Issue` → `tracker.issue`.
        let tail = rest
            .strip_prefix("class:")
            .or_else(|| rest.strip_prefix("mixin:"))
            .unwrap_or(rest);
        return format!("{plugin}.{}", tail.to_lowercase().replace(':', "."));
    }
    class.to_lowercase()
}

/// Internal carrier for tool errors that preserves the structured
/// transactor [`Status`]-shape when one was decoded. The
/// [`HulyMcpServer::record_tool`] wrapper consumes this to produce the
/// `huly.mcp.error` payload; the user-facing tool result is the
/// `display` string.
#[derive(Debug, Clone)]
pub(crate) struct AuditedError {
    pub display: String,
    pub code: Option<String>,
    pub message: Option<String>,
    pub params: Option<Value>,
}

impl AuditedError {
    pub fn plain(s: impl Into<String>) -> Self {
        Self {
            display: s.into(),
            code: None,
            message: None,
            params: None,
        }
    }
}

impl From<String> for AuditedError {
    fn from(s: String) -> Self {
        Self::plain(s)
    }
}

impl From<&str> for AuditedError {
    fn from(s: &str) -> Self {
        Self::plain(s.to_string())
    }
}

impl From<&FactoryError> for AuditedError {
    fn from(e: &FactoryError) -> Self {
        let display = format_factory_error(e);
        match e {
            FactoryError::Mint(code, msg) => Self {
                display,
                code: Some(code.clone()),
                message: Some(msg.clone()),
                params: None,
            },
            _ => Self::plain(display),
        }
    }
}

impl From<&ClientError> for AuditedError {
    fn from(e: &ClientError) -> Self {
        let display = format_client_error(e);
        match e {
            ClientError::Rpc { code, message } => Self {
                display,
                code: Some(code.clone()),
                message: Some(message.clone()),
                params: None,
            },
            _ => Self::plain(display),
        }
    }
}

fn format_factory_error(e: &FactoryError) -> String {
    match e {
        FactoryError::Mint(code, msg) if code == "unknown_workspace" => {
            format!(
                "workspace not registered with bridge JWT broker: {msg}. \
                 Add a [[workspace_credentials]] entry to bridge.toml."
            )
        }
        _ => format!("{e}"),
    }
}

fn format_client_error(e: &ClientError) -> String {
    format!("{e}")
}

#[tool_router]
impl HulyMcpServer {
    /// List all workspaces the holder of the (resolved) account-service JWT
    /// belongs to. Per the P1 spike, this endpoint goes against the
    /// **account service** with `account_service_jwt`, not the workspace
    /// JWT — so the caller must supply a `workspace` slug whose mint round
    /// produces a valid account-service token.
    #[tool(
        name = "huly_list_workspaces",
        description = "List workspaces the account-service JWT can see. Pass `workspace` to nominate which JWT round-trip to use."
    )]
    async fn list_workspaces(
        &self,
        Parameters(params): Parameters<ListWorkspacesParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_list_workspaces",
            workspace.clone(),
            digest_json(&params),
            |_rid| async move {
                let workspace = params.workspace.ok_or_else(|| AuditedError::plain(
                    "huly_list_workspaces requires a `workspace` argument: pass any \
                     workspace slug the bridge can mint for; the account-service JWT \
                     returned alongside its workspace JWT is what backs this call.",
                ))?;
                let acct_jwt = self
                    .factory
                    .account_service_jwt(&workspace)
                    .await
                    .map_err(|e| AuditedError::from(&e))?
                    .ok_or_else(|| AuditedError::plain(
                        "bridge JWT broker did not return an account_service_jwt for this workspace; \
                         huly_list_workspaces is unavailable.",
                    ))?;
                let accounts_base = self
                    .factory
                    .accounts_url(&workspace)
                    .await
                    .map_err(|e| AuditedError::from(&e))?
                    .ok_or_else(|| AuditedError::plain(
                        "bridge JWT broker did not advertise an accounts_url for this workspace; \
                         set [huly] accounts_url in the bridge config so huly_list_workspaces \
                         knows where to query.",
                    ))?;
                let accounts = AccountsClient::new(accounts_base);
                let workspaces = accounts
                    .get_user_workspaces(&acct_jwt)
                    .await
                    .map_err(|e| AuditedError::plain(format!("account service: {e}")))?;
                serde_json::to_string_pretty(&workspaces)
                    .map_err(|e| AuditedError::plain(format!("{e}")))
            },
        )
        .await
    }

    /// Status / health of the JWT broker round-trip for a given workspace.
    /// Replaces the old `huly_status` which read a bridge announcement.
    #[tool(
        name = "huly_status",
        description = "Probe the JWT broker for a given workspace and report success/failure."
    )]
    async fn status(
        &self,
        Parameters(params): Parameters<StatusParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_status",
            workspace.clone(),
            digest_json(&params),
            |_rid| async move {
                let workspace = params.workspace.ok_or_else(|| AuditedError::plain(
                    "huly_status requires a `workspace` argument; bridge announcements \
                     have been removed (P4).",
                ))?;
                // Force a fresh mint to surface any current broker error, then drop
                // the cache so subsequent tool calls see the same fresh state.
                self.factory.forget(&workspace).await;
                match self.factory.for_workspace(&workspace).await {
                    Ok(_) => Ok(serde_json::json!({
                        "workspace": workspace,
                        "broker": "ok",
                    })
                    .to_string()),
                    Err(e) => Err(AuditedError::from(&e)),
                }
            },
        )
        .await
    }

    /// Find documents in a Huly workspace by class and query filter.
    #[tool(
        name = "huly_find",
        description = "Find documents in a Huly workspace. Returns matching documents with total count."
    )]
    async fn find(
        &self,
        Parameters(params): Parameters<FindParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_find",
            Some(workspace.clone()),
            digest_json(&params),
            |_rid| async move {
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let options = params.limit.map(|limit| huly_common::types::FindOptions {
                    limit: Some(limit),
                    ..Default::default()
                });
                let result = client
                    .find_all(&params.class, params.query, options)
                    .await
                    .map_err(|e| AuditedError::from(&e))?;
                serde_json::to_string_pretty(&result)
                    .map_err(|e| AuditedError::plain(format!("{e}")))
            },
        )
        .await
    }

    /// Get a single document from a Huly workspace.
    #[tool(
        name = "huly_get",
        description = "Get a single document from a Huly workspace by class and query."
    )]
    async fn get(
        &self,
        Parameters(params): Parameters<GetParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_get",
            Some(workspace.clone()),
            digest_json(&params),
            |_rid| async move {
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                match client.find_one(&params.class, params.query, None).await {
                    Ok(Some(doc)) => serde_json::to_string_pretty(&doc)
                        .map_err(|e| AuditedError::plain(format!("{e}"))),
                    Ok(None) => Ok("null".to_string()),
                    Err(e) => Err(AuditedError::from(&e)),
                }
            },
        )
        .await
    }

    /// Create a new document in a Huly workspace.
    #[tool(
        name = "huly_create",
        description = "Create a new document in a Huly workspace. Returns the created document ID."
    )]
    async fn create(
        &self,
        Parameters(params): Parameters<CreateParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        let class = params.class.clone();
        self.record_tool(
            "huly_create",
            Some(workspace.clone()),
            digest_json(&params),
            |rid| async move {
                self.audit
                    .action(&audit_class(&class), "create", &workspace, &rid, None, None)
                    .await;
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let id = client
                    .create_doc(&params.class, &params.space, params.attributes)
                    .await
                    .map_err(|e| AuditedError::from(&e))?;
                Ok(serde_json::json!({"id": id}).to_string())
            },
        )
        .await
    }

    /// Update a document in a Huly workspace.
    #[tool(
        name = "huly_update",
        description = "Update an existing document in a Huly workspace."
    )]
    async fn update(
        &self,
        Parameters(params): Parameters<UpdateParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        let class = params.class.clone();
        let id = params.id.clone();
        self.record_tool(
            "huly_update",
            Some(workspace.clone()),
            digest_json(&params),
            |rid| async move {
                let fields = changed_fields(&params.operations);
                self.audit
                    .action(
                        &audit_class(&class),
                        "update",
                        &workspace,
                        &rid,
                        Some(&id),
                        fields,
                    )
                    .await;
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let result = client
                    .update_doc(&params.class, &params.space, &params.id, params.operations)
                    .await
                    .map_err(|e| AuditedError::from(&e))?;
                serde_json::to_string_pretty(&result)
                    .map_err(|e| AuditedError::plain(format!("{e}")))
            },
        )
        .await
    }

    /// Delete a document from a Huly workspace.
    #[tool(
        name = "huly_delete",
        description = "Delete a document from a Huly workspace."
    )]
    async fn delete(
        &self,
        Parameters(params): Parameters<DeleteParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        let class = params.class.clone();
        let id = params.id.clone();
        self.record_tool(
            "huly_delete",
            Some(workspace.clone()),
            digest_json(&params),
            |rid| async move {
                self.audit
                    .action(
                        &audit_class(&class),
                        "delete",
                        &workspace,
                        &rid,
                        Some(&id),
                        None,
                    )
                    .await;
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let result = client
                    .remove_doc(&params.class, &params.space, &params.id)
                    .await
                    .map_err(|e| AuditedError::from(&e))?;
                serde_json::to_string_pretty(&result)
                    .map_err(|e| AuditedError::plain(format!("{e}")))
            },
        )
        .await
    }

    /// Find issues with optional filters.
    #[tool(
        name = "huly_find_issues",
        description = "Find tracker issues, optionally filtered by component, status, or title substring."
    )]
    async fn find_issues_tool(
        &self,
        Parameters(params): Parameters<FindIssuesParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_find_issues",
            Some(workspace.clone()),
            digest_json(&params),
            |_rid| async move {
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let issues = tools::find_issues(
                    &*client,
                    params.component.as_deref(),
                    params.status,
                    params.query.as_deref(),
                    params.limit.unwrap_or(200),
                )
                .await
                .map_err(AuditedError::plain)?;
                serde_json::to_string_pretty(&issues)
                    .map_err(|e| AuditedError::plain(format!("{e}")))
            },
        )
        .await
    }

    /// Get an issue by identifier, plus its incoming/outgoing relations.
    #[tool(
        name = "huly_get_issue",
        description = "Fetch a single tracker issue by project-prefixed identifier."
    )]
    async fn get_issue_tool(
        &self,
        Parameters(params): Parameters<GetIssueParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_get_issue",
            Some(workspace.clone()),
            digest_json(&params),
            |_rid| async move {
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                match tools::get_issue(&*client, &params.identifier)
                    .await
                    .map_err(AuditedError::plain)?
                {
                    Some(v) => serde_json::to_string_pretty(&v)
                        .map_err(|e| AuditedError::plain(format!("{e}"))),
                    None => Ok("null".into()),
                }
            },
        )
        .await
    }

    /// Sparse update of a tracker issue.
    #[tool(
        name = "huly_update_issue",
        description = "Update a tracker issue. Pass only the fields that should change."
    )]
    async fn update_issue_tool(
        &self,
        Parameters(params): Parameters<UpdateIssueParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        let identifier = params.identifier.clone();
        self.record_tool(
            "huly_update_issue",
            Some(workspace.clone()),
            digest_json(&params),
            |rid| async move {
                let intended: Vec<String> = [
                    params.title.as_ref().map(|_| "title"),
                    params.description_ref.as_ref().map(|_| "description"),
                    params.status.as_ref().map(|_| "status"),
                    params.priority.as_ref().map(|_| "priority"),
                    params.component.as_ref().map(|_| "component"),
                ]
                .into_iter()
                .flatten()
                .map(str::to_string)
                .collect();
                let fields_for_action = (!intended.is_empty()).then(|| intended.clone());
                self.audit
                    .action(
                        "tracker.issue",
                        "update",
                        &workspace,
                        &rid,
                        Some(&identifier),
                        fields_for_action,
                    )
                    .await;
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let changed = tools::update_issue(
                    &*client,
                    &params.identifier,
                    params.title.as_deref(),
                    params.description_ref.as_deref(),
                    params.status,
                    params.priority,
                    params.component.as_deref(),
                )
                .await
                .map_err(AuditedError::plain)?;
                match changed {
                    Some(fields) => Ok(serde_json::json!({
                        "identifier": params.identifier,
                        "changed": fields,
                    })
                    .to_string()),
                    None => Err(AuditedError::plain(format!(
                        "Issue '{}' not found.",
                        params.identifier
                    ))),
                }
            },
        )
        .await
    }

    /// Create a tracker issue (race-resistant via apply_if_tx).
    #[tool(
        name = "huly_create_issue",
        description = "Create a tracker issue under a project. Returns issue id + identifier."
    )]
    async fn create_issue_tool(
        &self,
        Parameters(params): Parameters<CreateIssueParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_create_issue",
            Some(workspace.clone()),
            digest_json(&params),
            |rid| async move {
                self.audit
                    .action("tracker.issue", "create", &workspace, &rid, None, None)
                    .await;
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let project = tools::resolve_project(&*client, params.project.as_deref())
                    .await
                    .map_err(AuditedError::plain)?;
                let modified_by = params
                    .modified_by
                    .as_deref()
                    .unwrap_or(crate::txcud::SYSTEM_ACCOUNT);
                let (id, identifier) = tools::create_issue_in_project(
                    &*client,
                    &project,
                    &params.title,
                    params.description_ref.as_deref(),
                    params.status.unwrap_or(IssueStatus::Backlog),
                    params.priority.unwrap_or(0),
                    params.component.as_deref(),
                    modified_by,
                )
                .await
                .map_err(AuditedError::plain)?;
                Ok(serde_json::json!({"id": id, "identifier": identifier}).to_string())
            },
        )
        .await
    }

    /// Create a tracker component (race-resistant via apply_if_tx).
    #[tool(
        name = "huly_create_component",
        description = "Create a tracker component under a project. Idempotent on (project, label)."
    )]
    async fn create_component_tool(
        &self,
        Parameters(params): Parameters<CreateComponentParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_create_component",
            Some(workspace.clone()),
            digest_json(&params),
            |rid| async move {
                self.audit
                    .action("tracker.component", "create", &workspace, &rid, None, None)
                    .await;
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let project = tools::resolve_project(&*client, params.project.as_deref())
                    .await
                    .map_err(AuditedError::plain)?;
                let modified_by = params
                    .modified_by
                    .as_deref()
                    .unwrap_or(crate::txcud::SYSTEM_ACCOUNT);
                let r = tools::create_component(
                    &*client,
                    &project,
                    &params.label,
                    params.description.as_deref().unwrap_or(""),
                    modified_by,
                )
                .await
                .map_err(AuditedError::plain)?;
                let v = match r {
                    tools::ComponentResult::Created { id, label } => serde_json::json!({
                        "status": "created",
                        "id": id,
                        "label": label,
                    }),
                    tools::ComponentResult::Existing { id, label } => serde_json::json!({
                        "status": "existing",
                        "id": id,
                        "label": label,
                    }),
                };
                Ok(v.to_string())
            },
        )
        .await
    }

    /// Create a tracker project.
    #[tool(
        name = "huly_create_project",
        description = "Create a tracker project. `identifier` defaults to the name's initials."
    )]
    async fn create_project_tool(
        &self,
        Parameters(params): Parameters<CreateProjectParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_create_project",
            Some(workspace.clone()),
            digest_json(&params),
            |rid| async move {
                self.audit
                    .action("tracker.project", "create", &workspace, &rid, None, None)
                    .await;
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let identifier = params
                    .identifier
                    .clone()
                    .unwrap_or_else(|| tools::derive_identifier(&params.name));
                let id = tools::create_project(
                    &*client,
                    &params.name,
                    &identifier,
                    params.description.as_deref().unwrap_or(""),
                )
                .await
                .map_err(AuditedError::plain)?;
                Ok(serde_json::json!({"id": id, "identifier": identifier}).to_string())
            },
        )
        .await
    }

    /// Find cards (MasterTag instances), optionally filtered by kind name and title.
    #[tool(
        name = "huly_find_cards",
        description = "Find cards. Pass `kind` (MasterTag name) to scope; omit to enumerate all card types."
    )]
    async fn find_cards_tool(
        &self,
        Parameters(params): Parameters<FindCardsParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_find_cards",
            Some(workspace.clone()),
            digest_json(&params),
            |_rid| async move {
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let schema = self
                    .factory
                    .schema(&params.workspace)
                    .await
                    .map_err(|e| AuditedError::from(&e))?;
                let cards = tools::find_cards(
                    &*client,
                    &schema,
                    params.kind.as_deref(),
                    params.query.as_deref(),
                    params.limit.unwrap_or(200),
                )
                .await
                .map_err(AuditedError::plain)?;
                serde_json::to_string_pretty(&cards)
                    .map_err(|e| AuditedError::plain(format!("{e}")))
            },
        )
        .await
    }

    /// Create a card under a MasterTag.
    #[tool(
        name = "huly_create_card",
        description = "Create a card of the given MasterTag. Returns the new card id."
    )]
    async fn create_card_tool(
        &self,
        Parameters(params): Parameters<CreateCardParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_create_card",
            Some(workspace.clone()),
            digest_json(&params),
            |rid| async move {
                self.audit
                    .action("card", "create", &workspace, &rid, None, None)
                    .await;
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let schema = self
                    .factory
                    .schema(&params.workspace)
                    .await
                    .map_err(|e| AuditedError::from(&e))?;
                let id = tools::create_card(
                    &*client,
                    &schema,
                    &params.kind,
                    &params.space,
                    &params.title,
                    params.attributes,
                )
                .await
                .map_err(AuditedError::plain)?;
                Ok(serde_json::json!({"id": id}).to_string())
            },
        )
        .await
    }

    /// Link an issue to a card via an Association.
    #[tool(
        name = "huly_link_issue_to_card",
        description = "Create (or detect existing) a relation linking a tracker issue to a card."
    )]
    async fn link_issue_to_card_tool(
        &self,
        Parameters(params): Parameters<LinkIssueToCardParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_link_issue_to_card",
            Some(workspace.clone()),
            digest_json(&params),
            |rid| async move {
                self.audit
                    .action(
                        "tracker.issue",
                        "link",
                        &workspace,
                        &rid,
                        Some(&params.issue_identifier),
                        Some(vec!["relation".into()]),
                    )
                    .await;
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let schema = self
                    .factory
                    .schema(&params.workspace)
                    .await
                    .map_err(|e| AuditedError::from(&e))?;
                let r = tools::link_issue_to_card(
                    &*client,
                    &schema,
                    &params.issue_identifier,
                    &params.card_id,
                    &params.relation,
                    params.modified_by.as_deref(),
                )
                .await
                .map_err(AuditedError::plain)?;
                let v = match r {
                    tools::LinkResult::Created { id } => {
                        serde_json::json!({"status": "created", "id": id})
                    }
                    tools::LinkResult::AlreadyLinked { id } => {
                        serde_json::json!({"status": "already_linked", "id": id})
                    }
                    tools::LinkResult::IssueNotFound => {
                        return Err(AuditedError::plain(format!(
                            "Issue '{}' not found.",
                            params.issue_identifier
                        )));
                    }
                };
                Ok(v.to_string())
            },
        )
        .await
    }

    /// Workspace introspection — projects, components, statuses, master
    /// tags, associations, and an issue summary by status.
    #[tool(
        name = "huly_discover",
        description = "One-shot snapshot of a workspace: projects, components, statuses, card types, associations, and an issue-by-status summary."
    )]
    async fn discover_tool(
        &self,
        Parameters(params): Parameters<DiscoverParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_discover",
            Some(workspace.clone()),
            digest_json(&params),
            |_rid| async move {
                let client = self.client_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let v = tools::discover(&*client).await.map_err(AuditedError::plain)?;
                serde_json::to_string_pretty(&v)
                    .map_err(|e| AuditedError::plain(format!("{e}")))
            },
        )
        .await
    }

    /// Upload markdown as a ProseMirror markup blob and return the resulting
    /// MarkupBlobRef. Stamp the ref into the corresponding doc field via
    /// `huly_update` / `huly_update_issue`.
    #[tool(
        name = "huly_upload_markup",
        description = "Upload markdown as a ProseMirror markup blob; returns the MarkupBlobRef."
    )]
    async fn upload_markup_tool(
        &self,
        Parameters(params): Parameters<UploadMarkupParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_upload_markup",
            Some(workspace.clone()),
            digest_json(&params),
            |_rid| async move {
                let collaborator = self.collaborator_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let (workspace_uuid, jwt) = self
                    .factory
                    .collaborator_auth(&params.workspace)
                    .await
                    .map_err(|e| AuditedError::from(&e))?;
                let blob_ref = tools::upload_markup(
                    &collaborator,
                    &jwt,
                    &workspace_uuid,
                    &params.object_class,
                    &params.object_id,
                    &params.object_attr,
                    &params.markdown,
                )
                .await
                .map_err(AuditedError::plain)?;
                Ok(serde_json::json!({"ref": blob_ref}).to_string())
            },
        )
        .await
    }

    /// Fetch a markup blob and return it as markdown.
    #[tool(
        name = "huly_fetch_markup",
        description = "Fetch a markup blob and return it as markdown."
    )]
    async fn fetch_markup_tool(
        &self,
        Parameters(params): Parameters<FetchMarkupParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_fetch_markup",
            Some(workspace.clone()),
            digest_json(&params),
            |_rid| async move {
                let collaborator = self.collaborator_for(&params.workspace).await
                    .map_err(AuditedError::plain)?;
                let (workspace_uuid, jwt) = self
                    .factory
                    .collaborator_auth(&params.workspace)
                    .await
                    .map_err(|e| AuditedError::from(&e))?;
                let md = tools::fetch_markup(
                    &collaborator,
                    &jwt,
                    &workspace_uuid,
                    &params.object_class,
                    &params.object_id,
                    &params.object_attr,
                    params.source_ref.as_deref(),
                )
                .await
                .map_err(AuditedError::plain)?;
                Ok(serde_json::json!({"markdown": md}).to_string())
            },
        )
        .await
    }

    #[tool(
        name = "huly_sync_status",
        description = "Run the upstream sync pipeline in status-only mode."
    )]
    async fn sync_status(
        &self,
        Parameters(params): Parameters<SyncStatusParams>,
    ) -> Result<String, String> {
        self.record_tool(
            "huly_sync_status",
            None,
            digest_json(&params),
            |_rid| async move {
                let runner = self
                    .sync_runner
                    .as_ref()
                    .ok_or_else(|| AuditedError::plain(SyncRunner::not_configured_error()))?;
                let report = runner
                    .status()
                    .await
                    .map_err(|e| AuditedError::plain(format!("{e}")))?;
                serde_json::to_string_pretty(&report)
                    .map_err(|e| AuditedError::plain(format!("{e}")))
            },
        )
        .await
    }

    #[tool(
        name = "huly_sync_cards",
        description = "Run the upstream sync pipeline."
    )]
    async fn sync_cards(
        &self,
        Parameters(params): Parameters<SyncCardsParams>,
    ) -> Result<String, String> {
        let workspace = params.workspace.clone();
        self.record_tool(
            "huly_sync_cards",
            workspace.clone(),
            digest_json(&params),
            |_rid| async move {
                let runner = self
                    .sync_runner
                    .as_ref()
                    .ok_or_else(|| AuditedError::plain(SyncRunner::not_configured_error()))?;
                let output = runner
                    .sync(params.dry_run)
                    .await
                    .map_err(|e| AuditedError::plain(format!("{e}")))?;
                let v = serde_json::json!({
                    "stdout": output.stdout,
                    "stderr": output.stderr,
                });
                serde_json::to_string_pretty(&v)
                    .map_err(|e| AuditedError::plain(format!("{e}")))
            },
        )
        .await
    }
}

#[tool_handler]
impl ServerHandler for HulyMcpServer {}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::handler::server::wrapper::Parameters;

    /// Without a real NATS broker we can still exercise the tool entry-point
    /// signatures: every tool returns Result<String, String> via the rmcp
    /// macros, and the factory error formatter handles the "unknown
    /// workspace" code mention.
    #[test]
    fn format_factory_error_explains_unknown_workspace() {
        let e = FactoryError::Mint("unknown_workspace".into(), "no creds".into());
        let s = format_factory_error(&e);
        assert!(s.contains("not registered"), "msg: {s}");
        assert!(s.contains("[[workspace_credentials]]"), "msg: {s}");
    }

    #[test]
    fn format_factory_error_passes_through_other_codes() {
        let e = FactoryError::Mint("accounts_failure".into(), "down".into());
        let s = format_factory_error(&e);
        assert!(s.contains("accounts_failure"), "msg: {s}");
        assert!(s.contains("down"), "msg: {s}");
    }

    #[test]
    fn format_factory_error_passes_broker_request() {
        let e = FactoryError::BrokerRequest(
            crate::jwt_broker_client::MintClientError::Decode("bad".into()),
        );
        let s = format_factory_error(&e);
        assert!(s.contains("bad"), "msg: {s}");
    }

    #[test]
    fn format_client_error_passes_through() {
        let e = ClientError::Rpc {
            code: "401".into(),
            message: "denied".into(),
        };
        let s = format_client_error(&e);
        assert!(s.contains("401"));
        assert!(s.contains("denied"));
    }

    #[tokio::test]
    async fn list_workspaces_requires_workspace_arg() {
        // Build a server with a no-op factory; we never reach the network
        // because the missing-arg branch fires first.
        let Ok(c) = async_nats::connect("nats://127.0.0.1:4222").await else {
            return;
        };
        let factory = HulyClientFactory::new(c.clone(), "agent");
        let server = HulyMcpServer::new(factory, c, "agent");
        let err = server
            .list_workspaces(Parameters(ListWorkspacesParams { workspace: None }))
            .await
            .unwrap_err();
        assert!(err.contains("workspace"), "msg: {err}");
    }

    /// Asserts the subject ordering for a mutating tool: every
    /// `huly_create_issue` (or any `huly_create*`) emits exactly the
    /// sequence `tool.invoked` → `action.<class>.create` →
    /// `tool.completed`. Failure of this ordering breaks downstream
    /// audit consumers that correlate events by `request_id` arrival
    /// order.
    ///
    /// Driven via [`HulyMcpServer::record_tool`] — the same wrapper
    /// every `#[tool]` method goes through — with a synthetic body
    /// that imitates a `huly_create_issue` (publishes action, returns
    /// Ok). Skipped when no NATS broker is reachable.
    #[tokio::test]
    async fn record_tool_emits_invoked_then_action_then_completed_for_create() {
        use futures::StreamExt;
        let Ok(c) = async_nats::connect("nats://127.0.0.1:4222").await else {
            return;
        };
        let factory = HulyClientFactory::new(c.clone(), "agent");
        let server = HulyMcpServer::new(factory, c.clone(), "agent");

        let mut sub = c.subscribe("huly.mcp.>".to_string()).await.unwrap();

        let workspace = "ws-test".to_string();
        let audit_clone = server.audit.clone();
        let ws_clone = workspace.clone();
        let result = server
            .record_tool(
                "huly_create_issue",
                Some(workspace.clone()),
                "deadbeefcafebabe".into(),
                |rid| async move {
                    audit_clone
                        .action("tracker.issue", "create", &ws_clone, &rid, None, None)
                        .await;
                    Ok::<_, AuditedError>(serde_json::json!({"id": "iss-1"}).to_string())
                },
            )
            .await;
        assert!(result.is_ok(), "tool should succeed: {result:?}");
        c.flush().await.unwrap();

        // Pull three messages in publish order; the subscription
        // delivers in arrival order on a single subject filter.
        let mut subjects = Vec::new();
        for _ in 0..3 {
            let m = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                sub.next(),
            )
            .await
            .ok()
            .flatten()
            .expect("expected three audit messages");
            subjects.push(m.subject.to_string());
        }
        assert_eq!(
            subjects,
            vec![
                "huly.mcp.tool.invoked".to_string(),
                "huly.mcp.action.tracker.issue.create".to_string(),
                "huly.mcp.tool.completed".to_string(),
            ],
            "subject order for a mutating tool must be invoked → action → completed"
        );
    }

    #[test]
    fn audit_class_drops_class_infix_for_huly_classes() {
        assert_eq!(audit_class("tracker:class:Issue"), "tracker.issue");
        assert_eq!(audit_class("core:class:Account"), "core.account");
        assert_eq!(audit_class("card:mixin:Tag"), "card.tag");
        // Unknown shape passes through lower-cased.
        assert_eq!(audit_class("freeform"), "freeform");
    }

    #[test]
    fn changed_fields_extracts_set_keys() {
        let v = serde_json::json!({"$set": {"title": "x", "priority": 2}});
        let mut got = changed_fields(&v).unwrap();
        got.sort();
        assert_eq!(got, vec!["priority", "title"]);
    }

    #[test]
    fn changed_fields_returns_none_for_empty_object() {
        assert!(changed_fields(&serde_json::json!({})).is_none());
    }

    #[test]
    fn changed_fields_handles_top_level_keys() {
        let v = serde_json::json!({"name": "x"});
        assert_eq!(changed_fields(&v), Some(vec!["name".into()]));
    }
}
