use crate::policy::Role;

pub(crate) fn print_config_help(role: Option<Role>) {
    println!(
        "Local configuration for {}:\n\n  show               Print a redacted snapshot of the local SQLite database\n  set SETTING [VALUE|--stdin]  Persist a setting (canonical PHASEGENT_* names or kebab-case aliases; secrets require --stdin or prompt)\n  clear SETTING      Remove a persisted setting\n  provider get       Print the persisted machine-wide default provider (null when unset)\n  provider set NAME  Validate and persist the machine-wide default provider (forgejo, redmine, gitlab)\n  provider clear     Remove the persisted machine-wide default provider\n\nUse 'phasegent --help config <subcommand>' for options.\n`config show` and `config provider *` do not require --role because the global default and the global settings are machine-wide; `config set/clear` for global settings also works without --role, while role-scoped settings require --role.\n\nIndex backend (phase 4): `PHASEGENT_INDEX_BACKEND` (`index-backend`, sqlite default, postgres alternative) and secret `PHASEGENT_INDEX_PG_URL` (`index-pg-url`) select the issue index storage. PostgreSQL uses tsvector + GIN, auto-applies migrations from `migrations/pg/0001_issue_index.sql`, and never stores credentials.",
        role.map_or("all roles", Role::as_str)
    );
}

pub(crate) fn print_config_command_help(role: Option<Role>, command: &str) {
    match command {
        "show" => {
            println!(
                "Usage: phasegent [config show]\n\nPrints a compact JSON snapshot of the local SQLite database:\n  database_path              absolute path to the SQLite file\n  roles                      array with one entry per role (admin, orchestrator, executor, reviewer, tester)\n  global_settings            array of PHASEGENT_REDMINE_GIT_MIRROR_API_KEY, PHASEGENT_REDMINE_REPOSITORY_URL, PHASEGENT_DEFAULT_PROVIDER, PHASEGENT_INDEX_BACKEND, and PHASEGENT_INDEX_PG_URL\n  global_default_provider    machine-wide default provider literal (forgejo, redmine, or gitlab); null when unset\n\nCredential rows report presence and length only; the bearer key for the git mirror plugin and the index PG URL are also reported as presence/length, and the repository URL override is sanitised so embedded userinfo, password, query, and fragment are stripped before the snapshot is rendered. The machine-wide default provider and index backend are non-secret literals and are rendered both as a top-level field (provider) or inside `global_settings` (backend) so the snapshot stays self-contained.\n\nWith --role ROLE the snapshot is the same JSON with the roles array restricted to that single role."
            );
        }
        "set" => {
            let role_text = role.map_or("ROLE", Role::as_str);
            println!(
                "Usage: phasegent [--role {role_text}] config set <SETTING> [VALUE|--stdin]\n\nPersists a single setting in the local SQLite database. The value is never echoed in output.\n\nSupported settings (canonical name and kebab-case alias):\n  PHASEGENT_PROVIDER / provider\n  PHASEGENT_API_BASE / api-base\n  PHASEGENT_REPOSITORY / repository\n  PHASEGENT_REDMINE_API_BASE / redmine-api-base\n  PHASEGENT_REDMINE_CLOSE_STATUS_ID / redmine-close-status-id\n  PHASEGENT_GITLAB_API_BASE / gitlab-api-base\n  PHASEGENT_CLOSE_STATUS_ID / close-status-id      (generic Redmine alias)\n  PHASEGENT_REDMINE_GIT_MIRROR_API_KEY / redmine-git-mirror-api-key   (secret)\n  PHASEGENT_REDMINE_REPOSITORY_URL / redmine-repository-url\n  PHASEGENT_DEFAULT_PROVIDER / default-provider      (validated through ProviderKind)\n  PHASEGENT_INDEX_BACKEND / index-backend            (sqlite or postgres)\n  PHASEGENT_INDEX_PG_URL / index-pg-url              (secret)\n\nProject-id settings (redmine-project-id, gitlab-project-id, project-id) were removed in Phase 1;\nuse explicit --project-id per invocation instead. Secrets and project-id persistence are rejected.\n\nSecret settings (redmine-git-mirror-api-key, index-pg-url) never accept a direct value:\n  phasegent config set redmine-git-mirror-api-key            # secure prompt\n  phasegent config set redmine-git-mirror-api-key --stdin    # read from stdin\n  phasegent config set index-pg-url --stdin < /secure/path/pg-url\n\nNon-secret settings use a positional value or --stdin:\n  phasegent --role executor config set api-base https://forgejo.example\n  phasegent --role executor config set api-base --stdin\n  phasegent config set index-backend postgres\n  phasegent config set index-backend sqlite\n\nGlobal settings (mirror key, repository URL, default provider, index backend/url) are machine-wide and work without --role;\nrole-scoped settings require --role. `config set default-provider` reuses the same validation as `config provider set`; `index-backend` is validated to sqlite or postgres."
            );
        }
        "clear" => {
            println!(
                "Usage: phasegent [--role ROLE] config clear <SETTING>\n\nRemoves the persisted setting from SQLite. Prints the canonical setting name and whether a row/field was cleared.\n\nGlobal settings are machine-wide and can be cleared without --role. Role-scoped settings require --role.\nThe bearer key (`redmine-git-mirror-api-key`) and index PG URL are reported only as presence/length in `config show`."
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
