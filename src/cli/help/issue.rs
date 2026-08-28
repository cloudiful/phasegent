use crate::policy::{Capability, Role};

pub(crate) fn print_issue_help(role: Option<Role>) {
    println!(
        "Issue commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    for (name, capability) in [
        ("get", Capability::IssueRead),
        ("search", Capability::IssueSearch),
        ("create", Capability::IssueCreate),
        ("update-body", Capability::IssueUpdateBody),
        ("close", Capability::IssueClose),
    ] {
        if role.is_none_or(|role| role.allows(capability)) {
            println!("  {name:<14} {}", capability.description());
        }
    }
    println!("\nLocal branch context (no provider or network access):");
    println!("  bind             Bind the current branch to a Redmine issue in local Git config");
    println!("  unbind           Remove the current branch's Redmine issue binding");
    println!("  status           Show the current branch and its bound Redmine issue, if any");
    println!("\nUse 'phasegent --help issue <command>' for options.");
}

pub(crate) fn print_issue_command_help(role: Option<Role>, command: &str) {
    let (capability, text) = match command {
        "get" => (Capability::IssueRead, "Usage: issue get <NUMBER>"),
        "search" => (
            Capability::IssueSearch,
            "Usage: issue search [--query TEXT] [--state open|closed|all]\n\nValues beginning with `-` must use the inline form: --query=TEXT or --state=STATE.",
        ),
        "create" => (
            Capability::IssueCreate,
            "Usage: issue create --title TEXT [--body TEXT] [--tracker NAME_OR_ID] [--parent-issue ID] [--fixed-version NAME_OR_ID] [--start-date YYYY-MM-DD] [--due-date YYYY-MM-DD] [--estimated-hours HOURS] [--done-ratio 0-100]\n\n--tracker accepts a validated tracker name (Bug, Feature) or numeric id and is Redmine-only (GitLab maps it to a `type::bug` / `type::feature` label). Planning flags set native Redmine fields; --fixed-version resolves by exact version name or numeric id within the configured project. All Redmine planning flags are Redmine-only except --estimated-hours, which GitLab forwards through the native time_estimate endpoint. Forgejo rejects every planning flag.\n\nValues beginning with `-` (Markdown bullets, separator lines) must use the inline form: --title=TEXT or --body=TEXT.",
        ),
        "update-body" => (
            Capability::IssueUpdateBody,
            "Usage: issue update-body <NUMBER> --body TEXT [--tracker NAME_OR_ID] [--parent-issue ID] [--fixed-version NAME_OR_ID] [--start-date YYYY-MM-DD] [--due-date YYYY-MM-DD] [--estimated-hours HOURS] [--done-ratio 0-100]\n\n--tracker re-targets the issue's tracker in the same update (Redmine native; GitLab maps to a type::* label). Planning flags update native Redmine fields in the same PUT; --fixed-version resolves by exact version name or numeric id within the configured project. --estimated-hours is also accepted for GitLab (time_estimate); every other planning flag is Redmine-only. Forgejo rejects every planning flag.\n\nValues beginning with `-` (Markdown bullets, separator lines) must use the inline form: --body=TEXT.",
        ),
        "close" => (Capability::IssueClose, "Usage: issue close <NUMBER>"),
        "bind" => (
            Capability::IssueRead,
            "Usage: issue bind <ID> [--replace]\n\nStores `branch.<name>.redmine-issue-id = <ID>` in the local Git config for the current named branch. Detached HEAD is rejected. A different existing binding is rejected unless --replace is given; re-binding the same issue is a no-op.",
        ),
        "unbind" => (
            Capability::IssueRead,
            "Usage: issue unbind\n\nRemoves the current branch's Redmine issue binding from the local Git config. Absence is a no-op.",
        ),
        "status" => (
            Capability::IssueRead,
            "Usage: issue status\n\nPrints the current branch and its bound Redmine issue, if any. Detached HEAD is an error.",
        ),
        _ => {
            print_issue_help(role);
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
