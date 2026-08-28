use crate::policy::Role;

pub(crate) fn print_config_help(role: Option<Role>) {
    println!(
        "Local configuration for {}:\n\n  show               Print a redacted snapshot of the local SQLite database\n  import-env         Persist current PHASEGENT_* environment variables\n  provider get       Print the persisted machine-wide default provider (null when unset)\n  provider set NAME  Validate and persist the machine-wide default provider (forgejo, redmine, gitlab)\n  provider clear     Remove the persisted machine-wide default provider\n\nUse 'phasegent --help config <subcommand>' for options.\n`config show` and `config provider *` do not require --role because the global default and the global settings are machine-wide; `config import-env` does because most settings are role-scoped.",
        role.map_or("all roles", Role::as_str)
    );
}

pub(crate) fn print_config_command_help(role: Option<Role>, command: &str) {
    match command {
        "show" => {
            println!(
                "Usage: phasegent [config show]\n\nPrints a compact JSON snapshot of the local SQLite database:\n  database_path              absolute path to the SQLite file\n  roles                      array with one entry per role (admin, orchestrator, executor, reviewer)\n  global_settings            array of PHASEGENT_REDMINE_GIT_MIRROR_API_KEY, PHASEGENT_REDMINE_REPOSITORY_URL, and PHASEGENT_DEFAULT_PROVIDER\n  global_default_provider    machine-wide default provider literal (forgejo, redmine, or gitlab); null when unset\n\nCredential rows report presence and length only; the bearer key for the git mirror plugin is also reported as presence/length, and the repository URL override is sanitised so embedded userinfo, password, query, and fragment are stripped before the snapshot is rendered. The machine-wide default provider is a non-secret literal and is rendered both as a top-level field and inside `global_settings` so the snapshot stays self-contained.\n\nWith --role ROLE the snapshot is the same JSON with the roles array restricted to that single role."
            );
        }
        "import-env" => {
            let role_text = role.map_or("ROLE", Role::as_str);
            println!(
                "Usage: phasegent --role {role_text} config import-env\n\nPersists every PHASEGENT_* environment variable that is currently set in the process environment for the role selected by --role.\n\nRole-scoped variables:\n  PHASEGENT_PROVIDER\n  PHASEGENT_API_BASE\n  PHASEGENT_REPOSITORY\n  PHASEGENT_REDMINE_API_BASE\n  PHASEGENT_REDMINE_PROJECT_ID\n  PHASEGENT_REDMINE_CLOSE_STATUS_ID\n  PHASEGENT_PROJECT_ID                 (generic Redmine alias)\n  PHASEGENT_CLOSE_STATUS_ID            (generic Redmine alias)\n\nGlobal settings:\n  PHASEGENT_REDMINE_GIT_MIRROR_API_KEY\n  PHASEGENT_REDMINE_REPOSITORY_URL\n  PHASEGENT_DEFAULT_PROVIDER           (validated through ProviderKind; rejects unknown literals)\n\nThe command returns counts and a per-name report; secret values are never echoed. Environment variables are not modified by the command. Ordinary provider commands do not implicitly persist environment variables; persistence happens only through this explicit invocation."
            );
        }
        _ => print_config_help(role),
    }
}

pub(crate) fn print_config_provider_help() {
    println!(
        "Machine-wide default provider:\n\n  get               Print the persisted PHASEGENT_DEFAULT_PROVIDER (null when unset)\n  set NAME          Validate and persist the default (forgejo, redmine, or gitlab)\n  clear             Remove the persisted default so the resolver falls back to the role-scoped provider\n\n`config provider` subcommands do not require --role because the default is global. The resolver precedence is: explicit --provider > PHASEGENT_PROVIDER > PHASEGENT_DEFAULT_PROVIDER (env) > persisted PHASEGENT_DEFAULT_PROVIDER (SQLite) > role-scoped role_config.provider > forgejo fallback."
    );
}

pub(crate) fn print_config_provider_command_help(command: &str) {
    match command {
        "get" => {
            println!(
                "Usage: phasegent config provider get\n\nPrints a JSON object with the persisted PHASEGENT_DEFAULT_PROVIDER literal (`forgejo`, `redmine`, or `gitlab`) or `null` when the default has never been set. The output never echoes any secret value."
            );
        }
        "set" => {
            println!(
                "Usage: phasegent config provider set <forgejo|redmine|gitlab>\n\nValidates NAME through ProviderKind::from_str and persists the result in the global_setting table. Unknown literals return a structured config error before any write happens."
            );
        }
        "clear" => {
            println!(
                "Usage: phasegent config provider clear\n\nRemoves the PHASEGENT_DEFAULT_PROVIDER row from SQLite so the resolver falls back to the role-scoped provider. Returns {{\"cleared\": true}} when a row existed or {{\"cleared\": false}} when the default was already absent."
            );
        }
        _ => print_config_provider_help(),
    }
}
