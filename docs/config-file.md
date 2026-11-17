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

Environment variables are runtime-only overrides. They keep their
`PHASEGENT_*` names, they are never written into the file, and they are never
ingested by the migration importer. The file itself never stores an
environment-style name.

The provider resolver follows the same chain, concretely:

1. explicit `--provider`,
2. `PHASEGENT_PROVIDER` (environment),
3. `PHASEGENT_DEFAULT_PROVIDER` (environment),
4. the persisted `provider` in the file,
5. the role-scoped `provider` in the file,
6. the Redmine fallback.

A configuration file this build cannot read is an error, never an absent
value: the resolver never degrades a file it cannot interpret into "not
configured", because that would silently change the effective provider.

The Redmine REST address is machine-wide: it is one value
(`redmine.apiBase`) shared by every role, never a per-role address.
Credentials, provisioned identities, provider selection, and the Redmine
close-status id stay role-scoped.

## Schema

The document is versioned (`version: 2`). It is a hierarchy of typed values:
a boolean stays a boolean, a port stays a number, and a recipient list stays
an array. Comments and trailing commas are accepted; every other JSON5
extension is rejected. Unknown fields, unknown role names, unknown settings,
unknown credential providers, duplicate keys, a wrong `version`, and malformed
types are refused with an error that never echoes a value.

An optional value stays absent. A section that carries nothing is the same as
a missing section, so the file never gains a default it did not hold.

```jsonc
{
  // Bumped only by a schema change; 2 is the current version.
  "version": 2,
  "provider": "forgejo",
  "redmine": {
    "apiBase": "https://redmine.example.invalid",
    "repositoryUrl": "https://git.example.invalid/owner/repository.git",
    "gitMirror": { "apiKey": "EXAMPLE-TOKEN-NOT-A-REAL-SECRET" }
  },
  "index": {
    "postgres": { "url": "postgres://user:password@db.example.invalid/phasegent" }
  },
  "notifications": {
    "enabled": true,
    "channel": "email",
    "email": {
      "smtp": { "host": "smtp.example.invalid", "port": 587, "tls": "starttls" },
      "username": "phasegent",
      "password": "EXAMPLE-PASSWORD-NOT-A-REAL-SECRET",
      "from": "phasegent@example.invalid",
      "to": ["ops@example.invalid", "oncall@example.invalid"],
      "replyTo": "phasegent@example.invalid"
    }
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
| `version` | Schema version; only `2` is accepted. |
| `provider` | The machine-wide default provider: `redmine`, `local`, `forgejo`, or `gitlab`. |
| `redmine.apiBase` | The one Redmine REST address every role shares. |
| `redmine.repositoryUrl` | The Redmine repository URL override. |
| `redmine.gitMirror.apiKey` | Secret; the bearer key of the git-mirror plugin. |
| `index.postgres.url` | Secret; its presence selects the PostgreSQL issue index, its absence SQLite. |
| `notifications.enabled` | Boolean; whether agent notifications are enabled. |
| `notifications.channel` | The selected channel: `ntfy`, `webhook`, `dingtalk`, or `email`. |
| `notifications.ntfy.baseUrl` | The ntfy server base URL. |
| `notifications.ntfy.topic` | The ntfy topic. |
| `notifications.ntfy.token` | Secret; the ntfy token. |
| `notifications.webhook.url` | The webhook URL. |
| `notifications.webhook.token` | Secret; the webhook token. |
| `notifications.dingtalk.webhookUrl` | The DingTalk webhook URL. |
| `notifications.dingtalk.secret` | Secret; the DingTalk signing secret. |
| `notifications.dingtalk.keywords` | Array of strings; the DingTalk keywords. |
| `notifications.dingtalk.messageType` | `text` or `markdown`. |
| `notifications.email.smtp.host` | The SMTP host name. |
| `notifications.email.smtp.port` | Number, 1-65535; the SMTP port. |
| `notifications.email.smtp.tls` | `auto`, `implicit`, `starttls`, or `plain`. |
| `notifications.email.username` | The SMTP user name. |
| `notifications.email.password` | Secret; the SMTP password. |
| `notifications.email.from` | The sender address. |
| `notifications.email.to` | Array of strings; the recipient addresses. |
| `notifications.email.replyTo` | The reply-to address. |
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

The old `PHASEGENT_INDEX_BACKEND` selector was always inert - the presence of
the PostgreSQL URL selects the backend - and it is no longer part of the
supported configuration.

## Writing settings

`phasegent admin config set` and `phasegent admin config clear` name a
machine-wide setting by its dotted path inside the tree above:

```sh
phasegent admin config set redmine.apiBase https://redmine.example.invalid
phasegent admin config set notifications.email.smtp.port 587
phasegent admin config set notifications.email.to ops@example.invalid,oncall@example.invalid
phasegent admin config set notifications.enabled true
```

The historical names stay accepted as aliases, so an existing script keeps
working: `PHASEGENT_REDMINE_API_BASE`, `redmine-api-base`, `notify-enabled`,
and the other kebab-case forms all resolve to the same setting. The file
itself never stores one of them.

Two names carry a scope, and the scope is never guessed:

| Name | Scope |
| --- | --- |
| `provider` (also `PHASEGENT_DEFAULT_PROVIDER`, `default-provider`) | The machine-wide default provider. |
| `PHASEGENT_PROVIDER` | The role-scoped provider preference of one role. |

`--target-role` selects the role-scoped entry and is rejected for a
machine-wide setting, so a script that used it for the provider fails loudly
instead of silently writing the machine-wide default. A role-scoped write
without `--target-role` uses the admin scope for an ordinary operator.

The role-scoped settings are `PHASEGENT_PROVIDER`,
`PHASEGENT_REDMINE_CLOSE_STATUS_ID`, and `PHASEGENT_CLOSE_STATUS_ID`.

Secrets are write-only: `redmine.gitMirror.apiKey`, `index.postgres.url`,
`notifications.ntfy.token`, `notifications.webhook.token`,
`notifications.dingtalk.secret`, and `notifications.email.password` are never
accepted as a direct value on the command line. Use `--stdin` or the secure
interactive prompt. The one exception is `notifications.email.password`,
which also accepts a direct value because SMTP providers hand it out as a
pasteable API token; that value then appears in shell history and process
listings, so pass `--stdin` when that exposure matters.

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
clear`, `admin config provider set|clear`, `admin auth setup`, and the
automatic schema preparation described below write this configuration file
(and, for a conversion, its private backup) and nothing else.

The lock is the sidecar file `phasegent.jsonc.lock` next to the configuration
file; it is created with mode `0600` on Unix.

`config show` reports credential presence and length only, never a value.

A read never creates a missing file: an absent configuration resolves to an
empty document, so `config show` and `doctor` stay read-only on a fresh
install. An existing supported older file is the one exception described under
automatic schema preparation.

## Automatic schema preparation

The current schema is version 2. The first admitted command that actually
needs the configuration file prepares a supported version-1 file
automatically: it converts the document with the same comment-preserving
transformation as the explicit upgrade, creates one unique owner-only plaintext
backup of the exact original bytes next to the file, and only then lets the
ordinary strict parser read the result. Later commands see version 2 and only
read it. The backup is never overwritten or swept.

Preparation is a system-maintenance step of an already authorized operation,
not an `admin` command and not a credential bootstrap: it grants no role and
changes no role, provider, credential, business database, or legacy SQLite
source. A command the caller may not run writes nothing. A managed session
without a verified role never prepares, even when the roleless utility
allowlist lets it read: a utility command such as `config show` or `doctor`
reports the version-1 refusal instead. Help, `--version`, configuration-independent
operations, and the explicit `admin config upgrade` or `admin config migrate`
flows never trigger the automatic step. Local-provider issue operations can read
the configured index backend and therefore prepare when admitted.

After a real conversion the CLI writes one stderr line naming the success and
the private backup path. Stdout keeps the command's ordinary result, including
the single JSON document under `--output json`. A version-2 or missing file
produces no notice and no backup. An unknown or invalid version, a failed
conversion, a permission problem, or a competing writer fails closed with the
original file and its values unchanged.

Preparation takes the configuration lock once. If another phasegent process
holds it, the CLI re-reads the file once and continues only when that writer
already installed a strictly valid version 2; otherwise it reports the
contention and leaves the retry to the operator, with no polling loop. The
automatic step never runs before the command's own authorization checks, so
record and comment `--authorized`, worktree and workflow role gates, and the
human-operator checks on the config writes keep their existing order. See
[configuration migration](config-migration.md).

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
open to every role. The explicit configuration writes (`admin config set`,
`admin config clear`, `admin config provider set|clear`, `admin config
upgrade`, `admin auth setup`, `admin workflow bootstrap`, `admin config
migrate`) are a human-operator surface: a verified agent role is denied before
the command runs, and a managed session without a verified role fails closed.
The automatic schema preparation is not one of these writes: it is a
schema-only system step of an already admitted command, it never grants or
changes a role, and it is skipped for a denied command or a managed session
without a verified role. See [cli-caller-context.md](cli-caller-context.md).

## Migrating an older installation

A file that still uses schema version 1 is converted automatically with a
private backup by the first admitted command that needs the configuration, as
described above. `phasegent admin config upgrade` remains the explicit
report/retry entry for that conversion. An installation that still stores its
configuration in SQLite migrates once, by hand, with `phasegent admin config
migrate`: that import is never automatic. Both runbooks are in
[config-migration.md](config-migration.md).
