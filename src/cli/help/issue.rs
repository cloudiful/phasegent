use crate::policy::{Capability, Role};

/// Issue commands listed in `issue` help.
pub(crate) fn normal_issue_commands() -> Vec<(&'static str, Capability)> {
    vec![
        ("get", Capability::IssueRead),
        ("search", Capability::IssueSearch),
        ("create", Capability::IssueCreate),
        ("update-body", Capability::IssueUpdateBody),
        ("close", Capability::IssueClose),
        ("upload-attachment", Capability::IssueAttachmentUpload),
    ]
}

pub(crate) fn print_issue_help(role: Option<Role>) {
    println!(
        "Issue commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    for (name, capability) in normal_issue_commands() {
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
            "Usage: issue search (--query TEXT | --all) [--state open|closed|all] [--page N] [--limit N] [--include-body]\n\nBounded single-page search (default page 1, limit 50, max 100). Uses native pagination: Redmine limit/offset, Forgejo page/limit, GitLab page/per_page. Never fetches all pages for one invocation. Ordinary search is provider-fresh by default and automatically warms the selected local index with the returned full summaries (one provider request; index open/write failures are bounded stderr warnings only).\n\n--query filters by subject/search text; empty or whitespace-only queries are rejected unless --all is given for a bounded listing of all visible issues.\n--state selects open (default all shows both), closed, or all.\n--page and --limit control the single page returned (page >=1, 1 <= limit <= 100).\nDefault output is compact metadata without bodies (id, number, title, state, html_url). Pass --include-body to include bounded bodies (byte cap 8192, truncated bodies report body_truncated: true).\nOutput envelope: { items: [...], page, limit, total_count?, has_more } where total_count is present when the provider returns it and has_more is derived from provider pagination metadata or item count.\n\nWhen provider resolution, auth, network, or search fails for a non-empty --query, search falls back to the local lexical index without provider credential/network lookup. Fallback is scoped to the known provider/project when available, else global, and returns { items: [{id, number, title, state, html_url, body?, body_truncated?, source?, project?, external_id?}], page, limit, total_count, has_more, data_source: \"local_index\", stale: true } with a concise stderr warning. Queryless --all has no fallback; with no local match/backend the original provider error is preserved. The index has no coverage/freshness model, so local-first is never the default; do not treat stale rows as fresh.\n\nSuccessful get/create/update/close opportunistically upsert the returned summary into the selected index (close is the closed document); index failures are warnings only. Backend selection is URL-driven: a non-empty PHASEGENT_INDEX_PG_URL (env overrides persisted) selects PostgreSQL, absent selects SQLite; PHASEGENT_INDEX_BACKEND is legacy and ignored.\n\nRedmine: when --project-id is omitted the current Git origin is matched against existing redmine_git_mirror records. Exactly one match uses that project; multiple matches fail with a listing of candidate ids/names and require --project-id; no match automatically bootstraps the project (admin credentials) as before. Explicit --project-id always wins and skips discovery. An explicit --repository that does not equal the origin is not silently matched; it keeps the existing bootstrap behavior.\n\nValues beginning with `-` must use the inline form: --query=TEXT or --state=STATE.",
        ),
        "create" => (
            Capability::IssueCreate,
            "Usage: issue create --title TEXT [--body TEXT] [--tracker NAME_OR_ID] [--parent-issue ID] [--fixed-version NAME_OR_ID] [--start-date YYYY-MM-DD] [--due-date YYYY-MM-DD] [--estimated-hours HOURS] [--done-ratio 0-100]\n\n--tracker accepts a validated tracker name (Bug, Feature) or numeric id and is Redmine-only (GitLab maps it to a `type::bug` / `type::feature` label). Planning flags set native Redmine fields; --fixed-version resolves by exact version name or numeric id within the configured project. All Redmine planning flags are Redmine-only except --estimated-hours, which GitLab forwards through the native time_estimate endpoint. Forgejo rejects every planning flag.\n\nRedmine: when --project-id is omitted the current Git origin is matched against existing redmine_git_mirror records. Exactly one match uses that project and bypasses bootstrap; multiple matches fail before any write with candidate ids/names and require --project-id; no match automatically bootstraps the project (admin credentials) as before. Explicit --project-id always wins and skips discovery. An explicit --repository that does not equal the origin is not silently matched; it keeps the existing bootstrap behavior.\n\nValues beginning with `-` (Markdown bullets, separator lines) must use the inline form: --title=TEXT or --body=TEXT.",
        ),
        "update-body" => (
            Capability::IssueUpdateBody,
            "Usage: issue update-body <NUMBER> --body TEXT [--tracker NAME_OR_ID] [--parent-issue ID] [--fixed-version NAME_OR_ID] [--start-date YYYY-MM-DD] [--due-date YYYY-MM-DD] [--estimated-hours HOURS] [--done-ratio 0-100]\n\n--tracker re-targets the issue's tracker in the same update (Redmine native; GitLab maps to a type::* label). Planning flags update native Redmine fields in the same PUT; --fixed-version resolves by exact version name or numeric id within the configured project. --estimated-hours is also accepted for GitLab (time_estimate); every other planning flag is Redmine-only. Forgejo rejects every planning flag.\n\nValues beginning with `-` (Markdown bullets, separator lines) must use the inline form: --body=TEXT.",
        ),
        "close" => (Capability::IssueClose, "Usage: issue close <NUMBER>"),
        "upload-attachment" => (
            Capability::IssueAttachmentUpload,
            "Usage: issue upload-attachment <NUMBER> --path PATH [--description TEXT]\n\nRedmine-only, orchestrator or tester. Validates the local file (must exist, be a regular non-empty file, valid filename, and not exceed 25 MiB) and uploads it via raw POST /uploads.json?filename=<basename> with Content-Type: application/octet-stream, then attaches it with PUT /issues/<id>.json {\"issue\":{\"uploads\":[{\"token\":...,\"filename\":...}],\"notes\":...}}. The transient upload token is never printed. Outputs compact JSON with issue, filename, bytes, and success. Forgejo and GitLab return not-supported without touching the filesystem or network. Values beginning with `-` must use the inline form: --path=PATH or --description=TEXT.",
        ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_help_lists_only_supported_commands() {
        let names: Vec<&str> = normal_issue_commands()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert!(!names.iter().any(|name| name.starts_with("index")));
        assert!(names.contains(&"get"));
        assert!(names.contains(&"search"));
    }

    #[test]
    fn removed_index_help_topics_are_rejected() {
        for topic in ["index", "index sync", "index search"] {
            let mut parts = vec![
                "--role".to_owned(),
                "executor".to_owned(),
                "--help".to_owned(),
                "issue".to_owned(),
            ];
            parts.extend(topic.split_whitespace().map(str::to_owned));
            let error = crate::command::parse(&parts)
                .err()
                .unwrap_or_else(|| panic!("help {topic} must be rejected"));
            assert!(
                error.contains("unknown issue help topic"),
                "help {topic} must be rejected as unknown help topic, got: {error}"
            );
        }
    }
}
