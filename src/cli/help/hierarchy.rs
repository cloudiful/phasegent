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
            "Usage: hierarchy get <ID>\n\nRead the native parent/child hierarchy for one work item. IDs are provider-specific: Redmine IDs are issue IDs; GitLab IDs are the numeric global Work Item IDs shown in hierarchy output, so the same number never implies the same item across providers, projects, or kinds. Returns the item with its parent and at most 50 direct children plus an explicit children_truncated indicator. Hierarchy is never a relation and closing a parent never closes its children.",
        ),
        "set" => (
            Capability::IssueUpdateBody,
            "Usage: hierarchy set --parent <ID> --child <ID>\n\nAssign the native parent for one work item. Redmine writes parent_issue_id; GitLab resolves both items' native kind/scope through hierarchy reads first (only Epic-to-Issue and Issue-to-Task) and then issues the Work Item hierarchy widget mutation. Unsupported pairs fail with a structured not-supported error and never fall back to a relation. `issue create/update --parent-issue` stays the Redmine-only planning flag for the same native field; the typed hierarchy commands are the provider-aware surface covering GitLab too. Closing a parent never closes its children. Orchestrator-only.",
        ),
        "unset" => (
            Capability::IssueUpdateBody,
            "Usage: hierarchy unset --child <ID>\n\nRemove the native parent from one work item. Redmine writes an explicit null parent_issue_id; GitLab issues the Work Item hierarchy widget mutation with a null parent. Never touches relations and never closes the item. Orchestrator-only.",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue 641 P4b: the hierarchy detail pages must keep the
    /// provider-specific ID semantics, the documented 50-child bound with
    /// its explicit truncation indicator, the no-cascade-on-close rule,
    /// and the split from the Redmine-only `--parent-issue` planning flag.
    #[test]
    fn hierarchy_help_pages_pin_the_issue_641_contract_lines() {
        let (get_capability, get_text) =
            hierarchy_command_help_text("get").expect("get help entry");
        assert_eq!(get_capability, Capability::IssueRead);
        assert!(
            get_text.contains("IDs are provider-specific"),
            "get help must state provider-specific id semantics; got: {get_text}"
        );
        assert!(
            get_text.contains("at most 50 direct children")
                && get_text.contains("children_truncated"),
            "get help must document the 50-child bound and its truncation indicator; got: {get_text}"
        );
        assert!(
            get_text.contains("closing a parent never closes its children"),
            "get help must state the no-cascade rule; got: {get_text}"
        );

        for command in ["set", "unset"] {
            let (capability, text) =
                hierarchy_command_help_text(command).expect("write help entry");
            assert_eq!(capability, Capability::IssueUpdateBody);
            assert!(
                text.contains("Orchestrator-only"),
                "{command} help must carry the role gate; got: {text}"
            );
        }

        let (_, set_text) = hierarchy_command_help_text("set").expect("set help entry");
        assert!(
            set_text.contains("--parent-issue") && set_text.contains("provider-aware"),
            "set help must distinguish the typed hierarchy command from the Redmine-only --parent-issue flag; got: {set_text}"
        );
    }
}
