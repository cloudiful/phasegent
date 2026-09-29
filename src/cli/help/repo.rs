use crate::policy::{Capability, Role};
use crate::providers::ProviderKind;

pub(crate) fn print_repo_help(role: Option<Role>) {
    if role.is_some_and(|role| !role.allows(Capability::RepoCreate)) {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
        return;
    }
    println!(
        "Repository commands for {}:\n\n  create         Create a private repository\n\nUse 'phasegent --help repo <command>' for options.",
        role.map_or("all roles", Role::as_str)
    );
}

pub(crate) fn print_repo_command_help(
    role: Option<Role>,
    command: &str,
    provider: Option<ProviderKind>,
) {
    if command != "create" {
        print_repo_help(role);
        return;
    }
    if role.is_none_or(|role| role.allows(Capability::RepoCreate)) {
        // `repo create` has exactly one live route: the GitLab provider.
        // Redmine and Local have no repository endpoint, so their hint
        // names the limitation instead of describing namespace routing.
        let provider_hint = match provider {
            Some(ProviderKind::Gitlab) | None => {
                "GitLab resolves OWNER via the authenticated user's namespace unless an explicit namespace id was supplied."
            }
            Some(ProviderKind::Redmine) => "Redmine does not support repository creation.",
            Some(ProviderKind::Local) => "Local does not support repository creation.",
        };
        println!(
            "Usage: repo create OWNER/REPO --private [--description TEXT] [--auto-init]\n\n--private is required; repository creation is never public by default. {provider_hint}\n\n{}",
            Capability::RepoCreate.description()
        );
    } else {
        println!(
            "No command available for {}.",
            role.map_or("this role", Role::as_str)
        );
    }
}
