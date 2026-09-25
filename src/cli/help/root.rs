use crate::policy::Role;
use crate::providers::ProviderKind;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Root usage block. The session role comes from the managed session or
/// `PHASEGENT_ROLE`, so the common invocation shape never carries a role
/// flag. `--provider` is resolved from configuration and named only in the
/// options list, so the common invocation shape never tags it on either.
pub(crate) const ROOT_USAGE: &str = "Usage:\n  phasegent [GLOBAL OPTIONS] <COMMAND> [ARGS]\n  phasegent gui\n\nRole resolution:\n  Managed sessions supply the role; other hosts set PHASEGENT_ROLE.";

/// Root-overview order (issue 597 Phase 2). Visibility comes from the command
/// registry; this list only pins the print order and omits the retired
/// top-level `auth`/`workflow` redirect leaves, which exist so the parser can
/// resolve their moved-error help topics. A test keeps the two in sync.
const ROOT_OVERVIEW: &[&str] = &[
    "gui", "issue", "comment", "admin", "config", "doctor", "hooks", "notify", "mcp", "plugin",
    "repo", "project", "status", "version", "relation", "timer", "worktree",
];

pub(crate) fn print_root_help(role: Option<Role>, provider: Option<ProviderKind>) {
    let role_text = role.map_or("all roles", Role::as_str);
    println!(
        "phasegent {VERSION}\n\nProvider-backed workflow CLI ({role_text}).\n\n{ROOT_USAGE}\n\nOptions:\n  --provider <NAME>      forgejo, redmine, gitlab, or local (default: forgejo)\n  --api-base <URL>       Override the provider API base\n  --repository <O/R>     Override the Forgejo owner/repository\n  --project-id <ID>      Override the Redmine or GitLab project id\n  --close-status-id <ID> Override the Redmine closed status\n  -h, --help             Print help\n  -V, --version          Print version\n\nCommands:"
    );
    for &name in ROOT_OVERVIEW {
        if !crate::command::top_level_visible(name, role, provider) {
            continue;
        }
        if let Some(summary) = crate::command::top_level_summary(name) {
            println!("  {name:<23}{summary}");
        }
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
        "MCP server for {}:\n\n  serve [--transport stdio|http] [--bind 127.0.0.1:3000 (HTTP-only)] [--authorized]  Serve contracted tools\n\nTools: capabilities, issue_get, issue_search, status_next, comment_create (needs server-side --authorized unless orchestrator), notify_send. Excluded: status_advance, timer start/finish, role elevation. The server resolves its role from PHASEGENT_ROLE and its provider from the provider flags; clients never supply a role. Stdio is the default; HTTP mounts streamable HTTP at /mcp with graceful shutdown. --bind is HTTP-only and requires --transport http.\n\nUse 'phasegent --help mcp serve' for options.",
        role.map_or("all roles", Role::as_str)
    );
}

pub(crate) fn print_mcp_command_help(role: Option<Role>, command: &str) {
    match command {
        "serve" => {
            let role_text = role.map_or("ROLE", Role::as_str);
            println!(
                "Usage: PHASEGENT_ROLE={role_text} phasegent mcp serve [--transport stdio|http] [--bind 127.0.0.1:3000 (HTTP-only)] [--authorized]\n\nServe the contracted MCP tools with the role from PHASEGENT_ROLE. --transport stdio (default) speaks JSON-RPC on stdin/stdout; --transport http serves streamable HTTP via axum at /mcp on --bind (HTTP-only; requires --transport http). --authorized enables comment_create for non-orchestrator roles; without it the tool rejects with an authorization error. status_advance, timer start/finish, and role elevation are never exposed."
            );
        }
        _ => print_mcp_help(role),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_usage_leads_with_the_role_less_command_shape() {
        assert!(
            ROOT_USAGE.contains("Usage:"),
            "root usage must label itself: {ROOT_USAGE}"
        );
        let primary = ROOT_USAGE
            .lines()
            .nth(1)
            .expect("root usage must carry a primary invocation line");
        assert!(
            !primary.contains("--role"),
            "the primary usage line must not carry a role prefix: {primary}"
        );
        assert!(
            primary.contains("phasegent [GLOBAL OPTIONS] <COMMAND> [ARGS]"),
            "the primary usage line must keep the global-option/command shape: {primary}"
        );
        assert!(
            ROOT_USAGE.contains("phasegent gui"),
            "root usage must keep the gui entry: {ROOT_USAGE}"
        );
        assert!(
            !ROOT_USAGE.contains("--provider"),
            "root usage must not tag --provider onto the common invocation: {ROOT_USAGE}"
        );
    }

    #[test]
    fn root_usage_names_the_role_source() {
        assert!(
            ROOT_USAGE.contains("Managed sessions supply the role")
                && ROOT_USAGE.contains("PHASEGENT_ROLE"),
            "root usage must state that managed sessions supply the role and other hosts set PHASEGENT_ROLE: {ROOT_USAGE}"
        );
        assert!(
            !ROOT_USAGE.contains("--role"),
            "root usage must not document a role flag: {ROOT_USAGE}"
        );
    }

    /// The overview order must stay complete: every registered top-level
    /// command is listed except the retired `auth`/`workflow` redirect leaves.
    #[test]
    fn root_overview_covers_every_surface_command() {
        let mut expected: Vec<&str> = crate::command::top_level_names()
            .into_iter()
            .filter(|name| !matches!(*name, "auth" | "workflow"))
            .collect();
        let mut listed: Vec<&str> = ROOT_OVERVIEW.to_vec();
        expected.sort_unstable();
        listed.sort_unstable();
        assert_eq!(
            listed, expected,
            "root overview must list every real top-level command exactly once"
        );
    }
}
