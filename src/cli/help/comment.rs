use crate::policy::{Capability, Role};

pub(crate) fn print_comment_help(role: Option<Role>) {
    println!(
        "Comment commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    for (name, capability) in [
        ("create", Capability::CommentCreate),
        ("get", Capability::CommentRead),
        ("list", Capability::CommentRead),
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
            "Usage: comment create <ISSUE> (--body TEXT | --body-file PATH [--keep-body-file]) --marker MARKER [--authorized]\n\n--body-file reads the body from a one-shot Markdown file (regular file, at most 2 MiB, valid UTF-8) instead of passing long text through the shell. It is mutually exclusive with --body. The file is read and validated locally before any provider or network access. After a successful write the file is deleted unless --keep-body-file is given; any read, validation, or provider failure keeps the file, and a path that was replaced or modified after the read is never deleted (a bounded warning is emitted instead). The file content must contain the --marker text.\n\nValues beginning with `-` must use the inline form: --body=TEXT or --marker=MARKER.",
        ),
        "get" => (
            Capability::CommentRead,
            "Usage: comment get <ISSUE> <COMMENT_ID>",
        ),
        "list" => (
            Capability::CommentRead,
            "Usage: comment list <ISSUE>\n\nFull bodies of every comment on the issue, in provider order, as {issue, comments}. The approved bulk-read path: prefer it over raw provider calls when all notes are needed at once.",
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
