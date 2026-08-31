use crate::policy::{Capability, Role};

pub(crate) fn print_version_help(role: Option<Role>) {
    println!(
        "Version commands for {}:\n",
        role.map_or("all roles", Role::as_str)
    );
    if role.is_none_or(|role| role.allows(Capability::VersionRead)) {
        println!(
            "  list             {}",
            Capability::VersionRead.description()
        );
    }
    println!("\nUse 'phasegent --help version <command>' for options.");
}

pub(crate) fn print_version_command_help(role: Option<Role>, command: &str) {
    if command != "list" {
        print_version_help(role);
        return;
    }
    let capability = Capability::VersionRead;
    if role.is_none_or(|role| role.allows(capability)) {
        println!(
            "Usage: version list\n\n{}\n\nRedmine: when --project-id is omitted the current Git origin is matched against existing redmine_git_mirror records. Exactly one match lists that project's versions; multiple matches fail with candidate ids/names and require --project-id; no match returns an actionable error telling the operator to pass --project-id or run 'workflow bootstrap'. The command never auto-bootstraps. An explicit --repository that does not equal the origin is not silently matched.",
            capability.description()
        );
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}
