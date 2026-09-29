//! GitLab Work Item hierarchy reads (issue 641 P3).
//!
//! Reads use `workItem(id:)` plus its hierarchy widget from the `widgets`
//! collection (`... on WorkItemWidgetHierarchy` with `parent` and bounded
//! `children(first: 50)`); each item's `namespace.fullPath` provides its true
//! group/project scope. The child connection `pageInfo.hasNextPage` is
//! reported as truncation without fetching beyond the 50-item bound. A
//! missing hierarchy widget, an unknown type name, and permission failures
//! fail explicitly; hierarchy never falls back to a REST issue relation.
//! Parent writes live in the adjacent `hierarchy_write` module.

use crate::providers::api::ForgejoError;
use crate::providers::gitlab::model::work_items;
use crate::providers::hierarchy::{HierarchyNode, HierarchyPage, WorkItemRef};

use super::core::GitlabProvider;

const HIERARCHY_GET: &str = "issue hierarchy get";

const WORK_ITEM_QUERY: &str = "query($id: WorkItemID!) { workItem(id: $id) { id workItemType { name } namespace { fullPath } widgets { __typename ... on WorkItemWidgetHierarchy { parent { id workItemType { name } namespace { fullPath } } children(first: 50) { nodes { id workItemType { name } namespace { fullPath } } pageInfo { hasNextPage } } } } } }";

impl GitlabProvider {
    fn configured_project(&self) -> Option<String> {
        Some(self.config.project_id.to_string())
    }

    /// Fetch the native hierarchy view for one Work Item global ID.
    /// Truncation is dropped here; callers that project the bounded child
    /// list use [`Self::get_hierarchy_page`].
    pub fn get_hierarchy(&self, id: u64) -> Result<HierarchyNode, ForgejoError> {
        Ok(self.get_hierarchy_page(id)?.node)
    }

    /// Fetch the bounded hierarchy view plus an explicit truncation
    /// indicator for one Work Item global ID. The child list never grows
    /// past 50 entries; `children_truncated` reports the child connection
    /// `pageInfo.hasNextPage` so callers surface it instead of fetching on.
    pub fn get_hierarchy_page(&self, id: u64) -> Result<HierarchyPage, ForgejoError> {
        if id == 0 {
            return Err(ForgejoError::config(
                "GitLab work item id must be greater than zero",
            ));
        }
        let variables = serde_json::json!({"id": work_items::work_item_gid(id)});
        let data: work_items::WorkItemQueryData = crate::providers::gitlab::graphql::execute(
            &self.http,
            WORK_ITEM_QUERY,
            variables,
            HIERARCHY_GET,
        )?;
        let node = data.work_item.ok_or_else(|| {
            ForgejoError::not_found(HIERARCHY_GET, "GitLab work item was not found")
        })?;
        let item_project = work_items::scope_of(&node.namespace, self.configured_project());
        let kind = work_items::parse_work_item_kind(&node.work_item_type.name, HIERARCHY_GET)?;
        let item_id = work_items::parse_work_item_id(&node.id, HIERARCHY_GET)?;
        let widget = node
            .hierarchy_widget()
            .ok_or_else(|| ForgejoError::not_supported("gitlab", HIERARCHY_GET))?;
        let parent = widget
            .parent
            .as_ref()
            .map(|link| {
                work_items::link_ref(
                    work_items::scope_of(&link.namespace, item_project.clone()),
                    &link.id,
                    &link.work_item_type.name,
                    HIERARCHY_GET,
                )
            })
            .transpose()?;
        let mut children = Vec::new();
        let mut children_truncated = false;
        if let Some(connection) = widget.children.as_ref() {
            for link in &connection.nodes {
                children.push(work_items::link_ref(
                    work_items::scope_of(&link.namespace, item_project.clone()),
                    &link.id,
                    &link.work_item_type.name,
                    HIERARCHY_GET,
                )?);
            }
            children_truncated = connection
                .page_info
                .as_ref()
                .map(|page| page.has_next_page)
                .unwrap_or(false);
        }
        Ok(HierarchyPage {
            node: HierarchyNode {
                item: WorkItemRef::gitlab(item_project, item_id, kind),
                parent,
                children,
            },
            children_truncated,
        })
    }
}
