use crate::policy::{Capability, Role};
use crate::providers::ProviderKind;

const VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) fn print_root_help(role: Option<Role>, provider: Option<ProviderKind>) {
    let role_text = role.map_or("all roles", Role::as_str);
    println!(
        "phasegent {VERSION}\n\nProvider-backed workflow CLI ({role_text}).\n\nUsage:\n  phasegent --role <ROLE> [--provider forgejo|redmine|gitlab] <COMMAND> [OPTIONS]\n\nOptions:\n  --role <ROLE>          admin, orchestrator, executor, reviewer, or tester\n  --provider <NAME>      forgejo, redmine, or gitlab (default: forgejo)\n  --api-base <URL>       Override the provider API base\n  --repository <O/R>     Override the Forgejo owner/repository\n  --project-id <ID>      Override the Redmine or GitLab project id\n  --close-status-id <ID> Override the Redmine closed status\n  -h, --help             Print help\n  -V, --version          Print version\n\nCommands:\n  issue                  Issue operations\n  comment                Comment operations\n  auth                   Authentication setup\n  config                 Local configuration show, set, clear, and provider default\n  hooks                  Managed Git hook installation"
    );
    if provider != Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::RepoCreate))
    {
        println!("  repo                   Repository operations");
    }
    if provider == Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::ProjectRead))
    {
        println!("  project                Redmine project operations");
    }
    if provider == Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::IssueStatusRead))
    {
        println!("  status                 Redmine issue status operations");
    }
    if provider == Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::VersionRead))
    {
        println!("  version                Redmine project version operations");
    }
    if provider == Some(ProviderKind::Redmine)
        && role.is_none_or(|role| role.allows(Capability::RelationRead))
    {
        println!("  relation               Redmine issue relations");
    }
    if provider == Some(ProviderKind::Redmine) && role.is_none_or(|role| role == Role::Orchestrator)
    {
        println!("  timer                  Redmine phase time tracking");
    }
    if provider == Some(ProviderKind::Redmine) && role.is_none_or(|role| role == Role::Admin) {
        println!("  workflow               Redmine workflow bootstrap");
    }
    println!(
        "\nUse 'phasegent --help <command>' for the next level.\n\
         Provider resolution chain and machine-wide default: 'phasegent --help config provider'.\n\
         Role and credential guidance: 'phasegent --help auth'."
    );
}
