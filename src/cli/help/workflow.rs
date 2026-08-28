use crate::policy::Role;

pub(crate) fn print_workflow_help(role: Option<Role>) {
    if role.is_none_or(|role| role == Role::Admin) {
        println!(
            "Workflow commands for {}:\n\n  bootstrap        Find or create and configure the Redmine project\n\nUse 'phasegent --help workflow bootstrap' for options.",
            role.map_or("all roles", Role::as_str)
        );
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}

pub(crate) fn print_workflow_command_help(role: Option<Role>, command: &str) {
    if command != "bootstrap" || !role.is_none_or(|role| role == Role::Admin) {
        print_workflow_help(role);
        return;
    }
    println!(
        "Usage: workflow bootstrap [--repository OWNER/REPOSITORY] [--close-status-id ID | --close-status-name NAME]\n\nFinds the exact Redmine project identifier derived from the repository, creates a missing private project automatically when missing, selects a closed issue status, then reconciles direct project memberships for the existing orchestrator (Maintainer), executor (Developer), and reviewer (Reporter) users. Each agent identity is resolved through that role's Redmine API key via `/users/current.json`; the admin API key performs project lookup/creation and the membership writes. The workflow is reported ready only when every direct membership is added, updated, or already present. Missing or ambiguous users or roles fail with an actionable error before any partial identity mapping is persisted."
    );
}
