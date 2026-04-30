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

use crate::huly_client_factory::{FactoryError, HulyClientFactory};
use crate::mcp::catalog::IssueStatus;
use crate::mcp::tools;
use crate::sync::SyncRunner;
use huly_client::accounts::AccountsClient;
use huly_client::client::{ClientError, PlatformClient};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

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
    nats: async_nats::Client,
    /// Echoed in error messages for operator clarity.
    #[allow(dead_code)]
    agent_id: String,
    sync_runner: Option<Arc<SyncRunner>>,
    tool_router: ToolRouter<Self>,
}

impl HulyMcpServer {
    pub fn new(
        factory: HulyClientFactory,
        nats: async_nats::Client,
        agent_id: impl Into<String>,
    ) -> Self {
        Self {
            factory,
            nats,
            agent_id: agent_id.into(),
            sync_runner: None,
            tool_router: Self::tool_router(),
        }
    }

    pub fn with_sync_runner(mut self, runner: Option<SyncRunner>) -> Self {
        self.sync_runner = runner.map(Arc::new);
        self
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
        let workspace = params.workspace.ok_or_else(|| {
            "huly_list_workspaces requires a `workspace` argument: pass any \
             workspace slug the bridge can mint for; the account-service JWT \
             returned alongside its workspace JWT is what backs this call."
                .to_string()
        })?;
        let acct_jwt = self
            .factory
            .account_service_jwt(&workspace)
            .await
            .map_err(|e| format_factory_error(&e))?
            .ok_or_else(|| {
                "bridge JWT broker did not return an account_service_jwt for this workspace; \
                 huly_list_workspaces is unavailable."
                    .to_string()
            })?;
        let accounts_base = self
            .factory
            .accounts_url(&workspace)
            .await
            .map_err(|e| format_factory_error(&e))?
            .ok_or_else(|| {
                "bridge JWT broker did not advertise an accounts_url for this workspace; \
                 set [huly] accounts_url in the bridge config so huly_list_workspaces \
                 knows where to query."
                    .to_string()
            })?;
        let accounts = AccountsClient::new(accounts_base);
        let workspaces = accounts
            .get_user_workspaces(&acct_jwt)
            .await
            .map_err(|e| format!("account service: {e}"))?;
        serde_json::to_string_pretty(&workspaces).map_err(|e| format!("{e}"))
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
        let workspace = params.workspace.ok_or_else(|| {
            "huly_status requires a `workspace` argument; bridge announcements \
             have been removed (P4)."
                .to_string()
        })?;
        // Force a fresh mint to surface any current broker error, then drop
        // the cache so subsequent tool calls see the same fresh state.
        self.factory.forget(&workspace).await;
        match self.factory.for_workspace(&workspace).await {
            Ok(_) => Ok(serde_json::json!({
                "workspace": workspace,
                "broker": "ok",
            })
            .to_string()),
            Err(e) => Err(format_factory_error(&e)),
        }
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
        let client = self.client_for(&params.workspace).await?;
        let options = params.limit.map(|limit| huly_common::types::FindOptions {
            limit: Some(limit),
            ..Default::default()
        });
        let result = client
            .find_all(&params.class, params.query, options)
            .await
            .map_err(|e| format_client_error(&e))?;
        serde_json::to_string_pretty(&result).map_err(|e| format!("{e}"))
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
        let client = self.client_for(&params.workspace).await?;
        match client.find_one(&params.class, params.query, None).await {
            Ok(Some(doc)) => serde_json::to_string_pretty(&doc).map_err(|e| format!("{e}")),
            Ok(None) => Ok("null".to_string()),
            Err(e) => Err(format_client_error(&e)),
        }
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
        let client = self.client_for(&params.workspace).await?;
        let id = client
            .create_doc(&params.class, &params.space, params.attributes)
            .await
            .map_err(|e| format_client_error(&e))?;
        Ok(serde_json::json!({"id": id}).to_string())
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
        let client = self.client_for(&params.workspace).await?;
        let result = client
            .update_doc(&params.class, &params.space, &params.id, params.operations)
            .await
            .map_err(|e| format_client_error(&e))?;
        serde_json::to_string_pretty(&result).map_err(|e| format!("{e}"))
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
        let client = self.client_for(&params.workspace).await?;
        let result = client
            .remove_doc(&params.class, &params.space, &params.id)
            .await
            .map_err(|e| format_client_error(&e))?;
        serde_json::to_string_pretty(&result).map_err(|e| format!("{e}"))
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
        let client = self.client_for(&params.workspace).await?;
        let issues = tools::find_issues(
            &*client,
            params.component.as_deref(),
            params.status,
            params.query.as_deref(),
            params.limit.unwrap_or(200),
        )
        .await?;
        serde_json::to_string_pretty(&issues).map_err(|e| format!("{e}"))
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
        let client = self.client_for(&params.workspace).await?;
        match tools::get_issue(&*client, &params.identifier).await? {
            Some(v) => serde_json::to_string_pretty(&v).map_err(|e| format!("{e}")),
            None => Ok("null".into()),
        }
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
        let client = self.client_for(&params.workspace).await?;
        let changed = tools::update_issue(
            &*client,
            &params.identifier,
            params.title.as_deref(),
            params.description_ref.as_deref(),
            params.status,
            params.priority,
            params.component.as_deref(),
        )
        .await?;
        match changed {
            Some(fields) => Ok(serde_json::json!({
                "identifier": params.identifier,
                "changed": fields,
            })
            .to_string()),
            None => Err(format!("Issue '{}' not found.", params.identifier)),
        }
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
        let client = self.client_for(&params.workspace).await?;
        let project = tools::resolve_project(&*client, params.project.as_deref()).await?;
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
        .await?;
        Ok(serde_json::json!({"id": id, "identifier": identifier}).to_string())
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
        let client = self.client_for(&params.workspace).await?;
        let project = tools::resolve_project(&*client, params.project.as_deref()).await?;
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
        .await?;
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
        let client = self.client_for(&params.workspace).await?;
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
        .await?;
        Ok(serde_json::json!({"id": id, "identifier": identifier}).to_string())
    }

    #[tool(
        name = "huly_sync_status",
        description = "Run the upstream sync pipeline in status-only mode."
    )]
    async fn sync_status(
        &self,
        _params: Parameters<SyncStatusParams>,
    ) -> Result<String, String> {
        let runner = self.sync_runner.as_ref().ok_or_else(SyncRunner::not_configured_error)?;
        let report = runner.status().await.map_err(|e| format!("{e}"))?;
        serde_json::to_string_pretty(&report).map_err(|e| format!("{e}"))
    }

    #[tool(
        name = "huly_sync_cards",
        description = "Run the upstream sync pipeline."
    )]
    async fn sync_cards(
        &self,
        Parameters(params): Parameters<SyncCardsParams>,
    ) -> Result<String, String> {
        let runner = self.sync_runner.as_ref().ok_or_else(SyncRunner::not_configured_error)?;
        let output = runner.sync(params.dry_run).await.map_err(|e| format!("{e}"))?;
        let v = serde_json::json!({
            "stdout": output.stdout,
            "stderr": output.stderr,
        });
        serde_json::to_string_pretty(&v).map_err(|e| format!("{e}"))
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
}
