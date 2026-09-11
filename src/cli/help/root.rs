use crate::policy::{Capability, Role};
use crate::providers::ProviderKind;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) fn print_root_help(role: Option<Role>, provider: Option<ProviderKind>) {
    let role_text = role.map_or("all roles", Role::as_str);
    println!(
        "phasegent {VERSION}\n\nProvider-backed workflow CLI ({role_text}).\n\nUsage:\n  phasegent --role <ROLE> [--provider forgejo|redmine|gitlab|local] <COMMAND> [OPTIONS]\n  phasegent gui\n\nOptions:\n  --role <ROLE>          admin, orchestrator, executor, reviewer, or tester\n  --provider <NAME>      forgejo, redmine, gitlab, or local (default: forgejo)\n  --api-base <URL>       Override the provider API base\n  --repository <O/R>     Override the Forgejo owner/repository\n  --project-id <ID>      Override the Redmine or GitLab project id\n  --close-status-id <ID> Override the Redmine closed status\n  -h, --help             Print help\n  -V, --version          Print version\n\nCommands:\n  gui                    Open the desktop GUI (single-binary shell)\n  issue                  Issue operations\n  comment                Comment operations\n  admin                  Human-operator provisioning: auth setup, config writes, workflow bootstrap (AI roles must never invoke)\n  config                 Local configuration (read-only show/get; writes live under admin)\n  doctor                 Read-only self-check: credential presence, index backend, masked PG URL (no --role needed)\n  hooks                  Managed Git hook installation\n  notify                 Bounded agent notifications\n  mcp                    MCP server over stdio or streamable HTTP
  plugin                 Managed OpenCode plugin installation (issue #239 Phase 3)"
    );
    if provider != Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::RepoCreate))
    {
        println!("  repo                   Repository operations");
    }
    if provider == Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::ProjectRead))
    {
        println!("  project                Redmine project operations");
    }
    if provider == Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::IssueStatusRead))
    {
        println!("  status                 Redmine issue status operations");
    }
    if provider == Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::VersionRead))
    {
        println!("  version                Redmine project version operations");
    }
    if provider == Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::RelationRead))
    {
        println!("  relation               Redmine issue relations");
    }
    if provider == Some(ProviderKind::Redmine) && role.is_none_or(|role| role == Role::Orchestrator)
    {
        println!(
            "  timer                  Redmine phase time tracking (internal/auto; manual fallback only)"
        );
    }
    if role.is_none_or(|role| {
        role == Role::Orchestrator || role == Role::Executor || role == Role::Reviewer
    }) {
        println!(
            "  worktree               Local per-(repo, issue, session) worktree leases (acquire/release/prune orchestrator-only; status/list readable by orchestrator, executor, reviewer)"
        );
    }
    println!(
        "\nUse 'phasegent --help <command>' for the next level.\n\
          Provider resolution chain and machine-wide default: 'phasegent --help config provider'.\n\
          Role and credential guidance: 'phasegent --help admin'."
    );
}

/// Help for `mcp`. Kept in the root help module so the MCP phase
/// needs no new help file; dispatched from the help router.
pub(crate) fn print_mcp_help(role: Option<Role>) {
    println!(
        "MCP server for {}:\n\n  serve [--transport stdio|http] [--bind 127.0.0.1:3000 (HTTP-only)] [--authorized]  Serve contracted tools\n\nTools: capabilities, issue_get, issue_search, status_next, comment_create (needs server-side --authorized unless orchestrator), notify_send. Excluded: status_advance, timer start/finish, role elevation. The server runs with the startup --role and provider flags; clients never supply a role. Stdio is the default; HTTP mounts streamable HTTP at /mcp with graceful shutdown. --bind is HTTP-only and requires --transport http.\n\nUse 'phasegent --help mcp serve' for options.",
        role.map_or("all roles", Role::as_str)
    );
}

/// Help for `mcp serve`.
pub(crate) fn print_mcp_command_help(role: Option<Role>, command: &str) {
    match command {
        "serve" => {
            let role_text = role.map_or("ROLE", Role::as_str);
            println!(
                "Usage: phasegent --role {role_text} [--provider forgejo|redmine|gitlab|local] mcp serve [--transport stdio|http] [--bind 127.0.0.1:3000 (HTTP-only)] [--authorized]\n\nServe the contracted MCP tools with the startup role. --transport stdio (default) speaks JSON-RPC on stdin/stdout; --transport http serves streamable HTTP via axum at /mcp on --bind (HTTP-only; requires --transport http). --authorized enables comment_create for non-orchestrator roles; without it the tool rejects with an authorization error. status_advance, timer start/finish, and role elevation are never exposed."
            );
        }
        _ => print_mcp_help(role),
    }
}
