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
        "Usage: PHASEGENT_ROLE=admin phasegent admin workflow bootstrap [--repository OWNER/REPOSITORY] [--close-status-id ID | --close-status-name NAME] [--all-projects [--dry-run]]\n\nHuman-operator only; AI roles must never invoke the admin group.\n\nBy default bootstrap finds the exact Redmine project identifier derived from the repository, creates a missing private project automatically when missing, selects a closed issue status, then provisions the built-in orchestrator (Maintainer), executor (Developer), reviewer (Reporter), and explore (Reporter) users through the admin API and reconciles their direct project memberships. Only the admin Redmine API key is required; missing role keys never block provisioning. Each built-in login is found or created idempotently, its API key retrieved via the admin API and stored locally in SQLite (never TOML); the admin API key performs project lookup/creation, user provisioning, and the membership writes. Provisioned identities must stay distinct across roles and generated role credentials stay in SQLite. Stable non-secret endpoint settings resolve CLI flags > PHASEGENT_* environment > TOML phasegent.toml (read-only overlay, absolute PHASEGENT_CONFIG_PATH override) > SQLite > defaults; `admin config set`/`clear` remain SQLite-only. The workflow is reported ready only when every direct membership is added, updated, or already present. Missing or ambiguous users or roles fail with an actionable error before any partial identity mapping is persisted.\n\n--all-projects reconciles the four built-in users on every existing project returned by the Redmine admin API instead of the current repository's project; it needs no Git repository, creates no projects or mirrors, and writes no close-status/provider/project settings. It enumerates the complete project set (an incomplete or repeated page fails before any write), provisions the four shared identities once with the same distinct-identity gate, and adds only missing direct memberships, preserving unrelated roles already attached. --dry-run is a read-only preview valid only with --all-projects: it performs provider reads, plans missing-user creation and membership add/update/existing without creating users, fetching user API keys, or writing identities, credentials, memberships, or settings. Both modes may still open the local SQLite store, which can create the schema or database file when absent; print the full global JSON report (mode, roles, projects, memberships, summary) and exit 0 when every project is ready, 1 when any project is blocked, warned, or errored. --all-projects cannot be combined with --repository, --close-status-id, --close-status-name, or --project-id."
    );
}
