use crate::policy::Role;

pub(crate) fn print_timer_help(role: Option<Role>) {
    if role.is_none_or(|role| role == Role::Orchestrator) {
        println!(
            "Timer commands for orchestrators:\n\n  start <ISSUE> --phase NAME --agent-role executor|reviewer|tester --attempt N [--run-id ID] [--owner-session-id S --owner-call-id C]    Persist a local phase run\n  finish <RUN_ID> --result DONE|PARTIAL|BLOCKED|FAILED    Finish the run and project its rounded time to Redmine or GitLab\n  list [--status running|finished|all] [--limit N]        Inspect local phase runs (read-only, local-only)\n  get <RUN_ID>                                            Show one local phase run (read-only, local-only)\n  recover <RUN_ID>                                         Mark a known orphan FAILED and project via the configured provider\n\nTimer start is local-only and must write the ledger before any projection. Finish and recover are Redmine- or GitLab-only (Forgejo rejects both) and orchestrator-only. List and get never make provider or network requests; they expose the SQLite ledger minus secrets and full responses. Recover is the explicit orphan path: it never infers success, never reopens a finished row, and reuses the same-run provider reconciliation as finish. tester is a first-class role with its own Redmine credential (IssueRead, CommentRead/FindMarker/Create with --authorized, IssueAttachmentUpload); timers remain orchestrator-only and --agent-role tester is persisted as a child run identity with optional bootstrap membership."
        );
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}

pub(crate) fn print_timer_command_help(role: Option<Role>, command: &str) {
    match command {
        "start" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: timer start <ISSUE> --phase NAME --agent-role executor|reviewer|tester --attempt N [--run-id ID] [--owner-session-id S --owner-call-id C]\n\nThe orchestrator writes a local ledger row before any remote operation. --agent-role is executor, reviewer, or tester (tester is a first-class role with its own Redmine credential; timers remain orchestrator-only and tester project membership is optional during bootstrap); --attempt is a positive integer. Optional --owner-session-id / --owner-call-id record the OpenCode subagent identity (bounded, control-character-free, never projected). Redmine-only when --agent-role is set; Forgejo rejects timer start."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        "finish" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: timer finish <RUN_ID> --result DONE|PARTIAL|BLOCKED|FAILED\n\nThe orchestrator records exact elapsed seconds, then projects them to the configured provider (Redmine or GitLab). Retries on the same run id are safe; the marker-based reconciliation short-circuits before any duplicate Time Entry or spent-time POST. Redmine receives the rounded 0.01-hour summary with a stable run-marker comment; GitLab receives the exact elapsed seconds in human-format duration with the marker embedded in the spent-time summary."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        "list" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: timer list [--status running|finished|all] [--limit N]\n\nRead-only listing of the local execution ledger. --status defaults to all; --limit caps the rows returned (default 100, max 1000). Never reaches the provider; never echoes secrets or full responses. Useful for spotting orphans before explicit recovery."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        "get" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: timer get <RUN_ID>\n\nRead a single execution-ledger row. Returns a structured config error when the run id is unknown; never mutates state and never reaches the provider. Use this before recover to confirm the orphan still belongs to the local run id you have."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        "recover" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: timer recover <RUN_ID>\n\nMark a known orphan FAILED and project it through the configured provider with the same-run marker reconciliation used by finish. Never infers a successful outcome from a missing child transcript. If the row is already terminal, the command returns the unchanged run without reopening it. Concurrent recovers on the same run id are safe; the SQLite primary key and finish_time idempotency make them no-ops. Missing owner metadata does not change recover's behaviour."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        _ => print_timer_help(role),
    }
}
