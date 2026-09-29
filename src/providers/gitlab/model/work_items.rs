//! GitLab Work Item hierarchy DTOs and kind/ref mapping (issue 641 P3).
//!
//! Decodes the `workItem(id:)` hierarchy widget (`parent` plus bounded
//! `children(first: 50)`) and the `workItemUpdate` mutation payload. Global
//! IDs look like `gid://gitlab/WorkItem/123`; the trailing number is the
//! external ID carried by [`WorkItemRef`]. Type names map exactly to
//! [`WorkItemKind`]: `Epic`/`Issue`/`Task`; anything else is a decode error.
//! Nested Epic-to-Epic is out of scope and fails as `not_supported` at the
//! caller via [`supported_parent_child`].

use serde::Deserialize;

use crate::providers::api::ForgejoError;
use crate::providers::hierarchy::{WorkItemKind, WorkItemRef};

/// Global ID for a work item, e.g. `gid://gitlab/WorkItem/123`.
pub(crate) fn work_item_gid(id: u64) -> String {
    format!("gid://gitlab/WorkItem/{id}")
}

/// Parse the trailing numeric ID from a `gid://gitlab/WorkItem/<id>` value.
pub(crate) fn parse_work_item_id(gid: &str, operation: &str) -> Result<u64, ForgejoError> {
    gid.rsplit('/')
        .next()
        .and_then(|tail| tail.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| ForgejoError::Decode {
            operation: operation.to_owned(),
            message: format!("unknown GitLab work item id shape: {gid}"),
        })
}

/// Map a GraphQL `workItemType.name` to its typed kind.
pub(crate) fn parse_work_item_kind(
    name: &str,
    operation: &str,
) -> Result<WorkItemKind, ForgejoError> {
    match name.trim().to_ascii_lowercase().as_str() {
        "epic" => Ok(WorkItemKind::GitLabEpic),
        "issue" => Ok(WorkItemKind::GitLabIssue),
        "task" => Ok(WorkItemKind::GitLabTask),
        _ => Err(ForgejoError::Decode {
            operation: operation.to_owned(),
            message: format!("unknown GitLab work item type: {name}"),
        }),
    }
}

/// Build a typed ref from a hierarchy link (GID plus type name).
pub(crate) fn link_ref(
    project: Option<String>,
    gid: &str,
    type_name: &str,
    operation: &str,
) -> Result<WorkItemRef, ForgejoError> {
    Ok(WorkItemRef::gitlab(
        project,
        parse_work_item_id(gid, operation)?,
        parse_work_item_kind(type_name, operation)?,
    ))
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkItemQueryData {
    #[serde(rename = "workItem")]
    pub work_item: Option<WorkItemNode>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkItemNode {
    pub id: String,
    #[serde(rename = "workItemType")]
    pub work_item_type: WorkItemTypeRef,
    #[serde(default)]
    pub hierarchy: Option<HierarchyWidget>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkItemTypeRef {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct HierarchyWidget {
    #[serde(default)]
    pub parent: Option<WorkItemLink>,
    #[serde(default)]
    pub children: Option<WorkItemChildren>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkItemChildren {
    #[serde(default)]
    pub nodes: Vec<WorkItemLink>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkItemLink {
    pub id: String,
    #[serde(rename = "workItemType")]
    pub work_item_type: WorkItemTypeRef,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkItemUpdateData {
    #[serde(rename = "workItemUpdate")]
    pub work_item_update: Option<WorkItemUpdatePayload>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkItemUpdatePayload {
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(rename = "workItem", default)]
    pub work_item: Option<WorkItemIdOnly>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkItemIdOnly {
    #[allow(dead_code)]
    pub id: String,
}
