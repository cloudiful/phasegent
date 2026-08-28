use crate::cli;
use crate::command::RepoCommand;
use crate::policy::{Capability, Role};
use crate::providers::{ProviderKind, RepoProvider};

pub fn execute(
    role_value: Option<Role>,
    api_base: Option<&str>,
    repository: Option<&str>,
    command: RepoCommand,
) -> i32 {
    let role = cli::required_role(role_value);
    let capability = Capability::RepoCreate;
    if !role.allows(capability) {
        return cli::permission_error(role, capability);
    }
    let provider = match cli::provider(role, api_base, repository) {
        Ok(provider) => provider,
        Err(error) => return cli::provider_error(error),
    };
    match command {
        RepoCommand::Create {
            target,
            private,
            description,
            auto_init,
        } => cli::print_result(provider.create_repo(&target, private, &description, auto_init)),
    }
}

pub fn print_help(role: Option<Role>) {
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

pub fn print_command_help(role: Option<Role>, command: &str, provider: Option<ProviderKind>) {
    if command != "create" {
        print_help(role);
        return;
    }
    if role.is_none_or(|role| role.allows(Capability::RepoCreate)) {
        // Help text is intentionally provider-agnostic so the same
        // description works for the Forgejo and GitLab routes; the
        // concrete namespace behaviour is documented by each provider
        // (Forgejo: personal vs organisation endpoints; GitLab:
        // personal-namespace resolution).
        let provider_hint = match provider {
            Some(ProviderKind::Gitlab) => {
                "GitLab resolves OWNER via the authenticated user's namespace unless an explicit namespace id was supplied."
            }
            Some(ProviderKind::Redmine) => "Redmine does not support repository creation.",
            _ => {
                "Forgejo routes to the personal endpoint when OWNER matches the configured owner and to the organisation endpoint otherwise."
            }
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
