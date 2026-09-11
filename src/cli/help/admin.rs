use crate::policy::Role;

pub(crate) fn print_admin_help(role: Option<Role>) {
    println!(
        "Human-operator provisioning for {}:\n\n  auth setup           Store a provider credential securely (per-role; use --stdin or the secure prompt)\n  config set/clear     Persist settings in SQLite (canonical PHASEGENT_* names; secrets need --stdin)\n  config provider set/clear  Persist or remove the machine-wide default provider\n  workflow bootstrap   Provision the Redmine project and role identities (admin API key)\n\nUsage: phasegent --role <ROLE> admin <GROUP> ...\n\nThis group is human-operator only: AI roles (orchestrator, executor, reviewer, tester) must never invoke it, and agent permission rules deny the single `admin` token. Day-to-day workflow commands (issue, comment, status, timer, worktree, notify) stay top-level and keep their --role gating; read-only views (`config show`, `config provider get`) also stay top-level.\n\nUse 'phasegent --help admin <group>' for the next level (auth, config, workflow).",
        role.map_or("all roles", Role::as_str)
    );
}
