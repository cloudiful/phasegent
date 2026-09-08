use crate::policy::Role;

pub(crate) fn print_config_help(role: Option<Role>) {
    println!(
        "Local configuration for {}:\n\n  show               Print a redacted snapshot of the local SQLite database\n  set SETTING [VALUE|--stdin]  Persist a setting (canonical PHASEGENT_* names or kebab-case aliases; secrets require --stdin or prompt)\n  clear SETTING      Remove a persisted setting\n  provider get       Print the persisted machine-wide default provider (null when unset)\n  provider set NAME  Validate and persist the machine-wide default provider (forgejo, redmine, gitlab, or local)\n  provider clear     Remove the persisted machine-wide default provider\n\nUse 'phasegent --help config <subcommand>' for options.\n`config show` and `config provider *` do not require --role because the global default and the global settings are machine-wide; `config set/clear` for global settings also works without --role, while role-scoped settings require --role.\n\nEffective precedence per field is CLI flags > PHASEGENT_* environment > TOML phasegent.toml (read-only overlay) > legacy SQLite > built-in defaults. `config set`/`clear` write SQLite only; a TOML value shadows SQLite until the file (or env) is removed. Credentials stay in SQLite/env and never belong in TOML. Override the file with an absolute PHASEGENT_CONFIG_PATH; the default is the ProjectDirs config_dir phasegent.toml alongside phasegent.sqlite3.\n\nIndex backend is URL-driven: a non-empty secret `PHASEGENT_INDEX_PG_URL` (`index-pg-url`, env overrides persisted) selects PostgreSQL, absent or blank selects SQLite. Use `config set index-pg-url --stdin` for Postgres and `config clear index-pg-url` to return to SQLite. `PHASEGENT_INDEX_BACKEND` (`index-backend`) is legacy, ignored for selection, and kept only for compatibility. PostgreSQL uses tsvector + GIN, auto-applies migrations from `migrations/pg/0001_issue_index.sql`, and never stores credentials.\n\nAgent notifications use one global_setting row per channel field (env overrides SQLite; no TOML overlay). Enable with `config set notify-enabled true` and `config set notify-channel <ntfy|webhook|dingtalk|email>`, then per-channel fields (notify-ntfy-base-url, notify-ntfy-topic, notify-webhook-url, ...). Secrets (notify-ntfy-token, notify-webhook-token, notify-dingtalk-secret, notify-email-password) require --stdin and stay write-only.",
        role.map_or("all roles", Role::as_str)
    );
}

pub(crate) fn print_config_command_help(role: Option<Role>, command: &str) {
    match command {
        "show" => {
            println!(
                "Usage: phasegent [config show]\n\nPrints a compact JSON snapshot of the local SQLite database:\n  database_path              absolute path to the SQLite file\n  roles                      array with one entry per role (admin, orchestrator, executor, reviewer, tester)\n  global_settings            array of PHASEGENT_REDMINE_GIT_MIRROR_API_KEY, PHASEGENT_REDMINE_REPOSITORY_URL, PHASEGENT_DEFAULT_PROVIDER, PHASEGENT_INDEX_BACKEND, PHASEGENT_INDEX_PG_URL, plus PHASEGENT_NOTIFY_* channel fields\n  global_default_provider    machine-wide default provider literal (forgejo, redmine, gitlab, or local); null when unset\n\nCredential rows report presence and length only; the bearer key for the git mirror plugin, the index PG URL, and notify secrets (ntfy-token, webhook-token, dingtalk-secret, email-password) are also reported as presence/length, and URL overrides (repository URL, ntfy-base-url, webhook-url, dingtalk-webhook-url) are sanitised so embedded userinfo is stripped before the snapshot is rendered. The machine-wide default provider is a non-secret literal rendered both as a top-level field and inside `global_settings` so the snapshot stays self-contained. The legacy index backend literal is also rendered inside `global_settings` for compatibility but is ignored for selection; only `PHASEGENT_INDEX_PG_URL` presence selects PostgreSQL. Notify non-URL fields report presence/length only; notify precedence is env over SQLite with no TOML overlay.\n\nSnapshot reflects persisted SQLite only; a TOML phasegent.toml value shadows at resolve time but is not shown (notify has no TOML overlay). Effective precedence is CLI flags > PHASEGENT_* environment > TOML (read-only overlay, absolute PHASEGENT_CONFIG_PATH override) > legacy SQLite > built-in defaults.\n\nWith --role ROLE the snapshot is the same JSON with the roles array restricted to that single role."
            );
        }
        "set" => {
            let role_text = role.map_or("ROLE", Role::as_str);
            println!(
                "Usage: phasegent [--role {role_text}] config set <SETTING> [VALUE|--stdin]

Persists a single setting in the local SQLite database. The value is never echoed in output.

Supported settings (canonical name and kebab-case alias):
  PHASEGENT_PROVIDER / provider
  PHASEGENT_API_BASE / api-base
  PHASEGENT_REPOSITORY / repository
  PHASEGENT_REDMINE_API_BASE / redmine-api-base
  PHASEGENT_REDMINE_CLOSE_STATUS_ID / redmine-close-status-id
  PHASEGENT_GITLAB_API_BASE / gitlab-api-base
  PHASEGENT_CLOSE_STATUS_ID / close-status-id      (generic Redmine alias)
  PHASEGENT_REDMINE_GIT_MIRROR_API_KEY / redmine-git-mirror-api-key   (secret)
  PHASEGENT_REDMINE_REPOSITORY_URL / redmine-repository-url
  PHASEGENT_DEFAULT_PROVIDER / default-provider      (validated through ProviderKind)
  PHASEGENT_INDEX_BACKEND / index-backend            (legacy, ignored for selection)
  PHASEGENT_INDEX_PG_URL / index-pg-url              (secret; presence selects PostgreSQL)
  PHASEGENT_WORKTREE_AUTO / worktree-auto           (boolean, default false; gates worktree acquire auto-isolation, see issue #247)

Notify channel fields (one row per field; secrets need --stdin):
  PHASEGENT_NOTIFY_ENABLED / notify-enabled, PHASEGENT_NOTIFY_CHANNEL / notify-channel,
  PHASEGENT_NOTIFY_NTFY_BASE_URL / notify-ntfy-base-url, PHASEGENT_NOTIFY_NTFY_TOPIC / notify-ntfy-topic,
  PHASEGENT_NOTIFY_NTFY_TOKEN / notify-ntfy-token (secret), PHASEGENT_NOTIFY_WEBHOOK_URL / notify-webhook-url,
  PHASEGENT_NOTIFY_WEBHOOK_TOKEN / notify-webhook-token (secret), plus dingtalk/email fields (see notify help)

Project-id settings (redmine-project-id, gitlab-project-id, project-id) were removed in Phase 1;
use explicit --project-id per invocation instead. Secrets and project-id persistence are rejected.

Secret settings (redmine-git-mirror-api-key, index-pg-url, notify-ntfy-token, notify-webhook-token, notify-dingtalk-secret, notify-email-password) never accept a direct value:
  phasegent config set redmine-git-mirror-api-key            # secure prompt
  phasegent config set redmine-git-mirror-api-key --stdin    # read from stdin
  phasegent config set index-pg-url --stdin < /secure/path/pg-url

Non-secret settings use a positional value or --stdin. Index selection is URL-driven (no index-backend value needed):
  phasegent --role executor config set api-base https://forgejo.example
  phasegent --role executor config set api-base --stdin
  phasegent config set index-pg-url --stdin < /secure/path/pg-url  # select PostgreSQL
  phasegent config clear index-pg-url  # return to SQLite (index-backend is legacy, ignored)

Global settings (mirror key, repository URL, default provider, index pg-url) are machine-wide and work without --role;
role-scoped settings require --role. `config set default-provider` reuses the same validation as `config provider set`; `index-backend` is legacy, validated when set but ignored for selection. `config set` writes SQLite only (TOML is a read-only overlay); a TOML value shadows SQLite until the file (or env) is removed. Effective precedence is CLI flags > PHASEGENT_* environment > TOML phasegent.toml (absolute PHASEGENT_CONFIG_PATH override) > legacy SQLite > built-in defaults. Stable non-secret settings may instead be edited directly in phasegent.toml; credentials stay in SQLite/env and never belong in TOML."
            );
        }
        "clear" => {
            println!(
                "Usage: phasegent [--role ROLE] config clear <SETTING>\n\nRemoves the persisted setting from SQLite. Prints the canonical setting name and whether a row/field was cleared.\n\nGlobal settings are machine-wide and can be cleared without --role. Role-scoped settings require --role.\nThe bearer key (`redmine-git-mirror-api-key`), index PG URL, and notify secrets (`notify-ntfy-token`, `notify-webhook-token`, `notify-dingtalk-secret`, `notify-email-password`) are reported only as presence/length in `config show`. Clearing `index-pg-url` returns the index to SQLite; clearing legacy `index-backend` never changes selection. Notify fields clear the same way (`config clear notify-channel` disables routing). `config clear` removes SQLite only; a TOML phasegent.toml value still shadows until the file (or env) is removed (notify has no TOML overlay)."
            );
        }
        _ => print_config_help(role),
    }
}

pub(crate) fn print_config_provider_help() {
    println!(
        "Machine-wide default provider:\n\n  get               Print the persisted PHASEGENT_DEFAULT_PROVIDER (null when unset)\n  set NAME          Validate and persist the default (forgejo, redmine, gitlab, or local)\n  clear             Remove the persisted default so the resolver falls back to the role-scoped provider\n\n`config provider` subcommands do not require --role because the default is global. The resolver precedence is: explicit --provider > PHASEGENT_PROVIDER (env) > PHASEGENT_DEFAULT_PROVIDER (env) > TOML default_provider in phasegent.toml (read-only overlay, absolute PHASEGENT_CONFIG_PATH override) > persisted PHASEGENT_DEFAULT_PROVIDER (SQLite) > role-scoped provider (TOML [roles.<role>] shadows SQLite role_config.provider) > forgejo fallback. `config provider set`/`clear` write SQLite only; a TOML value shadows SQLite until the file (or env) is removed."
    );
}

pub(crate) fn print_config_provider_command_help(command: &str) {
    match command {
        "get" => {
            println!(
                "Usage: phasegent config provider get\n\nPrints a JSON object with the persisted PHASEGENT_DEFAULT_PROVIDER literal (`forgejo`, `redmine`, `gitlab`, or `local`) or `null` when the default has never been set. The output never echoes any secret value. Snapshot is persisted SQLite only; effective resolution still applies TOML before this value."
            );
        }
        "set" => {
            println!(
                "Usage: phasegent config provider set <forgejo|redmine|gitlab|local>\n\nValidates NAME through ProviderKind::from_str and persists the result in the global_setting table. Unknown literals return a structured config error before any write happens. Writes SQLite only; a TOML default_provider shadows until the file (or env) is removed."
            );
        }
        "clear" => {
            println!(
                "Usage: phasegent config provider clear\n\nRemoves the PHASEGENT_DEFAULT_PROVIDER row from SQLite so the resolver falls back to the TOML overlay then the role-scoped provider. Removes SQLite only; a TOML value still shadows until removed. Returns {{\"cleared\": true}} when a row existed or {{\"cleared\": false}} when the default was already absent."
            );
        }
        _ => print_config_provider_help(),
    }
}
