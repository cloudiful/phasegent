//! Redmine native parent/children reads (issue 641 P2).
//!
//! Uses the native `parent` / `children` issue fields with
//! `GET /issues/:id.json?include=children`. Never reads `relations` for
//! hierarchy and never implies a `relates` edge. Writes keep using the
//! existing `--parent-issue` (`parent_issue_id`) create/update path.

use serde::Deserialize;

use crate::providers::api::ForgejoError;
use crate::providers::config::RedmineProvider;
use crate::providers::hierarchy::{HierarchyNode, WorkItemRef};

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
