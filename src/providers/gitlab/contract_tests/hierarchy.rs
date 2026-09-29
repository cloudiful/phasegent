//! Shared GitLab Work Item hierarchy fixtures (issue 641 P3).
//!
//! Lives adjacent to the read/write contract modules so neither test file
//! exceeds the size budget; behavior and coverage are unchanged from the
//! single-file cases these helpers came from.

pub(super) fn epic_with_issue_children() -> String {
    serde_json::json!({
        "data": {"workItem": {
            "id": "gid://gitlab/WorkItem/100",
            "workItemType": {"name": "Epic"},
            "hierarchy": {
                "parent": null,
                "children": {"nodes": [
                    {"id": "gid://gitlab/WorkItem/101", "workItemType": {"name": "Issue"}},
                    {"id": "gid://gitlab/WorkItem/102", "workItemType": {"name": "Issue"}},
                ]},
            },
        }}
    })
    .to_string()
}

pub(super) fn issue_with_epic_parent_and_task_child() -> String {
    serde_json::json!({
        "data": {"workItem": {
            "id": "gid://gitlab/WorkItem/101",
            "workItemType": {"name": "Issue"},
            "hierarchy": {
                "parent": {"id": "gid://gitlab/WorkItem/100", "workItemType": {"name": "Epic"}},
                "children": {"nodes": [
                    {"id": "gid://gitlab/WorkItem/201", "workItemType": {"name": "Task"}},
                ]},
            },
        }}
    })
    .to_string()
}

pub(super) fn update_ok(child: u64) -> String {
    serde_json::json!({
        "data": {"workItemUpdate": {
            "errors": [],
            "workItem": {"id": format!("gid://gitlab/WorkItem/{child}")},
        }}
    })
    .to_string()
}
