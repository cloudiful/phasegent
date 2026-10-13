use crate::policy::{Capability, Role};

pub(crate) fn print_project_help(role: Option<Role>) {
    println!(
        "Project commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    if role.is_none_or(|role| role.allows(Capability::ProjectRead)) {
        println!(
            "  list             {} (does not require --project-id)",
            Capability::ProjectRead.description()
        );
    }
    if role.is_none_or(|role| role.allows(Capability::ProjectCreate)) {
        println!(
            "  create           {}",
            Capability::ProjectCreate.description()
        );
    }
    println!("\nUse 'phasegent --help project <command>' for options.");
}

pub(crate) fn print_project_command_help(role: Option<Role>, command: &str) {
    let (capability, text) = match command {
        "list" => (
            Capability::ProjectRead,
            "Usage: project list\n\nLists projects visible to the configured provider credential. Does not require --project-id. Ordinary human Operator runs without a role (uses the admin credential centrally); explicit roles keep their own credential; managed sessions without a role fail closed.\n  * Redmine: native `GET /projects` (paginated, mapped onto RedmineProject).\n  * Local: lists the seeded local projects.\n\nUse the returned project identifier with `phasegent --project-id <ID> <command>` per invocation; project IDs are never persisted. Ordinary CLI needs no agent role; PHASEGENT_ROLE only selects agent-filtered help and scoped config.\n\nIssue search/create and version list can also derive the project automatically from the current Git origin's redmine_git_mirror records: exactly one match uses that project and bypasses bootstrap, multiple matches fail before any write and require --project-id, and no match for search/create runs internal auto-bootstrap as system preparation with the admin credential (no caller admin authority, no fallback, no role switch) while version list returns an actionable error. Authorized orchestrator issue create reaches this internal path without invoking admin commands or touching credentials.",
        ),
        "create" => (
            Capability::ProjectCreate,
            "Usage: project create --name NAME --identifier IDENTIFIER --confirm [--description TEXT]\n\nRedmine and Local only.",
        ),
        _ => {
            print_project_help(role);
            return;
        }
    };
    if role.is_none_or(|role| role.allows(capability)) {
        println!("{text}\n\n{}", capability.description());
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}
