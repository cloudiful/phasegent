use super::common::{HelpRow, print_group_help, render_group_help};
use crate::policy::{Capability, Role};

/// Split the top-level `timer` overview into header plus rows so the shape is
/// testable without capturing stdout. Rows use an orchestrator-only
/// capability purely as a `role.allows` gate (the executors in
/// `src/time_tracking/` check `Role::Orchestrator` directly); the overview
/// stays one-line-per-command and contract prose lives on the detail pages.
fn timer_help_parts(role: Option<Role>) -> (String, Vec<HelpRow<'static>>) {
    let header = format!(
        "Timer commands for {}:",
        role.map_or("all roles", Role::as_str)
    );
    let rows: Vec<HelpRow<'static>> = vec![
        (
            "start",
            "Persist a local phase run (manual fallback)",
            Capability::IssueCreate,
        ),
        (
            "finish",
            "Finish a manually-opened run and project its time",
            Capability::IssueCreate,
        ),
        (
            "list",
            "Inspect local phase runs (read-only, local-only)",
            Capability::IssueCreate,
        ),
        (
            "get",
            "Show one local phase run (read-only, local-only)",
            Capability::IssueCreate,
        ),
        (
            "recover",
            "Mark a known orphan FAILED and project via the configured provider",
            Capability::IssueCreate,
        ),
    ];
    (header, rows)
}

/// Top-level `timer` help body rendered through the shared group helper.
/// Contract prose (local-only ledger order, Redmine/GitLab-only projection,
/// orphan reconciliation, tester child-identity note) lives on the
/// per-subcommand detail pages and stays out of this overview.
pub(crate) fn timer_help_text(role: Option<Role>) -> String {
    if role.is_some_and(|role| role != Role::Orchestrator) {
        return format!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
    let (header, rows) = timer_help_parts(role);
    render_group_help(
        role,
        &header,
        &[(None, &rows)],
        "Use 'phasegent --help timer <command>' for options.",
    )
}

pub(crate) fn print_timer_help(role: Option<Role>) {
    if role.is_some_and(|role| role != Role::Orchestrator) {
        println!("{}", timer_help_text(role));
        return;
    }
    let (header, rows) = timer_help_parts(role);
    print_group_help(
        role,
        &header,
        &[(None, &rows)],
        "Use 'phasegent --help timer <command>' for options.",
    )
}

pub(crate) fn print_timer_command_help(role: Option<Role>, command: &str) {
    println!("{}", timer_command_help_text(role, command));
}

/// Per-subcommand `timer` help body (no trailing newline).
pub(crate) fn timer_command_help_text(role: Option<Role>, command: &str) -> String {
    match command {
        "start" => orchestrator_help(
            role,
            "Usage: timer start <ISSUE> --phase NAME --agent-role executor|reviewer|tester --attempt N [--run-id ID] [--owner-session-id S --owner-call-id C]\n\nManual fallback for opening a local ledger row. The lifecycle path opens runs automatically on status transitions, so AI workflows should not call timer start directly; this command exists so an operator can recover after a missed auto-start or a crashed orchestrator. The orchestrator writes a local ledger row before any remote operation. --agent-role is executor, reviewer, or tester (tester is a first-class role with its own Redmine credential; timers remain orchestrator-only and tester project membership is optional during bootstrap); --attempt is a positive integer. Optional --owner-session-id / --owner-call-id record the OpenCode subagent identity (bounded, control-character-free, never projected). Redmine-only when --agent-role is set; Forgejo rejects timer start.",
        ),
        "finish" => orchestrator_help(
            role,
            "Usage: timer finish <RUN_ID> --result DONE|PARTIAL|BLOCKED|FAILED\n\nManual fallback for closing a run that the lifecycle path could not auto-finish. The lifecycle path auto-finishes on the next status transition and on issue close, so AI workflows should not call timer finish directly. When called manually, the orchestrator records exact elapsed seconds, then projects them to the configured provider (Redmine or GitLab). Retries on the same run id are safe; the marker-based reconciliation short-circuits before any duplicate Time Entry or spent-time POST. Redmine receives the rounded 0.01-hour summary with a stable run-marker comment; GitLab receives the exact elapsed seconds in human-format duration with the marker embedded in the spent-time summary.",
        ),
        "list" => orchestrator_help(
            role,
            "Usage: timer list [--status running|finished|all] [--limit N]\n\nRead-only listing of the local execution ledger. --status defaults to all; --limit caps the rows returned (default 100, max 1000). Never reaches the provider; never echoes secrets or full responses. Useful for spotting orphans before explicit recovery.",
        ),
        "get" => orchestrator_help(
            role,
            "Usage: timer get <RUN_ID>\n\nRead a single execution-ledger row. Returns a structured config error when the run id is unknown; never mutates state and never reaches the provider. Use this before recover to confirm the orphan still belongs to the local run id you have.",
        ),
        "recover" => orchestrator_help(
            role,
            "Usage: timer recover <RUN_ID>\n\nMark a known orphan FAILED and project it through the configured provider with the same-run marker reconciliation used by finish. Never infers a successful outcome from a missing child transcript. If the row is already terminal, the command returns the unchanged run without reopening it. Concurrent recovers on the same run id are safe; the SQLite primary key and finish_time idempotency make them no-ops. Missing owner metadata does not change recover's behaviour.",
        ),
        _ => timer_help_text(role),
    }
}

fn orchestrator_help(role: Option<Role>, text: &str) -> String {
    if role.is_none_or(|role| role == Role::Orchestrator) {
        text.to_owned()
    } else {
        format!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overview_is_tabular_with_one_row_per_command() {
        let text = timer_help_text(Some(Role::Orchestrator));
        assert!(
            text.contains("Timer commands for orchestrator:"),
            "header must use the grouped shape; got: {text}"
        );
        for (name, desc) in [
            ("start", "Persist a local phase run (manual fallback)"),
            (
                "finish",
                "Finish a manually-opened run and project its time",
            ),
            ("list", "Inspect local phase runs (read-only, local-only)"),
            ("get", "Show one local phase run (read-only, local-only)"),
            (
                "recover",
                "Mark a known orphan FAILED and project via the configured provider",
            ),
        ] {
            assert!(
                text.contains(&format!("  {name:<14} {desc}")),
                "rows stay one-command-per-line; missing {name}; got: {text}"
            );
        }
        assert!(
            text.contains("Use 'phasegent --help timer <command>' for options."),
            "footer must point to detail pages; got: {text}"
        );
    }

    #[test]
    fn overview_sinks_contract_prose_to_detail_pages_without_loss() {
        let overview = timer_help_text(Some(Role::Orchestrator));
        for sunk in [
            "Forgejo rejects",
            "never echoes secrets",
            "never infers success",
            "without reopening it",
            "same-run marker reconciliation used by finish",
            "same-run provider reconciliation",
            "tester is a first-class role",
            "SQLite ledger minus secrets",
            "local-only and must happen before any projection",
        ] {
            assert!(
                !overview.contains(sunk),
                "overview must not repeat contract prose ({sunk}); got: {overview}"
            );
        }
        let start = timer_command_help_text(Some(Role::Orchestrator), "start");
        assert!(
            start.contains("writes a local ledger row before any remote operation")
                && start.contains("Forgejo rejects timer start"),
            "start detail keeps ledger order + provider boundary; got: {start}"
        );
        assert!(
            start.contains("tester is a first-class role"),
            "start detail keeps the tester child-identity note; got: {start}"
        );
        let finish = timer_command_help_text(Some(Role::Orchestrator), "finish");
        assert!(
            finish.contains("marker-based reconciliation"),
            "finish detail keeps reconciliation prose; got: {finish}"
        );
        let list = timer_command_help_text(Some(Role::Orchestrator), "list");
        assert!(
            list.contains("never echoes secrets"),
            "list detail keeps the ledger redaction note; got: {list}"
        );
        let recover = timer_command_help_text(Some(Role::Orchestrator), "recover");
        assert!(
            recover.contains("Never infers a successful outcome")
                && recover.contains("without reopening it")
                && recover.contains("same-run marker reconciliation used by finish"),
            "recover detail keeps the orphan boundaries; got: {recover}"
        );
    }

    #[test]
    fn overview_lists_every_command_and_filters_by_role() {
        let full = timer_help_text(Some(Role::Orchestrator));
        for command in ["start", "finish", "list", "get", "recover"] {
            assert!(
                full.contains(command),
                "orchestrator overview missing {command}; got: {full}"
            );
        }
        let all_roles = timer_help_text(None);
        for command in ["start", "finish", "list", "get", "recover"] {
            assert!(
                all_roles.contains(command),
                "all-roles overview missing {command}; got: {all_roles}"
            );
        }
        for role in [Role::Executor, Role::Reviewer, Role::Tester, Role::Admin] {
            let denied = timer_help_text(Some(role));
            assert!(
                denied.contains(&format!("No command available for {}", role.as_str())),
                "{role:?} must stay denied without a table; got: {denied}"
            );
        }
    }

    #[test]
    fn unknown_command_falls_back_to_top_level_help() {
        let text = timer_command_help_text(Some(Role::Orchestrator), "fly");
        assert_eq!(text, timer_help_text(Some(Role::Orchestrator)));
    }

    #[test]
    fn read_role_is_denied_detail_pages() {
        let text = timer_command_help_text(Some(Role::Executor), "list");
        assert!(
            text.contains("No command available for executor"),
            "got: {text}"
        );
    }
}
