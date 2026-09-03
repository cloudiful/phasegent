# phasegent

`phasegent` is a role-aware Rust CLI for the Forgejo-first OpenCode workflow.
Forgejo remains the default provider. Redmine can provide issue and journal
operations when selected explicitly. It is one binary with an explicit role:

```text
phasegent --role admin ...
phasegent --role orchestrator ...
phasegent --role executor ...
phasegent --role reviewer ...
```

Forgejo supports issue lifecycle operations, comment lookup, and repository creation.
Redmine supports issue lifecycle operations including
tracker selection and status updates by validated name or id, journal-backed
comments with `#note-<id>` anchors, project discovery/creation,
issue-status discovery, and orchestrator-only raw attachment uploads
(`issue upload-attachment` via `POST /uploads.json` + `PUT /issues/<id>.json`).
The orchestrator-only `timer` foundation records one
wall-clock run for each executor/reviewer/tester phase and projects the rounded summary
to a Redmine Time Entry. `tester` is a timer-only child identity for the
optional Bun/Playwright checker. All successful operations emit compact JSON; failures emit structured
JSON on stderr and return a non-zero status.

```text
phasegent --role orchestrator issue get 3
phasegent --role orchestrator comment find-marker 3 --marker '<!-- marker -->'
phasegent --role executor comment create 3 --body '<!-- marker --> DONE' --marker '<!-- marker -->' --authorized
```

## Provider Selection

Use `--provider redmine` for Redmine issue and comment commands. Without it,
Forgejo remains the default. Repository commands are available for Forgejo and GitLab (Redmine does not support repository creation). phasegent does not expose CI/Actions commands — use `fj` for generic Forgejo CI.

Provider resolution precedence, highest first:

1. Explicit `--provider` argument supplied on the command line.
2. `PHASEGENT_PROVIDER` environment variable (one-process override).
3. `PHASEGENT_DEFAULT_PROVIDER` environment variable (one-process override
   for the persistent machine-wide default).
4. Persisted `PHASEGENT_DEFAULT_PROVIDER` row in the platform-standard
   phasegent database (see [Local Configuration](#local-configuration);
   managed via `config provider set / get / clear`).
5. Role-scoped `role_config.provider` row written by `auth setup`.
6. Forgejo fallback.

The resolver is read-only; `--provider` is the per-command override and the
persisted default is machine-wide. Use the precedence chain to switch between
external Redmine and GitLab environments without touching every
role-scoped config row.

Redmine requires REST API access under **Administration > Settings > API**.
Enable REST; JSONP and webhooks are not needed. Store the API key through
`auth setup`, which reads the key from stdin and persists it under the
operator's HOME in the SQLite database documented in
[Local Configuration](#local-configuration):

```text
phasegent --role admin auth setup --stdin --provider redmine \
  --api-base https://redmine.example.com
```

`auth setup` reads the key from stdin; the key never appears as a
command-line argument, log line, shell-history entry, or in any JSON output
that the CLI renders. `config show` reports the credential as `present` and
`length` only.

In addition to the per-role Redmine API key, `workflow bootstrap` registers the
current repository with the companion `redmine_git_mirror` Redmine plugin so
the asynchronous mirror job creates the project repository. The plugin uses
its own bearer token read from `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY`; the
token can be persisted with `config set` and is never echoed in any JSON output
or logs. The command uses a secure prompt by default; `--stdin` is available
for a secret source that does not expose the key in shell history:

```text
phasegent config set redmine-git-mirror-api-key
phasegent config set redmine-git-mirror-api-key --stdin < /secure/path/plugin-bearer-key
```

A missing or invalid plugin key fails bootstrap with an actionable config
error. The repository URL the plugin receives is the credential-free origin
URL by default; set `PHASEGENT_REDMINE_REPOSITORY_URL` to override it (for
example to point at a self-hosted mirror) without touching the local `origin`
remote. The override is also persisted via `config set`:

```text
phasegent config set redmine-repository-url https://git.example.com/owner/repo.git
```

When `--repository OWNER/REPOSITORY` does not match the local `origin`,
`PHASEGENT_REDMINE_REPOSITORY_URL` is required: silently sending the wrong
repository to the plugin would attach a mirror the plugin cannot clone.

Use one Redmine project per Forgejo repository. The project must be reachable
through three existing Redmine users, one per agent role, each with its own
API key:

- The `admin` API key must belong to a Redmine user that can list roles/users
  and create or update project memberships; bootstrap uses it to look up and
  create the project and to write every membership.
- The `orchestrator`, `executor`, and `reviewer` API keys must each belong to a
  distinct Redmine user. Bootstrap resolves every agent identity through
  `/users/current.json` with the corresponding role-scoped key and reconciles
  direct project memberships in this default mapping:

  | Role           | Redmine role |
  | -------------- | ------------ |
  | orchestrator   | Maintainer   |
  | executor       | Developer    |
  | reviewer       | Reporter     |

  The role names are defaults; install localized or custom names in Redmine
  before bootstrap and they remain authoritative as long as they match exactly.

Store the per-role API keys with `auth setup` (stdin) before bootstrap.
Pass `--api-base https://redmine.example.com` on every setup so each
role's SQLite row carries the API base; later issue and comment commands
resolve the base from the stored config without needing `PHASEGENT_API_BASE`
or `--api-base` on every invocation:

```text
phasegent --role admin auth setup --stdin --provider redmine \
  --api-base https://redmine.example.com
phasegent --role orchestrator auth setup --stdin --provider redmine \
  --api-base https://redmine.example.com
phasegent --role executor auth setup --stdin --provider redmine \
  --api-base https://redmine.example.com
phasegent --role reviewer auth setup --stdin --provider redmine \
  --api-base https://redmine.example.com
```

### Discovering an existing project

On another machine, a bootstrapped repository's Redmine project can be found
without knowing its project ID. After configuring the API key and API base for
one role, list the projects visible to that Redmine user:

```text
phasegent --role orchestrator --provider redmine project list
```

`project list` does not require `--project-id`; its JSON output includes each
project's numeric `id`, `name`, and `identifier`. Use the selected ID as an
explicit per-invocation `--project-id` (project IDs are no longer persisted):

```text
phasegent --role orchestrator --provider redmine --project-id 42 issue get 3
```

Project IDs are invocation-only and never read from SQLite or environment.
When `--project-id` is omitted and the provider is Redmine, the current Git
origin is matched against existing `redmine_git_mirror` records (credential-free
canonical identity): exactly one match uses that project for the invocation,
multiple matches fail with a bounded listing of candidate ids/names and require
`--project-id`, and discovery HTTP/auth/decode errors are propagated. An
explicit `--project-id` always wins and skips discovery; an explicit
`--repository` that does not equal the current origin is never silently
matched to the origin.

After configuring all four keys, bootstrap the current repository before any
Redmine issue create, search, update, or close operation. The command derives
the identifier from `OWNER/REPOSITORY` or the current `origin` remote, reuses
only an exact project match, and stores the Redmine close-status ID for the
role (the project ID is not persisted):

```text
phasegent --role admin --provider redmine workflow bootstrap \
  --repository OWNER/REPOSITORY
```

If no exact project exists, bootstrap creates a private project automatically
and then reconciles direct memberships for the existing orchestrator
(Maintainer), executor (Developer), and reviewer (Reporter) users on it.
Select a close status explicitly when Redmine has more than one closed status:

```text
phasegent --role admin --provider redmine workflow bootstrap \
  --repository OWNER/REPOSITORY --close-status-name Closed
```

Bootstrap is admin-only and Redmine-only. It automatically selects the sole
closed status when that choice is unambiguous. Bootstrap enables the
`repository` module on the Redmine project so the `redmine_git_mirror`
plugin can attach the Git mirror, queues an asynchronous mirror job through
`POST /sys/redmine_git_mirror/projects/<id>/repository` with
`Authorization: Bearer <key>`, and surfaces the resulting mirror status
(`pending`, `cloning`, `ready`, `failed`, or `existing` if the bootstrap
only observes a previously queued mirror) in its JSON output. `pending` is a
successful bootstrap result because the asynchronous job is queued; the
plugin's first `GET
/sys/redmine_git_mirror/projects/<id>/repository/mirror_<id>_<owner>_<repo>`
call short-circuits a duplicate `POST` when the mirror already exists.
Bootstrap fails with a structured error before any partial identity mapping
is persisted whenever an agent identity cannot be resolved, its default role
name does not match an existing Redmine role, or the mirror plugin returns
an HTTP error or a `failed` status.

Issue `search`/`create` and `version list` can derive the project automatically
when `--project-id` is omitted (Redmine-only, current Git origin is matched
against `redmine_git_mirror` records). For `issue search`/`create`, a unique
match bypasses bootstrap entirely and uses the discovered project; no match
keeps the existing automatic bootstrap; multiple matches or any discovery
HTTP/auth/decode error fail before writes with a bounded actionable message
listing candidate ids/names (never URLs/credentials) and require
`--project-id`. For `version list`, a unique match lists that project's
versions, no match returns an actionable error telling the operator to pass
`--project-id` or run `workflow bootstrap`, and the command never
auto-bootstraps. Explicit `--project-id` always wins. Discovery is read-only
and never persists the project id.

After successful bootstrap, issue and journal operations use the stored Redmine settings:

```text
phasegent --role orchestrator --provider redmine issue create --title 'Plan' --body 'Details' --tracker Bug
phasegent --role orchestrator --provider redmine issue update-body 3 --body 'Updated plan' --tracker Feature
phasegent --role orchestrator --provider redmine status set 3 --status 'In Progress'
phasegent --role orchestrator --provider redmine status set 3 --status Resolved
phasegent --role orchestrator --provider redmine issue close 3
phasegent --role executor --provider redmine comment create 3 \
  --body '<!-- marker --> DONE' --marker '<!-- marker -->' --authorized
phasegent --role executor --provider redmine comment find-marker 3 --marker '<!-- marker -->'
phasegent --role orchestrator --provider redmine issue search --query 'phase'
phasegent --role executor --provider redmine version list
```

Issue creation and body updates accept an explicit Redmine tracker with
`--tracker`, which accepts a validated tracker name (`Bug`, `Feature`) or a
numeric id resolved against `/trackers.json`; unknown or ambiguous trackers
are rejected. `status list` shows the statuses visible to the API key, and
`status set <ISSUE> --status NAME_OR_ID` moves an issue to any status by
validated name or id. Redmine note output anchors each journal comment at
`#note-<id>` so audit references link to the exact note, and `comment get`
still reads one note in full.

Issue creation and body updates also accept native Redmine planning fields
in the same request: `--parent-issue`, `--fixed-version`, `--start-date`,
`--due-date`, `--estimated-hours`, and `--done-ratio`. `--fixed-version`
resolves by exact version name or numeric id against the configured
project's versions, so malformed values are rejected before any write.
Omitted flags keep the legacy request payload unchanged. `version list`
discovers the project versions visible to the API key:

```text
phasegent --role orchestrator --provider redmine issue create \
  --title 'Plan' --body 'Details' --fixed-version 'Sprint 1' \
  --parent-issue 12 --start-date 2026-08-01 --done-ratio 0
phasegent --role orchestrator --provider redmine issue update-body 3 \
  --body 'Updated plan' --done-ratio 40
phasegent --role executor --provider redmine version list
```

Redmine issue relations are managed with three subcommands. `relation list
<ISSUE>` shows every relation of an issue, rendered from that issue's viewpoint
(the inverse names `blocked` and `follows` appear when the issue is the target,
while `blocks`, `precedes`, and `relates` appear from the source). `relation
create <ISSUE> --to <ISSUE> --type blocks|precedes|relates [--delay N]` creates a
relation from the first issue to `--to`; only the forward canonical types are
accepted as `--type` (the inverse names `blocked` and `follows` are never
accepted, so a relation can never be created backwards), and `--delay N` (a
non-negative integer lag in days) is only valid with `--type precedes`.
`relation delete <RELATION_ID>` removes a relation by its numeric id. Relation
list is readable by orchestrator, executor, and reviewer; create and delete are
orchestrator-only; the admin role is denied all three; Forgejo reports a
structured not-supported error before any network access.

```text
phasegent --role executor --provider redmine relation list 3
phasegent --role orchestrator --provider redmine relation create 3 --to 5 --type blocks
phasegent --role orchestrator --provider redmine relation create 3 --to 5 --type precedes --delay 2
phasegent --role orchestrator --provider redmine relation delete 9
```

Redmine attachments are uploaded with `issue upload-attachment <ISSUE> --path PATH [--description TEXT]`. The local file must exist, be a regular non-empty file, have a valid filename, and not exceed 25 MiB; the CLI validates this before any network call and performs a raw `POST /uploads.json?filename=<basename>` (`Content-Type: application/octet-stream`) followed by `PUT /issues/<id>.json` with `{"issue":{"uploads":[{"token":..., "filename":...}],"notes":...}}`. Orchestrator-only and Redmine-only; Forgejo and GitLab return `not_supported` without touching the filesystem. Success prints compact JSON with `issue`, `filename`, `bytes`, and `success`; the transient upload token is never exposed.

```text
phasegent --role orchestrator --provider redmine issue upload-attachment 3 --path /tmp/screenshot.png --description "failure evidence"
```

### Phase timer ledger

The orchestrator starts and finishes the local execution ledger around the
executor/reviewer/tester child invocation. `tester` is a timer-only child
identity for the optional Bun/Playwright checker; it is not a global `Role`,
auth, or bootstrap member. `timer start` is local-only: it persists
`run_id`, issue, phase, agent role, attempt, and the wall-clock start timestamp
in SQLite before any provider or network operation. The generated or supplied
`run_id` is the stable marker used for retries.

```text
phasegent --role orchestrator --provider redmine timer start 28 \
  --phase implementation --agent-role executor --attempt 1 --run-id phase-5a-28
phasegent --role orchestrator --provider redmine timer finish phase-5a-28 --result DONE
```

The ledger preserves exact whole-second elapsed time and rounds the Redmine hours
up to 0.01 (with a 0.01-hour minimum for any positive duration). Finish lists
time-entry activities, preferring exact `AI automation`, then exact
`Development`, then a single default activity. Missing, duplicate preferred
names, and multiple default activities are structured configuration errors; the
ledger is never misclassified with an arbitrary activity. Finish is Redmine-only
and orchestrator-only. The stable run marker is re-list-checked before posting,
so a 204/empty create response can be retried without creating a duplicate Time
Entry. A 201 response is decoded and linked to `redmine_time_entry_id`.

**Timing boundary:** the SQLite ledger is the source of truth for wall-clock
duration; the rounded hours are a derived projection. The provider receives only
the rounded summary (Redmine) or exact seconds via `add_spent_time` (GitLab).
The local `elapsed_seconds` never leaves SQLite except as that bounded projection.

**Recovery and inspection (local-only, bounded):**

```text
phasegent --role orchestrator --provider redmine timer list --status running --limit 20
phasegent --role orchestrator --provider redmine timer get <RUN_ID>
phasegent --role orchestrator --provider redmine timer recover <RUN_ID>
```

`list` and `get` never touch the provider or network. `list` filters by
`running|finished|all` (default `all`) and caps at `--limit` (default 100, max
1000). Both hide secrets and full provider responses and return only the ledger
row fields. `recover` is the explicit orphan path: it marks a known `running`
row as `FAILED` and reuses the same-run provider reconciliation as `finish`. It
never infers success, never reopens a terminal row (concurrent recovers are
safe), and surfaces projection failures through `sync_status=failed`/`sync_error`
without overwriting the FAILED decision.

**No-guessing warning:** a missing child transcript after a crash is always
treated as `FAILED`. The system never guesses `DONE/PARTIAL/BLOCKED` from absent
state; only an explicit operator `recover` closes the orphan.

**Crash vs graceful dispose:** hard-crash leftovers stay `running` in SQLite
until you run `timer recover` after restart — this makes orphans diagnosable via
`timer list --status running`. Graceful plugin `dispose` (normal shutdown)
finishes in-memory active runs as `FAILED` via the bounded retry path; if that
retry exhausts, dispose logs a warning naming the exact `timer recover <RUN_ID>`
to run.

**Owner metadata:** `timer start --owner-session-id/--owner-call-id` records the
OpenCode subagent session/call that owns the run (bounded to 128 chars,
control-character-free, trimmed). The columns are `NULL` on old databases and
added idempotently via `ALTER TABLE` migration; legacy callers remain compatible.
Owner fields are local-only and never projected.

**Projection lease:** `timer finish`/`recover` claim the provider projection with a
caller-bound lease token (`projection_token` + `projection_claimed_at`) and
hold an `IMMEDIATE` SQLite transaction from claim until token-bound
finalization. While the transaction is held, a concurrent `BEGIN IMMEDIATE`
blocks on `busy`/`locked` and surfaces "projection already in progress"
without ever POSTing, so a live projector is never stealable merely because
provider reconciliation took longer than a wall-clock constant. Only the holder
may finalize `projecting` to `synced`/`failed`/`unconfirmed`; a loaded
`projecting` row without the matching token is never treated as this caller's
claim. A hard crash inside the transaction rolls the `projecting` claim back to
`pending`/`failed`/`unconfirmed` (no stale row); a crash that leaves a
`projecting` row (legacy autocommit window or pre-transaction database) is
explicitly recoverable via `timer recover` which force-resets a stale
`projecting` (`NULL` legacy or older than `PROJECTION_LEASE_SECS`) to `failed`
and then continues through the same token-bound projection retry path — it never
returns immediately with failed state. The next `recover` retry therefore
re-uses the marker-based reconciliation before any POST, preserving idempotency.

**Invariant:** every `projecting` transition and every finalization (`synced`/
`failed`/`unconfirmed`) is atomic and owner-bound (`projection_token` check);
callers that did not acquire ownership never mutate the row. Activity
initialization (`list_time_entry_activities` + persist) is covered by the same
serialization: the claim precedes the activity lookup, and the activity_id
persist is `UPDATE ... WHERE projection_token = ? AND sync_status='projecting'`,
so two concurrent `activity_id == NULL` callers cannot both list and POST.
Projection failure in `timer finish` / `timer recover` removes the round-3
unconditional fallback: the row's `sync_status` is owned exclusively by the
lease holder. When the holder is gone (rolled back / never claimed) the row's
terminal `failed` state is recorded locally before any provider attempt; the
projection error is then surfaced via `record_failed_sync_error`, which only
mutates a non-`projecting` row so a concurrent live holder is never clobbered.

**Liveness-protected stale reset:** `reset_stale_projection_to_failed` itself
acquires `BEGIN IMMEDIATE` so the reset blocks against any live holder that
is currently inside its own `IMMEDIATE`. The wall-clock lease window is
retained only as a documented crash-recovery check on a `projecting` row
whose holder has died without holding a live transaction (legacy autocommit
window or pre-projection-token databases). A modern crash inside a held
`IMMEDIATE` rolls the claim back on process exit, so no stale row remains
for the reset to discover; only legacy or pre-transaction orphans are
recoverable.

**Migration and legacy:** `Storage::open` runs additive `ALTER TABLE` migrations
under `BEGIN IMMEDIATE` with bounded retry on `busy`/`locked`; `COMMIT`
failures are propagated with rollback so initialization never reports success
when the lock or commit failed. Legacy `NULL` projection state remains
compatible; a `projecting` row with `NULL` `claimed_at` is treated as
immediately stale and is recoverable via the explicit `timer recover` retry
path.

**Hard-crash semantics:** a crash between the provider POST success and the
token-bound `synced` write inside the held transaction rolls back the claim
(Redmine: the next retry re-lists by marker and reconciles without duplicate;
GitLab: retry may duplicate spent time because the API has no read-back marker
— this is documented and `unconfirmed` is used when totals are missing). No
success is inferred from a missing transcript; orphans stay `running` until
`timer recover` durably marks `FAILED` locally before any provider lookup.

**Background tasks:** `task(background:true)` executor/reviewer delegations are
skipped with no timer and a concise `warn` via `client.app.log` that names the
issue/role and suggests `rerun foreground or use 'phasegent --role orchestrator
--provider redmine timer recover <RUN_ID>' if a previous run is orphaned`. Other
skipped cases (non-Redmine prompt, missing context, explore/general) remain
silent.

**Same-run retry and idempotency:** `finish` and `recover` share the marker-based
reconciliation: a second call with the same `run_id`/`result` is idempotent, and
a different result on a terminal row is rejected. Bounded retry (3 attempts,
exponential backoff) is safe because retries re-list by the run marker before
POST; Redmine duplicates remain prevented, GitLab's known crash-window (POST
succeeded but SQLite write lost) remains documented — retry may duplicate GitLab
spent time.

**Binary resolution:** the plugin resolves `phasegent` via `PHASEGENT_BIN` if set
and executable (honored when valid), otherwise the worktree
`target/debug/phasegent` and `~/.cargo/bin/phasegent` when they are regular
executable files, otherwise `PATH` lookup. Every invocation uses the same
fallback: automatic candidates are tried sequentially and the next is attempted
after a spawn or compatibility failure (`unknown option`, `unrecognized`,
`incompatible`); a bounded structured error is returned when no candidate is
available. Set `PHASEGENT_BIN` in tests for deterministic fake binaries.

The lower-level project metadata command remains confirmation-gated:

```text
phasegent --role admin --provider redmine project create \
  --name 'OpenCode workflow' --identifier opencode-workflow --confirm
```

The orchestrator can read, search, create, update, and close issues, set issue
statuses by validated name or id, and manage comments. The admin can list and
create Redmine projects and list issue statuses.
Executors and reviewers can read issues, comments, Redmine projects, issue
statuses, and project versions, and create only an explicitly authorized
marked comment.

`--role` is a capability policy and workflow routing label, not a hard identity
boundary: a caller who can execute the binary can choose another role. Persisted
tokens and provider configuration are stored per role, and token file permissions
and Forgejo token scopes remain the security boundary.

The API base and repository may be passed with `--api-base` and
`--repository OWNER/REPOSITORY`, configured with environment variables
`PHASEGENT_API_BASE` and `PHASEGENT_REPOSITORY`, or resolved from the
`origin` git remote. SSH remotes use the hostname with HTTPS and never reuse the
SSH transport port as an API port. HTTPS remotes retain an explicitly configured
port.

### Option values that begin with `-`

The two-argument form `--option value` rejects a value that starts with `-`,
because the parser would otherwise have to guess whether it is a new flag.
Markdown bodies that begin with `- Goal` or `---` are common in plan text, so
the affected options also accept the inline form `--option=value`:

```text
phasegent --role orchestrator issue create --title 'Plan' --body='- Goal'
phasegent --role orchestrator issue update-body 3 --body=---
phasegent --role executor comment create 3 --body=--- --marker='<!-- marker -->' --authorized
phasegent --role orchestrator issue search --query=-tag:regression
phasegent --role executor comment find-marker 3 --marker=---
```

The inline form is only an escape hatch for the leading-dash case. Plain
values still work with the two-argument form, and the structured error
message names the inline form when the strict missing-value detection fires.

## Branch-Scoped Redmine Issue Context

Redmine issue context is native local Git config, not SQLite state. Binding an
issue writes

```text
[branch "feature/name"]
    redmine-issue-id = 123
```

into `.git/config`. The binding is local to the checkout and the branch: it is
never pushed or shared automatically, and switching branches switches the
active issue context with them.

The commands operate purely on the current checkout and need no provider
access:

```text
phasegent issue bind 123
phasegent issue bind 123 --replace
phasegent issue unbind
phasegent issue status
phasegent hooks install
```

`issue bind` stores the binding for the current named branch. Detached HEAD is
rejected. Re-binding the same issue is a no-op; a different existing binding is
rejected unless `--replace` is given. `issue unbind` removes the current
branch's binding; absence is a no-op. `issue status` prints the current branch
and its bound issue, if any.

### Managed Git hooks

`workflow bootstrap` automatically installs the managed `prepare-commit-msg`
and `commit-msg` hooks when — and only when — the current checkout's `origin`
matches the bootstrapped `OWNER/REPOSITORY`. Bootstrapping a different
repository from the wrong checkout does not install hooks locally. `hooks
install` performs the same installation at any time; it is local and free of
roles, providers, credentials, and network access.

Unrelated existing hooks are preserved: they are moved to
`.git/hooks/phasegent-original/<hook-name>` and the managed wrapper chains to
the original so it still runs first. Managed installation is idempotent;
re-running it updates the managed wrappers in place.

Hook behavior:

- With a bound named branch, normal and template commit messages get
  `Refs #<id>` appended exactly once.
- Messages generated by merge, squash, or cherry-pick (`commit`) sources are
  never rewritten.
- A message referencing a different Redmine issue than the branch binding fails
  commit validation; duplicate generated trailers are rejected too.
- The default relation is always `Refs`. The hooks never add `Fixes`
  automatically; use `Fixes #<id>` explicitly in the message when the commit
  should close the issue.
- `git commit --no-verify` bypasses local hooks. It remains an explicit escape
  hatch but should not be used by the agent workflow.

### Issue lifecycle integration

- A successful Redmine `issue create` auto-binds the returned issue to the
  current matching branch when possible.
- A successful Redmine `issue close` unbinds only the current branch if its
  binding points to that exact issue.
- Detached HEAD, mismatched bindings, non-Git directories, and local failures
  never undo the remote success; they emit a bounded warning instead.
- Forgejo issue create/close does not touch the Redmine branch context.

## Local Configuration

`auth setup`, the per-role provider settings, the phase execution ledger, and the
two `redmine_git_mirror` plugin settings are persisted in a single SQLite
database. The database lives at the OS-standard config directory reported by
`directories::ProjectDirs` (qualified `com` / `Cloud1ful` / `phasegent`):

```text
Linux :   ~/.config/phasegent/phasegent.sqlite3
macOS :   ~/Library/Application Support/com.Cloud1ful.phasegent/phasegent.sqlite3
Windows: %APPDATA%\Cloud1ful\phasegent\config\phasegent.sqlite3
```

The additive `execution_timer_runs` table stores exact elapsed seconds, rounded
hours, and optional activity/Time Entry projection state. On Unix the directory
is created with mode `0700` and the database file with mode `0600` so accidental
disclosure is limited to the owner.

`auth setup` stores Forgejo role tokens
and Redmine API keys as plaintext rows in SQLite by design. The CLI never
echoes credentials: the stdin-only path is preserved, structured errors name
only the missing variable, and `config show` never prints secret values. Role
separation is preserved. Each role keeps its own `(forgejo, redmine)` provider,
API base, repository, close status id, and credential (project IDs are
invocation-only via `--project-id` and never persisted); the three global
settings (`PHASEGENT_REDMINE_GIT_MIRROR_API_KEY`,
`PHASEGENT_REDMINE_REPOSITORY_URL`, and the machine-wide
`PHASEGENT_DEFAULT_PROVIDER`) live in a separate `global_setting` table.

### Inspecting the database with `config show`

`config show` prints a redacted JSON snapshot of the local SQLite database. It
does not require `--role` and returns every role by default. Pass
`--role ROLE` to restrict the `roles` array to a single role:

```text
phasegent config show
phasegent --role executor config show
```

The output contains:

- `database_path` — absolute path to the SQLite file.
- `roles[]` — one entry per role with provider, Forgejo/Redmine URLs, close
  status id, and credential summaries (project IDs are not persisted and do
  not appear). Each credential reports `present` and `length` only; the value
  itself is never echoed.
- `global_settings[]` — `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY` (presence and
  length only), `PHASEGENT_REDMINE_REPOSITORY_URL` (sanitised: embedded
  userinfo, password, query string, and fragment are stripped before the
  snapshot is rendered; SSH-style `user@host` is preserved because it is the
  SSH transport handle, not a credential), and `PHASEGENT_DEFAULT_PROVIDER`
  (the non-secret machine-wide default provider literal).
- `global_default_provider` — top-level alias for the persisted
  `PHASEGENT_DEFAULT_PROVIDER` so a single `config show` invocation reports
  the resolver-relevant default alongside the rest of the snapshot.
  `null` when the default has never been set.

`config show` never prints full secrets. Reading the actual key value is not
a supported operator workflow; secrets stay inside SQLite and stay inside the
credential read path.

### Setting values with `config set`

Ordinary provider commands read `PHASEGENT_*` values from the process
environment as runtime overrides but never write them to SQLite. Persist one
setting explicitly with `config set`, or remove it with `config clear`:

```text
phasegent config set redmine-repository-url https://git.example.com/owner/repo.git
phasegent config clear redmine-repository-url
```

Project IDs are not persisted; use explicit `--project-id` per invocation
(e.g. `phasegent --role orchestrator --provider redmine --project-id 42 issue
get 3`). Legacy `redmine-project-id`, `gitlab-project-id`, and `project-id`
aliases are rejected as unknown settings.

Canonical `PHASEGENT_*` names and kebab-case aliases are accepted. Global
settings (`redmine-git-mirror-api-key`, `redmine-repository-url`, and
`default-provider`) do not require `--role`; role-scoped settings do.
`config show` remains the read-only, redacted view of the resulting SQLite
state.

The mirror plugin bearer key is a secret and is never accepted as a command
line value. Use the secure prompt, or read it from stdin:

```text
phasegent config set redmine-git-mirror-api-key
phasegent config set redmine-git-mirror-api-key --stdin < /secure/path/plugin-bearer-key
```

`--stdin` also works for non-secret settings. Values are trimmed before they
are persisted, and successful output reports only the canonical setting name
and role.

### Precedence and supported environment variables

Resolution precedence is **CLI arguments > environment variables > SQLite >
origin/default discovery** for both role-scoped settings and the two global
plugin settings. Environment variables continue to work as runtime overrides;
SQLite is read only when the matching environment variable is unset or empty.
The provider selection follows the same pattern, with an additional
machine-wide default that sits between the `PHASEGENT_PROVIDER` runtime
override and the role-scoped `role_config.provider` row:

1. explicit `--provider` argument
2. `PHASEGENT_PROVIDER` environment variable
3. `PHASEGENT_DEFAULT_PROVIDER` environment variable
4. persisted `PHASEGENT_DEFAULT_PROVIDER` (set via
   `config provider set / get / clear`)
5. role-scoped `role_config.provider` (written by `auth setup`)
6. forgejo fallback

Role-scoped settings accepted by `config set`:

- `PHASEGENT_PROVIDER` — selects `forgejo`, `redmine`, or `gitlab`.
- `PHASEGENT_API_BASE` — generic API base; used by Forgejo and as the Redmine
  fallback when the provider-specific variable is unset.
- `PHASEGENT_REPOSITORY` — `OWNER/REPOSITORY` for Forgejo.
- `PHASEGENT_REDMINE_API_BASE` — Redmine API base, takes precedence over the
  generic `PHASEGENT_API_BASE`.
- `PHASEGENT_REDMINE_CLOSE_STATUS_ID` — Redmine close status id.
- `PHASEGENT_CLOSE_STATUS_ID` — generic alias for the Redmine close status
  id.

Project-id persistence (`PHASEGENT_REDMINE_PROJECT_ID`,
`PHASEGENT_GITLAB_PROJECT_ID`, `PHASEGENT_PROJECT_ID`) was removed in Phase 1;
use explicit `--project-id` per invocation instead.

Global settings accepted by `config set`:

- `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY` — bearer token for the companion
  `redmine_git_mirror` Redmine plugin.
- `PHASEGENT_REDMINE_REPOSITORY_URL` — repository URL override sent to the
  mirror plugin.
- `PHASEGENT_DEFAULT_PROVIDER` — machine-wide default provider
  (`forgejo`, `redmine`, or `gitlab`). Validated through `ProviderKind` so a
  typo never lands in SQLite; managed through `config set default-provider`
  or the `config provider set / get / clear` subcommands.

### Managing the global default provider

The machine-wide default lives in `global_setting` and is managed through
three subcommands. None of them require `--role` because the default is
global, not role-scoped:

```text
phasegent config provider get         # null when no default has been set
phasegent config provider set gitlab # validate and persist
phasegent config provider clear      # remove the persisted row
```

`config provider get` prints the persisted literal or `null` when no default
has been recorded; the value is one of `forgejo`, `redmine`, or `gitlab`.
`config provider set` validates the literal through `ProviderKind::from_str`
and persists it; unknown values return a structured config error before any
SQLite write. `config provider clear` deletes the row so the resolver falls
back to the role-scoped provider, returning
`{"cleared": true}` when a row was removed or
`{"cleared": false}` when the default was already absent. The same default
is reported in `config show` through both the
`global_default_provider` top-level field and the
`global_settings[]` entry, alongside the existing role-scoped snapshot.

### Practical commands

Verify the database exists without printing secrets:

```text
test -f "$HOME/.config/phasegent/phasegent.sqlite3" \
  && echo "phasegent SQLite database exists" \
  || echo "run any phasegent command to initialise it"
```

Print a redacted snapshot of every role or a single role:

```text
phasegent config show
phasegent --role executor config show
```

Persist a global mirror setting:

```text
phasegent config set redmine-git-mirror-api-key
```

Use environment variables as runtime overrides without persisting them:

```text
export PHASEGENT_API_BASE=https://forgejo.example
export PHASEGENT_REPOSITORY=owner/repo
phasegent --role executor issue get 3
```

None of the example commands accept or print credentials; supply provider
credentials through `auth setup` and the mirror key through the secure
`config set` prompt or stdin path. Never inline secrets in shell history.

## Install

From this directory:

```sh
cargo install --path .
```

Set up a Forgejo role token from secure interactive input or stdin:

```sh
phasegent --role orchestrator auth setup
phasegent --role executor auth setup --stdin
```

Forgejo tokens and Redmine API keys are both stored in the
`~/.config/phasegent/phasegent.sqlite3` database under the
operator's platform-standard config directory on Unix with directory
mode `0700` and file mode `0600`.
The CLI never accepts them as command-line arguments, never echoes them
back, and never includes them in any rendered JSON; `auth setup` reads
the credential from stdin and `config show` reports the row as
`present` and `length` only. Never pass, log, or commit either
credential.

`workflow bootstrap` resolves the companion `redmine_git_mirror` plugin
bearer token through the `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY`
environment variable, falling back to the SQLite `global_setting` row
written by `config set` when the variable is unset. The token is treated as a
deployment secret; persist it once through the secure prompt or stdin path
rather than re-supplying it on every bootstrap.

Inspect the available surface with:

```sh
phasegent --help
phasegent --role executor --help
phasegent --help issue
phasegent --help issue create
phasegent --help config provider
phasegent --help auth
phasegent --help workflow bootstrap
```

## Release

Push a version tag `vX.Y.Z` (for example `v0.1.0`) to trigger a public GitHub release (no branch-triggered publish):

```sh
git tag vX.Y.Z
git push origin vX.Y.Z
```

`.github/workflows/release.yml` builds five `phasegent` binaries with `sccache`:

- `x86_64-unknown-linux-gnu` on `ubuntu-24.04`
- `aarch64-unknown-linux-gnu` on `ubuntu-24.04-arm`
- `aarch64-apple-darwin` on `macos-14`
- `x86_64-pc-windows-msvc` on `windows-2022`
- `aarch64-pc-windows-msvc` on `windows-2022` (via `ilammy/msvc-dev-cmd` arch `amd64_arm64`)

Each binary is published as `phasegent-<tag>-<target>` (`*.exe` on Windows) plus a `SHA256SUMS` checksum file generated on the Ubuntu release job. Releases are published with `softprops/action-gh-release` and generated release notes.

Remote compilation cache is optional. To enable Cloudflare R2 for `sccache`, configure these repository Secrets (no value is hardcoded in the workflows; `SCCACHE_REGION` is set to `auto`):

- `R2_ACCESS_KEY_ID`
- `R2_SECRET_ACCESS_KEY`
- `SCCACHE_ENDPOINT`
- `SCCACHE_BUCKET`

The workflows map `R2_ACCESS_KEY_ID` / `R2_SECRET_ACCESS_KEY` to `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` and pass `SCCACHE_BUCKET` / `SCCACHE_ENDPOINT` through. When the secrets are absent, `sccache` falls back to a local disk cache and CI/release builds still succeed. No Docker or container registry publishing is performed.

Licensed under Apache-2.0. See [LICENSE](LICENSE).
