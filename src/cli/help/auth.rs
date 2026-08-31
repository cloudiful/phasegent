use crate::policy::Role;

pub(crate) fn print_auth_help(role: Option<Role>) {
    println!(
        "Authentication for {}:\n\n  setup              Store a provider credential securely\n\nOptions for setup:\n  --provider NAME     forgejo, redmine, or gitlab\n  --stdin             Read the credential from stdin\n  --api-base <URL>    Store the provider API base\n  --repository <O/R>  Store the Forgejo repository\n  --close-status-id <ID> Store the Redmine closed status\n\nCredentials and persisted provider config are role-scoped.\n--role is a capability policy, not identity isolation; credentials must still be least-privilege.\nCredentials are never accepted as command-line arguments.\nProject IDs are invocation-only via --project-id and are never persisted.",
        role.map_or("all roles", Role::as_str)
    );
}
