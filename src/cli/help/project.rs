use crate::policy::{Capability, Role};

pub(crate) fn print_project_help(role: Option<Role>) {
    println!(
        "Project commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    if role.is_none_or(|role| role.allows(Capability::ProjectRead)) {
        println!(
            "  list             {} (does not require --project-id; Redmine/GitLab/local parity)",
            Capability::ProjectRead.description()
        );
    }
    if role.is_none_or(|role| role.allows(Capability::ProjectCreate)) {
        println!(
            "  create           {} (Redmine or local; Forgejo/GitLab use `repo create`)",
            Capability::ProjectCreate.description()
        );
    }
    println!("\nUse 'phasegent --help project <command>' for options.");
}

pub(crate) fn print_project_command_help(role: Option<Role>, command: &str) {
    let (capability, text) = match command {
        "list" => (
            Capability::ProjectRead,
            "Usage: project list\n\nLists projects visible to the configured provider credential. Does not require --project-id.\n  * Redmine: native `GET /projects` (paginated, mapped onto RedmineProject).\n  * GitLab: equivalent read backed by `GET /projects` (mapped onto RedmineProject).\n  * Local: lists the seeded local projects.\n  * Forgejo: not supported.\n\nUse the returned project identifier with `phasegent --role <ROLE> --project-id <ID> <command>` per invocation; project IDs are never persisted.\n\nIssue search/create and version list can also derive the project automatically from the current Git origin's redmine_git_mirror records: exactly one match uses that project, multiple matches require --project-id, and no match for search/create auto-bootstraps while version list returns an actionable error. The mirror discovery flow is Redmine-only because GitLab `project list` does not need a project id.",
        ),
        "create" => (
            Capability::ProjectCreate,
            "Usage: project create --name NAME --identifier IDENTIFIER --confirm [--description TEXT]\n\nRedmine and Local only. The equivalent for Forgejo and GitLab is `repo create --private`, which lives on the RepoProvider surface and intentionally keeps a single entry point to the underlying `POST /projects` endpoint.",
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
