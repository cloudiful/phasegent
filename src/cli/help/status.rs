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
        println!("  set              Update a Redmine issue status by validated name or id");
        println!(
            "  advance          Update a Redmine issue status with a policy preflight and structured guidance"
        );
    }
    println!("\nUse 'phasegent --help status <command>' for options.");
}

pub(crate) fn print_status_command_help(role: Option<Role>, command: &str) {
    if !matches!(command, "list" | "next" | "set" | "advance") {
        print_status_help(role);
        return;
    }
    let capability = Capability::IssueStatusRead;
    if command == "set" || command == "advance" {
        if role.is_none_or(|role| role == Role::Orchestrator) {
            if command == "set" {
                println!(
                    "Usage: status set <NUMBER> --status NAME_OR_ID\n\nUpdates a Redmine issue status by validated numeric id or exact name and prints the updated issue. Orchestrator-only; Redmine-only.\n\nValues beginning with `-` must use the inline form: --status=VALUE."
                );
            } else {
                println!(
                    "Usage: status advance <NUMBER> --status NAME_OR_ID\n\nUpdates a Redmine issue status after a centralized policy preflight. The same status is an idempotent no-op, a policy-illegal transition fails before any write with current/target/allowed_next/recovery guidance, and unknown or custom statuses are forwarded to the server as advisory. Orchestrator-only; Redmine-only."
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
                "Usage: status next <NUMBER>\n\nPrints the issue's current status, the policy-allowed next statuses resolved to this installation's status ids, the policy source, and the recovery command. Read-only; Redmine-only. Server workflow permissions remain authoritative."
            );
        } else {
            println!("Usage: status list\n\n{}", capability.description());
        }
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}
