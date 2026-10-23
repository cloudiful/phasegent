//! Unit tests for the scoped output context and the text renderer.

use super::context::{
    FormatInputs, HumanView, OutputContext, OutputMode, OutputOptions, OutputSelection,
};
use super::render;
use crate::policy::{CallerContext, Role};
use serde_json::json;

fn inputs(
    selection: OutputSelection,
    caller: CallerContext,
    stdout_is_terminal: bool,
    ci_active: bool,
) -> FormatInputs {
    FormatInputs {
        selection,
        caller,
        stdout_is_terminal,
        ci_active,
    }
}

/// The automatic default keeps every agent role on machine JSON, even on a
/// terminal, and gives the human operator text only on a real terminal outside
/// CI.
#[test]
fn automatic_default_keeps_agents_on_machine_json() {
    for role in [
        Role::Orchestrator,
        Role::Executor,
        Role::Reviewer,
        Role::Explore,
    ] {
        assert_eq!(
            super::context::resolve_format(&inputs(
                OutputSelection::Auto,
                CallerContext::Agent(role),
                true,
                false,
            )),
            OutputMode::Json,
            "role {role} must read machine JSON on a terminal"
        );
    }
}

/// A managed session without a role fails closed and stays machine-readable.
#[test]
fn automatic_default_keeps_managed_without_role_on_machine_json() {
    assert_eq!(
        super::context::resolve_format(&inputs(
            OutputSelection::Auto,
            CallerContext::ManagedWithoutRole,
            true,
            false,
        )),
        OutputMode::Json
    );
}

/// The operator and the explicit admin role read text on a terminal and
/// machine JSON everywhere else.
#[test]
fn automatic_default_gives_the_operator_text_on_a_terminal_only() {
    for caller in [CallerContext::Operator, CallerContext::Agent(Role::Admin)] {
        assert_eq!(
            super::context::resolve_format(&inputs(OutputSelection::Auto, caller, true, false,)),
            OutputMode::Text,
            "{caller:?} on a terminal reads text"
        );
        assert_eq!(
            super::context::resolve_format(&inputs(OutputSelection::Auto, caller, false, false,)),
            OutputMode::Json,
            "{caller:?} on a pipe reads machine JSON"
        );
        assert_eq!(
            super::context::resolve_format(&inputs(OutputSelection::Auto, caller, true, true,)),
            OutputMode::Json,
            "{caller:?} in CI reads machine JSON"
        );
    }
}

/// An explicit selection always wins, whatever the caller or the terminal.
#[test]
fn explicit_selection_wins_over_the_automatic_default() {
    for caller in [
        CallerContext::Operator,
        CallerContext::Agent(Role::Executor),
        CallerContext::ManagedWithoutRole,
    ] {
        assert_eq!(
            super::context::resolve_format(&inputs(OutputSelection::Text, caller, true, false,)),
            OutputMode::Text
        );
        assert_eq!(
            super::context::resolve_format(&inputs(OutputSelection::Json, caller, true, false,)),
            OutputMode::Json
        );
    }
}

/// The scope is thread-local, nest-safe, and restores the previous state.
#[test]
fn the_scope_nests_and_restores_the_previous_state() {
    assert!(super::context::current().is_none(), "no scope is active");
    let outer = super::context::install(
        &OutputOptions {
            selection: OutputSelection::Text,
            no_progress: false,
            legacy: None,
        },
        Some(CallerContext::Operator),
    );
    assert_eq!(
        super::context::current().expect("scope").mode,
        OutputMode::Text
    );
    {
        let _inner = super::context::install(
            &OutputOptions {
                selection: OutputSelection::Json,
                no_progress: true,
                legacy: None,
            },
            Some(CallerContext::Operator),
        );
        let inner = super::context::current().expect("scope");
        assert_eq!(inner.mode, OutputMode::Json);
        assert!(
            !inner.progress,
            "--no-progress disables the progress surface"
        );
    }
    let restored = super::context::current().expect("scope");
    assert_eq!(
        restored.mode,
        OutputMode::Text,
        "the outer scope is restored"
    );
    assert!(restored.progress);
    drop(outer);
    assert!(super::context::current().is_none(), "the scope is released");
}

/// Without a scope the safe default is machine JSON.
#[test]
fn a_missing_scope_defaults_to_machine_json() {
    let context = super::context::current_or_default();
    assert_eq!(context.mode, OutputMode::Json);
    assert!(!context.color);
    assert!(!context.progress);
}

/// An explicit legacy format wins over the resolved global format.
#[test]
fn an_explicit_legacy_format_overrides_the_global_format() {
    let context = OutputContext {
        mode: OutputMode::Text,
        view: HumanView::Generic,
        label: String::new(),
        width: 80,
        color: false,
        stderr_color: false,
        stdout_is_terminal: false,
        stderr_is_terminal: false,
        ci_active: false,
        term_capable: false,
        human_caller: false,
        progress: false,
        legacy: Some(OutputMode::Json),
    };
    assert_eq!(context.effective_mode(), OutputMode::Json);
    let plain = OutputContext {
        legacy: None,
        ..context
    };
    assert_eq!(plain.effective_mode(), OutputMode::Text);
}

fn render(value: &serde_json::Value) -> String {
    render::render_value(value, &context(HumanView::Generic, 80))
}

/// A rendering context for one view and one terminal width.
fn context(view: HumanView, width: usize) -> OutputContext {
    OutputContext {
        mode: OutputMode::Text,
        view,
        label: String::new(),
        width,
        color: false,
        stderr_color: false,
        stdout_is_terminal: false,
        stderr_is_terminal: false,
        ci_active: false,
        term_capable: false,
        human_caller: false,
        progress: false,
        legacy: None,
    }
}

/// Render one value with the view of a command.
fn render_view(view: HumanView, value: &serde_json::Value, width: usize) -> String {
    render::render_value(value, &context(view, width))
}

/// One lease of the `worktree list` envelope.
fn lease(id: &str, issue: u64, state: &str, branch: &str, path: &str) -> serde_json::Value {
    json!({
        "lease_id": id,
        "repo_identity": "/repo/.git",
        "issue": issue,
        "session": "ses_a",
        "checkout_path": "/repo",
        "worktree_path": path,
        "branch": branch,
        "status": state,
        "created_at": 1_700_000_000,
        "heartbeat_at": 1_700_000_060,
    })
}

/// The generic rendering is a readable key/value block, never a JSON dump.
#[test]
fn the_generic_rendering_reads_as_key_value_blocks() {
    let text = render(&json!({
        "title": "Fix the thing",
        "id": 1054,
        "state": "open",
    }));
    assert!(text.contains("Title: Fix the thing"), "got: {text}");
    assert!(text.contains("Id: 1054"), "got: {text}");
    assert!(text.contains("State: open"), "got: {text}");
    assert!(!text.contains('{'), "no JSON braces: {text}");
    assert!(!text.contains('"'), "no quoted dump: {text}");
}

/// Nested maps, arrays, nulls, and empty collections stay readable.
#[test]
fn the_generic_rendering_handles_nested_and_empty_values() {
    let text = render(&json!({
        "items": [
            {"name": "first", "count": 1},
            {"name": "second", "count": 2},
        ],
        "empty_list": [],
        "empty_map": {},
        "nothing": null,
    }));
    assert!(text.contains("Items:"), "got: {text}");
    assert!(text.contains("Name: first"), "got: {text}");
    assert!(text.contains("Count: 2"), "got: {text}");
    assert!(text.contains("Empty list: (empty)"), "got: {text}");
    assert!(text.contains("Empty map: (empty)"), "got: {text}");
    assert!(text.contains("Nothing: (null)"), "got: {text}");
}

/// Terminal control sequences never reach the rendered output.
#[test]
fn the_renderer_strips_terminal_control_sequences() {
    let text = render(&json!({
        "title": "\u{1b}[31mred\u{1b}[0m",
        "link": "\u{1b}]8;;https://example.com\u{7}label",
    }));
    assert!(!text.contains('\u{1b}'), "no escape sequence: {text}");
    assert!(text.contains("Title: red"), "got: {text}");
    assert!(text.contains("Link: label"), "got: {text}");
}

/// A value can never spoof the layout with a forged line.
#[test]
fn a_value_cannot_spoof_the_layout() {
    let text = render(&json!({
        "body": "first line\nerror: forged diagnostic",
    }));
    let forged = text
        .lines()
        .position(|line| line.starts_with("error: forged diagnostic"));
    assert_eq!(forged, None, "the forged line stays indented: {text}");
    assert!(text.contains("Body: first line"), "got: {text}");
}

/// A long value wraps to the terminal width instead of overflowing it.
#[test]
fn a_long_value_wraps_to_the_width() {
    let context = OutputContext {
        mode: OutputMode::Text,
        view: HumanView::Generic,
        label: String::new(),
        width: 40,
        color: false,
        stderr_color: false,
        stdout_is_terminal: false,
        stderr_is_terminal: false,
        ci_active: false,
        term_capable: false,
        human_caller: false,
        progress: false,
        legacy: None,
    };
    let value = json!({"body": "word ".repeat(40)});
    let text = render::render_value(&value, &context);
    for line in text.lines() {
        assert!(
            console::measure_text_width(line) <= 40,
            "line exceeds the width: {line}"
        );
    }
    assert!(text.lines().count() > 1, "the value wraps: {text}");
}

/// The diagnostic rendering is a readable label with the operation.
#[test]
fn a_diagnostic_renders_as_a_readable_label() {
    let text = render::render_diagnostic(
        "error",
        &json!({"kind": "argument", "operation": "issue create", "message": "title is required"}),
        false,
    );
    assert_eq!(text, "error: title is required\n  operation: issue create");
    let warning = render::render_diagnostic(
        "warning",
        &json!({"operation": "issue search", "message": "index warm skipped"}),
        false,
    );
    assert_eq!(
        warning,
        "warning: index warm skipped\n  operation: issue search"
    );
}

/// A message that already names the operation is not repeated.
#[test]
fn a_diagnostic_does_not_repeat_the_operation() {
    let text = render::render_diagnostic(
        "error",
        &json!({"operation": "issue create", "message": "issue create failed"}),
        false,
    );
    assert_eq!(text, "error: issue create failed");
}

/// The lease list names every lease, its issue, its state, and its branch.
#[test]
fn the_lease_list_names_every_lease() {
    let value = json!({
        "repo_identity": "/repo/.git",
        "leases": [
            lease("las-aaaa", 1054, "active", "phasegent/1054", "/repo/wt-1054"),
            lease("las-bbbb", 1056, "retained", "phasegent/1056", "/repo/wt-1056"),
        ],
    });
    let text = render_view(HumanView::WorktreeLeaseList, &value, 80);
    assert!(
        text.contains("2 leases: 1 active, 1 retained"),
        "got: {text}"
    );
    assert!(text.contains("las-aaaa"), "got: {text}");
    assert!(text.contains("#1054"), "got: {text}");
    assert!(text.contains("active"), "got: {text}");
    assert!(text.contains("phasegent/1054"), "got: {text}");
    assert!(text.contains("/repo/wt-1054"), "got: {text}");
    assert!(text.contains("las-bbbb"), "got: {text}");
    assert!(!text.contains('{'), "no JSON dump: {text}");
}

/// A narrow terminal stacks the entries instead of overflowing a table.
#[test]
fn a_narrow_terminal_stacks_the_lease_entries() {
    let value = json!({
        "repo_identity": "/repo/.git",
        "leases": [lease(
            "las-aaaa",
            1054,
            "active",
            "phasegent/1054-a-very-long-branch-name",
            "/repo/wt-1054",
        )],
    });
    let text = render_view(HumanView::WorktreeLeaseList, &value, 40);
    assert!(text.contains("las-aaaa"), "got: {text}");
    assert!(text.contains("Issue: #1054"), "got: {text}");
    assert!(text.contains("State: active"), "got: {text}");
    assert!(
        text.contains("Branch: phasegent/1054-a-very-long-bran"),
        "got: {text}"
    );
    for line in text.lines() {
        assert!(
            console::measure_text_width(line) <= 40,
            "line exceeds the width: {line}"
        );
    }
}

/// A large list keeps every distinct lease; nothing is deduplicated or
/// truncated.
#[test]
fn a_large_lease_list_keeps_every_distinct_lease() {
    let leases: Vec<serde_json::Value> = (0..50)
        .map(|index| {
            lease(
                &format!("las-{index:04}"),
                1000 + index as u64,
                "active",
                &format!("phasegent/{index}"),
                &format!("/repo/wt-{index}"),
            )
        })
        .collect();
    let value = json!({"repo_identity": "/repo/.git", "leases": leases});
    let text = render_view(HumanView::WorktreeLeaseList, &value, 80);
    assert!(text.contains("50 leases: 50 active"), "got: {text}");
    for index in 0..50 {
        assert!(
            text.contains(&format!("las-{index:04}")),
            "lease {index} is missing: {text}"
        );
    }
}

/// An empty lease list is an explicit empty state.
#[test]
fn an_empty_lease_list_states_that_nothing_is_recorded() {
    let value = json!({"repo_identity": "/repo/.git", "leases": []});
    let text = render_view(HumanView::WorktreeLeaseList, &value, 80);
    assert!(text.contains("0 leases"), "got: {text}");
    assert!(
        text.contains("No worktree leases are recorded"),
        "got: {text}"
    );
}

/// The probe shows the path, the branch, the state, and the lease.
#[test]
fn the_probe_shows_the_path_branch_state_and_lease() {
    let value = json!({
        "resolved": true,
        "path": "/repo/wt-1054",
        "exists": true,
        "is_git_worktree": true,
        "clean": true,
        "branch": "phasegent/1054",
        "head": "ff31031",
        "is_main_checkout": false,
        "lease": {
            "lease_id": "las-aaaa",
            "repo_identity": "/repo/.git",
            "issue": 1054,
            "session": "ses_a",
            "status": "active",
            "branch": "phasegent/1054",
            "worktree_path": "/repo/wt-1054",
        },
        "errors": [],
    });
    let text = render_view(HumanView::WorktreeProbe, &value, 80);
    assert!(text.contains("Path: /repo/wt-1054"), "got: {text}");
    assert!(text.contains("Branch: phasegent/1054"), "got: {text}");
    assert!(text.contains("Head: ff31031"), "got: {text}");
    assert!(text.contains("State: clean worktree"), "got: {text}");
    assert!(
        text.contains("Lease: las-aaaa (issue #1054, active)"),
        "got: {text}"
    );
}

/// An unresolved probe states that nothing matched and keeps the errors.
#[test]
fn an_unresolved_probe_states_that_nothing_matched() {
    let value = json!({
        "resolved": false,
        "path": null,
        "exists": false,
        "is_git_worktree": false,
        "clean": null,
        "branch": null,
        "head": null,
        "is_main_checkout": null,
        "lease": null,
        "errors": [{"kind": "argument", "message": "no active lease matched issue 99"}],
    });
    let text = render_view(HumanView::WorktreeProbe, &value, 80);
    assert!(
        text.contains("No worktree matched the selector."),
        "got: {text}"
    );
    assert!(
        text.contains("error: no active lease matched issue 99"),
        "got: {text}"
    );
    assert!(!text.contains("Lease:"), "no lease is invented: {text}");
}

/// The acquire result states the lease, the path, the branch, and the outcome.
#[test]
fn the_acquire_result_states_the_outcome() {
    let value = json!({
        "lease_id": "las-aaaa",
        "path": "/repo/wt-1054",
        "branch": "phasegent/1054",
        "repo_identity": "/repo/.git",
        "created": true,
        "reason": "new_worktree",
    });
    let text = render_view(HumanView::WorktreeAcquire, &value, 80);
    assert!(text.contains("Lease: las-aaaa"), "got: {text}");
    assert!(text.contains("Path: /repo/wt-1054"), "got: {text}");
    assert!(text.contains("Branch: phasegent/1054"), "got: {text}");
    assert!(text.contains("Outcome: created"), "got: {text}");
    assert!(text.contains("Reason: new_worktree"), "got: {text}");
}

/// The issue detail shows the id, the title, the state, the project, the URL,
/// and the body.
#[test]
fn the_issue_detail_shows_the_readable_blocks() {
    let value = json!({
        "id": 1054,
        "number": 1054,
        "title": "Human-first CLI presentation",
        "body": "First line.\nSecond line.",
        "state": "open",
        "html_url": "https://redmine.example.com/issues/1054",
        "project": {"id": 23, "name": "tools/phasegent", "identifier": "phasegent"},
    });
    let text = render_view(HumanView::IssueDetail, &value, 80);
    assert!(
        text.contains("#1054 Human-first CLI presentation"),
        "got: {text}"
    );
    assert!(text.contains("State: open"), "got: {text}");
    assert!(
        text.contains("Project: tools/phasegent (phasegent)"),
        "got: {text}"
    );
    assert!(
        text.contains("URL: https://redmine.example.com/issues/1054"),
        "got: {text}"
    );
    assert!(text.contains("Body:"), "got: {text}");
    assert!(text.contains("First line."), "got: {text}");
    assert!(text.contains("Second line."), "got: {text}");
}

/// A write result is concise: it states what changed, never the body again.
#[test]
fn a_write_result_does_not_echo_the_body() {
    let value = json!({
        "id": 1054,
        "number": 1054,
        "title": "Human-first CLI presentation",
        "body": "A body that must not be echoed.",
        "state": "open",
        "html_url": "https://redmine.example.com/issues/1054",
    });
    let text = render_view(HumanView::IssueWrite, &value, 80);
    assert!(
        text.contains("#1054 Human-first CLI presentation"),
        "got: {text}"
    );
    assert!(text.contains("State: open"), "got: {text}");
    assert!(
        !text.contains("A body that must not be echoed."),
        "the body is not echoed: {text}"
    );
}

/// The search result lists readable rows and an explicit empty state.
#[test]
fn the_search_result_lists_rows_and_an_empty_state() {
    let value = json!({
        "items": [
            {"id": 1, "number": 1, "title": "First issue", "state": "open"},
            {"id": 2, "number": 2, "title": "Second issue", "state": "closed"},
        ],
        "page": 1,
        "limit": 20,
        "total_count": 5,
        "has_more": true,
    });
    let text = render_view(HumanView::IssueSearch, &value, 80);
    assert!(
        text.contains("5 results (page 1, limit 20), more available"),
        "got: {text}"
    );
    assert!(text.contains("#1"), "got: {text}");
    assert!(text.contains("First issue"), "got: {text}");
    assert!(text.contains("#2"), "got: {text}");
    assert!(text.contains("Second issue"), "got: {text}");

    let empty = json!({"items": [], "page": 1, "limit": 20, "has_more": false});
    let text = render_view(HumanView::IssueSearch, &empty, 80);
    assert!(text.contains("0 results"), "got: {text}");
    assert!(
        text.contains("No issues matched the search."),
        "got: {text}"
    );
}

/// The status report names the current state, the next states, and the
/// recovery.
#[test]
fn the_status_report_names_the_current_next_and_recovery() {
    let value = json!({
        "issue": 1054,
        "current": {"id": 2, "name": "In Progress", "canonical": true},
        "allowed_next": [
            {"id": 3, "name": "In Review", "canonical": true},
            {"id": 6, "name": "Resolved", "canonical": true},
        ],
        "policy_source": "canonical",
        "advisory": false,
        "caveat": "custom statuses are advisory",
        "recovery": "set the status by hand",
    });
    let text = render_view(HumanView::StatusNext, &value, 80);
    assert!(text.contains("Issue: #1054"), "got: {text}");
    assert!(text.contains("Current status: In Progress"), "got: {text}");
    assert!(
        text.contains("Next allowed: In Review, Resolved"),
        "got: {text}"
    );
    assert!(
        text.contains("Recovery: set the status by hand"),
        "got: {text}"
    );
}

/// The status change names the transition and whether it changed anything.
#[test]
fn the_status_change_names_the_transition() {
    let value = json!({
        "issue": 1054,
        "changed": true,
        "from": {"name": "In Progress"},
        "to": {"name": "In Review"},
        "policy_source": "canonical",
        "advisory": false,
    });
    let text = render_view(HumanView::StatusTransition, &value, 80);
    assert!(text.contains("Issue: #1054"), "got: {text}");
    assert!(
        text.contains("Status: In Progress -> In Review"),
        "got: {text}"
    );
    assert!(text.contains("Changed: yes"), "got: {text}");

    let unchanged = json!({
        "issue": 1054,
        "changed": false,
        "from": {"name": "In Review"},
        "to": {"name": "In Review"},
    });
    let text = render_view(HumanView::StatusTransition, &unchanged, 80);
    assert!(
        text.contains("Changed: no (already in that status)"),
        "got: {text}"
    );
}

/// The configuration view uses the redacted values and the presence
/// indicators, and never a secret.
#[test]
fn the_configuration_view_stays_redacted() {
    let value = json!({
        "database_path": "/home/dev/.config/phasegent/phasegent.sqlite3",
        "roles": [
            {
                "role": "admin",
                "provider": "redmine",
                "redmine_close_status_id": 5,
                "redmine_credential": {"present": true, "length": 12, "fingerprint": "…1234"},
            },
            {
                "role": "executor",
                "provider": null,
                "redmine_close_status_id": null,
                "redmine_credential": {"present": false},
            },
        ],
        "global_settings": [
            {"name": "PHASEGENT_REDMINE_API_BASE", "present": true, "sanitized_value": "https://redmine.example.com"},
            {"name": "PHASEGENT_REDMINE_API_KEY", "present": false},
        ],
        "global_default_provider": null,
    });
    let text = render_view(HumanView::Config, &value, 80);
    assert!(text.contains("Database: /home/dev/.config"), "got: {text}");
    assert!(text.contains("Default provider: (unset)"), "got: {text}");
    assert!(text.contains("Global settings:"), "got: {text}");
    assert!(
        text.contains("present (https://redmine.example.com)"),
        "got: {text}"
    );
    assert!(
        text.contains("PHASEGENT_REDMINE_API_KEY: missing"),
        "got: {text}"
    );
    assert!(text.contains("Roles:"), "got: {text}");
    assert!(text.contains("admin: provider redmine"), "got: {text}");
    assert!(text.contains("credential present"), "got: {text}");
    assert!(text.contains("executor: provider (unset)"), "got: {text}");
    assert!(text.contains("credential missing"), "got: {text}");
}

/// The doctor view separates the healthy checks from the warnings.
#[test]
fn the_doctor_view_separates_healthy_checks_from_warnings() {
    let value = json!({
        "database_path": "/home/dev/.config/phasegent/phasegent.sqlite3",
        "roles": [
            {"role": "admin", "provider": null, "redmine_close_status_id": null, "redmine_credential": {"present": false}},
        ],
        "global_default_provider": "redmine",
        "index": {"backend": "sqlite", "sqlite_path": "/home/dev/.config/phasegent/index.sqlite3", "sqlite_exists": false},
    });
    let text = render_view(HumanView::Doctor, &value, 80);
    assert!(text.contains("Healthy:"), "got: {text}");
    assert!(text.contains("Warnings:"), "got: {text}");
    assert!(text.contains("default provider: redmine"), "got: {text}");
    assert!(text.contains("is not created yet"), "got: {text}");
    assert!(text.contains("Roles:"), "got: {text}");

    let healthy = json!({
        "database_path": "/home/dev/.config/phasegent/phasegent.sqlite3",
        "roles": [],
        "global_default_provider": "redmine",
        "index": {"backend": "sqlite", "sqlite_path": "/home/dev/.config/phasegent/index.sqlite3", "sqlite_exists": true},
    });
    let text = render_view(HumanView::Doctor, &healthy, 80);
    assert!(
        !text.contains("Warnings:"),
        "a healthy report has no warnings: {text}"
    );
}

/// A Unicode path and a long path stay readable and never overflow.
#[test]
fn unicode_and_long_paths_stay_readable() {
    let value = json!({
        "repo_identity": "/repo/.git",
        "leases": [lease(
            "las-aaaa",
            1054,
            "active",
            "phasegent/1054",
            "/repo/作业-目录/very-long-worktree-directory-name-that-keeps-going",
        )],
    });
    let text = render_view(HumanView::WorktreeLeaseList, &value, 80);
    assert!(text.contains("作业-目录"), "got: {text}");
    for line in text.lines() {
        assert!(
            console::measure_text_width(line) <= 80,
            "line exceeds the width: {line}"
        );
    }
}

/// A terminal that captures the drawn frames, so the lifecycle test observes
/// the ticker without a real terminal.
#[derive(Clone, Debug, Default)]
struct FakeTerm {
    written: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl FakeTerm {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.written.lock().expect("lock")).into_owned()
    }
}

impl indicatif::TermLike for FakeTerm {
    fn width(&self) -> u16 {
        80
    }
    fn height(&self) -> u16 {
        24
    }
    fn move_cursor_up(&self, _lines: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn move_cursor_down(&self, _lines: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn move_cursor_right(&self, _columns: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn move_cursor_left(&self, _columns: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_line(&self, text: &str) -> std::io::Result<()> {
        self.write_str(text)?;
        self.write_str("\n")
    }
    fn write_str(&self, text: &str) -> std::io::Result<()> {
        self.written
            .lock()
            .expect("lock")
            .extend_from_slice(text.as_bytes());
        Ok(())
    }
    fn clear_line(&self) -> std::io::Result<()> {
        Ok(())
    }
    fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A list item that is an object renders nested children as keyed blocks, so a
/// batch result never dumps the machine shape back at the reader.
#[test]
fn a_list_object_renders_nested_children_as_keyed_blocks() {
    let text = render(&json!({
        "items": [
            {
                "id": 1,
                "project": {"id": 23, "name": "Tools"},
                "labels": ["bug", "cli"],
            },
            {
                "meta": {"id": 9},
            },
        ],
    }));
    assert!(text.contains("Project:"), "got: {text}");
    assert!(text.contains("Name: Tools"), "got: {text}");
    assert!(text.contains("Labels:"), "got: {text}");
    assert!(text.contains("bug") && text.contains("cli"), "got: {text}");
    assert!(
        text.contains("Meta:"),
        "a nested first field starts a block: {text}"
    );
    assert!(text.contains("Id: 9"), "got: {text}");
    assert!(
        !text.contains('{') && !text.contains('"'),
        "no serialized JSON child: {text}"
    );
}

/// A heading is a single structural line, so an untrusted title can never
/// impersonate a field, and a real multiline body keeps its own lines.
#[test]
fn a_title_cannot_impersonate_a_field() {
    let value = json!({
        "number": 1,
        "title": "Title\nState: closed",
        "state": "open",
        "body": "line one\nline two",
    });
    let text = render_view(HumanView::IssueDetail, &value, 80);
    let state_lines: Vec<&str> = text
        .lines()
        .filter(|line| line.starts_with("State:"))
        .collect();
    assert_eq!(
        state_lines,
        vec!["State: open"],
        "the forged State line must not become structural: {text}"
    );
    assert!(text.contains("#1 Title State: closed"), "got: {text}");
    assert!(
        text.contains(" line one") && text.contains(" line two"),
        "the real body keeps both lines: {text}"
    );
    assert!(
        !text.lines().any(|line| line == "line two"),
        "a body line stays indented: {text}"
    );

    let search = render_view(
        HumanView::IssueSearch,
        &json!({"items": [{"number": 1, "state": "open", "title": "Title\nState: closed"}], "total_count": 1, "page": 1, "limit": 20}),
        80,
    );
    assert!(
        !search.lines().any(|line| line.starts_with("State: closed")),
        "a search row keeps a multiline title on one row: {search}"
    );
    assert!(search.contains("Title State: closed"), "got: {search}");
}

/// A title that wraps must not recreate a structural field at column zero, at
/// either width and in both curated issue views, while the whole title and a
/// real multiline body survive.
#[test]
fn a_wrapped_title_cannot_impersonate_a_field() {
    for width in [40, 80] {
        let run = "X".repeat(width - 3);
        let title = format!("{run}\nState: closed");
        let value = json!({
            "number": 1,
            "title": title,
            "state": "open",
            "body": "first\nsecond",
        });
        for view in [HumanView::IssueDetail, HumanView::IssueWrite] {
            let text = render_view(view, &value, width);
            for line in text.lines() {
                assert!(
                    console::measure_text_width(line) <= width,
                    "width {width}, view {view:?} overflows: {line:?}\n{text}"
                );
            }
            let state_lines: Vec<&str> = text
                .lines()
                .filter(|line| line.starts_with("State:"))
                .collect();
            assert_eq!(
                state_lines,
                vec!["State: open"],
                "width {width}, view {view:?}: the wrapped title must not add a State field\n{text}"
            );
            assert!(
                text.contains("State: closed"),
                "width {width}, view {view:?}: the title text is preserved\n{text}"
            );
            assert!(
                text.contains(&run),
                "width {width}, view {view:?}: the whole title run is preserved\n{text}"
            );
            if view == HumanView::IssueDetail {
                assert!(
                    text.contains(" first") && text.contains(" second"),
                    "width {width}: the real body keeps both lines\n{text}"
                );
            }
        }
    }
}

/// A shipped configuration name stacks instead of pushing a narrow terminal
/// past its width, and every character of the name and its state survive.
#[test]
fn a_shipped_config_label_stacks_instead_of_overflowing() {
    let value = json!({
        "database_path": "/home/operator/.config/phasegent/phasegent.sqlite3",
        "global_default_provider": null,
        "global_settings": [{
            "name": "PHASEGENT_NOTIFY_DINGTALK_WEBHOOK_URL",
            "present": false,
        }],
        "roles": [],
    });
    let compact = |text: &str| text.replace(['\n', ' '], "");
    for width in [40, 80] {
        let text = render_view(HumanView::Config, &value, width);
        for line in text.lines() {
            assert!(
                console::measure_text_width(line) <= width,
                "width {width} overflows: {line:?}\n{text}"
            );
        }
        assert!(
            compact(&text).contains("PHASEGENT_NOTIFY_DINGTALK_WEBHOOK_URL:"),
            "width {width}: the full setting name survives\n{text}"
        );
        assert!(
            text.contains("missing"),
            "width {width}: the state survives\n{text}"
        );
    }
}

/// A long generic key stacks and wraps without truncation at either width.
#[test]
fn a_long_generic_key_wraps_at_narrow_width() {
    let key = "a_very_long_generic_key_name_that_does_not_fit_at_forty_columns";
    for width in [40, 80] {
        let text = render_view(HumanView::Generic, &json!({ key: "value" }), width);
        for line in text.lines() {
            assert!(
                console::measure_text_width(line) <= width,
                "width {width} overflows: {line:?}\n{text}"
            );
        }
        let normalized = text.replace('\n', " ");
        assert!(
            normalized.contains("A very long generic key name") && normalized.contains("columns:"),
            "width {width}: the key keeps every character\n{text}"
        );
        assert!(
            text.contains("value"),
            "width {width}: the value survives\n{text}"
        );
    }
}

/// Every heading and prune row fits the terminal width at 40 and 80 columns
/// while keeping the actionable path and reason.
#[test]
fn headings_and_prune_rows_fit_the_width_without_truncation() {
    let issue = json!({
        "number": 1,
        "title": "A very long issue title that keeps going well past a narrow terminal boundary",
        "state": "open",
    });
    let path = "/home/operator/cache/phasegent/worktrees/fingerprint/phasegent-7-5136c6";
    let reason = "the configured provider reports the issue as still open";
    let prune = json!({
        "dry_run": false,
        "repos": [{
            "worktrees": {"actions": [{
                "worktree_path": path,
                "result": "skipped_issue",
                "message": reason,
            }]},
        }],
        "totals": {
            "removed": 0, "eligible": 0, "absent": 0, "kept_active": 0,
            "kept_unknown": 0, "kept_protected": 0, "kept_issue": 1, "failed": 0,
        },
    });
    let search = json!({
        "items": [{"number": 1, "state": "open", "title": "A very long issue title that keeps going past the boundary"}],
        "total_count": 1,
        "page": 1,
        "limit": 20,
    });
    let write = json!({
        "number": 1,
        "title": "A very long issue title that keeps going well past a narrow terminal boundary",
        "state": "closed",
    });
    let nested = json!({
        "items": [{
            "id": 1,
            "project": {"id": 23, "name": "A project name that is long enough to need wrapping"},
        }],
    });
    let config = json!({
        "database_path": "/home/operator/.config/phasegent/phasegent.sqlite3",
        "global_default_provider": null,
        "global_settings": [{
            "name": "PHASEGENT_NOTIFY_DINGTALK_WEBHOOK_URL",
            "present": false,
        }],
        "roles": [{
            "role": "admin",
            "provider": null,
            "redmine_close_status_id": null,
            "redmine_credential": {"present": false},
        }],
    });
    let doctor = json!({
        "database_path": "/home/operator/.config/phasegent/phasegent.sqlite3",
        "roles": [{"role": "admin", "provider": null, "redmine_close_status_id": null, "redmine_credential": {"present": false}}],
        "global_default_provider": "redmine",
        "index": {"backend": "sqlite", "sqlite_path": "/home/operator/.config/phasegent/phasegent-index.sqlite3", "sqlite_exists": false},
    });
    for width in [40, 80] {
        for (view, value) in [
            (HumanView::IssueDetail, &issue),
            (HumanView::IssueWrite, &write),
            (HumanView::IssueSearch, &search),
            (HumanView::Generic, &nested),
            (HumanView::WorktreePrune, &prune),
            (HumanView::Config, &config),
            (HumanView::Doctor, &doctor),
        ] {
            let text = render_view(view, value, width);
            for line in text.lines() {
                assert!(
                    console::measure_text_width(line) <= width,
                    "width {width}, view {view:?} overflows: {line:?}\n{text}"
                );
            }
            if view == HumanView::WorktreePrune {
                assert!(
                    text.replace('\n', "").contains(path),
                    "the path survives the wrap at width {width}: {text}"
                );
                assert!(
                    text.replace('\n', " ").contains(reason),
                    "the reason survives the wrap at width {width}: {text}"
                );
            }
        }
    }
}

/// The progress gate reads the effective mode, so a legacy JSON selection never
/// animates even under an automatic text default.
#[test]
fn the_progress_gate_reads_the_effective_mode() {
    let allowed = crate::output::progress::allowed;
    let text = OutputContext {
        mode: OutputMode::Text,
        human_caller: true,
        stdout_is_terminal: true,
        stderr_is_terminal: true,
        term_capable: true,
        progress: true,
        ..context(HumanView::Generic, 80)
    };
    assert!(allowed(&text), "an interactive human text run may animate");
    let legacy_json = OutputContext {
        legacy: Some(OutputMode::Json),
        ..text.clone()
    };
    assert!(
        !allowed(&legacy_json),
        "a legacy JSON selection overrides the text default and must stay quiet"
    );
    let machine = OutputContext {
        mode: OutputMode::Json,
        ..text.clone()
    };
    assert!(!allowed(&machine), "a machine default is quiet");
    let legacy_text = OutputContext {
        legacy: Some(OutputMode::Text),
        ..machine
    };
    assert!(
        allowed(&legacy_text),
        "a legacy text selection is the effective text mode"
    );
    let not_human = OutputContext {
        human_caller: false,
        ..legacy_text
    };
    assert!(!allowed(&not_human), "an agent role never animates");
}

/// The progress needs the text mode, a human caller, an interactive terminal
/// on both streams, no CI, a capable terminal, and no `--no-progress`.
#[test]
fn the_progress_is_only_allowed_for_a_human_interactive_run() {
    let allowed = crate::output::progress::allowed;
    let mut context = context(HumanView::Generic, 80);
    context.mode = OutputMode::Text;
    context.human_caller = true;
    context.stdout_is_terminal = true;
    context.stderr_is_terminal = true;
    context.term_capable = true;
    context.progress = true;
    assert!(
        allowed(&context),
        "a human on an interactive terminal may animate"
    );

    // Every missing condition switches the progress off.
    let mut machine = context.clone();
    machine.mode = OutputMode::Json;
    assert!(!allowed(&machine), "the machine mode never animates");
    let mut no_human = context.clone();
    no_human.human_caller = false;
    assert!(!allowed(&no_human), "an agent role never animates");
    let mut no_stdout = context.clone();
    no_stdout.stdout_is_terminal = false;
    assert!(!allowed(&no_stdout), "a piped stdout never animates");
    let mut no_stderr = context.clone();
    no_stderr.stderr_is_terminal = false;
    assert!(!allowed(&no_stderr), "a piped stderr never animates");
    let mut ci = context.clone();
    ci.ci_active = true;
    assert!(!allowed(&ci), "CI never animates");
    let mut dumb = context.clone();
    dumb.term_capable = false;
    assert!(!allowed(&dumb), "a dumb terminal never animates");
    let mut switched_off = context.clone();
    switched_off.progress = false;
    assert!(
        !allowed(&switched_off),
        "--no-progress switches the progress off"
    );
}

/// The ticker draws the stage, never a terminal control sequence, and the
/// guard leaves nothing behind.
#[test]
fn the_progress_lifecycle_draws_sanitized_stages_and_leaves_nothing_behind() {
    let term = FakeTerm::default();
    let guard = crate::output::progress::ProgressGuard::start_with_target(
        "worktree prune",
        indicatif::ProgressDrawTarget::term_like(Box::new(term.clone())),
    );
    guard.tick();
    assert!(term.contents().contains("worktree prune"), "got: {term:?}");
    guard.update("scanning \u{1b}[31m2 repositories\u{1b}[0m");
    guard.tick();
    let drawn = term.contents();
    assert!(drawn.contains("scanning 2 repositories"), "got: {drawn:?}");
    assert!(
        !drawn.contains("\u{1b}[31m"),
        "the stage is sanitized: {drawn:?}"
    );
    guard.clear();
    drop(guard);
    assert!(
        !crate::output::progress::active(),
        "no ticker is left behind after the run"
    );
}

/// A stage outside a progress run is dropped instead of reaching a stream.
#[test]
fn a_stage_outside_a_progress_run_is_dropped() {
    assert!(!crate::output::progress::active());
    crate::output::progress::stage("nothing is running");
    crate::output::progress::clear();
    assert!(!crate::output::progress::active());
}
