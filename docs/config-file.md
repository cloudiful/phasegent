# Configuration file

All phasegent configuration and all credentials live in one JSONC file. It is
the single runtime authority: nothing is read from SQLite and no TOML file is
consulted at runtime.

Business state is unaffected and stays in the SQLite stores next to it: the
execution ledger, the notification outbox, the task identity tables, the
worktree directory registry, the local provider store, and the issue index.
SQLite was not removed; it simply no longer holds configuration.

## Location

The default path is `phasegent.jsonc` in the OS configuration directory
(`ProjectDirs` for `com.Cloud1ful.phasegent`):

| Platform | Default file |
| --- | --- |
| Linux | `~/.config/phasegent/phasegent.jsonc` (`$XDG_CONFIG_HOME` wins) |
| macOS | `~/Library/Application Support/com.Cloud1ful.phasegent/phasegent.jsonc` |
| Windows | `%APPDATA%\Cloud1ful\phasegent\config\phasegent.jsonc` |

`PHASEGENT_CONFIG_PATH` overrides the location with an absolute path. A
relative value is rejected instead of being resolved against the working
directory. The override is read on every call, so it selects the file for one
process without changing the default for anyone else.

The business SQLite stores keep their own locations and are not affected by
`PHASEGENT_CONFIG_PATH`.

## Precedence

Per field, the effective value is:

1. an explicit CLI flag,
2. the `PHASEGENT_*` environment variable,
3. the configuration file,
4. the built-in default.

Environment variables are runtime-only overrides. They are never written into
the file and never ingested by the migration importer.

The provider resolver follows the same chain, concretely:

1. explicit `--provider`,
2. `PHASEGENT_PROVIDER` (environment),
3. `PHASEGENT_DEFAULT_PROVIDER` (environment),
4. the persisted `PHASEGENT_DEFAULT_PROVIDER` in the file,
5. the role-scoped `provider` in the file,
6. the Redmine fallback.

The Redmine REST address is machine-wide: it is one value in `settings`
(`PHASEGENT_REDMINE_API_BASE`) shared by every role, never a per-role address.
Credentials, provisioned identities, provider selection, and the Redmine
close-status id stay role-scoped.

## Schema

The document is versioned (`version: 1`). Comments and trailing commas are
accepted; every other JSON5 extension is rejected. Unknown top-level fields,
unknown role names, unknown settings, unknown credential providers, duplicate
keys, a wrong `version`, and malformed types are refused with an error that
never echoes a value.

```jsonc
{
  // Bumped only by a schema change; 1 is the current version.
  "version": 1,
  "settings": {
    "PHASEGENT_DEFAULT_PROVIDER": "forgejo",
    "PHASEGENT_REDMINE_API_BASE": "https://redmine.example.invalid",
    "PHASEGENT_NOTIFY_ENABLED": "true"
  },
  "roles": {
    "executor": {
      "provider": "forgejo",
      "redmine": {
        "closeStatusId": 5
      },
      "forgejo": {
        "apiBase": "https://forgejo.example.invalid",
        "repository": "owner/repository",
        "source": "admin-auth-setup",
        "updatedAt": 1700000000
      },
      "credentials": {
        "forgejo": {
          "value": "EXAMPLE-TOKEN-NOT-A-REAL-SECRET",
          "updatedAt": 1700000000
        }
      },
      "redmineUser": {
        "userId": 7,
        "login": "phasegent-executor"
      }
    }
  }
}
```

Every example above is a placeholder; no real credential belongs in this
document or in any commit.

Field reference:

| Path | Meaning |
| --- | --- |
| `version` | Schema version; only `1` is accepted. |
| `settings` | Machine-wide settings keyed by the canonical `PHASEGENT_*` names. |
| `settings.PHASEGENT_REDMINE_API_BASE` | The one Redmine REST address every role shares. |
| `settings.PHASEGENT_DEFAULT_PROVIDER` | The machine-wide default provider. |
| `settings.PHASEGENT_INDEX_PG_URL` | Secret; its presence selects the PostgreSQL issue index, its absence SQLite. |
| `settings.PHASEGENT_INDEX_BACKEND` | Legacy literal, ignored for selection, kept for compatibility. |
| `roles` | One entry per role name: `admin`, `orchestrator`, `executor`, `reviewer`, `explore`. |
| `roles.<role>.provider` | The role-scoped provider preference. |
| `roles.<role>.redmine` | `closeStatusId` is the live role-scoped value; `apiBase` and `projectId` are retained legacy fields. |
| `roles.<role>.forgejo` | `apiBase`, `repository`, `source`, `updatedAt`. |
| `roles.<role>.gitlab` | `apiBase`, `repository`, `source`, `updatedAt`; `projectId` is a retained legacy field. |
| `roles.<role>.credentials` | Keyed by `redmine`, `forgejo`, or `gitlab`; each entry has `value` and an optional `updatedAt`. |
| `roles.<role>.redmineUser` | The provisioned Redmine identity: `userId` and `login`. |

`roles.<role>.legacy` (`apiBase`, `repository`) holds the inert columns of the
old `role_config` table, and the retained `apiBase`/`projectId` fields of the
old `role_redmine_config` and `role_gitlab_config` tables are kept the same
way. They are preserved so a migration never loses a value; the runtime does
not resolve a provider, an address, or a project from them. The migration
carries them over exactly as they were persisted, and a persisted value it
cannot represent exactly is refused instead of being changed.

## Secrets and file permissions

Credentials are stored in plaintext. The file is protected by the filesystem,
not by encryption:

- On Unix the configuration directory is created with mode `0700` and the
  file with mode `0600`. Reading the file fails closed when it is a symlink,
  not a regular file, or accessible by group or others.
- On Windows the file stays in the per-user configuration directory and its
  protection is the inherited user-profile ACL boundary. No explicit file ACL
  is created or checked.

Writes take a sibling advisory lock, re-read the current bytes, apply the
change through a comment-preserving parser, and rename the result over the
file. The replacement renames the prepared file over the live one on every
platform, so the live configuration is never removed before its replacement
exists. A concurrent external edit detected before the rename is refused
rather than overwritten. `admin config set`, `admin config
clear`, `admin config provider set|clear`, and `admin auth setup` all write
this file and nothing else.

The lock is the sidecar file `phasegent.jsonc.lock` next to the configuration
file; it is created with mode `0600` on Unix.

`config show` reports credential presence and length only, never a value.

A read never creates the file: a missing configuration resolves to an empty
document, so `config show` and `doctor` stay read-only on a fresh install.

A configuration-only command such as `config show` never opens or creates the
business SQLite database.

## Bootstrap

`phasegent admin workflow bootstrap` provisions one Redmine service user per
agent role, reads each one's API key through the Redmine admin API, and
stores the key and the identity in this file
(`roles.<role>.credentials.redmine` and `roles.<role>.redmineUser`). It is
idempotent: a rerun reuses every identity it already holds. The keys are not
written to SQLite.

## Role gates

Reading the configuration (`config show`, `config provider get`, `doctor`) is
open to every role. Writing it (`admin config set`, `admin config clear`,
`admin config provider set|clear`, `admin auth setup`,
`admin workflow bootstrap`, `admin config migrate`) is a human-operator
surface: a verified agent role is denied before the command runs, and a
managed session without a verified role fails closed. See
[cli-caller-context.md](cli-caller-context.md).

## Migrating an older installation

An installation that still stores its configuration in SQLite migrates once,
by hand, with `phasegent admin config migrate`. The runbook is in
[config-migration.md](config-migration.md).
