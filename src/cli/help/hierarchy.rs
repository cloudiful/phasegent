use crate::policy::{Capability, Role};

use super::common::{HelpRow, print_group_help};

pub(crate) fn hierarchy_rows() -> Vec<HelpRow<'static>> {
    vec![
        (
            "get",
            "Read the native parent/child hierarchy for one work item",
            &["hierarchy", "get"],
        ),
        (
            "set",
            "Set the native parent for one work item (orchestrator-only)",
            &["hierarchy", "set"],
        ),
        (
            "unset",
            "Remove the native parent from one work item (orchestrator-only)",
            &["hierarchy", "unset"],
        ),
    ]
}

pub(crate) fn print_hierarchy_help(role: Option<Role>) {
    print_group_help(
        role,
        &format!(
            "Hierarchy commands for {}:",
            role.map_or("all roles", Role::as_str)
        ),
        &[(None, &hierarchy_rows())],
        "Use 'phasegent --help hierarchy <command>' for options.",
    );
}

/// Resolve the help entry for one `hierarchy` subcommand, or `None` when the
/// command is unknown.
pub(crate) fn hierarchy_command_help_entry(command: &str) -> Option<(Capability, &'static str)> {
    let entry = match command {
        "get" => (
            Capability::IssueRead,
            "Usage: hierarchy get <ID>\n\nRead the native parent/child hierarchy for one work item. Redmine IDs are issue IDs; GitLab IDs are the numeric global Work Item IDs shown in hierarchy output. Returns the item with its parent and at most 50 direct children plus an explicit children_truncated indicator. Hierarchy is never a relation and closing a parent never closes its children.",
        ),
        "set" => (
            Capability::IssueUpdateBody,
            "Usage: hierarchy set --parent <ID> --child <ID>\n\nAssign the native parent for one work item. Redmine writes parent_issue_id; GitLab resolves both items' native kind/scope through hierarchy reads first (only Epic-to-Issue and Issue-to-Task) and then issues the Work Item hierarchy widget mutation. Unsupported pairs fail with a structured not-supported error and never fall back to a relation. Orchestrator-only.",
        ),
        "unset" => (
            Capability::IssueUpdateBody,
            "Usage: hierarchy unset --child <ID>\n\nRemove the native parent from one work item. Redmine writes an explicit null parent_issue_id; GitLab issues the Work Item hierarchy widget mutation with a null parent. Never touches relations. Orchestrator-only.",
        ),
        _ => return None,
    };
    Some(entry)
}

pub(crate) fn hierarchy_command_help_text(command: &str) -> Option<(Capability, String)> {
    let (capability, entry) = hierarchy_command_help_entry(command)?;
    Some((
        capability,
        format!("{entry}\n\n{}", capability.description()),
    ))
}

pub(crate) fn print_hierarchy_command_help(role: Option<Role>, command: &str) {
    let Some((_capability, text)) = hierarchy_command_help_text(command) else {
        print_hierarchy_help(role);
        return;
    };
    let path = ["hierarchy", command];
    if role.is_some_and(|role| !crate::command::registry_allows_role(role, &path)) {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    } else {
        println!("{text}");
    }
}
