//! Provider-neutral typed hierarchy contract (issue 641).
//!
//! Hierarchy is a directed parent-child tree edge backed by provider-native
//! fields (Redmine `parent_issue_id` / subtasks, GitLab Work Item hierarchy
//! widget). It is never a graph relation (`relates` / `blocks`), never Kanban
//! board membership, and never inferred from `@user` mentions or `#N`
//! cross-references. Closing a parent never cascades to children.

use serde::Serialize;

/// Native work-item kind. Redmine has a single issue kind; GitLab kinds stay
/// distinct so Epic/Issue/Task identities never collapse. Local has no native
/// hierarchy surface.
#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkItemKind {
    RedmineIssue,
    GitLabEpic,
    GitLabIssue,
    GitLabTask,
    LocalIssue,
}

impl WorkItemKind {
    #[allow(dead_code, clippy::wrong_self_convention)]
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RedmineIssue => "redmine_issue",
            Self::GitLabEpic => "gitlab_epic",
            Self::GitLabIssue => "gitlab_issue",
            Self::GitLabTask => "gitlab_task",
            Self::LocalIssue => "local_issue",
        }
    }
}

/// Cross-provider work-item identity: provider plus project plus numeric id
/// plus native kind. A bare numeric id is never assumed globally unique.
#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WorkItemRef {
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub id: u64,
    pub kind: WorkItemKind,
}

impl WorkItemRef {
    #[allow(dead_code)]
    #[must_use]
    pub fn redmine(project: Option<String>, id: u64) -> Self {
        Self {
            provider: "redmine".to_owned(),
            project,
            id,
            kind: WorkItemKind::RedmineIssue,
        }
    }

    #[allow(dead_code)]
    #[must_use]
    pub fn gitlab(project: Option<String>, id: u64, kind: WorkItemKind) -> Self {
        Self {
            provider: "gitlab".to_owned(),
            project,
            id,
            kind,
        }
    }

    #[allow(dead_code)]
    #[must_use]
    pub fn is_same_item(&self, other: &Self) -> bool {
        // Cross-scope identity is the full 4-tuple: provider, project scope,
        // external numeric id, and native item kind. A bare numeric id never
        // implies identity across projects, providers, or kinds.
        self.provider == other.provider
            && self.project == other.project
            && self.id == other.id
            && self.kind == other.kind
    }
}

/// Directed parent-child edge. Validation rejects only the self-parent shape;
/// capability/type support is checked by [`supported_parent_child`].
#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HierarchyEdge {
    pub parent: WorkItemRef,
    pub child: WorkItemRef,
}

impl HierarchyEdge {
    /// Reject a zero id or a parent that is the same item as the child.
    #[allow(dead_code)]
    pub fn validate(&self) -> Result<(), String> {
        if self.parent.id == 0 || self.child.id == 0 {
            return Err("hierarchy edge ids must be greater than zero".to_owned());
        }
        if self.parent.is_same_item(&self.child) {
            return Err("hierarchy parent cannot be the same item as the child".to_owned());
        }
        Ok(())
    }
}

/// Bounded read-only parent-with-children view. The parent artifact stays a
/// concise umbrella; each child keeps its own focused plan and status.
///
/// Reads are bounded: GitLab returns at most 50 direct children in one call
/// (see [`HierarchyPage`] for the explicit truncation indicator); Redmine
/// returns the complete child list.
#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HierarchyNode {
    pub item: WorkItemRef,
    pub parent: Option<WorkItemRef>,
    pub children: Vec<WorkItemRef>,
}

impl HierarchyNode {
    #[allow(dead_code)]
    #[must_use]
    pub fn parent_id(&self) -> Option<u64> {
        self.parent.as_ref().map(|parent| parent.id)
    }

    #[allow(dead_code)]
    #[must_use]
    pub fn children_ids(&self) -> Vec<u64> {
        self.children.iter().map(|child| child.id).collect()
    }
}

/// Bounded hierarchy view with an explicit truncation indicator. The child
/// list never grows past the provider bound; callers must surface
/// `children_truncated` instead of silently dropping children or fetching
/// further pages. Redmine always reports false (complete list); GitLab
/// reports the child connection `pageInfo.hasNextPage`.
#[allow(dead_code)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct HierarchyPage {
    pub node: HierarchyNode,
    pub children_truncated: bool,
}

/// Maximum direct children projected by `hierarchy get`. Reads are bounded
/// at the provider (GitLab `children(first: 50)`); longer native lists
/// (Redmine subtasks) are truncated to this bound at projection time with
/// the truncation indicator set.
#[allow(dead_code)]
pub const HIERARCHY_MAX_CHILDREN: usize = 50;

#[allow(dead_code)]
impl HierarchyPage {
    /// Cap the child list at [`HIERARCHY_MAX_CHILDREN`], setting the
    /// truncation indicator when the cap drops children. A page already at
    /// or under the bound (or already marked truncated) passes through
    /// with its indicator preserved.
    #[must_use]
    pub fn bounded(mut node: HierarchyNode, children_truncated: bool) -> Self {
        let truncated = children_truncated || node.children.len() > HIERARCHY_MAX_CHILDREN;
        node.children.truncate(HIERARCHY_MAX_CHILDREN);
        Self {
            node,
            children_truncated: truncated,
        }
    }
}

/// Native parent/child type support. Redmine issues nest; GitLab allows only
/// Epic-to-Issue and Issue-to-Task. Every other combination, including any
/// Local nesting, reports false so callers return a structured
/// `not_supported` instead of a relation fallback.
#[allow(dead_code)]
#[must_use]
pub const fn supported_parent_child(parent: &WorkItemKind, child: &WorkItemKind) -> bool {
    matches!(
        (parent, child),
        (WorkItemKind::RedmineIssue, WorkItemKind::RedmineIssue)
            | (WorkItemKind::GitLabEpic, WorkItemKind::GitLabIssue)
            | (WorkItemKind::GitLabIssue, WorkItemKind::GitLabTask)
    )
}

#[cfg(test)]
#[path = "hierarchy_tests.rs"]
mod tests;
