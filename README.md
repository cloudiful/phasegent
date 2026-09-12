# phasegent

[简体中文](README.zh-CN.md)

`phasegent` is a role-aware CLI for provider-backed OpenCode workflows. It
provides one command-line interface for issue tracking, repository operations,
comments, and workflow automation.

## Features

- Forgejo, Redmine, and GitLab providers; the provider is resolved from
  configuration and falls back to Forgejo when none is configured.
- Local provider (`--provider local`) that runs offline with no credential
  and no network.
- Roles for `admin`, `orchestrator`, `executor`, `reviewer`, and `tester`.
- Issue search, creation, updates, closing, comments, statuses,
  relations, versions, and project listing where supported by the
  provider (see the [Provider capability matrix](#provider-capability-matrix)).
- Local issue index (SQLite by default, optional PostgreSQL).
- Local branch-to-issue context and managed Git hooks.
- MCP server over stdio or streamable HTTP with the same role policy.
- Compact JSON output and structured errors.

## Install

Requirements: Rust stable and Cargo.

```sh
git clone https://forgejo.cloud1ful.com/tools/phasegent.git
cd phasegent
cargo install --path .
```

Enable the optional PostgreSQL index backend when needed:

```sh
cargo install --path . --features postgres
```

## OpenCode integration

`phasegent` ships an OpenCode skill at `skills/phasegent-workflow`. On the
OpenCode side it is a thin entry that delegates to the CLI: it picks the tracking
mode, delegates to executor/reviewer/tester, and enforces the marker, VERDICT,
note-pointer, and orchestrator-owned timer/status/notify contracts. Nothing on
the OpenCode side reimplements the CLI; `phasegent --role <role>` stays the
authoritative syntax reference.

Install by copying the skill tree into the per-user skills directory:

```sh
cp -r skills/phasegent-workflow ~/.config/opencode/skills/
```

The skill then loads by its `name: phasegent-workflow` frontmatter. Tracking
lives as a `TRACKED_ISSUE` on the configured provider, as a local provider issue
(`--provider local`, which replaces `.opencode/plans/*.md` markdown), or inline
for trivial read-only work.

## Quick Start

The provider is resolved from configuration (`--provider` > environment >
`phasegent.toml` > SQLite > Forgejo fallback). Configure a credential for each
role that will use the CLI. Credentials are read through a secure prompt or
stdin and are never accepted as command-line values.

```sh
# Secure prompt
phasegent --role orchestrator admin auth setup

# Read from a protected file or another secure source
phasegent --role executor admin auth setup --stdin < /secure/path/token
```

Select another provider and its API base explicitly:

```sh
phasegent --role orchestrator --provider redmine admin auth setup \
  --stdin --api-base https://redmine.example.com
```

Prepare the Redmine project and role memberships:

```sh
phasegent --role admin --provider redmine admin workflow bootstrap \
  --repository OWNER/REPOSITORY
```

## Common Commands

```sh
phasegent --role orchestrator issue search --query "bug"
phasegent --role orchestrator issue get 123
phasegent --role orchestrator issue get 123 124 125
phasegent --role orchestrator comment list 123
phasegent --role orchestrator issue create \
  --title "Short title" --body "Issue details"
phasegent --role orchestrator issue update 123 --body "Updated details"
phasegent --role orchestrator issue close 123
phasegent doctor
```

Use `--provider redmine` or `--provider gitlab` on a command when the selected
provider is not the default. Use `--repository OWNER/REPOSITORY` and
`--project-id ID` to override repository or project discovery when required.

Provisioning (`auth setup`, config writes, `workflow bootstrap`) lives under
the human-operator `admin` group (`phasegent admin ...`) and is never invoked
by AI roles. `phasegent doctor` reports credential presence (fingerprint, never
values) and index state without a role.

### One-shot Markdown bodies (`--body-file`)

`issue create`, `issue update`, and `comment create` accept
`--body-file PATH` instead of `--body`, so long Markdown never has to pass
through the shell. The flags are mutually exclusive, and `--body-file` reads a
regular file of at most 2 MiB that must be valid UTF-8.

The file is read and validated locally before provider resolution, project
discovery, or any network access, and its content is exactly what the provider
receives. After a successful write the file is deleted unless
`--keep-body-file` is given; any read, validation, argument, or provider failure
keeps it. A path that was replaced or modified after the read is never deleted —
a bounded warning is emitted instead.

```sh
phasegent --role orchestrator issue create --title "Plan" --body-file /tmp/plan.md
phasegent --role orchestrator issue update 123 --body-file /tmp/plan.md
phasegent --role executor comment create 123 --body-file /tmp/audit.md \
  --marker "<!-- ai-executor ... -->" --authorized
```

Inspect the available commands with:

```sh
phasegent --help
phasegent --help issue
phasegent --help admin
```

## Provider capability matrix

`phasegent` targets four providers (`forgejo`, `redmine`, `gitlab`,
`local`). The matrix below mirrors `src/policy.rs`; every CLI/MCP guard
and dispatcher arm aligns with it. Cells read `yes` (capability is
implemented for that provider) or `no` (structured `not_supported` error
before any network/file access).

| Capability | Forgejo | Redmine | GitLab | Local |
|---|:---:|:---:|:---:|:---:|
| IssueRead / IssueSearch / IssueCreate / IssueUpdateBody / IssueClose | yes | yes | yes | yes |
| IssueAttachmentUpload | no | **no** | no | no |
| CommentCreate / CommentRead / CommentFindMarker | yes | yes | yes | yes |
| RepoCreate | yes | no | yes | no |
| ProjectRead | no | yes | yes | yes |
| ProjectCreate | no | yes | no | yes |
| IssueStatusRead | no | yes | yes | yes |
| VersionRead | no | yes | yes | yes |
| RelationRead / RelationCreate / RelationDelete | no | yes | yes | no |

### IssueAttachmentUpload — uniformly not-supported

Every provider rejects `issue upload-attachment` (exit 1,
`not_supported`). The capability stays reserved (orchestrator or
tester) so a future phase may re-enable the underlying upload path
without a capability rename. Evidence moves to comments or external
links.

### Read-side parity for GitLab (Phase 2)

Three rows that originally stayed `no` for GitLab are now backed by
equivalent reads:

- `ProjectRead` → `GET /projects` mapped onto `RedmineProject`.
- `IssueStatusRead` → static `WORKFLOW_LABELS` catalogue mapped onto
  `RedmineIssueStatus` (GitLab has no native status enum; the workflow
  is encoded as project labels).
- `VersionRead` → `GET /projects/:id/milestones` mapped onto
  `RedmineVersion`.

`ProjectCreate` stays `no` for GitLab because the equivalent lives on
the `repo create` path (`POST /projects`); there is intentionally one
entry point to that endpoint. The GitLab `ApiIssue` DTO widened in
Phase 2 to decode `milestone`, `due_date`, `weight`, `time_stats`,
`assignee(s)`, `created_at`, and `updated_at` while keeping the legacy
field shape required so older fixtures and audit-comment consumers
stay compatible.

### Planning-flag exceptions

`--tracker`, `--parent-issue`, `--fixed-version`, `--start-date`,
`--due-date`, `--estimated-hours`, and `--done-ratio` are accepted as
CLI input but forwarded or rejected per provider:

- Redmine: every flag is a native field. `--fixed-version` resolves
  by exact version name or numeric id within the configured project.
- GitLab: `--estimated-hours` is forwarded through the native
  `time_estimate` endpoint; `--tracker` maps to a `type::bug` /
  `type::feature` label; every other planning flag is rejected.
- Forgejo: rejects every planning flag.
- Local: accepts every flag for parser compatibility but does not
  persist any of them (the local index only stores title, body, and
  state).

### Write-side relation auto (Phase 3)

When `issue create --parent-issue <ID>` is used on Redmine or GitLab,
a `relates` link from the new child to the parent is created
automatically by the lifecycle helper, with idempotency (a pre-existing
`relates` link to the same parent is detected via `list_relations` /
`list_issue_links` and does not produce a duplicate). The helper
returns a bounded warning on failure (parent id `0`, self-link, or
provider error) and never pollutes stdout JSON or exit code. AI
workflows do not need to call `relation create` separately; `status
set`, `status advance`, and `issue close` also have the hook wired,
but they pass `None` today because the read-side DTO does not yet
expose the parent linkage — the helper returns `Skipped` silently.
Forgejo and Local have no relation surface, so they skip the helper.

## Desktop app

Normal `phasegent <command>` invocations stay in the CLI. Open the desktop
app explicitly with:

```sh
phasegent gui
```

Running `phasegent` with no arguments in a terminal still shows help.
Launching it from Explorer/Finder (no terminal) opens the desktop app
instead.

Release downloads: Windows provides one x64 MSI plus the matching raw exe;
macOS provides an unsigned app inside the `.dmg`, so Gatekeeper may warn on
first open.

## Configuration

`admin auth setup` stores provider credentials locally. `config show` provides a
redacted view; secret values are never printed.

```sh
phasegent config show
phasegent config provider get
phasegent admin config provider set redmine
phasegent admin config provider clear
```

Provider selection can be set per command with `--provider` or with
`PHASEGENT_PROVIDER`. Stable non-secret settings can be edited in
`phasegent.toml` (override its location with `PHASEGENT_CONFIG_PATH`).
Precedence is CLI flags > environment > TOML > SQLite > defaults.

To use PostgreSQL as the issue index backend, configure its URL through
stdin:

```sh
phasegent admin config set index-pg-url --stdin
```

## Local provider

`--provider local` runs fully offline with no credential, no network, and no
`admin auth setup` token. It uses an independent local database file
(`phasegent-local.sqlite3`) next to the config database, seeded with the
default project, issues, comments, and the canonical status transitions.
`admin auth setup --provider local` only records the role-scoped provider
preference and never prompts for a secret:

```sh
phasegent --role executor --provider local admin auth setup
phasegent --role executor --provider local issue create \
  --title "Local task" --body "Works offline"
```

The same issue, comment, and status commands work against the local backend
(`issue search`, `issue get`, `issue create`, `issue update`,
`issue close`, `comment create`, status list/next/advance/set, and project
list/create). Repository creation, attachment upload, and relation
operations surface a structured `not_supported` error; `version list`
returns an empty catalogue, matching the parity matrix. Envelopes follow
the Redmine-aligned shape so scripts that select `--provider local` keep
a stable format.

PostgreSQL is selected when the same non-empty `PHASEGENT_INDEX_PG_URL` used
by the index is set; otherwise SQLite is used. Only one local backend is
active at a time (single-active, never dual-written). Additive migrations
under `migrations/pg/0002_local.sql` mirror the SQLite schema.

## Local Branch Context

Bind an issue to the current Git branch and install managed hooks locally:

```sh
phasegent issue bind 123
phasegent issue status
phasegent hooks install
phasegent issue unbind
```

These commands operate on the local checkout and do not require provider
access.

## Worktrees

Run isolated per-issue worktrees so multiple tasks can share one repo without
colliding. A lease is keyed by `(repo, issue, session)`. Resolve the session in
this order: `--session` (or `issue close --worktree-session`), then
`PHASEGENT_SESSION_ID`, then the legacy `phasegent` fallback. The legacy
fallback only warns on stderr and can let concurrent sessions share a lease, so
the OpenCode workflow must generate one stable id per session and reuse it for
every worktree call. Auto-isolation defaults off: on conflict, `acquire` reuses
the current checkout and warns. Enable it with
`admin config set worktree-auto true` or per call with `--isolate`. When the
`git status` probe itself fails, the dirty state is unknown: auto-isolation on
creates a fresh worktree, off reuses the checkout, and both warn on stderr — an
unknown status is never silently treated as clean. Copy `.env` files by hand
when the new worktree needs them.

```sh
export PHASEGENT_SESSION_ID="<stable-session-id>"
phasegent --role orchestrator worktree acquire --issue ISSUE --session "$PHASEGENT_SESSION_ID" --isolate
phasegent --role orchestrator worktree heartbeat --lease LEASE --session "$PHASEGENT_SESSION_ID"
phasegent --role executor worktree status --issue ISSUE
phasegent --role executor worktree list
phasegent --role orchestrator worktree prune --stale-days 14
phasegent --role orchestrator worktree prune --stale-days 14 --release-stale --reason "stale session recovery"
phasegent --role orchestrator worktree prune --stale-days 14 --remove
phasegent --role orchestrator worktree release --lease LEASE
```

`heartbeat` refreshes only an active lease whose stored session matches the
caller; a foreign session or terminal lease returns a structured conflict and
is left untouched. `worktree prune` is the single pruning entry point, and it
is a read-only report by default: it lists every active lease whose heartbeat
is older than `--stale-days` (default 14) and every removable worktree, and
changes nothing. `--release-stale` requires
`--reason TEXT` and flips exactly those stale active leases to `retained` with
the reason recorded; `--remove` deletes only worktrees that are `retained`,
expired, and clean, after any requested recovery runs first. Either action is
opt-in, and `--reason` without `--release-stale` is rejected. No action ever
deletes a branch, and `git worktree remove` never receives `--force`, so dirty
worktrees, active leases, and uncommitted changes are never removed. Closing an
issue releases only the leases matching the current repo, issue, and session: a
failed remote close changes no local lease, a close without a resolved session
releases nothing, and other sessions are never affected.

## MCP server

Serve the contracted operations over the Model Context Protocol:

```sh
# Stdio (default, for local MCP clients)
phasegent --role executor mcp serve

# Streamable HTTP at /mcp
phasegent --role executor mcp serve --transport http --bind 127.0.0.1:3000
```

Tools run with the startup `--role` and `--provider` flags; MCP clients
never supply a role.

## Notifications

Notifications are manual-only: nothing sends automatically. Send explicitly
with `notify send` (CLI) or `notify_send` (MCP):

```sh
phasegent --role executor notify send --event completion --title "Done" --body "Details"
```

Events: `completion`, `blocked`, `failure`, `interruption_suspected`,
`publish_failed`. Configure with `admin config set notify-enabled true` and
`admin config set notify-channel <name>`; secrets require `--stdin`. The admin group is human-operator only.

## Container image

CLI-only image running as non-root. The default command serves
authenticated MCP over streamable HTTP on loopback; state persists under
`/data`. Images publish for version tags (`v*`) as `<tag>` plus `latest`.
Substitute your repository slug for `OWNER/REPO`:

```sh
docker pull ghcr.io/OWNER/REPO:latest
mkdir -p ./phasegent-data
docker run --rm -p 127.0.0.1:3000:3000 \
  -e PHASEGENT_MCP_AUTH_TOKEN="$(cat /secure/path/mcp-token)" \
  -v ./phasegent-data:/data \
  ghcr.io/OWNER/REPO:latest
```

- Token: pass `PHASEGENT_MCP_AUTH_TOKEN` with `-e`; never baked into the
  image. HTTP without it exits before binding.
- Storage: `/data` is a volume; defaults are
  `PHASEGENT_DB_PATH=/data/phasegent.sqlite3` and
  `PHASEGENT_CONFIG_PATH=/data/phasegent.toml`.
- Stdio override for local MCP clients:
  `docker run --rm -i -v ./phasegent-data:/data ghcr.io/OWNER/REPO:latest --role executor mcp serve --transport stdio`
- Warning: the default binds loopback only. Serving with `--bind 0.0.0.0:3000`
  exposes HTTP beyond loopback: keep the token secret and use a firewall
  or reverse proxy.

Successful commands return compact JSON. Errors are written to stderr and use
a non-zero exit status.

## License

Apache-2.0. See [LICENSE](LICENSE).
