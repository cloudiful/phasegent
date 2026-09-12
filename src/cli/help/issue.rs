use crate::policy::{Capability, Role};

/// Issue commands listed in `issue` help.
pub(crate) fn normal_issue_commands() -> Vec<(&'static str, Capability)> {
    vec![
        ("get", Capability::IssueRead),
        ("search", Capability::IssueSearch),
        ("create", Capability::IssueCreate),
        ("update", Capability::IssueUpdateBody),
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

/// Resolve the help entry for one `issue` subcommand, or `None` when the
/// command is unknown. Split out from [`print_issue_command_help`] so the
/// help text itself is testable without capturing stdout.
pub(crate) fn issue_command_help_entry(command: &str) -> Option<(Capability, &'static str)> {
    let entry = match command {
        "get" => (
            Capability::IssueRead,
            "Usage: issue get <NUMBER> [<NUMBER>...] (at most 20, unique, positive)\n\nOne number returns the legacy single-issue object. Two or more return an {issues, errors} envelope: successes and per-number failures are collected side by side so one missing issue never discards the rest; the exit code is 0 only when every fetch succeeds. Each fetched summary warms the local index exactly like a single get.",
        ),

        "search" => (
            Capability::IssueSearch,
            concat!(
                "Usage: issue search (--query TEXT | --all) [--state open|closed|all] [--page N] [--limit N] [--include-body]\n\n",
                "Bounded single-page search (default page 1, limit 50, max 100). Uses native pagination: Redmine limit/offset, Forgejo page/limit, GitLab page/per_page. Never fetches all pages for one invocation. Ordinary search is provider-fresh by default and automatically warms the selected local index with the returned full summaries (one provider request; index open/write failures are bounded stderr warnings only).\n\n",
                "--query filters by subject/search text; empty or whitespace-only queries are rejected unless --all is given for a bounded listing of all visible issues.\n--state selects open (default all shows both), closed, or all.\n--page and --limit control the single page returned (page >=1, 1 <= limit <= 100).\nDefault output is compact metadata without bodies (id, number, title, state, html_url). Pass --include-body to include bounded bodies (byte cap 8192, truncated bodies report body_truncated: true).\nOutput envelope: { items: [...], page, limit, total_count?, has_more } where total_count is present when the provider returns it and has_more is derived from provider pagination metadata or item count.\n\n",
                "When provider resolution, auth, network, or search fails for a non-empty --query, search falls back to the local lexical index without provider credential/network lookup. Fallback is scoped to the known provider/project when available, else global, and returns { items: [{id, number, title, state, html_url, body?, body_truncated?, source?, project?, external_id?}], page, limit, total_count, has_more, data_source: \"local_index\", stale: true } with a concise stderr warning. Queryless --all has no fallback; with no local match/backend the original provider error is preserved. The index has no coverage/freshness model, so local-first is never the default; do not treat stale rows as fresh.\n\n",
                "Successful get/create/update/close opportunistically upsert the returned summary into the selected index (close is the closed document); index failures are warnings only. Backend selection is URL-driven: a non-empty PHASEGENT_INDEX_PG_URL (env overrides persisted) selects PostgreSQL, absent selects SQLite; PHASEGENT_INDEX_BACKEND is legacy and ignored.\n\n",
                "Redmine: when --project-id is omitted the current Git origin is matched against existing redmine_git_mirror records. Exactly one match uses that project; multiple matches fail with a listing of candidate ids/names and require --project-id; no match automatically bootstraps the project (admin credentials) as before. Explicit --project-id always wins and skips discovery. An explicit --repository that does not equal the origin is not silently matched; it keeps the existing bootstrap behavior.\n\n",
                "Values beginning with `-` must use the inline form: --query=TEXT or --state=STATE.",
            ),
        ),
        "create" => (
            Capability::IssueCreate,
            "Usage: issue create --title TEXT [--body TEXT | --body-file PATH [--keep-body-file]] [--tracker NAME_OR_ID] [--parent-issue ID] [--fixed-version NAME_OR_ID] [--start-date YYYY-MM-DD] [--due-date YYYY-MM-DD] [--estimated-hours HOURS] [--done-ratio 0-100]\n\n--body-file reads the body from a one-shot Markdown file (regular file, at most 2 MiB, valid UTF-8) instead of passing long text through the shell. It is mutually exclusive with --body. The file is read and validated locally before any provider or network access. After a successful write the file is deleted unless --keep-body-file is given; any read, validation, or provider failure keeps the file, and a path that was replaced or modified after the read is never deleted (a bounded warning is emitted instead).\n\n--tracker accepts a validated tracker name (Bug, Feature) or numeric id and is Redmine-only (GitLab maps it to a `type::bug` / `type::feature` label). Planning flags set native Redmine fields; --fixed-version resolves by exact version name or numeric id within the configured project. All Redmine planning flags are Redmine-only except --estimated-hours, which GitLab forwards through the native time_estimate endpoint. Forgejo rejects every planning flag.\n\nRedmine: when --project-id is omitted the current Git origin is matched against existing redmine_git_mirror records. Exactly one match uses that project and bypasses bootstrap; multiple matches fail before any write with candidate ids/names and require --project-id; no match automatically bootstraps the project (admin credentials) as before. Explicit --project-id always wins and skips discovery. An explicit --repository that does not equal the origin is not silently matched; it keeps the existing bootstrap behavior.\n\nValues beginning with `-` (Markdown bullets, separator lines) must use the inline form: --title=TEXT or --body=TEXT.",
        ),
        "update" => (
            Capability::IssueUpdateBody,
            "Usage: issue update <NUMBER> (--body TEXT | --body-file PATH [--keep-body-file]) [--tracker NAME_OR_ID] [--parent-issue ID] [--fixed-version NAME_OR_ID] [--start-date YYYY-MM-DD] [--due-date YYYY-MM-DD] [--estimated-hours HOURS] [--done-ratio 0-100]\n\nUpdate one issue in a single PUT. --body-file reads the body from a one-shot Markdown file (regular file, at most 2 MiB, valid UTF-8) instead of passing long text through the shell. It is mutually exclusive with --body. The file is read and validated locally before any provider or network access. After a successful write the file is deleted unless --keep-body-file is given; any read, validation, or provider failure keeps the file, and a path that was replaced or modified after the read is never deleted (a bounded warning is emitted instead).\n\n--tracker re-targets the issue's tracker in the same update (Redmine native; GitLab maps to a type::* label). Planning flags update native Redmine fields in the same PUT; --fixed-version resolves by exact version name or numeric id within the configured project. --estimated-hours is also accepted for GitLab (time_estimate); every other planning flag is Redmine-only. Forgejo rejects every planning flag.\n\nValues beginning with `-` (Markdown bullets, separator lines) must use the inline form: --body=TEXT.",
        ),
        "close" => (
            Capability::IssueClose,
            "Usage: issue close <NUMBER> [--worktree-session SESSION]\n\nClose the issue on the provider. Only after the remote close succeeds are the active worktree leases matching the resolved repo identity, this issue, and the current session flipped to `retained` with release_reason \"issue closed: <session>\". The current session resolves from --worktree-session, else PHASEGENT_SESSION_ID, else the legacy \"phasegent\" fallback; on the legacy fallback no owner is guessed and no lease is released, and a migration warning is written to stderr. A failed remote close leaves every local lease untouched, and other sessions, issues, and repo identities are never affected. No worktree directory or branch is deleted.",
        ),
        "upload-attachment" => (
            Capability::IssueAttachmentUpload,
            "Usage: issue upload-attachment <NUMBER> --path PATH [--description TEXT]\n\nUniformly not-supported (Phase 1 parity + Phase 4 sink); every provider rejects the command with `not_supported` (exit 1) before any file, network, or credential access. The capability stays reserved (orchestrator or tester) so a future phase may re-enable the underlying upload path. The wire shape it would have used is documented for reference only: raw POST /uploads.json?filename=<basename> with Content-Type application/octet-stream, then PUT /issues/<id>.json {\"issue\":{\"uploads\":[{\"token\":...,\"filename\":...}],\"notes\":...}}; the transient upload token is never printed; outputs compact JSON with issue, filename, bytes, and success. Values beginning with `-` must use the inline form: --path=PATH or --description=TEXT.",
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
        _ => return None,
    };
    Some(entry)
}

pub(crate) fn print_issue_command_help(role: Option<Role>, command: &str) {
    let Some((capability, text)) = issue_command_help_entry(command) else {
        print_issue_help(role);
        return;
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
    fn close_help_documents_worktree_session_and_release_rule() {
        let (capability, text) = issue_command_help_entry("close").expect("close help entry");
        assert_eq!(capability, Capability::IssueClose);
        assert!(
            text.contains("--worktree-session"),
            "close help must advertise --worktree-session; got: {text}"
        );
        assert!(
            text.contains("PHASEGENT_SESSION_ID"),
            "close help must document the session environment variable; got: {text}"
        );
        assert!(
            text.contains("retained"),
            "close help must document the retained lease outcome; got: {text}"
        );
        assert!(
            text.contains("failed remote close leaves every local lease untouched"),
            "close help must state the remote-failure boundary; got: {text}"
        );
    }

    #[test]
    fn unknown_issue_command_has_no_help_entry() {
        assert!(issue_command_help_entry("fly").is_none());
    }

    #[test]
    fn update_help_entry_replaces_removed_update_body_topic() {
        let (capability, text) = issue_command_help_entry("update").expect("update help entry");
        assert_eq!(capability, Capability::IssueUpdateBody);
        assert!(text.contains("Usage: issue update <NUMBER>"), "got: {text}");
        assert!(
            issue_command_help_entry("update-body").is_none(),
            "the removed update-body topic must not resolve"
        );
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

    #[test]
    fn help_routes_update_and_rejects_removed_update_body() {
        let invocation = crate::command::parse(&[
            "--role".to_owned(),
            "orchestrator".to_owned(),
            "--help".to_owned(),
            "issue".to_owned(),
            "update".to_owned(),
        ])
        .expect("help issue update must route");
        match invocation.command {
            crate::command::Command::Help(crate::command::HelpTopic::IssueCommand(value)) => {
                assert_eq!(value, "update");
            }
            other => panic!("unexpected command {other:?}"),
        }

        let error = crate::command::parse(&[
            "--role".to_owned(),
            "orchestrator".to_owned(),
            "--help".to_owned(),
            "issue".to_owned(),
            "update-body".to_owned(),
        ])
        .expect_err("help issue update-body must be rejected");
        assert!(
            error.contains("unknown issue help topic 'update-body'"),
            "unexpected error: {error}"
        );
    }
}
