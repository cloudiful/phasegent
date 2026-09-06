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
        "Usage: workflow bootstrap [--repository OWNER/REPOSITORY] [--close-status-id ID | --close-status-name NAME]\n\nFinds the exact Redmine project identifier derived from the repository, creates a missing private project automatically when missing, selects a closed issue status, then provisions the built-in orchestrator (Maintainer), executor (Developer), reviewer (Reporter), and tester (Reporter) users through the admin API and reconciles their direct project memberships. Only the admin Redmine API key is required; missing role keys never block provisioning. Each built-in login is found or created idempotently, its API key retrieved via the admin API and stored locally in SQLite (never TOML); the admin API key performs project lookup/creation, user provisioning, and the membership writes. Provisioned identities must stay distinct across roles and generated role credentials stay in SQLite. Stable non-secret endpoint settings resolve CLI flags > PHASEGENT_* environment > TOML phasegent.toml (read-only overlay, absolute PHASEGENT_CONFIG_PATH override) > SQLite > defaults; `config set`/`clear` remain SQLite-only. The workflow is reported ready only when every direct membership is added, updated, or already present. Missing or ambiguous users or roles fail with an actionable error before any partial identity mapping is persisted."
    );
}
