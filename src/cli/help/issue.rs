#[cfg(test)]
use super::common::render_group_help;
use super::common::{HelpRow, print_group_help};
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

fn issue_help_parts(role: Option<Role>) -> (String, Vec<HelpRow<'static>>, Vec<HelpRow<'static>>) {
    let header = format!(
        "Issue commands for {}:",
        role.map_or("all roles", Role::as_str)
    );
    let main: Vec<HelpRow<'static>> = normal_issue_commands()
        .into_iter()
        .map(|(name, capability)| (name, capability.description(), capability))
        .collect();
    let local: Vec<HelpRow<'static>> = vec![
        (
            "bind",
            "Bind the current branch to a Redmine issue in local Git config",
            Capability::IssueRead,
        ),
        (
            "unbind",
            "Remove the current branch's Redmine issue binding",
            Capability::IssueRead,
        ),
        (
            "status",
            "Show the current branch and its Redmine issue with source bound/named/none",
            Capability::IssueRead,
        ),
    ];
    (header, main, local)
}

#[cfg(test)]
pub(crate) fn render_issue_help(role: Option<Role>) -> String {
    let (header, main, local) = issue_help_parts(role);
    render_group_help(
        role,
        &header,
        &[
            (None, &main),
            (
                Some("Local branch context (no provider or network access)"),
                &local,
            ),
        ],
        "Use 'phasegent --help issue <command>' for options.",
    )
}

pub(crate) fn print_issue_help(role: Option<Role>) {
    let (header, main, local) = issue_help_parts(role);
    print_group_help(
        role,
        &header,
        &[
            (None, &main),
            (
                Some("Local branch context (no provider or network access)"),
                &local,
            ),
        ],
        "Use 'phasegent --help issue <command>' for options.",
    )
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
            "Usage: issue create --title TEXT [--body TEXT | --body-file PATH [--keep-body-file]] [--tracker NAME_OR_ID] [--parent-issue ID] [--fixed-version NAME_OR_ID] [--start-date YYYY-MM-DD] [--due-date YYYY-MM-DD] [--estimated-hours HOURS] [--done-ratio 0-100] [--assignee ID_OR_USERNAME | --no-assign] [--branch [NAME]] [--base REF]\n\n--body-file reads the body from a one-shot Markdown file (regular file, at most 2 MiB, valid UTF-8) instead of passing long text through the shell. It is mutually exclusive with --body. The file is read and validated locally before any provider or network access. After a successful write the file is deleted unless --keep-body-file is given; any read, validation, or provider failure keeps the file, and a path that was replaced or modified after the read is never deleted (a bounded warning is emitted instead).\n\n--tracker accepts a validated tracker name (Bug, Feature) or numeric id and is Redmine-only (GitLab maps it to a `type::bug` / `type::feature` label). Planning flags set native Redmine fields; --fixed-version resolves by exact version name or numeric id within the configured project. All Redmine planning flags are Redmine-only except --estimated-hours, which GitLab forwards through the native time_estimate endpoint. Forgejo rejects every planning flag.\n\n--assignee is GitLab-only and accepts a numeric user id or a username resolved via GET /users?username=. GitLab self-assigns the authenticated user when neither flag is given; --no-assign keeps the issue unassigned, and --assignee is mutually exclusive with --no-assign. If the default self-assignment lookup (GET /user) fails, the issue is still created unassigned and a warning is written to stderr; the stdout JSON is unchanged. Other providers reject --assignee and treat --no-assign as a no-op with the legacy payload.\n\n--branch creates and binds a local branch explicitly (no default auto-create; Redmine-only, --base requires --branch). Bare --branch generates <type>/<id> from the tracker (Bug->fix, everything else->feat, e.g. feat/452); --branch NAME uses NAME verbatim; --base selects the new branch start point and defaults to HEAD. When --branch is given the target branch is created when missing and bound to the new issue instead of the current branch (an existing different binding is never overwritten); without --branch only the legacy current-branch auto-bind runs. Local failures warn on stderr; stdout JSON is unchanged.\n\nRedmine: when --project-id is omitted the current Git origin is matched against existing redmine_git_mirror records. Exactly one match uses that project and bypasses bootstrap; multiple matches fail before any write with candidate ids/names and require --project-id; no match automatically bootstraps the project (admin credentials) as before. Explicit --project-id always wins and skips discovery. An explicit --repository that does not equal the origin is not silently matched; it keeps the existing bootstrap behavior.\n\nValues beginning with `-` (Markdown bullets, separator lines) must use the inline form: --title=TEXT or --body=TEXT.",
        ),
        "update" => (
            Capability::IssueUpdateBody,
            "Usage: issue update <NUMBER> (--body TEXT | --body-file PATH [--keep-body-file]) [--tracker NAME_OR_ID] [--parent-issue ID] [--fixed-version NAME_OR_ID] [--start-date YYYY-MM-DD] [--due-date YYYY-MM-DD] [--estimated-hours HOURS] [--done-ratio 0-100]\n\nUpdate one issue in a single PUT. --body-file reads the body from a one-shot Markdown file (regular file, at most 2 MiB, valid UTF-8) instead of passing long text through the shell. It is mutually exclusive with --body. The file is read and validated locally before any provider or network access. After a successful write the file is deleted unless --keep-body-file is given; any read, validation, or provider failure keeps the file, and a path that was replaced or modified after the read is never deleted (a bounded warning is emitted instead).\n\n--tracker re-targets the issue's tracker in the same update (Redmine native; GitLab maps to a type::* label). Planning flags update native Redmine fields in the same PUT; --fixed-version resolves by exact version name or numeric id within the configured project. --estimated-hours is also accepted for GitLab (time_estimate); every other planning flag is Redmine-only. Forgejo rejects every planning flag.\n\nValues beginning with `-` (Markdown bullets, separator lines) must use the inline form: --body=TEXT.",
        ),
        "close" => (
            Capability::IssueClose,
            "Usage: issue close <NUMBER> [--worktree-session SESSION]\n\nClose the issue on the provider. Only after the remote close succeeds are the active worktree leases matching the resolved repo identity and this issue flipped to `retained` across every session, with release_reason \"issue closed: <session>\" naming the closing session. The closing session resolves from --worktree-session, else PHASEGENT_SESSION_ID, else the legacy \"phasegent\" fallback; on the legacy fallback no owner is guessed and no lease is released, and a migration warning is written to stderr. A failed remote close leaves every local lease untouched, and other issues and repo identities are never affected. No worktree directory or branch is deleted.",
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
            "Usage: issue status\n\nPrints the current branch and its Redmine issue with source bound/named/none (key first, then branch-name fallback). Detached HEAD is an error.",
        ),
        _ => return None,
    };
    Some(entry)
}

/// Full rendered text for one `issue` subcommand help entry: the usage body
/// plus the capability description. Split out from
/// [`print_issue_command_help`] so the rendered text is testable without
/// capturing stdout.
pub(crate) fn issue_command_help_text(command: &str) -> Option<(Capability, String)> {
    let (capability, entry) = issue_command_help_entry(command)?;
    Some((
        capability,
        format!("{entry}\n\n{}", capability.description()),
    ))
}

pub(crate) fn print_issue_command_help(role: Option<Role>, command: &str) {
    let Some((capability, text)) = issue_command_help_text(command) else {
        print_issue_help(role);
        return;
    };
    if role.is_none_or(|role| role.allows(capability)) {
        println!("{text}");
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

    /// Help text never enumerates `--provider`: it resolves from configuration
    /// and is named only in the root options list, so no issue page may tag it
    /// onto a usage line or repeat a global-option position note.
    #[test]
    fn issue_help_omits_the_global_option_note_and_never_advertises_provider() {
        for command in [
            "get",
            "search",
            "create",
            "update",
            "close",
            "upload-attachment",
            "bind",
            "unbind",
            "status",
        ] {
            let (_, text) = issue_command_help_text(command).expect("issue help must render");
            assert!(
                !text.contains("Global options ("),
                "{command} help must not carry the global-option position note; got: {text}"
            );
            assert!(
                !text.contains("--provider"),
                "{command} help must not advertise --provider; got: {text}"
            );
        }
    }

    /// Issue 337 renamed `issue update-body` to `issue update`; the removed
    /// token must stay unreachable from the help surface and from the CLI
    /// parser, while `update` remains the single write entry carrying body,
    /// tracker, and planning fields.
    #[test]
    fn update_is_the_write_entry_and_removed_update_body_surface_is_rejected() {
        let (capability, text) = issue_command_help_entry("update").expect("update help entry");
        assert_eq!(capability, Capability::IssueUpdateBody);
        assert!(text.contains("Usage: issue update <NUMBER>"), "got: {text}");
        assert!(
            issue_command_help_entry("update-body").is_none(),
            "the removed update-body topic must not resolve"
        );

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
        let help_error = crate::command::parse(&[
            "--role".to_owned(),
            "orchestrator".to_owned(),
            "--help".to_owned(),
            "issue".to_owned(),
            "update-body".to_owned(),
        ])
        .expect_err("help issue update-body must be rejected");
        assert!(
            help_error.contains("unknown issue help topic 'update-body'"),
            "unexpected error: {help_error}"
        );

        let update = [
            "--role",
            "orchestrator",
            "issue",
            "update",
            "9",
            "--body",
            "Updated",
            "--tracker",
            "Bug",
            "--due-date",
            "2026-09-15",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        match crate::command::parse(&update).unwrap().command {
            crate::command::Command::Issue(crate::command::IssueCommand::Update {
                number,
                body,
                tracker,
                planning,
                ..
            }) => {
                assert_eq!(number, 9);
                assert_eq!(body, "Updated");
                assert_eq!(tracker.as_deref(), Some("Bug"));
                assert_eq!(planning.due_date.as_deref(), Some("2026-09-15"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
        let removed = [
            "--role",
            "orchestrator",
            "issue",
            "update-body",
            "9",
            "--body",
            "x",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        let error =
            crate::command::parse(&removed).expect_err("removed update-body must be rejected");
        assert!(error.contains("unknown issue command"), "got: {error}");
    }

    #[test]
    fn help_routes_branch_context_commands_through_outer_dispatch() {
        for topic in ["bind", "unbind", "status"] {
            let invocation = crate::command::parse(&[
                "--role".to_owned(),
                "executor".to_owned(),
                "--help".to_owned(),
                "issue".to_owned(),
                topic.to_owned(),
            ])
            .unwrap_or_else(|error| panic!("help issue {topic} must route; got: {error}"));
            match invocation.command {
                crate::command::Command::Help(crate::command::HelpTopic::IssueCommand(value)) => {
                    assert_eq!(value, topic);
                }
                other => panic!("unexpected command {other:?}"),
            }
            let (capability, text) = issue_command_help_entry(topic)
                .unwrap_or_else(|| panic!("issue {topic} must have a help entry"));
            assert_eq!(capability, Capability::IssueRead);
            assert!(
                text.contains(&format!("Usage: issue {topic}")),
                "issue {topic} help must document its own usage; got: {text}"
            );
        }
    }

    #[test]
    fn overview_keeps_local_branch_context_table() {
        let text = render_issue_help(None);
        assert!(
            text.contains("Issue commands for all roles:"),
            "got: {text}"
        );
        assert!(
            text.contains("Local branch context (no provider or network access):"),
            "Local section must stay; got: {text}"
        );
        for command in ["bind", "unbind", "status"] {
            assert!(text.contains(command), "got: {text}");
        }
        assert!(
            text.contains("Use 'phasegent --help issue <command>' for options."),
            "got: {text}"
        );
        for (name, desc) in [
            ("get", Capability::IssueRead.description()),
            ("search", Capability::IssueSearch.description()),
        ] {
            assert!(
                text.contains(&format!("  {name:<14} {desc}")),
                "main rows stay one-command-per-line; got: {text}"
            );
        }
    }

    #[test]
    fn overview_filters_by_role_without_touching_detail_pages() {
        let text = render_issue_help(Some(Role::Executor));
        assert!(text.contains("  get"), "executor keeps get; got: {text}");
        assert!(
            !text.contains("create"),
            "executor denies create; got: {text}"
        );
        assert!(
            text.contains("bind"),
            "executor keeps local rows; got: {text}"
        );
        let full = render_issue_help(Some(Role::Orchestrator));
        for command in ["get", "search", "create", "update", "close", "bind"] {
            assert!(
                full.contains(command),
                "orchestrator sees {command}; got: {full}"
            );
        }
        let (_, bind_text) = issue_command_help_text("bind").expect("bind detail stays");
        assert!(
            bind_text.contains("Usage: issue bind <ID>"),
            "got: {bind_text}"
        );
    }
}
