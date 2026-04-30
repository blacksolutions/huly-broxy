//! Platform-stable identifiers used by MCP tools.
//!
//! Workspace-local IDs (MasterTags, Associations) used to live here as
//! constants — they are now resolved per-workspace at runtime via the
//! bridge schema responder. See [`crate::mcp::schema_cache`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Issue status enum. Status IDs (returned by [`status_id`]) are stable
/// Huly model references and the same on every workspace.
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

pub const NO_PARENT: &str = "tracker:ids:NoParent";
pub const MODEL_SPACE: &str = "core:space:Model";
pub const TASK_TYPE_ISSUE: &str = "tracker:taskTypes:Issue";

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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn issue_status_name_round_trip() {
        for st in IssueStatus::all() {
            assert!(!st.name().is_empty());
        }
        assert_eq!(IssueStatus::Backlog.name(), "backlog");
        assert_eq!(IssueStatus::InProgress.name(), "inProgress");
    }

    #[test]
    fn no_parent_constant_pinned_to_upstream() {
        assert_eq!(NO_PARENT, "tracker:ids:NoParent");
        assert_eq!(MODEL_SPACE, "core:space:Model");
        assert_eq!(TASK_TYPE_ISSUE, "tracker:taskTypes:Issue");
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
