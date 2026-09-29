//! GitLab Work Item hierarchy DTOs and kind/ref mapping (issue 641 P3).
//!
//! Decodes the `workItem(id:)` hierarchy widget through the `widgets`
//! collection (`... on WorkItemWidgetHierarchy` with `parent` plus bounded
//! `children(first: 50)` and its `pageInfo.hasNextPage`), plus each item's
//! `namespace.fullPath` for true group/project scope, and the
//! `workItemUpdate` mutation payload. Global IDs look like
//! `gid://gitlab/WorkItem/123`; the trailing number is the external ID
//! carried by [`WorkItemRef`]. Type names map exactly to [`WorkItemKind`]:
//! `Epic`/`Issue`/`Task`; anything else is a decode error. Nested Epic-to-Epic
//! is out of scope and fails as `not_supported` at the caller via
//! [`supported_parent_child`].

use serde::Deserialize;

use crate::providers::api::ProviderError;
use crate::providers::hierarchy::{WorkItemKind, WorkItemRef};

/// Global ID for a work item, e.g. `gid://gitlab/WorkItem/123`.
pub(crate) fn work_item_gid(id: u64) -> String {
    format!("gid://gitlab/WorkItem/{id}")
}

/// Parse the trailing numeric ID from a `gid://gitlab/WorkItem/<id>` value.
pub(crate) fn parse_work_item_id(gid: &str, operation: &str) -> Result<u64, ProviderError> {
    gid.rsplit('/')
        .next()
        .and_then(|tail| tail.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| ProviderError::Decode {
            operation: operation.to_owned(),
            message: format!("unknown GitLab work item id shape: {gid}"),
        })
}

/// Map a GraphQL `workItemType.name` to its typed kind.
pub(crate) fn parse_work_item_kind(
    name: &str,
    operation: &str,
) -> Result<WorkItemKind, ProviderError> {
    match name.trim().to_ascii_lowercase().as_str() {
        "epic" => Ok(WorkItemKind::GitLabEpic),
        "issue" => Ok(WorkItemKind::GitLabIssue),
        "task" => Ok(WorkItemKind::GitLabTask),
        _ => Err(ProviderError::Decode {
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
) -> Result<WorkItemRef, ProviderError> {
    Ok(WorkItemRef::gitlab(
        project,
        parse_work_item_id(gid, operation)?,
        parse_work_item_kind(type_name, operation)?,
    ))
}

/// Resolve the true group/project scope for one item: the wire
/// `namespace.fullPath` when present, otherwise the caller's configured
/// project (legacy tolerance for payloads that omit the namespace).
pub(crate) fn scope_of(
    namespace: &Option<NamespaceRef>,
    fallback: Option<String>,
) -> Option<String> {
    namespace
        .as_ref()
        .and_then(|namespace| namespace.full_path.clone())
        .filter(|path| !path.trim().is_empty())
        .or(fallback)
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
    pub namespace: Option<NamespaceRef>,
    #[serde(default)]
    pub widgets: Option<Vec<WorkItemWidget>>,
}

impl WorkItemNode {
    /// Find the hierarchy widget by its GraphQL type name. Every other
    /// widget (description, labels, ...) decodes with no parent/children
    /// and is skipped; a missing entry means the instance exposes no
    /// hierarchy support for this item.
    pub(crate) fn hierarchy_widget(&self) -> Option<&WorkItemWidget> {
        self.widgets
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .find(|widget| widget.typename == "WorkItemWidgetHierarchy")
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkItemTypeRef {
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NamespaceRef {
    #[serde(rename = "fullPath", default)]
    pub full_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkItemWidget {
    #[serde(rename = "__typename", default)]
    pub typename: String,
    #[serde(default)]
    pub parent: Option<WorkItemLink>,
    #[serde(default)]
    pub children: Option<WorkItemChildren>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkItemChildren {
    #[serde(default)]
    pub nodes: Vec<WorkItemLink>,
    #[serde(rename = "pageInfo", default)]
    pub page_info: Option<PageInfo>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct PageInfo {
    #[serde(rename = "hasNextPage", default)]
    pub has_next_page: bool,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkItemLink {
    pub id: String,
    #[serde(rename = "workItemType")]
    pub work_item_type: WorkItemTypeRef,
    #[serde(default)]
    pub namespace: Option<NamespaceRef>,
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
