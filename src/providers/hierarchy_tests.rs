//! Focused unit tests for the typed hierarchy contract (issue 641).
//!
//! Lives adjacent to `hierarchy.rs` so the production module stays under the
//! file-size budget.

use super::*;

#[test]
fn redmine_nesting_is_supported_and_local_is_not() {
    assert!(supported_parent_child(
        &WorkItemKind::RedmineIssue,
        &WorkItemKind::RedmineIssue
    ));
    assert!(supported_parent_child(
        &WorkItemKind::GitLabEpic,
        &WorkItemKind::GitLabIssue
    ));
    assert!(supported_parent_child(
        &WorkItemKind::GitLabIssue,
        &WorkItemKind::GitLabTask
    ));
    assert!(!supported_parent_child(
        &WorkItemKind::RedmineIssue,
        &WorkItemKind::GitLabIssue
    ));
    assert!(!supported_parent_child(
        &WorkItemKind::LocalIssue,
        &WorkItemKind::LocalIssue
    ));
}

#[test]
fn edge_validation_rejects_self_and_zero_ids() {
    let item = WorkItemRef::redmine(Some("42".to_owned()), 7);
    let good = HierarchyEdge {
        parent: WorkItemRef::redmine(Some("42".to_owned()), 6),
        child: item.clone(),
    };
    assert!(good.validate().is_ok());
    let looped = HierarchyEdge {
        parent: item.clone(),
        child: item.clone(),
    };
    assert!(looped.validate().is_err());
    let zero = HierarchyEdge {
        parent: WorkItemRef::redmine(None, 0),
        child: item,
    };
    assert!(zero.validate().is_err());
}

#[test]
fn identical_ids_in_different_projects_are_distinct() {
    // Same numeric id in different project scopes must never compare
    // equal: cross-provider identity needs the full 4-tuple.
    let left = WorkItemRef::redmine(Some("42".to_owned()), 7);
    let right = WorkItemRef::redmine(Some("43".to_owned()), 7);
    assert!(!left.is_same_item(&right));
    assert!(!right.is_same_item(&left));
    let scoped = WorkItemRef::redmine(Some("42".to_owned()), 7);
    let unscoped = WorkItemRef::redmine(None, 7);
    assert!(!scoped.is_same_item(&unscoped));
    // A cross-project edge is not a self-parent loop.
    let cross_project = HierarchyEdge {
        parent: left,
        child: right,
    };
    assert!(cross_project.validate().is_ok());
}

#[test]
fn same_scope_identical_refs_compare_equal() {
    let left = WorkItemRef::redmine(Some("42".to_owned()), 7);
    let right = WorkItemRef::redmine(Some("42".to_owned()), 7);
    assert!(left.is_same_item(&right));
    assert!(right.is_same_item(&left));
    let looped = HierarchyEdge {
        parent: left,
        child: right,
    };
    assert!(looped.validate().is_err());
}

#[test]
fn gitlab_refs_carry_provider_project_and_kind() {
    let epic = WorkItemRef::gitlab(Some("42".to_owned()), 100, WorkItemKind::GitLabEpic);
    let issue = WorkItemRef::gitlab(Some("42".to_owned()), 101, WorkItemKind::GitLabIssue);
    let task = WorkItemRef::gitlab(Some("42".to_owned()), 102, WorkItemKind::GitLabTask);
    for item in [&epic, &issue, &task] {
        assert_eq!(item.provider, "gitlab");
        assert_eq!(item.project.as_deref(), Some("42"));
    }
    assert_eq!(epic.kind, WorkItemKind::GitLabEpic);
    assert_eq!(issue.kind, WorkItemKind::GitLabIssue);
    assert_eq!(task.kind, WorkItemKind::GitLabTask);
    assert!(!epic.is_same_item(&issue));
    let supported = HierarchyEdge {
        parent: epic.clone(),
        child: issue.clone(),
    };
    assert!(supported.validate().is_ok());
    assert!(supported_parent_child(&epic.kind, &issue.kind));
    assert!(supported_parent_child(&issue.kind, &task.kind));
    assert!(!supported_parent_child(&epic.kind, &task.kind));
    assert!(!supported_parent_child(&issue.kind, &issue.kind));
}

#[test]
fn hierarchy_page_reports_truncation_explicitly() {
    let node = HierarchyNode {
        item: WorkItemRef::redmine(Some("42".to_owned()), 7),
        parent: None,
        children: Vec::new(),
    };
    let complete = HierarchyPage {
        node: node.clone(),
        children_truncated: false,
    };
    assert!(!complete.children_truncated);
    assert!(complete.node.children_ids().is_empty());
    let truncated = HierarchyPage {
        node,
        children_truncated: true,
    };
    assert!(truncated.children_truncated);
    let json = serde_json::to_value(&truncated).unwrap();
    assert_eq!(json["children_truncated"], true);
    assert!(json.get("node").is_some());
}

#[test]
fn bounded_page_caps_children_at_fifty_and_reports_truncation() {
    let children = (1..=52)
        .map(|id| WorkItemRef::redmine(Some("42".to_owned()), id))
        .collect::<Vec<_>>();
    let node = HierarchyNode {
        item: WorkItemRef::redmine(Some("42".to_owned()), 7),
        parent: None,
        children,
    };
    let page = HierarchyPage::bounded(node, false);
    assert_eq!(page.node.children.len(), HIERARCHY_MAX_CHILDREN);
    assert_eq!(page.node.children_ids().len(), HIERARCHY_MAX_CHILDREN);
    assert!(page.children_truncated);
    let json = serde_json::to_value(&page).unwrap();
    assert_eq!(json["children_truncated"], true);

    let small = HierarchyNode {
        item: WorkItemRef::redmine(Some("42".to_owned()), 7),
        parent: None,
        children: vec![WorkItemRef::redmine(Some("42".to_owned()), 8)],
    };
    let page = HierarchyPage::bounded(small, false);
    assert_eq!(page.node.children_ids(), vec![8]);
    assert!(!page.children_truncated);
}
