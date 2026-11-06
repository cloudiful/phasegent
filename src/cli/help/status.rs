use crate::policy::{Capability, Role};

pub(crate) fn print_status_help(role: Option<Role>) {
    println!(
        "Status commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    if role.is_none_or(|role| role.allows(Capability::IssueStatusRead)) {
        println!(
            "  list             {}",
            Capability::IssueStatusRead.description()
        );
        println!(
            "  next             Show an issue's current status and the policy-allowed next statuses"
        );
    }
    if role.is_none_or(|role| role == Role::Orchestrator) {
        println!(
            "  set              Update an issue status by validated name or id (Redmine, local, Forgejo)"
        );
        println!(
            "  advance          Update an issue status with a policy preflight and structured guidance (Redmine, local, Forgejo)"
        );
        println!(
            "  transition       Preferred status write; --to/--status names the target, or omit it to auto-route to the policy's first allowed next status (Redmine, local, Forgejo)"
        );
    }
    println!(
        "\nForgejo has only the native Open and Closed states: `list` returns them without a request, `next` reports the current state and the single opposite state it may move to, and `set`, `advance` and `transition` accept `Open` or `Closed` (trimmed, case insensitive) or the provider-local selectors `1` and `0`. Any other target is refused before any write. A Forgejo issue stays Open until it is closed; its phase detail lives in the record, never in a status or a label."
    );
    println!(
        "\nOrdinary human Operator (no role, no session) shares orchestrator status authority; managed sessions without a role fail closed. See docs/cli-caller-context.md.\nUse 'phasegent --help status <command>' for options."
    );
}

pub(crate) fn print_status_command_help(role: Option<Role>, command: &str) {
    if !matches!(command, "list" | "next" | "set" | "advance" | "transition") {
        print_status_help(role);
        return;
    }
    let capability = Capability::IssueStatusRead;
    if command == "transition" {
        if role.is_none_or(|role| role == Role::Orchestrator) {
            println!(
                "Usage: status transition <NUMBER> [--to NAME_OR_ID]\n\nPreferred status write. --to names the target and --status is an accepted alias (supply only one). Omitting both auto-routes to the policy's first allowed next status; a terminal or advisory-custom status fails before any write with the policy guidance. Orchestrator-only; Redmine, local, and Forgejo. For Forgejo the target is the native state, `Open` or `Closed` (trimmed, case insensitive) or the provider-local selector `1` or `0`, and any other target is refused before any write. --note is reserved; post the note with `comment create` instead. Values beginning with `-` must use the inline form: --to=NAME."
            );
        } else {
            println!(
                "No command available for {}.",
                role.map_or("this role", Role::as_str)
            );
        }
        return;
    }
    if command == "set" || command == "advance" {
        if role.is_none_or(|role| role == Role::Orchestrator) {
            if command == "set" {
                println!(
                    "Usage: status set <NUMBER> --status NAME_OR_ID\n\nUpdates an issue status by validated numeric id or exact name and prints the updated issue. Orchestrator-only; Redmine, local, and Forgejo. For Forgejo the target is the native state, `Open` or `Closed` (trimmed, case insensitive) or the provider-local selector `1` or `0`, and any other target is refused before any write.\n\nValues beginning with `-` must use the inline form: --status=VALUE."
                );
            } else {
                println!(
                    "Usage: status advance <NUMBER> --status NAME_OR_ID\n\nUpdates an issue status after a centralized policy preflight. The same status is an idempotent no-op, a policy-illegal transition fails before any write with current/target/allowed_next/recovery guidance, and unknown or custom statuses are forwarded to the server as advisory. Orchestrator-only; Redmine, local, and Forgejo. For Forgejo the policy is the native state: the target is `Open` or `Closed` (trimmed, case insensitive) or the provider-local selector `1` or `0`, the single opposite state is the only allowed next, and any other target is refused before any write."
                );
            }
        } else {
            println!(
                "No command available for {}.",
                role.map_or("this role", Role::as_str)
            );
        }
        return;
    }
    if role.is_none_or(|role| role.allows(capability)) {
        if command == "next" {
            println!(
                "Usage: status next <NUMBER>\n\nPrints the issue's current status, the policy-allowed next statuses resolved to this installation's status ids, the policy source, and the recovery command. Read-only; Redmine, local, and Forgejo. For Forgejo the policy is the native state: the current `Open` or `Closed` state and the single opposite state it may move to. Server workflow permissions remain authoritative."
            );
        } else {
            println!(
                "Usage: status list\n\n{}\n\nForgejo lists the native Open and Closed states without a request.",
                capability.description()
            );
        }
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}
