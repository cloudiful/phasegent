//! Redmine native parent/children reads and parent writes (issue 641).
//!
//! Reads use the native `parent` / `children` issue fields with
//! `GET /issues/:id.json?include=children` and never read `relations` for
//! hierarchy. Writes use the native `parent_issue_id` field with
//! `PUT /issues/:child.json`: setting a parent assigns the id, unsetting
//! sends an explicit null. Hierarchy never implies a `relates` edge.

use serde::Deserialize;
use serde::Serialize;

use crate::providers::api::ForgejoError;
use crate::providers::config::RedmineProvider;
use crate::providers::hierarchy::{HierarchyEdge, HierarchyNode, HierarchyPage, WorkItemRef};

const HIERARCHY_UPDATE: &str = "issue hierarchy update";

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct RedmineHierarchyResponse {
    issue: RedmineHierarchyIssue,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct RedmineHierarchyIssue {
    id: u64,
    #[serde(default)]
    parent: Option<RedmineParentRef>,
    #[serde(default)]
    children: Vec<RedmineChildRef>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct RedmineParentRef {
    id: u64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct RedmineChildRef {
    id: u64,
}

#[allow(dead_code)]
impl RedmineProvider {
    /// Fetch the native hierarchy view for one issue.
    pub fn get_hierarchy(&self, number: u64) -> Result<HierarchyNode, ForgejoError> {
        let params = [("include", "children".to_owned())];
        let response: RedmineHierarchyResponse =
            self.http
                .get(&self.issue_path(number), &params, "issue hierarchy get")?;
        Ok(response.issue.into_node(self.configured_project()))
    }

    /// Fetch the bounded hierarchy view for one issue. Redmine returns the
    /// complete child list, so the truncation indicator is always false
    /// here; callers that project the bounded child list cap it at
    /// [`HIERARCHY_MAX_CHILDREN`](crate::providers::hierarchy::HIERARCHY_MAX_CHILDREN).
    pub fn get_hierarchy_page(&self, number: u64) -> Result<HierarchyPage, ForgejoError> {
        Ok(HierarchyPage {
            node: self.get_hierarchy(number)?,
            children_truncated: false,
        })
    }

    /// Assign the native parent for one issue via `parent_issue_id`. Ids are
    /// validated before any network; Redmine rejects unknown ids itself.
    pub fn set_hierarchy_parent(&self, child: u64, parent: u64) -> Result<(), ForgejoError> {
        let project = self.configured_project();
        HierarchyEdge {
            parent: WorkItemRef::redmine(project.clone(), parent),
            child: WorkItemRef::redmine(project, child),
        }
        .validate()
        .map_err(ForgejoError::config)?;
        let payload = RedmineParentUpdate::some(parent);
        let _: Option<serde_json::Value> =
            self.http
                .put(&self.issue_path(child), &payload, HIERARCHY_UPDATE)?;
        Ok(())
    }

    /// Clear the native parent for one issue by writing an explicit null
    /// `parent_issue_id`. Never touches relations.
    pub fn unset_hierarchy_parent(&self, child: u64) -> Result<(), ForgejoError> {
        if child == 0 {
            return Err(ForgejoError::config(
                "Redmine hierarchy ids must be greater than zero",
            ));
        }
        let payload = RedmineParentUpdate::none();
        let _: Option<serde_json::Value> =
            self.http
                .put(&self.issue_path(child), &payload, HIERARCHY_UPDATE)?;
        Ok(())
    }

    fn configured_project(&self) -> Option<String> {
        self.config
            .project_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
    }
}

#[allow(dead_code)]
impl RedmineHierarchyIssue {
    fn into_node(self, project: Option<String>) -> HierarchyNode {
        let item = WorkItemRef::redmine(project.clone(), self.id);
        let parent = self
            .parent
            .map(|parent| WorkItemRef::redmine(project.clone(), parent.id));
        let children = self
            .children
            .into_iter()
            .map(|child| WorkItemRef::redmine(project.clone(), child.id))
            .collect();
        HierarchyNode {
            item,
            parent,
            children,
        }
    }
}

/// Native parent-only update payload. `Some` assigns the parent issue;
/// `None` serializes as an explicit null that clears it. Carries no other
/// field so a hierarchy write can never move body, status, or relations.
#[derive(Debug, Serialize)]
struct RedmineParentUpdate {
    issue: RedmineParentUpdateFields,
}

#[derive(Debug, Serialize)]
struct RedmineParentUpdateFields {
    parent_issue_id: Option<u64>,
}

impl RedmineParentUpdate {
    fn some(parent: u64) -> Self {
        Self {
            issue: RedmineParentUpdateFields {
                parent_issue_id: Some(parent),
            },
        }
    }

    fn none() -> Self {
        Self {
            issue: RedmineParentUpdateFields {
                parent_issue_id: None,
            },
        }
    }
}
