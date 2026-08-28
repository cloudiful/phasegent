use crate::policy::{Capability, Role};

pub(crate) fn print_comment_help(role: Option<Role>) {
    println!(
        "Comment commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    for (name, capability) in [
        ("create", Capability::CommentCreate),
        ("get", Capability::CommentRead),
        ("find-marker", Capability::CommentFindMarker),
    ] {
        if role.is_none_or(|role| role.allows(capability)) {
            println!("  {name:<14} {}", capability.description());
        }
    }
    println!("\nUse 'phasegent --help comment <command>' for options.");
}

pub(crate) fn print_comment_command_help(role: Option<Role>, command: &str) {
    let (capability, text) = match command {
        "create" => (
            Capability::CommentCreate,
            "Usage: comment create <ISSUE> --body TEXT --marker MARKER [--authorized]\n\nValues beginning with `-` must use the inline form: --body=TEXT or --marker=MARKER.",
        ),
        "get" => (
            Capability::CommentRead,
            "Usage: comment get <ISSUE> <COMMENT_ID>",
        ),
        "find-marker" => (
            Capability::CommentFindMarker,
            "Usage: comment find-marker <ISSUE> --marker MARKER\n\nMarker beginning with `-` must use the inline form: --marker=MARKER.",
        ),
        _ => {
            print_comment_help(role);
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
