use crate::policy::Role;

pub(crate) fn print_relation_help(role: Option<Role>) {
    println!(
        "Relation commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    if role.is_none_or(|role| role.allows(crate::policy::Capability::RelationRead)) {
        println!(
            "  list              {}",
            crate::policy::Capability::RelationRead.description()
        );
    }
    if role.is_none_or(|role| role == Role::Orchestrator) {
        println!(
            "  create           Create a Redmine or GitLab issue relation (orchestrator-only)"
        );
        println!(
            "  delete           Delete a Redmine or GitLab issue relation by id (orchestrator-only)"
        );
    }
    println!("\nUse 'phasegent --help relation <command>' for options.");
}

pub(crate) fn print_relation_command_help(role: Option<Role>, command: &str) {
    match command {
        "list" => {
            if role.is_none_or(|role| role.allows(crate::policy::Capability::RelationRead)) {
                println!(
                    "Usage: relation list <ISSUE>\n\n{}",
                    crate::policy::Capability::RelationRead.description()
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        "create" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: relation create <ISSUE> --to <ISSUE> --type blocks|precedes|relates [--delay N]\n\nCreates an issue relation from <ISSUE> to --to of the given type. `blocks`/`blocked` and `precedes`/`follows` are inverse directions; only the forward canonical names (blocks, precedes, relates) are accepted as --type. `--delay N` (a non-negative integer lag) is only valid with --type precedes. Redmine honours every flag. GitLab currently accepts only --type relates for create; --type blocks and --type precedes are rejected with structured not-supported / config errors before any network traffic, and --delay is rejected as a structured config error. GitLab relation list still maps every server-returned direction (blocks, is_blocked_by) so listing an issue reflects whatever the server already recorded. Forgejo always rejects relation operations. Orchestrator-only."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        "delete" => {
            if role.is_none_or(|role| role == Role::Orchestrator) {
                println!(
                    "Usage: relation delete <RELATION_ID> [--issue <SOURCE_ISSUE_IID>]\n\nDeletes a Redmine, GitLab, or Forgejo-rejected issue relation by its numeric id. Orchestrator-only. GitLab additionally requires --issue <SOURCE_ISSUE_IID> because the DELETE endpoint is scoped per source issue; Redmine and Forgejo ignore the flag."
                );
            } else {
                println!(
                    "No command available for {}.",
                    role.map_or("this role", Role::as_str)
                );
            }
        }
        _ => print_relation_help(role),
    }
}
