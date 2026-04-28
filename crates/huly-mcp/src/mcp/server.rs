use crate::bridge_client::BridgeHttpClient;
use crate::discovery::BridgeRegistry;
use crate::mcp::catalog::{Catalog, CardType, IssueStatus, RelationType};
use crate::mcp::tools;
use crate::sync::SyncRunner;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

fn default_find_cards_limit() -> u32 { 50 }
fn default_find_issues_limit() -> u32 { 50 }
fn default_priority() -> u8 { 3 }
fn default_status() -> IssueStatus { IssueStatus::Todo }

/// Parameters for listing workspaces
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ListWorkspacesParams {}

/// Parameters for getting bridge status
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StatusParams {
    /// Workspace name. If omitted, returns status for all workspaces.
    pub workspace: Option<String>,
}

/// Parameters for finding documents
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FindParams {
    /// Target workspace name
    pub workspace: String,
    /// Huly class reference (e.g., "core:class:Issue")
    pub class: String,
    /// Query filter as JSON object
    #[serde(default)]
    pub query: serde_json::Value,
    /// Maximum number of results to return
    pub limit: Option<u64>,
}

/// Parameters for getting a single document
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetParams {
    /// Target workspace name
    pub workspace: String,
    /// Huly class reference (e.g., "core:class:Issue")
    pub class: String,
    /// Query filter as JSON object
    #[serde(default)]
    pub query: serde_json::Value,
}

/// Parameters for creating a document
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateParams {
    /// Target workspace name
    pub workspace: String,
    /// Huly class reference
    pub class: String,
    /// Space reference
    pub space: String,
    /// Document attributes as JSON object
    pub attributes: serde_json::Value,
}

/// Parameters for updating a document
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateParams {
    /// Target workspace name
    pub workspace: String,
    /// Huly class reference
    pub class: String,
    /// Space reference
    pub space: String,
    /// Document ID
    pub id: String,
    /// Update operations as JSON object
    pub operations: serde_json::Value,
}

/// Parameters for deleting a document
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DeleteParams {
    /// Target workspace name
    pub workspace: String,
    /// Huly class reference
    pub class: String,
    /// Space reference
    pub space: String,
    /// Document ID
    pub id: String,
}

/// Optional workspace selector — if missing, the only registered workspace is used.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DiscoverParams {
    pub workspace: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FindCardsParams {
    pub workspace: Option<String>,
    #[serde(rename = "type")]
    pub card_type: Option<CardType>,
    pub query: Option<String>,
    #[serde(default = "default_find_cards_limit")]
    pub limit: u32,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FindIssuesParams {
    pub workspace: Option<String>,
    pub project: Option<String>,
    pub component: Option<String>,
    pub status: Option<IssueStatus>,
    pub query: Option<String>,
    #[serde(default = "default_find_issues_limit")]
    pub limit: u32,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GetIssueParams {
    pub workspace: Option<String>,
    pub identifier: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateIssueParams {
    pub workspace: Option<String>,
    pub project: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub component: Option<String>,
    #[serde(default = "default_status")]
    pub status: IssueStatus,
    #[serde(default = "default_priority")]
    pub priority: u8,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UpdateIssueParams {
    pub workspace: Option<String>,
    pub identifier: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub status: Option<IssueStatus>,
    pub priority: Option<u8>,
    pub component: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateComponentParams {
    pub workspace: Option<String>,
    pub project: Option<String>,
    pub label: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct LinkIssueToCardParams {
    pub workspace: Option<String>,
    #[serde(rename = "issueIdentifier")]
    pub issue_identifier: String,
    #[serde(rename = "cardId")]
    pub card_id: String,
    #[serde(rename = "relationType")]
    pub relation_type: RelationType,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct CreateProjectParams {
    pub workspace: Option<String>,
    #[serde(rename = "readmePath")]
    pub readme_path: String,
    pub identifier: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct UploadMarkupParams {
    pub workspace: Option<String>,
    /// Class of the object that owns the attribute (e.g. "tracker:class:Issue").
    #[serde(rename = "objectClass")]
    pub object_class: String,
    /// ID of the object that owns the attribute.
    #[serde(rename = "objectId")]
    pub object_id: String,
    /// Attribute name (e.g. "description").
    #[serde(rename = "objectAttr")]
    pub object_attr: String,
    /// Markdown content to upload.
    pub markdown: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FetchMarkupParams {
    pub workspace: Option<String>,
    #[serde(rename = "objectClass")]
    pub object_class: String,
    #[serde(rename = "objectId")]
    pub object_id: String,
    #[serde(rename = "objectAttr")]
    pub object_attr: String,
    /// Existing blob reference to fetch. If omitted, the bridge resolves it from the object.
    #[serde(default, rename = "sourceRef")]
    pub source_ref: Option<String>,
    /// Output format: "markdown" (default, lossy on round-trip) or "prosemirror" (lossless).
    #[serde(default = "default_fetch_format")]
    pub format: String,
}

fn default_fetch_format() -> String { "markdown".to_string() }

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SyncStatusParams {}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SyncCardsParams {
    /// If true, list what would be synced without making changes.
    #[serde(default, rename = "dry_run", alias = "dryRun")]
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct HulyMcpServer {
    registry: BridgeRegistry,
    http_client: Arc<BridgeHttpClient>,
    catalog: Arc<Catalog>,
    sync_runner: Option<Arc<SyncRunner>>,
    tool_router: ToolRouter<Self>,
}

impl HulyMcpServer {
    #[allow(dead_code)]
    pub fn new(registry: BridgeRegistry, http_client: BridgeHttpClient) -> Self {
        Self::with_catalog(registry, http_client, Catalog::default())
    }

    pub fn with_catalog(
        registry: BridgeRegistry,
        http_client: BridgeHttpClient,
        catalog: Catalog,
    ) -> Self {
        Self {
            registry,
            http_client: Arc::new(http_client),
            catalog: Arc::new(catalog),
            sync_runner: None,
            tool_router: Self::tool_router(),
        }
    }

    /// Replace the sync runner (chainable). Pass `None` to disable sync tools.
    pub fn with_sync_runner(mut self, runner: Option<SyncRunner>) -> Self {
        self.sync_runner = runner.map(Arc::new);
        self
    }

    async fn resolve_proxy_url(&self, workspace: &str) -> Result<String, String> {
        match self.registry.get(workspace).await {
            Some(ann) if ann.ready => {
                validate_proxy_url(&ann.proxy_url)?;
                Ok(ann.proxy_url)
            }
            Some(_) => Err(format!("bridge for workspace '{}' is not ready", workspace)),
            None => Err(format!("workspace '{}' not found. Use huly_list_workspaces to see available workspaces.", workspace)),
        }
    }

    /// Resolve workspace name (explicit OR sole-registered) and return its
    /// proxy URL.
    async fn resolve_optional_workspace(
        &self,
        workspace: Option<&str>,
    ) -> Result<String, String> {
        let ws = tools::resolve_workspace(&self.registry, workspace).await?;
        self.resolve_proxy_url(&ws).await
    }
}

/// Validates that a proxy URL uses an allowed scheme (http or https only).
/// Prevents SSRF via malicious NATS announcements with file://, ftp://, etc.
fn validate_proxy_url(url: &str) -> Result<(), String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(())
    } else {
        Err(format!(
            "proxy URL '{}' must use http:// or https:// scheme",
            url
        ))
    }
}

#[tool_router]
impl HulyMcpServer {
    /// List all discovered Huly workspaces and their bridge status.
    #[tool(name = "huly_list_workspaces", description = "List all discovered Huly workspaces and their bridge connection status")]
    async fn list_workspaces(&self, _params: Parameters<ListWorkspacesParams>) -> String {
        let workspaces = self.registry.list_workspaces().await;
        if workspaces.is_empty() {
            return "No workspaces discovered. Ensure bridge instances are running and connected to NATS.".to_string();
        }
        serde_json::to_string_pretty(&workspaces).unwrap_or_else(|e| format!("Error: {e}"))
    }

    /// Get the status of a specific workspace or all workspaces.
    #[tool(name = "huly_status", description = "Get bridge status for a specific workspace or all workspaces")]
    async fn status(&self, Parameters(params): Parameters<StatusParams>) -> String {
        match params.workspace {
            Some(ws) => match self.registry.get(&ws).await {
                Some(ann) => serde_json::to_string_pretty(&ann).unwrap_or_else(|e| format!("Error: {e}")),
                None => format!("Workspace '{}' not found", ws),
            },
            None => {
                let workspaces = self.registry.list_workspaces().await;
                serde_json::to_string_pretty(&workspaces).unwrap_or_else(|e| format!("Error: {e}"))
            }
        }
    }

    /// Find documents in a Huly workspace by class and query filter.
    #[tool(name = "huly_find", description = "Find documents in a Huly workspace. Returns matching documents with total count.")]
    async fn find(&self, Parameters(params): Parameters<FindParams>) -> String {
        let proxy_url = match self.resolve_proxy_url(&params.workspace).await {
            Ok(url) => url,
            Err(e) => return e,
        };

        let options = params.limit.map(|limit| huly_common::types::FindOptions {
            limit: Some(limit),
            ..Default::default()
        });

        match self.http_client.find(&proxy_url, &params.class, params.query, options).await {
            Ok(result) => serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {e}")),
            Err(e) => format!("Error: {e}"),
        }
    }

    /// Get a single document from a Huly workspace.
    #[tool(name = "huly_get", description = "Get a single document from a Huly workspace by class and query.")]
    async fn get(&self, Parameters(params): Parameters<GetParams>) -> String {
        let proxy_url = match self.resolve_proxy_url(&params.workspace).await {
            Ok(url) => url,
            Err(e) => return e,
        };

        match self.http_client.find_one(&proxy_url, &params.class, params.query).await {
            Ok(Some(doc)) => serde_json::to_string_pretty(&doc).unwrap_or_else(|e| format!("Error: {e}")),
            Ok(None) => "null".to_string(),
            Err(e) => format!("Error: {e}"),
        }
    }

    /// Create a new document in a Huly workspace.
    #[tool(name = "huly_create", description = "Create a new document in a Huly workspace. Returns the created document ID.")]
    async fn create(&self, Parameters(params): Parameters<CreateParams>) -> String {
        let proxy_url = match self.resolve_proxy_url(&params.workspace).await {
            Ok(url) => url,
            Err(e) => return e,
        };

        match self.http_client.create(&proxy_url, &params.class, &params.space, params.attributes).await {
            Ok(id) => serde_json::json!({"id": id}).to_string(),
            Err(e) => format!("Error: {e}"),
        }
    }

    /// Update a document in a Huly workspace.
    #[tool(name = "huly_update", description = "Update an existing document in a Huly workspace.")]
    async fn update(&self, Parameters(params): Parameters<UpdateParams>) -> String {
        let proxy_url = match self.resolve_proxy_url(&params.workspace).await {
            Ok(url) => url,
            Err(e) => return e,
        };

        match self.http_client.update(&proxy_url, &params.class, &params.space, &params.id, params.operations).await {
            Ok(result) => serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {e}")),
            Err(e) => format!("Error: {e}"),
        }
    }

    /// Delete a document from a Huly workspace.
    #[tool(name = "huly_delete", description = "Delete a document from a Huly workspace.")]
    async fn delete(&self, Parameters(params): Parameters<DeleteParams>) -> String {
        let proxy_url = match self.resolve_proxy_url(&params.workspace).await {
            Ok(url) => url,
            Err(e) => return e,
        };

        match self.http_client.delete(&proxy_url, &params.class, &params.space, &params.id).await {
            Ok(result) => serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {e}")),
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(
        name = "huly_discover",
        description = "List workspace structure: Tracker projects, components, card types (MasterTags), associations, and issue status categories. Call this first to understand what exists in a workspace."
    )]
    async fn discover(&self, Parameters(params): Parameters<DiscoverParams>) -> String {
        let proxy_url = match self.resolve_optional_workspace(params.workspace.as_deref()).await {
            Ok(u) => u,
            Err(e) => return e,
        };
        match tools::discover(&self.http_client, &proxy_url).await {
            Ok(v) => serde_json::to_string_pretty(&v).unwrap_or_else(|e| format!("Error: {e}")),
            Err(e) => e,
        }
    }

    #[tool(
        name = "huly_find_cards",
        description = "Find cards (Module Spec, Data Entity, Business Flow, Compliance Item, Product Decision, Jurisdiction). Card type IDs default to the Muhasebot deployment; override via [mcp.catalog.card_types] config."
    )]
    async fn find_cards(&self, Parameters(params): Parameters<FindCardsParams>) -> String {
        let proxy_url = match self.resolve_optional_workspace(params.workspace.as_deref()).await {
            Ok(u) => u,
            Err(e) => return e,
        };
        let limit = params.limit.clamp(1, 100) as u64;
        match tools::find_cards(
            &self.http_client,
            &proxy_url,
            &self.catalog,
            params.card_type,
            params.query.as_deref(),
            limit,
        )
        .await
        {
            Ok(cards) => tools::render_card_summaries(&cards),
            Err(e) => e,
        }
    }

    #[tool(
        name = "huly_find_issues",
        description = "Find tracker issues. Filter by component, status (backlog/todo/inProgress/done/canceled), or title query."
    )]
    async fn find_issues(&self, Parameters(params): Parameters<FindIssuesParams>) -> String {
        let proxy_url = match self.resolve_optional_workspace(params.workspace.as_deref()).await {
            Ok(u) => u,
            Err(e) => return e,
        };
        let limit = params.limit.clamp(1, 200) as u64;
        match tools::find_issues(
            &self.http_client,
            &proxy_url,
            params.component.as_deref(),
            params.status,
            params.query.as_deref(),
            limit,
        )
        .await
        {
            Ok(issues) => tools::render_issue_summaries(&issues),
            Err(e) => e,
        }
    }

    #[tool(
        name = "huly_get_issue",
        description = "Get a single tracker issue by identifier (e.g. 'MUH-3') with linked relations."
    )]
    async fn get_issue(&self, Parameters(params): Parameters<GetIssueParams>) -> String {
        let proxy_url = match self.resolve_optional_workspace(params.workspace.as_deref()).await {
            Ok(u) => u,
            Err(e) => return e,
        };
        match tools::get_issue(&self.http_client, &proxy_url, &params.identifier).await {
            Ok(Some(v)) => serde_json::to_string_pretty(&v).unwrap_or_else(|e| format!("Error: {e}")),
            Ok(None) => format!("Issue {} not found.", params.identifier),
            Err(e) => e,
        }
    }

    #[tool(
        name = "huly_create_issue",
        description = "Create a new tracker issue. Accepts markdown in 'description' — it is uploaded to the Huly collaborator service so it renders correctly in the UI. If 'project' is omitted, the only project in the workspace is used (errors if multiple)."
    )]
    async fn create_issue(&self, Parameters(params): Parameters<CreateIssueParams>) -> String {
        let proxy_url = match self.resolve_optional_workspace(params.workspace.as_deref()).await {
            Ok(u) => u,
            Err(e) => return e,
        };
        let project = match tools::resolve_project(&self.http_client, &proxy_url, params.project.as_deref()).await {
            Ok(p) => p,
            Err(e) => return e,
        };
        let desc_md = if params.description.is_empty() {
            None
        } else {
            Some(params.description.as_str())
        };
        match tools::create_issue_in_project(
            &self.http_client,
            &proxy_url,
            &project,
            &params.title,
            desc_md,
            params.status,
            params.priority,
            params.component.as_deref(),
        )
        .await
        {
            Ok((id, identifier)) => format!(
                "Created issue {}: \"{}\" (ID: {})",
                identifier, params.title, id
            ),
            Err(e) => e,
        }
    }

    #[tool(
        name = "huly_update_issue",
        description = "Update an existing tracker issue by identifier. Only provided fields are changed. 'description' accepts markdown and is uploaded via the Huly collaborator service."
    )]
    async fn update_issue(&self, Parameters(params): Parameters<UpdateIssueParams>) -> String {
        let proxy_url = match self.resolve_optional_workspace(params.workspace.as_deref()).await {
            Ok(u) => u,
            Err(e) => return e,
        };
        let identifier = params.identifier.clone();
        match tools::update_issue(
            &self.http_client,
            &proxy_url,
            &identifier,
            params.title.as_deref(),
            params.description.as_deref(),
            params.status,
            params.priority,
            params.component.as_deref(),
        )
        .await
        {
            Ok(Some(changed)) if changed.is_empty() => {
                format!("No changes specified for {}.", identifier)
            }
            Ok(Some(changed)) => format!("Updated {}: {}", identifier, changed.join(", ")),
            Ok(None) => format!("Issue {} not found.", identifier),
            Err(e) => e,
        }
    }

    #[tool(
        name = "huly_upload_markup",
        description = "Upload markdown to the Huly collaborator service for an object's attribute (e.g. an issue 'description'). Returns the MarkupBlobRef to store on the object. Use this when you need to set rich-text content on a doc that already exists, or when an automated flow needs the ref to embed elsewhere."
    )]
    async fn upload_markup(&self, Parameters(params): Parameters<UploadMarkupParams>) -> String {
        let proxy_url = match self.resolve_optional_workspace(params.workspace.as_deref()).await {
            Ok(u) => u,
            Err(e) => return e,
        };
        match self
            .http_client
            .upload_markup(
                &proxy_url,
                &params.object_class,
                &params.object_id,
                &params.object_attr,
                &params.markdown,
            )
            .await
        {
            Ok(markup_ref) => serde_json::json!({"ref": markup_ref}).to_string(),
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(
        name = "huly_fetch_markup",
        description = "Fetch rich-text markup for an object attribute from the Huly collaborator. format='markdown' (default) is human-readable but lossy on round-trip; format='prosemirror' returns the raw ProseMirror JSON and is lossless. Pass sourceRef when you already have the blob reference; otherwise the bridge resolves it from the object."
    )]
    async fn fetch_markup(&self, Parameters(params): Parameters<FetchMarkupParams>) -> String {
        let proxy_url = match self.resolve_optional_workspace(params.workspace.as_deref()).await {
            Ok(u) => u,
            Err(e) => return e,
        };
        match self
            .http_client
            .fetch_markup(
                &proxy_url,
                &params.object_class,
                &params.object_id,
                &params.object_attr,
                params.source_ref.as_deref(),
                &params.format,
            )
            .await
        {
            Ok(resp) => serde_json::to_string_pretty(&resp).unwrap_or_else(|e| format!("Error: {e}")),
            Err(e) => format!("Error: {e}"),
        }
    }

    #[tool(
        name = "huly_create_component",
        description = "Create a tracker component in a project. Skips creation if a component with the same label already exists."
    )]
    async fn create_component(&self, Parameters(params): Parameters<CreateComponentParams>) -> String {
        let proxy_url = match self.resolve_optional_workspace(params.workspace.as_deref()).await {
            Ok(u) => u,
            Err(e) => return e,
        };
        let project = match tools::resolve_project(&self.http_client, &proxy_url, params.project.as_deref()).await {
            Ok(p) => p,
            Err(e) => return e,
        };
        match tools::create_component(
            &self.http_client,
            &proxy_url,
            &project,
            &params.label,
            &params.description,
        )
        .await
        {
            Ok(tools::ComponentResult::Existing { id, label }) => format!(
                "Component \"{}\" already exists (ID: {}). Skipped.",
                label, id
            ),
            Ok(tools::ComponentResult::Created { id, label }) => {
                format!("Created component \"{}\" (ID: {})", label, id)
            }
            Err(e) => e,
        }
    }

    #[tool(
        name = "huly_link_issue_to_card",
        description = "Link an issue to a card via a relation (module / entity / flow / compliance / decision). Relation IDs default to Muhasebot; override via [mcp.catalog.relations] config."
    )]
    async fn link_issue_to_card(&self, Parameters(params): Parameters<LinkIssueToCardParams>) -> String {
        let proxy_url = match self.resolve_optional_workspace(params.workspace.as_deref()).await {
            Ok(u) => u,
            Err(e) => return e,
        };
        let issue_ident = params.issue_identifier.clone();
        let card_id = params.card_id.clone();
        let rel_label = params.relation_type.name();
        match tools::link_issue_to_card(
            &self.http_client,
            &proxy_url,
            &self.catalog,
            &issue_ident,
            &card_id,
            params.relation_type,
        )
        .await
        {
            Ok(tools::LinkResult::IssueNotFound) => format!("Issue {} not found.", issue_ident),
            Ok(tools::LinkResult::AlreadyLinked { id }) => format!(
                "Relation already exists between {} and card {} ({}). Skipped. Existing relation ID: {}",
                issue_ident, card_id, rel_label, id
            ),
            Ok(tools::LinkResult::Created { id }) => format!(
                "Linked {} -> card {} ({}). Relation ID: {}",
                issue_ident, card_id, rel_label, id
            ),
            Err(e) => e,
        }
    }

    #[tool(
        name = "huly_create_project",
        description = "Create a tracker project from a local README.md (first '# heading' becomes name; first non-empty line after becomes description). Note: this creates a TRACKER PROJECT inside an existing Huly workspace, NOT a new workspace (upstream tool was misleadingly named 'create_workspace')."
    )]
    async fn create_project(&self, Parameters(params): Parameters<CreateProjectParams>) -> String {
        let proxy_url = match self.resolve_optional_workspace(params.workspace.as_deref()).await {
            Ok(u) => u,
            Err(e) => return e,
        };
        let content = match std::fs::read_to_string(&params.readme_path) {
            Ok(c) => c,
            Err(e) => return format!("Error reading README at '{}': {}", params.readme_path, e),
        };
        let (name, description) = tools::parse_readme(&content);
        let identifier = params
            .identifier
            .unwrap_or_else(|| tools::derive_identifier(&name));
        match tools::create_project(&self.http_client, &proxy_url, &name, &identifier, &description).await {
            Ok(id) => serde_json::to_string_pretty(&serde_json::json!({
                "id": id,
                "name": name,
                "identifier": identifier,
                "description": description,
            }))
            .unwrap_or_else(|e| format!("Error: {e}")),
            Err(e) => e,
        }
    }

    #[tool(
        name = "huly_sync_status",
        description = "Compare local docs/ files against the last sync state. Reports new files, modified files, deleted files. Reads sync state from .huly-sync-state.json in the configured working directory."
    )]
    async fn sync_status(&self, _params: Parameters<SyncStatusParams>) -> String {
        let Some(runner) = self.sync_runner.as_ref() else {
            return SyncRunner::not_configured_error();
        };
        match runner.status().await {
            Ok(report) => serde_json::to_string_pretty(&report)
                .unwrap_or_else(|e| format!("Error serialising status: {e}")),
            Err(crate::sync::SyncError::ParseStatus { raw, source }) => format!(
                "warning: status output was not valid JSON ({source}). Raw output:\n{raw}"
            ),
            Err(e) => format!("Sync status failed: {e}"),
        }
    }

    #[tool(
        name = "huly_sync_cards",
        description = "Run the card sync pipeline: Enums -> MasterTags -> Associations -> Cards -> Binaries -> Relations. Pushes local YAML/Markdown specs to Huly. Set dry_run=true to preview without changes."
    )]
    async fn sync_cards(&self, Parameters(params): Parameters<SyncCardsParams>) -> String {
        let Some(runner) = self.sync_runner.as_ref() else {
            return SyncRunner::not_configured_error();
        };
        match runner.sync(params.dry_run).await {
            Ok(out) => {
                let filtered = SyncRunner::filter_sync_output(&out.stdout);
                let mut text = format!(
                    "Sync {}complete:\n{}",
                    if params.dry_run { "(DRY RUN) " } else { "" },
                    filtered
                );
                if !out.stderr.trim().is_empty() {
                    text.push_str("\n--- stderr ---\n");
                    text.push_str(out.stderr.trim());
                }
                text
            }
            Err(crate::sync::SyncError::NonZeroExit { code, stderr_tail }) => {
                format!("Sync failed (exit {code}):\n{stderr_tail}")
            }
            Err(e) => format!("Sync failed: {e}"),
        }
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for HulyMcpServer {}

#[cfg(test)]
mod tests {
    use super::*;
    use huly_common::announcement::BridgeAnnouncement;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn make_announcement(workspace: &str, proxy_url: &str, ready: bool) -> BridgeAnnouncement {
        BridgeAnnouncement {
            workspace: workspace.into(),
            proxy_url: proxy_url.into(),
            huly_connected: true,
            nats_connected: true,
            ready,
            uptime_secs: 0,
            version: "0.1.0".into(),
            timestamp: 0,
        }
    }

    fn make_server(registry: BridgeRegistry) -> HulyMcpServer {
        HulyMcpServer::new(registry, BridgeHttpClient::new(None))
    }

    async fn make_server_with_mock(mock: &MockServer) -> HulyMcpServer {
        let registry = BridgeRegistry::new();
        registry
            .update(make_announcement("ws1", &mock.uri(), true))
            .await;
        HulyMcpServer::new(registry, BridgeHttpClient::new(None))
    }

    #[tokio::test]
    async fn resolve_proxy_url_returns_url_when_ready() {
        let registry = BridgeRegistry::new();
        registry
            .update(make_announcement("ws1", "http://bridge:9090", true))
            .await;
        let server = make_server(registry);
        assert_eq!(
            server.resolve_proxy_url("ws1").await.unwrap(),
            "http://bridge:9090"
        );
    }

    #[tokio::test]
    async fn resolve_proxy_url_errors_when_not_ready() {
        let registry = BridgeRegistry::new();
        registry
            .update(make_announcement("ws1", "http://bridge:9090", false))
            .await;
        let server = make_server(registry);
        let err = server.resolve_proxy_url("ws1").await.unwrap_err();
        assert!(err.contains("not ready"));
    }

    #[tokio::test]
    async fn resolve_proxy_url_errors_when_not_found() {
        let registry = BridgeRegistry::new();
        let server = make_server(registry);
        let err = server.resolve_proxy_url("missing").await.unwrap_err();
        assert!(err.contains("not found"));
    }

    #[tokio::test]
    async fn list_workspaces_empty_returns_message() {
        let registry = BridgeRegistry::new();
        let server = make_server(registry);
        let result = server
            .list_workspaces(Parameters(ListWorkspacesParams {}))
            .await;
        assert!(result.contains("No workspaces discovered"));
    }

    #[tokio::test]
    async fn list_workspaces_returns_json() {
        let registry = BridgeRegistry::new();
        registry
            .update(make_announcement("ws1", "http://bridge:9090", true))
            .await;
        let server = make_server(registry);
        let result = server
            .list_workspaces(Parameters(ListWorkspacesParams {}))
            .await;
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(parsed.is_array());
        assert_eq!(parsed.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn find_tool_returns_results() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [{"_id": "d1", "_class": "core:class:Issue"}],
                "total": 1
            })))
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .find(Parameters(FindParams {
                workspace: "ws1".into(),
                class: "core:class:Issue".into(),
                query: serde_json::json!({}),
                limit: None,
            }))
            .await;
        assert!(result.contains("d1"));
        assert!(result.contains("total"));
    }

    #[tokio::test]
    async fn find_tool_unknown_workspace_returns_error() {
        let server = make_server(BridgeRegistry::new());
        let result = server
            .find(Parameters(FindParams {
                workspace: "missing".into(),
                class: "cls".into(),
                query: serde_json::json!({}),
                limit: None,
            }))
            .await;
        assert!(result.contains("not found"));
    }

    #[tokio::test]
    async fn get_tool_returns_doc() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find-one"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"_id": "d1", "_class": "cls"})),
            )
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .get(Parameters(GetParams {
                workspace: "ws1".into(),
                class: "cls".into(),
                query: serde_json::json!({"_id": "d1"}),
            }))
            .await;
        assert!(result.contains("d1"));
    }

    #[tokio::test]
    async fn get_tool_returns_null_when_not_found() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find-one"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::Value::Null))
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .get(Parameters(GetParams {
                workspace: "ws1".into(),
                class: "cls".into(),
                query: serde_json::json!({}),
            }))
            .await;
        assert_eq!(result, "null");
    }

    #[tokio::test]
    async fn create_tool_returns_id() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/create"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "new-123"})),
            )
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .create(Parameters(CreateParams {
                workspace: "ws1".into(),
                class: "cls".into(),
                space: "sp".into(),
                attributes: serde_json::json!({"title": "test"}),
            }))
            .await;
        assert!(result.contains("new-123"));
    }

    #[tokio::test]
    async fn update_tool_returns_tx_result() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/update"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"success": true, "id": "d1"})),
            )
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .update(Parameters(UpdateParams {
                workspace: "ws1".into(),
                class: "cls".into(),
                space: "sp".into(),
                id: "d1".into(),
                operations: serde_json::json!({"title": "updated"}),
            }))
            .await;
        assert!(result.contains("success"));
        assert!(result.contains("true"));
    }

    #[tokio::test]
    async fn delete_tool_returns_tx_result() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/delete"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"success": true})),
            )
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .delete(Parameters(DeleteParams {
                workspace: "ws1".into(),
                class: "cls".into(),
                space: "sp".into(),
                id: "d1".into(),
            }))
            .await;
        assert!(result.contains("success"));
    }

    #[tokio::test]
    async fn tool_returns_error_on_bridge_failure() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .find(Parameters(FindParams {
                workspace: "ws1".into(),
                class: "cls".into(),
                query: serde_json::json!({}),
                limit: None,
            }))
            .await;
        assert!(result.contains("Error"));
    }

    #[test]
    fn validate_proxy_url_accepts_http() {
        assert!(super::validate_proxy_url("http://bridge:9090").is_ok());
    }

    #[test]
    fn validate_proxy_url_accepts_https() {
        assert!(super::validate_proxy_url("https://bridge.internal:9090").is_ok());
    }

    #[test]
    fn validate_proxy_url_rejects_file_scheme() {
        assert!(super::validate_proxy_url("file:///etc/passwd").is_err());
    }

    #[test]
    fn validate_proxy_url_rejects_empty() {
        assert!(super::validate_proxy_url("").is_err());
    }

    #[test]
    fn validate_proxy_url_rejects_bare_host() {
        assert!(super::validate_proxy_url("bridge:9090").is_err());
    }

    // -- Phase 5 tool tests --

    use wiremock::matchers::body_partial_json;

    fn empty_find_body() -> serde_json::Value {
        serde_json::json!({"docs": [], "total": 0})
    }

    /// Mount a "match anything POST /api/v1/find" handler returning empty.
    /// More specific mocks added with body_partial_json take precedence due
    /// to wiremock's longest-match strategy when expectations match.
    async fn mount_default_find_empty(mock: &MockServer) {
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty_find_body()))
            .mount(mock)
            .await;
    }

    #[tokio::test]
    async fn discover_combines_all_collections() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .and(body_partial_json(serde_json::json!({"class": "tracker:class:Project"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [{"_id": "p1", "_class": "tracker:class:Project", "name": "Muh", "identifier": "MUH"}],
                "total": 1
            })))
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .and(body_partial_json(serde_json::json!({"class": "tracker:class:Issue"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [
                    {"_id": "i1", "_class": "tracker:class:Issue", "status": "tracker:status:Todo"},
                    {"_id": "i2", "_class": "tracker:class:Issue", "status": "tracker:status:Todo"},
                    {"_id": "i3", "_class": "tracker:class:Issue", "status": "tracker:status:Done"}
                ],
                "total": 3
            })))
            .mount(&mock)
            .await;
        mount_default_find_empty(&mock).await;

        let server = make_server_with_mock(&mock).await;
        let result = server.discover(Parameters(DiscoverParams { workspace: None })).await;
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["projects"].as_array().unwrap().len(), 1);
        assert_eq!(v["issueSummary"]["total"], 3);
        assert_eq!(v["issueSummary"]["byStatus"]["tracker:status:Todo"], 2);
        assert_eq!(v["issueSummary"]["byStatus"]["tracker:status:Done"], 1);
    }

    #[tokio::test]
    async fn discover_errors_when_no_workspace() {
        let server = make_server(BridgeRegistry::new());
        let result = server.discover(Parameters(DiscoverParams { workspace: None })).await;
        assert!(result.contains("No workspaces"));
    }

    #[tokio::test]
    async fn find_cards_filters_by_query_and_sorts() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [
                    {"_id": "c1", "_class": "x", "title": "Zebra"},
                    {"_id": "c2", "_class": "x", "title": "Apple invoice"},
                    {"_id": "c3", "_class": "x", "title": "Banana"}
                ],
                "total": 3
            })))
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .find_cards(Parameters(FindCardsParams {
                workspace: None,
                card_type: Some(CardType::ModuleSpec),
                query: Some("invoice".into()),
                limit: 50,
            }))
            .await;
        assert!(result.starts_with("Found 1 cards:"));
        assert!(result.contains("Apple invoice"));
        assert!(!result.contains("Zebra"));
    }

    #[tokio::test]
    async fn find_issues_returns_summaries_and_skips_missing_number() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [
                    {"_id": "i1", "_class": "tracker:class:Issue", "title": "B", "number": 2, "status": "tracker:status:Todo", "priority": 3},
                    {"_id": "i2", "_class": "tracker:class:Issue", "title": "A", "number": 1, "status": "tracker:status:Todo", "priority": 2},
                    {"_id": "i3", "_class": "tracker:class:Issue", "title": "missing-number"}
                ],
                "total": 3
            })))
            .mount(&mock)
            .await;
        let server = make_server_with_mock(&mock).await;
        let result = server
            .find_issues(Parameters(FindIssuesParams {
                workspace: None, project: None, component: None,
                status: None, query: None, limit: 50,
            }))
            .await;
        assert!(result.starts_with("Found 2 issues:"));
        // Order: number 1 ("A") before number 2 ("B").
        let pos_a = result.find("\"title\": \"A\"").unwrap();
        let pos_b = result.find("\"title\": \"B\"").unwrap();
        assert!(pos_a < pos_b);
    }

    #[tokio::test]
    async fn get_issue_returns_not_found_text() {
        let mock = MockServer::start().await;
        mount_default_find_empty(&mock).await;
        let server = make_server_with_mock(&mock).await;
        let result = server
            .get_issue(Parameters(GetIssueParams {
                workspace: None,
                identifier: "MUH-99".into(),
            }))
            .await;
        assert_eq!(result, "Issue MUH-99 not found.");
    }

    #[tokio::test]
    async fn get_issue_includes_relations() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .and(body_partial_json(serde_json::json!({"class": "tracker:class:Issue"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [{"_id": "issue-1", "_class": "tracker:class:Issue", "identifier": "MUH-1", "title": "T"}],
                "total": 1
            })))
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .and(body_partial_json(serde_json::json!({"class": "core:class:Relation", "query": {"docA": "issue-1"}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [{"_id": "r1", "_class": "core:class:Relation", "docA": "issue-1", "docB": "card-9", "association": "assoc-x"}],
                "total": 1
            })))
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .and(body_partial_json(serde_json::json!({"class": "core:class:Relation", "query": {"docB": "issue-1"}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(empty_find_body()))
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .get_issue(Parameters(GetIssueParams {
                workspace: None,
                identifier: "MUH-1".into(),
            }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        let rels = v["linkedRelations"].as_array().unwrap();
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0]["direction"], "outgoing");
        assert_eq!(rels[0]["linkedDoc"], "card-9");
    }

    #[tokio::test]
    async fn create_issue_uses_apply_if_for_atomic_create() {
        // Tier B / QA #24: sequence-bump + issue creation bundled into a single
        // TxApplyIf scope — no more separate update + find-one + add-collection.
        //
        // Flow:
        //   1. find-one project (sequence=10)
        //   2. POST /api/v1/apply-if with scope + match (sequence=10) + 2 sub-txes
        //   → returns {success: true, serverTime: ...}
        //   → returns (issue_id, "MUH-11")
        let mock = MockServer::start().await;
        // Step 1 — initial project lookup via /api/v1/find (resolve_project).
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .and(body_partial_json(serde_json::json!({"class": "tracker:class:Project"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [{"_id": "proj-1", "_class": "tracker:class:Project", "identifier": "MUH", "sequence": 10}],
                "total": 1
            })))
            .mount(&mock)
            .await;
        // Step 2 — find-one inside create_issue_in_project (read current sequence).
        Mock::given(method("POST"))
            .and(path("/api/v1/find-one"))
            .and(body_partial_json(serde_json::json!({"class": "tracker:class:Project"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "_id": "proj-1", "_class": "tracker:class:Project", "identifier": "MUH", "sequence": 10
            })))
            .mount(&mock)
            .await;
        // Step 3 — apply-if: one call, scope matches project.
        Mock::given(method("POST"))
            .and(path("/api/v1/apply-if"))
            .and(body_partial_json(serde_json::json!({
                "scope": "tracker:project:proj-1:issue-create",
                "txes": [
                    {"_class": "core:class:TxUpdateDoc", "objectId": "proj-1", "objectClass": "tracker:class:Project"},
                    {"_class": "core:class:TxCreateDoc", "objectClass": "tracker:class:Issue", "collection": "subIssues", "attachedToClass": "tracker:class:Issue"}
                ]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"success": true, "serverTime": 99})))
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .create_issue(Parameters(CreateIssueParams {
                workspace: None, project: None,
                title: "New work".into(), description: "details".into(),
                component: None, status: IssueStatus::Todo, priority: 3,
            }))
            .await;
        assert!(result.contains("Created issue MUH-11"), "got: {result}");
        assert!(result.contains("New work"));
    }

    #[tokio::test]
    async fn create_issue_retries_on_apply_if_contention() {
        // When apply_if returns success:false (scope contended), we retry.
        // First attempt: contended (success=false); second attempt: succeeds.
        let mock = MockServer::start().await;

        // resolve_project — one project found
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [{"_id": "proj-1", "_class": "tracker:class:Project", "identifier": "MUH", "sequence": 5}],
                "total": 1
            })))
            .mount(&mock)
            .await;

        // Both attempts read sequence=5 (find-one) — wiremock returns same canned
        Mock::given(method("POST"))
            .and(path("/api/v1/find-one"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "_id": "proj-1", "_class": "tracker:class:Project", "identifier": "MUH", "sequence": 5
            })))
            .mount(&mock)
            .await;

        // First apply-if call → contended
        // Second apply-if call → success
        // wiremock matches by path only; we use .up_to_n_times to differentiate
        Mock::given(method("POST"))
            .and(path("/api/v1/apply-if"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"success": false, "serverTime": 0})))
            .up_to_n_times(1)
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/apply-if"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"success": true, "serverTime": 1})))
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .create_issue(Parameters(CreateIssueParams {
                workspace: None, project: None,
                title: "Retried work".into(), description: "desc".into(),
                component: None, status: IssueStatus::Todo, priority: 3,
            }))
            .await;
        assert!(result.contains("Created issue MUH-6"), "got: {result}");
    }

    /// Race-freedom property (Tier B / QA #24):
    /// The `TxApplyIf` scope guarantees that the server serializes concurrent
    /// requests with the same scope. A caller reading sequence=N and bundling
    /// $inc + create into TxApplyIf(match={sequence:N}) can only succeed if
    /// no other caller has incremented since the read — giving contiguous
    /// identifiers across concurrent callers.
    #[test]
    #[ignore = "documentation-only: see QA #24 for the contiguity guarantee"]
    fn create_issue_apply_if_guarantees_contiguous_identifiers() {}

    #[tokio::test]
    async fn update_issue_returns_changed_fields() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [{
                    "_id": "issue-1",
                    "_class": "tracker:class:Issue",
                    "space": "proj-1",
                    "identifier": "MUH-1",
                    "attachedTo": "tracker:ids:NoParent",
                    "attachedToClass": "tracker:class:Issue",
                    "collection": "subIssues"
                }],
                "total": 1
            })))
            .mount(&mock)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/update-collection"))
            .and(body_partial_json(serde_json::json!({
                "class": "tracker:class:Issue",
                "space": "proj-1",
                "id": "issue-1",
                "attachedTo": "tracker:ids:NoParent",
                "attachedToClass": "tracker:class:Issue",
                "collection": "subIssues"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"success": true, "id": "issue-1"})))
            .mount(&mock)
            .await;
        let server = make_server_with_mock(&mock).await;
        let result = server
            .update_issue(Parameters(UpdateIssueParams {
                workspace: None, identifier: "MUH-1".into(),
                title: Some("renamed".into()), description: None,
                status: Some(IssueStatus::InProgress), priority: None, component: None,
            }))
            .await;
        assert!(result.contains("Updated MUH-1"));
        assert!(result.contains("title"));
        assert!(result.contains("status"));
    }

    #[tokio::test]
    async fn create_component_skips_existing() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .and(body_partial_json(serde_json::json!({"class": "tracker:class:Project"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [{"_id": "proj-1", "_class": "tracker:class:Project", "identifier": "MUH"}],
                "total": 1
            })))
            .mount(&mock)
            .await;
        // notMatch precondition trips → server rejects the create.
        Mock::given(method("POST"))
            .and(path("/api/v1/apply-if"))
            .and(body_partial_json(serde_json::json!({
                "notMatches": [{"_class": "tracker:class:Component", "query": {"label": "Auth"}}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": false, "serverTime": 0
            })))
            .mount(&mock)
            .await;
        // Resolver lookup returns the existing dupe (find_one hits /find-one, not /find).
        Mock::given(method("POST"))
            .and(path("/api/v1/find-one"))
            .and(body_partial_json(serde_json::json!({"class": "tracker:class:Component"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "_id": "comp-1", "_class": "tracker:class:Component", "label": "Auth"
            })))
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .create_component(Parameters(CreateComponentParams {
                workspace: None, project: None,
                label: "Auth".into(), description: "".into(),
            }))
            .await;
        assert!(result.contains("already exists"));
        assert!(result.contains("comp-1"));
    }

    #[tokio::test]
    async fn create_component_creates_atomically_when_absent() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .and(body_partial_json(serde_json::json!({"class": "tracker:class:Project"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [{"_id": "proj-1", "_class": "tracker:class:Project", "identifier": "MUH"}],
                "total": 1
            })))
            .mount(&mock)
            .await;
        // The apply_if call must carry a notMatch + a TxCreateDoc (no collection fields).
        Mock::given(method("POST"))
            .and(path("/api/v1/apply-if"))
            .and(body_partial_json(serde_json::json!({
                "notMatches": [{"_class": "tracker:class:Component", "query": {"space": "proj-1", "label": "Frontend"}}],
                "txes": [{"_class": "core:class:TxCreateDoc", "objectClass": "tracker:class:Component"}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true, "serverTime": 1
            })))
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .create_component(Parameters(CreateComponentParams {
                workspace: None, project: None,
                label: "Frontend".into(), description: "ui".into(),
            }))
            .await;
        assert!(result.starts_with("Created component"));
        assert!(result.contains("Frontend"));
    }

    #[tokio::test]
    async fn link_issue_to_card_creates_relation() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .and(body_partial_json(serde_json::json!({"class": "tracker:class:Issue"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [{"_id": "issue-1", "_class": "tracker:class:Issue", "identifier": "MUH-3"}],
                "total": 1
            })))
            .mount(&mock)
            .await;
        // Atomic create: notMatch={no Relation linking docA/docB/assoc} + TxCreateDoc.
        Mock::given(method("POST"))
            .and(path("/api/v1/apply-if"))
            .and(body_partial_json(serde_json::json!({
                "notMatches": [{"_class": "core:class:Relation", "query": {"docA": "issue-1", "docB": "card-7"}}],
                "txes": [{"_class": "core:class:TxCreateDoc", "objectClass": "core:class:Relation"}]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": true, "serverTime": 1
            })))
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .link_issue_to_card(Parameters(LinkIssueToCardParams {
                workspace: None,
                issue_identifier: "MUH-3".into(),
                card_id: "card-7".into(),
                relation_type: RelationType::Module,
            }))
            .await;
        assert!(result.contains("Linked MUH-3 -> card card-7 (module)"));
    }

    #[tokio::test]
    async fn link_issue_to_card_already_linked() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/find"))
            .and(body_partial_json(serde_json::json!({"class": "tracker:class:Issue"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "docs": [{"_id": "issue-1", "_class": "tracker:class:Issue", "identifier": "MUH-3"}],
                "total": 1
            })))
            .mount(&mock)
            .await;
        // notMatch trips → server rejects.
        Mock::given(method("POST"))
            .and(path("/api/v1/apply-if"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "success": false, "serverTime": 0
            })))
            .mount(&mock)
            .await;
        // Resolver returns existing Relation.
        Mock::given(method("POST"))
            .and(path("/api/v1/find-one"))
            .and(body_partial_json(serde_json::json!({"class": "core:class:Relation"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "_id": "rel-existing", "_class": "core:class:Relation"
            })))
            .mount(&mock)
            .await;
        let server = make_server_with_mock(&mock).await;
        let result = server
            .link_issue_to_card(Parameters(LinkIssueToCardParams {
                workspace: None,
                issue_identifier: "MUH-3".into(),
                card_id: "card-7".into(),
                relation_type: RelationType::Flow,
            }))
            .await;
        assert!(result.contains("already exists"));
        assert!(result.contains("rel-existing"));
    }

    #[tokio::test]
    async fn upload_markup_tool_returns_ref_json() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/upload-markup"))
            .and(body_partial_json(serde_json::json!({
                "objectClass": "tracker:class:Issue",
                "objectId": "issue-1",
                "objectAttr": "description",
                "markdown": "# Heading"
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"ref": "blob-xyz"})),
            )
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .upload_markup(Parameters(UploadMarkupParams {
                workspace: None,
                object_class: "tracker:class:Issue".into(),
                object_id: "issue-1".into(),
                object_attr: "description".into(),
                markdown: "# Heading".into(),
            }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["ref"], "blob-xyz");
    }

    #[tokio::test]
    async fn upload_markup_tool_propagates_bridge_error() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/upload-markup"))
            .respond_with(ResponseTemplate::new(503).set_body_string("collaborator down"))
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .upload_markup(Parameters(UploadMarkupParams {
                workspace: None,
                object_class: "c".into(),
                object_id: "id".into(),
                object_attr: "description".into(),
                markdown: "x".into(),
            }))
            .await;
        assert!(result.starts_with("Error:"), "got: {result}");
        assert!(result.contains("503"));
    }

    #[tokio::test]
    async fn upload_markup_tool_errors_when_no_workspace() {
        let server = make_server(BridgeRegistry::new());
        let result = server
            .upload_markup(Parameters(UploadMarkupParams {
                workspace: None,
                object_class: "c".into(),
                object_id: "id".into(),
                object_attr: "description".into(),
                markdown: "x".into(),
            }))
            .await;
        assert!(result.contains("No workspaces"));
    }

    #[tokio::test]
    async fn fetch_markup_tool_returns_content_and_format() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/fetch-markup"))
            .and(body_partial_json(serde_json::json!({
                "objectClass": "tracker:class:Issue",
                "objectId": "issue-1",
                "objectAttr": "description",
                "sourceRef": "blob-abc",
                "format": "markdown"
            })))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "content": "# Heading",
                    "format": "markdown"
                })),
            )
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .fetch_markup(Parameters(FetchMarkupParams {
                workspace: None,
                object_class: "tracker:class:Issue".into(),
                object_id: "issue-1".into(),
                object_attr: "description".into(),
                source_ref: Some("blob-abc".into()),
                format: "markdown".into(),
            }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["content"], "# Heading");
        assert_eq!(v["format"], "markdown");
    }

    #[tokio::test]
    async fn fetch_markup_tool_supports_prosemirror_format() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/fetch-markup"))
            .and(body_partial_json(serde_json::json!({"format": "prosemirror"})))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "content": "{\"type\":\"doc\",\"content\":[]}",
                    "format": "prosemirror"
                })),
            )
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .fetch_markup(Parameters(FetchMarkupParams {
                workspace: None,
                object_class: "tracker:class:Issue".into(),
                object_id: "issue-1".into(),
                object_attr: "description".into(),
                source_ref: None,
                format: "prosemirror".into(),
            }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["format"], "prosemirror");
        assert!(v["content"].as_str().unwrap().contains("\"type\":\"doc\""));
    }

    #[tokio::test]
    async fn fetch_markup_tool_propagates_bridge_error() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/fetch-markup"))
            .respond_with(ResponseTemplate::new(503).set_body_string("collaborator down"))
            .mount(&mock)
            .await;

        let server = make_server_with_mock(&mock).await;
        let result = server
            .fetch_markup(Parameters(FetchMarkupParams {
                workspace: None,
                object_class: "c".into(),
                object_id: "id".into(),
                object_attr: "description".into(),
                source_ref: None,
                format: "markdown".into(),
            }))
            .await;
        assert!(result.starts_with("Error:"));
        assert!(result.contains("503"));
    }

    #[tokio::test]
    async fn create_project_reads_readme_and_creates() {
        let mock = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/create"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "proj-new"})))
            .mount(&mock)
            .await;
        let dir = tempdir_simple();
        let readme_path = format!("{}/README.md", dir);
        std::fs::write(&readme_path, "# Phase Five MCP\n\nHandles MCP tooling.\n").unwrap();

        let server = make_server_with_mock(&mock).await;
        let result = server
            .create_project(Parameters(CreateProjectParams {
                workspace: None,
                readme_path,
                identifier: None,
            }))
            .await;
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["id"], "proj-new");
        assert_eq!(v["name"], "Phase Five MCP");
        assert_eq!(v["identifier"], "PFM");
        assert_eq!(v["description"], "Handles MCP tooling.");
    }

    // -- Phase 6A sync tool tests --

    use crate::config::SyncConfig;

    fn fixture_path(name: &str) -> std::path::PathBuf {
        let manifest = env!("CARGO_MANIFEST_DIR");
        std::path::PathBuf::from(manifest)
            .join("tests/fixtures")
            .join(name)
    }

    fn server_with_sync(node_binary: std::path::PathBuf, working_dir: std::path::PathBuf) -> HulyMcpServer {
        let runner = crate::sync::SyncRunner::new(&SyncConfig {
            script_path: std::path::PathBuf::from("/fake/sync/dist/index.js"),
            node_binary: node_binary.to_string_lossy().into_owned(),
            working_dir,
            timeout_secs: 5,
        });
        HulyMcpServer::new(BridgeRegistry::new(), BridgeHttpClient::new(None))
            .with_sync_runner(Some(runner))
    }

    #[tokio::test]
    async fn sync_status_unconfigured_returns_helpful_error() {
        let server = make_server(BridgeRegistry::new());
        let result = server.sync_status(Parameters(SyncStatusParams {})).await;
        assert!(result.contains("script_path"));
        assert!(result.contains("[mcp.sync]"));
    }

    #[tokio::test]
    async fn sync_cards_unconfigured_returns_helpful_error() {
        let server = make_server(BridgeRegistry::new());
        let result = server
            .sync_cards(Parameters(SyncCardsParams { dry_run: false }))
            .await;
        assert!(result.contains("script_path"));
    }

    #[tokio::test]
    async fn sync_status_returns_json_summary() {
        let workdir = std::env::temp_dir().join(format!("huly-srv-status-{}", std::process::id()));
        std::fs::create_dir_all(&workdir).unwrap();
        let server = server_with_sync(fixture_path("fake_sync_status.sh"), workdir);
        let result = server.sync_status(Parameters(SyncStatusParams {})).await;
        let v: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["summary"], "2 changes detected.");
        assert_eq!(v["totalTracked"], 5);
    }

    #[tokio::test]
    async fn sync_cards_dry_run_renders_dry_run_prefix() {
        let workdir = std::env::temp_dir().join(format!("huly-srv-cards-{}", std::process::id()));
        std::fs::create_dir_all(&workdir).unwrap();
        let server = server_with_sync(fixture_path("fake_sync_ok.sh"), workdir);
        let result = server
            .sync_cards(Parameters(SyncCardsParams { dry_run: true }))
            .await;
        assert!(result.contains("Sync (DRY RUN) complete"));
        assert!(result.contains("DRY RUN"));
        // The "no document found" upstream noise must be filtered out.
        assert!(!result.contains("no document found"));
    }

    #[tokio::test]
    async fn sync_cards_failure_includes_exit_code() {
        let workdir = std::env::temp_dir().join(format!("huly-srv-fail-{}", std::process::id()));
        std::fs::create_dir_all(&workdir).unwrap();
        let server = server_with_sync(fixture_path("fake_sync_fail.sh"), workdir);
        let result = server
            .sync_cards(Parameters(SyncCardsParams { dry_run: false }))
            .await;
        assert!(result.contains("Sync failed"));
        assert!(result.contains("exit 7"));
        assert!(result.contains("connection refused"));
    }

    fn tempdir_simple() -> String {
        let d = std::env::temp_dir().join(format!(
            "huly-mcp-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d.to_string_lossy().into_owned()
    }
}
