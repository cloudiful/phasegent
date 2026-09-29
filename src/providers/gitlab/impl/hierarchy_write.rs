//! GitLab Work Item typed parent writes (issue 641 P3/P4a).
//!
//! Writes use `workItemUpdate` with `hierarchyWidget.parentId`; clearing
//! uses the same mutation with an explicit null parent. Only Epic-to-Issue
//! and Issue-to-Task are supported (nested Epic-to-Epic is out of scope);
//! every other pair fails as `not_supported` before any network. Reads that
//! resolve native kinds live in the adjacent `hierarchy` module; hierarchy
//! never falls back to a REST issue relation.

use crate::providers::api::ForgejoError;
use crate::providers::gitlab::model::work_items;
use crate::providers::hierarchy::{HierarchyEdge, WorkItemRef, supported_parent_child};

use super::core::GitlabProvider;

const HIERARCHY_UPDATE: &str = "issue hierarchy update";

const WORK_ITEM_UPDATE: &str = "mutation($id: WorkItemID!, $parentId: WorkItemID!) { workItemUpdate(input: {id: $id, hierarchyWidget: {parentId: $parentId}}) { errors workItem { id } } }";

/// Clearing mutation: the same `workItemUpdate` hierarchy widget with an
/// explicit null parent. `$parentId` stays nullable so the server receives
/// a clear-parent assignment rather than an omitted field.
const WORK_ITEM_UNSET: &str = "mutation($id: WorkItemID!, $parentId: WorkItemID) { workItemUpdate(input: {id: $id, hierarchyWidget: {parentId: $parentId}}) { errors workItem { id } } }";

impl GitlabProvider {
    /// Set the typed parent for one Work Item. Only Epic-to-Issue and
    /// Issue-to-Task are supported; validation runs before any network.
    pub fn set_hierarchy_parent(
        &self,
        child: &WorkItemRef,
        parent: &WorkItemRef,
    ) -> Result<(), ForgejoError> {
        let edge = HierarchyEdge {
            parent: parent.clone(),
            child: child.clone(),
        };
        edge.validate().map_err(ForgejoError::config)?;
        if !supported_parent_child(&parent.kind, &child.kind) {
            return Err(ForgejoError::not_supported("gitlab", HIERARCHY_UPDATE));
        }
        let variables = serde_json::json!({
            "id": work_items::work_item_gid(child.id),
            "parentId": work_items::work_item_gid(parent.id),
        });
        self.apply_update(WORK_ITEM_UPDATE, variables)
    }

    /// Set the typed parent from bare numeric Work Item IDs. The native
    /// kind and scope of both items resolve through hierarchy reads first,
    /// so an unsupported pair (or a missing item) fails before the mutation
    /// is ever issued. Hierarchy never falls back to a REST issue relation.
    pub fn set_hierarchy_parent_by_id(
        &self,
        child_id: u64,
        parent_id: u64,
    ) -> Result<(), ForgejoError> {
        if child_id == 0 || parent_id == 0 {
            return Err(ForgejoError::config(
                "GitLab work item id must be greater than zero",
            ));
        }
        if child_id == parent_id {
            return Err(ForgejoError::config(
                "hierarchy parent cannot be the same item as the child",
            ));
        }
        let child = self.get_hierarchy_page(child_id)?.node.item;
        let parent = self.get_hierarchy_page(parent_id)?.node.item;
        self.set_hierarchy_parent(&child, &parent)
    }

    /// Clear the parent of one Work Item via the native hierarchy widget
    /// with an explicit null parent. No kind resolution is needed: the
    /// mutation carries only the child GID and never touches relations.
    pub fn unset_hierarchy_parent_by_id(&self, child_id: u64) -> Result<(), ForgejoError> {
        if child_id == 0 {
            return Err(ForgejoError::config(
                "GitLab work item id must be greater than zero",
            ));
        }
        let variables = serde_json::json!({
            "id": work_items::work_item_gid(child_id),
            "parentId": serde_json::Value::Null,
        });
        self.apply_update(WORK_ITEM_UNSET, variables)
    }

    /// Run one `workItemUpdate` hierarchy mutation and map its payload:
    /// mutation `errors[]` become a redacted `request` error, a missing
    /// payload or work item becomes `decode`.
    fn apply_update(
        &self,
        mutation: &str,
        variables: serde_json::Value,
    ) -> Result<(), ForgejoError> {
        let data: work_items::WorkItemUpdateData = crate::providers::gitlab::graphql::execute(
            &self.http,
            mutation,
            variables,
            HIERARCHY_UPDATE,
        )?;
        let payload = data.work_item_update.ok_or_else(|| ForgejoError::Decode {
            operation: HIERARCHY_UPDATE.to_owned(),
            message: self
                .http
                .redact("GitLab work item update contained no data"),
        })?;
        let errors: Vec<String> = payload
            .errors
            .into_iter()
            .map(|message| message.trim().to_owned())
            .filter(|message| !message.is_empty())
            .collect();
        if !errors.is_empty() {
            return Err(ForgejoError::Request {
                operation: HIERARCHY_UPDATE.to_owned(),
                message: self.http.redact(&errors.join("; ")),
            });
        }
        payload.work_item.ok_or_else(|| ForgejoError::Decode {
            operation: HIERARCHY_UPDATE.to_owned(),
            message: self
                .http
                .redact("GitLab did not return the updated work item"),
        })?;
        Ok(())
    }
}
