# Migrating a legacy configuration (operator runbook)

Two configuration migrations exist:

| Installation state | Command |
| --- | --- |
| A JSONC configuration file that still uses schema version 1 | Prepared by the first admitted config-dependent command, or explicitly with `phasegent admin config upgrade` |
| Configuration and credentials in the legacy SQLite database (and optionally a `phasegent.toml` overlay) | `phasegent admin config migrate --from-sqlite <ABSOLUTE PATH>` |

The version-1 conversion runs automatically, with its private backup, as a
system-maintenance step of the first already-authorized command that needs the
configuration file. The SQLite/TOML import is always an explicit human
operator action: the CLI never imports one at startup, never imports one in
the background, and never reads a legacy SQLite or TOML source at runtime.

## Upgrading a version-1 configuration file

An admitted command that actually needs the configuration file converts a
version-1 file to the current schema before its first configuration read. The
explicit command is still available for a report, or to retry after a failure:

```sh
phasegent admin config upgrade
```

Both paths use the same conversion and safety rules. The command resolves the
configuration file: the absolute `PHASEGENT_CONFIG_PATH` when it is set,
otherwise `phasegent.jsonc` in the OS configuration directory. The current
schema is described in [config-file.md](config-file.md).

An automatic conversion writes one line to stderr naming the success and the
private backup path; stdout keeps the command's normal result. A missing or
version-2 file produces no write, backup, or notice.

Both paths create the same unique backup: the exact original bytes, exclusive so
an existing file is never overwritten, owner-only (`0600`) on Unix, and never
removed or swept afterwards. The backup holds credentials in plaintext, so
protect it like the configuration file and delete it yourself when it is no
longer needed. A symlink, a non-regular file, a file accessible by group or
others, an unknown version, an invalid document, or a failed verification is
refused before the original is replaced.

Before you run the explicit command:

- Stop every phasegent client. The command holds the configuration lock for
  the whole run, but a stopped client is what makes that guarantee meaningful.
- Note that the backup contains the credentials in plaintext. Protect it like
  the configuration file.
- Take your own copy if you want an independent one. The command makes its own
  backup (see below).

What it does, in order:

1. Claims the configuration sidecar lock, so the file cannot be changed by
   another phasegent process while the upgrade runs.
2. Reads the file as a version-1 document under the strict rules: an unknown
   field, a duplicate key, an unknown setting, an empty value, and a value
   the version-1 validators rejected are all refusals. The file is never
   adapted and never written when it is refused.
3. Maps every machine-wide setting onto its typed destination in the
   version-2 tree. A boolean stays a boolean, a port stays a number, and a
   list stays an array; a text value keeps its bytes. The deprecated
   `PHASEGENT_INDEX_BACKEND` selector was always inert and is dropped on
   purpose - the presence of the PostgreSQL URL selects the backend.
4. Renders the version-2 document from the original text, so every comment
   the operator wrote survives and moves with the value it belonged to. The
   role subtree is copied verbatim, so a role entry keeps its exact text, its
   credentials, and its timestamps.
5. Parses the rendered document again and compares it with the values the
   migration produced. A difference is a refusal, not a write.
6. Creates a private backup of the exact version-1 bytes next to the
   configuration file - exclusively, so an existing file with the same name is
   never overwritten, and owner-only (`0600`) on Unix. The name carries
   nanosecond resolution, the process id, and a counter
   (`phasegent.jsonc.v1-backup-<nanoseconds>-<pid>-<counter>.jsonc`). The
   backup is never removed again, not even when a later step fails.
7. Replaces the file atomically with owner-only permissions, and only when it
   still holds exactly the bytes that were read. A concurrent edit is refused
   instead of overwritten.

The report carries paths and counts only:

```json
{
  "config_path": "/home/operator/.config/phasegent/phasegent.jsonc",
  "upgraded": true,
  "backup_path": "/home/operator/.config/phasegent/phasegent.jsonc.v1-backup-1700000000000000000-4711-0.jsonc",
  "settings_migrated": 12,
  "roles_preserved": 5,
  "credentials_preserved": 7,
  "removed_settings": ["PHASEGENT_INDEX_BACKEND"]
}
```

No configuration value and no credential is ever printed.

| Situation | Behaviour |
| --- | --- |
| The file already uses schema version 2 | The explicit no-op: nothing is written, no backup is created, and the report has `"upgraded": false`. |
| The file uses another version | The upgrade is refused; the file keeps its bytes. |
| The file holds an unknown field, an unknown setting, a duplicate key, or an empty value | The upgrade is refused; the file keeps its bytes. |
| The file holds a role this build does not know | The upgrade is refused; no role is resurrected and no role is dropped. |
| The configuration file changed while the upgrade ran | The replacement is refused; the file keeps its bytes. |
| Another phasegent process holds the configuration lock | The explicit command refuses with "another phasegent configuration write is in progress". Automatic preparation re-reads once and continues only when the other writer already installed a strictly valid version 2; otherwise the operator retries. |

`admin config upgrade` is a human-operator command. The command registry
denies every AI role before the arguments are parsed, and a managed session
without a verified role fails closed.

The automatic path is narrower. It runs only for an already admitted
config-dependent command: a denied command, help or `--version`, a
configuration-independent path, the explicit `admin config upgrade`/`admin config
migrate` flow, and a managed session without a verified role never convert the
file. Local-provider issue operations can read the configured index backend and
therefore prepare when admitted. A roleless utility read reports the version-1
refusal instead of writing.

## Migrating a legacy SQLite configuration

Older phasegent installations kept their configuration and credentials in the
SQLite database `phasegent.sqlite3` and optionally in a `phasegent.toml`
overlay. Both are read by exactly one command, which a human operator runs
once:

```sh
phasegent admin config migrate --from-sqlite /absolute/path/to/phasegent.sqlite3
```

With a legacy TOML overlay:

```sh
phasegent admin config migrate \
  --from-sqlite /absolute/path/to/phasegent.sqlite3 \
  --from-toml /absolute/path/to/phasegent.toml
```

Both paths must be absolute; a relative path is rejected. The destination is
the JSONC configuration file described in
[config-file.md](config-file.md): the absolute `PHASEGENT_CONFIG_PATH` when it
is set, otherwise `phasegent.jsonc` in the OS configuration directory. The
destination is written in the current schema (version 2), so a fresh import
needs no upgrade afterwards.

There is no automatic import. The CLI never imports a configuration itself,
never imports one at startup, and never reads a legacy SQLite or TOML source at
runtime. This is separate from the automatic version-1 to version-2 conversion
above, which only rewrites the JSONC file.

## Who may run it

`admin config migrate` is a human-operator command. The command registry
denies every AI role before the arguments are parsed, and a managed session
without a verified role fails closed. It belongs to the same `admin` group as
`auth setup`, the config writes, and `workflow bootstrap`. See
[cli-caller-context.md](cli-caller-context.md).

## Before you start

- Stop every old phasegent client. The command re-reads the source rows and
  refuses to clear a source that changed while it ran, but a stopped client
  is what makes that guarantee meaningful.
- Copy the database file yourself if you want an independent copy. The
  command makes its own consistent backup (see below).
- Note that the backup contains the credentials in plaintext. Protect it like
  the configuration file.

## What it does, in order

The configuration file lock and a write transaction on the source database are
held for the whole sequence below, so no normal configuration writer and no
other database writer can change what is migrated between the export and the
purge.

1. Claims the configuration sidecar lock, so the destination file cannot be
   changed by another phasegent process while the migration runs.
2. Opens the source database and holds a write transaction on it, which keeps
   every other writer out for the whole run. A missing file is an error; the
   command never creates or migrates a source, and never opens any database
   other than the one it was given and the backup it writes.
3. Reads the seven legacy configuration tables: `role_config`,
   `role_redmine_config`, `role_forgejo_config`, `role_gitlab_config`,
   `role_credential`, `role_redmine_user`, and `global_setting`. A table that
   does not exist contributes no rows. A known older schema variant is read as
   it is: the historical three-column `role_credential`, a `role_credential`
   without the fingerprint or the timestamp, and a `role_gitlab_config`
   without the provenance columns are all accepted, and their absent nullable
   columns are read as empty. The source is never migrated or altered to make
   it fit.
4. Confirms that the source really is a phasegent configuration store before
   anything is deleted: every configuration table that is present must match
   one of its known schema variants exactly, every known phasegent business
   table that is present must match a known variant, the source must carry at
   least one whole known phasegent table bundle, and the configuration tables
   must not carry a trigger or a foreign key that could reach beyond the
   cleared rows. A source that fails any of these checks is refused and
   nothing is written or deleted.
5. Decodes the optional TOML overlay and folds it over the SQLite values with
   the old precedence: the TOML value wins for a field it defines. The
   ambient environment is never ingested. An unknown or secret key in the
   TOML file is refused.
6. Maps the result onto the typed configuration model. A value the model
   cannot represent is an error, never a silent drop. Every persisted
   value is carried over byte for byte: an optional string keeps its
   whitespace, an empty string stays an empty string instead of becoming
   an absent field, and a secret keeps its exact bytes. The mapped model
   is then compared with the persisted rows themselves, so a value that
   did not survive the mapping is caught before anything is written. An
   unknown setting name, a known setting whose stored value is NULL or
   empty, a role provider that is not the canonical literal, and a role
   configuration row whose every column is NULL are all refused here,
   before the destination file is written and before the backup or the
   purge exists.
7. Writes the destination file atomically, as a **create-only** operation.
   An existing destination is never overwritten.
8. Re-reads the written file and compares every value, including the
   credential timestamps.
9. Creates a private backup file next to the configuration file — exclusively,
   so an existing database, a symlink, or a leftover file with the same name
   is never overwritten — and fills it with a consistent snapshot of the source
   taken with the SQLite online backup API, so a WAL database is never copied
   file by file. The name carries nanosecond resolution, the process id, and a
   counter (`phasegent-config-backup-<nanoseconds>-<pid>-<counter>.sqlite3`)
   and the file is owner-only (`0600`) on Unix from the moment it is created.
10. Reads the configuration file again, right before the destructive step.
    A file that was changed or removed in the meantime aborts the migration,
    rolls the source transaction back, and clears nothing.
11. Re-reads the source rows and clears the seven configuration tables in the
    held transaction, but only when they still match what was exported. The
    configuration file is read once more **inside that same transaction**,
    after the deletes and immediately before the commit. A file that was
    changed or removed in the meantime rolls every delete back, so the
    source keeps its configuration rows.

Business state in the source database — the execution ledger, the notification
outbox, the task identity tables — is never read into the configuration file
and never purged. The source database file itself is never deleted, renamed,
or truncated. Only the rows of the seven configuration tables are removed,
and the removal is a logical delete: the values remain recoverable from the
backup.

## The report

The command prints a JSON report with paths and counts only:

```json
{
  "source": "/home/operator/.config/phasegent/phasegent.sqlite3",
  "destination": "/home/operator/.config/phasegent/phasegent.jsonc",
  "backup": "/home/operator/.config/phasegent/phasegent-config-backup-1700000000000000000-4711-0.sqlite3",
  "exported": [["role_config", 3], ["role_credential", 4]],
  "purged": [["role_config", 3], ["role_credential", 4]],
  "migrated_rows": 7,
  "resumed": false
}
```

`exported` counts the rows read from every legacy table, `purged` counts the
rows cleared from each of them, and `migrated_rows` is the total number of
migrated configuration values, including the values that came from an
explicit TOML overlay. No configuration value and no credential is ever
printed.

## Retry, resume, and refusal

| Situation | Behaviour |
| --- | --- |
| The destination file does not exist | It is created and the purge runs. |
| The destination exists and holds exactly the migrated values | The write is skipped and the purge resumes; the report has `"resumed": true`. |
| The destination exists and holds different values | The command refuses and overwrites nothing. Move the file aside or migrate into a clean profile. |
| The source holds no configuration row and no TOML overlay was given | The command errors with "there is nothing to migrate" instead of writing an empty file. |
| The source holds no configuration row but an explicit TOML overlay was given | The overlay is migrated into the destination file and the source is left as it is; the purge clears nothing. |
| The source is not a phasegent database | The source shape is refused; nothing is written and no row is deleted. |
| The configuration tables carry a trigger or a foreign key | The source is refused; nothing is written and no row is deleted. |
| Another phasegent process holds the configuration lock | The command waits for it by refusing with "another phasegent configuration write is in progress". |
| The source rows changed while the command ran | The purge is refused, so values that were never migrated are never removed. |
| A legacy table does not match a known schema variant | The source shape is refused; migrate from a known phasegent configuration schema. |
| The source carries no known phasegent table bundle | The source is refused; a database that only owns phasegent-like table names is not a phasegent database. |
| The source holds a global setting the model does not know | The migration is refused, whatever the stored value is; the persisted setting is never dropped silently. |
| The source holds a known global setting whose stored value is NULL or empty | The migration is refused before the destination file is written and before anything is purged; the persisted value is never dropped silently. |
| The source holds a role configuration row whose every column is NULL | The migration is refused before the destination file is written; the model cannot tell such a row apart from a missing one, so it is never migrated as an absent role. |
| The source holds a role provider that is not the canonical literal, for example ` forgejo ` | The migration is refused before the destination file is written; the model stores the canonical literal and cannot carry the persisted bytes. |
| The source holds a value the model rejects, for example an empty GitLab address | The migration is refused before the destination file is written; the field is never dropped silently. |
| A configuration table is stored under a different case, for example `ROLE_CREDENTIAL` | The table is recognised, read, counted, and purged with the same case-insensitive rule SQLite itself uses; a credential table can never be skipped because of the case of its name. |
| The configuration file changed while the command ran | The purge is refused and the source is left untouched; the migration can be re-run. |

A crash between the file write and the purge is therefore recoverable: run the
same command again and the purge resumes. A crash before the file write
leaves the source untouched and the command can simply be run again.

Re-running the command after a **successful** migration is not a resume: the
source no longer holds configuration rows, so the command reports that there
is nothing to migrate. That is the expected end state, not a failure.

## The concurrency boundary

The migration holds two things for the whole run: the advisory lock on the
configuration file, and a write transaction on the source database.

- The advisory lock is honoured by phasegent's own writers. A direct editor
  — a text editor, a `sed` command, a synchronisation client — does not take
  it and can change the configuration file while the command runs.
- The write transaction keeps every other database writer out, so the
  exported rows, the backup, and the purge describe one snapshot.
- The command reads the configuration file again immediately before it
  clears the source, and once more inside the held transaction, after the
  deletes and immediately before the commit. If the file was changed or
  removed in the meantime, the migration stops, rolls the source
  transaction back, and clears nothing.

What this does **not** give you is a single transaction across the
filesystem and the database: the destination file and the source database
are separate stores, and the source is committed after the final check.
An edit that lands in the narrow interval between that last read and the
commit statement itself cannot be detected. Stop every phasegent client
and do not edit the configuration file while the command runs; that is what
makes the guarantee meaningful. A crash between the file write and the
purge is recoverable by running the same command again.

## What the source must look like

Before anything is deleted, the source must be corroborated as a phasegent
database:

- Every configuration table that is present must match one of its known
  schema variants exactly: the column names, their declared types, and
  their place in the primary key. The variants cover the additive columns
  the tracked history introduced, so an older store is accepted as it is
  and its absent nullable columns are read as empty. The source is never
  migrated or altered to fit.
- Every known phasegent business table that is present must match a known
  variant, and the source must carry at least one whole known phasegent
  table bundle. A database that merely owns a table with a phasegent-like
  name is refused.
- The configuration tables must not carry a trigger, a foreign key, or be
  referenced by a foreign key of another table. Table names are compared
  case-insensitively, because SQLite resolves them that way, and that one
  rule covers every table operation: existence, read, count, purge, trigger
  filter, and foreign-key filter. A configuration table stored as
  `ROLE_CREDENTIAL` is therefore recognised, read, counted, and purged like
  any other one, and a trigger on it is still refused.

A global setting whose stored value is NULL or empty is **refused**: the
configuration model has no representation for an empty value, so the row
would be dropped while the purge deleted it. The migration stops before the
destination file is written and before anything is purged, and the operator
can set or clear the value by hand and run the command again. A global
setting whose *name* the model does not know is refused the same way,
whatever its value is.

Every persisted value is carried over **byte for byte**. An optional role,
Redmine or GitLab string keeps its whitespace, an empty string stays an
empty string instead of turning into an absent field, a secret keeps its
exact bytes, and the credential timestamp is preserved. The migration does
not normalise a persisted value: a value that the model cannot represent
exactly is refused, not trimmed, not dropped, and not archived elsewhere.
Two kinds of persisted value have no exact representation and are refused
before anything is written:

- a role configuration row whose every column is NULL, because the model
  cannot tell such a row apart from a missing one;
- a role provider that is not the canonical literal (`redmine`, `local`,
  `forgejo`, `gitlab`), because the model stores the canonical literal.

The optional legacy `role_config` columns (`api_base`, `repository`) are
inert, but they are still migrated: they are persisted values, and dropping
or rewriting them would lose data the operator may still need.

## After the migration

- `phasegent config show` reports the configuration file path and the
  migrated values (credentials as presence and length only).
- `phasegent doctor` reports the configuration file path, the credential
  presence per role, and the index backend.
- Keep the backup until the installation is confirmed working; delete it
  yourself when it is no longer needed.
- Nothing else has to change: the runtime reads the configuration file only.

## Limitations

- The end-to-end automatic conversion (detection, lock, unique backup,
  replacement, stderr notice) has no permanent automated test, because the
  permanent test suite is unit-only and never touches a filesystem or a
  database. Run the command yourself on a copy of the file before relying on a
  real installation, and keep the backup until the result is confirmed.
- When the configuration lock is held elsewhere, the automatic path performs
  its single re-read and gives up; there is no retry loop or wait. The operator
  must retry the command.
- The end-to-end behaviour of the SQLite import (file creation, backup, purge,
  resume) has no permanent automated test, because the permanent test suite
  is unit-only and never touches a filesystem or a database. Run the command
  yourself on a copy of the database before migrating a real installation.
- On Windows the configuration directory's protection is the inherited
  user-profile ACL rather than an explicit ACL check. The replacement itself
  renames the prepared file over the live one, so the live configuration is
  never missing between the write and the rename; the Windows path itself
  was not executed during verification, because no Windows host was
  available.
- The purge is a logical delete of seven tables' rows, not a forensic
  erasure; the backup file holds the migrated credentials.
- The command never touches a PostgreSQL instance, the issue index, or any
  other store.
