# Command contract

Source of truth: `src/cli/help/` role-filter plus `src/policy.rs`.
`phasegent --role <role> --help <topic> [<command>]` is the authoritative
syntax for every command below; no command here is invented. The provider is
resolved from configuration: an explicit `--provider` wins, otherwise the
configured default (role or global setting, `phasegent.toml`, or environment)
applies, and Forgejo is the final fallback. This file records role gates, not
flag tables. Credentials are never accepted as CLI values (`admin auth setup`
reads a secure prompt or `--stdin`).

Provisioning lives under the human-operator `admin` group (`admin auth
setup`, `admin config set/clear`, `admin config provider set/clear`,
`admin workflow bootstrap`). AI roles must never invoke the `admin`
token; agent permission rules deny that single prefix.

## Commands and role gates

| Command | Capability / gate | Roles allowed | Provider / surface notes |
|---|---|---|---|
| `issue get` | IssueRead | orchestrator, executor, reviewer, tester | one number returns the single-issue object; 2–20 return an `{issues, errors}` envelope (exit 1 unless every fetch succeeds) |
| `doctor` | none (read-only self-check) | any (no `--role` required) | credential presence (fingerprint, never values), index backend state, masked PG URL; approved replacement for schema dumps and raw setting reads |
| `issue search` | IssueSearch | orchestrator only | provider-fresh; auto-bootstraps project on no match; scoped local-index fallback on failure |
| `issue create` | IssueCreate | orchestrator only | planning flags Redmine/GitLab; Forgejo rejects every planning flag |
| `issue update` | IssueUpdateBody | orchestrator only | tracker/planning flags in same PUT |
| `issue close` | IssueClose | orchestrator only | — |
| `issue upload-attachment` | IssueAttachmentUpload | orchestrator, tester | Uniformly not-supported (Phase 1 parity + Phase 4 sink); every provider rejects with `not_supported` (exit 1) before any file, network, or credential access |
| `issue bind` / `issue unbind` / `issue status` | IssueRead | orchestrator, executor, reviewer, tester | local branch–issue binding; no provider/network |
| `comment create` | CommentCreate | orchestrator, executor, reviewer, tester | `--authorized` required unless orchestrator (CLI) |
| `comment get` | CommentRead | orchestrator, executor, reviewer, tester | single note, full body |
| `comment list` | CommentRead | orchestrator, executor, reviewer, tester | every note on the issue with full bodies, provider order, as `{issue, comments}`; the approved bulk-read path |
| `comment find-marker` | CommentFindMarker | orchestrator, executor, reviewer, tester | marker matched verbatim |
| `project list` | ProjectRead | orchestrator, admin, executor, reviewer | Redmine, GitLab, and local; Forgejo rejects; does not need `--project-id` |
| `project create` | ProjectCreate | orchestrator, admin | Redmine and local; GitLab/Forgejo use `repo create` (single entry point to `POST /projects`); requires `--confirm` |
| `status list` | IssueStatusRead | orchestrator, admin, executor, reviewer | Redmine, GitLab (static `WORKFLOW_LABELS` catalogue), and local; Forgejo returns not-supported |
| `status next` | IssueStatusRead | orchestrator, admin, executor, reviewer | read-only; current + policy-allowed next + recovery command; Redmine and local |
| `status set` | role == orchestrator | orchestrator only | validated name/id; Redmine, GitLab (workflow label), and local |
| `status advance` | role == orchestrator | orchestrator only | policy preflight; idempotent no-op; Redmine and local |
| `version list` | VersionRead | orchestrator, admin, executor, reviewer | Redmine (native) and GitLab (GET /projects/:id/milestones); local returns an empty catalogue (no versions table); Forgejo returns not-supported; never auto-bootstraps |
| `relation list` | RelationRead | orchestrator, executor, reviewer | Redmine/GitLab; Forgejo and local reject (no relation surface) |
| `relation create` | role == orchestrator | orchestrator only | Redmine/GitLab; Forgejo and local reject; Phase 3 lifecycle helper auto-creates a `relates` link on `issue create --parent-issue <ID>` (idempotent, bounded warning on failure) |
| `relation delete` | role == orchestrator | orchestrator only | Redmine/GitLab; Forgejo and local reject |
| `timer start/finish/list/get/recover` | role == orchestrator | orchestrator only | local ledger; finish/recover project to Redmine/GitLab (Forgejo rejects); list/get never reach a provider |
| `worktree acquire/release/heartbeat/prune` | role == orchestrator | orchestrator only | per-(repo, issue, session) leases; `prune` is read-only unless `--release-stale --reason TEXT` or `--remove` is given, removes only clean + expired + retained worktrees, and never deletes a branch or a dirty worktree; `release --force` requires non-empty `--reason`, persisted on the row and visible in status/list (rows are never deleted) |
| `worktree status/list` | command-level read gate | orchestrator, executor, reviewer | read-only lease inspection; tester denied |
| `plugin install/status/uninstall` | no role gate | any | local OpenCode adapter file; managed-marker ownership, foreign-file refusal; never touches worktrees or branches |
| `admin workflow bootstrap` | role == admin | admin only | Redmine-only; needs only the admin key |
| `repo create` | RepoCreate | orchestrator only | Forgejo/GitLab; `--private` required; Redmine/local reject as not-supported |
| `notify send` | role gate | orchestrator, executor, reviewer, tester (admin denied) | manual-only; bounded envelope; never automatic |
| `mcp serve` | startup `--role` | role-scoped toolset | tools: `capabilities`, `issue_get`, `issue_search`, `status_next`, `comment_create` (needs server-side `--authorized` unless orchestrator), `notify_send`; excludes `status_advance`, timer start/finish, role elevation |
| `admin auth setup` | all roles | admin, orchestrator, executor, reviewer, tester | credentials never a CLI value |
| `config show` / `config provider get` | machine-wide | any (no `--role` required) | redacted snapshot; secrets as presence/length/fingerprint, never values |
| `admin config set` / `admin config clear` | global or role-scoped | any; role-scoped settings need `--role` | SQLite only; secret settings require `--stdin` |
| `admin config provider set` / `admin config provider clear` | machine-wide | any (no `--role` required) | SQLite only |
| `hooks install` | no role gate | any | local checkout; managed prepare-commit-msg/commit-msg hooks |
| `gui` | none | any | desktop single-binary shell |

## Orchestrator-owned primitives (never client roles)

`timer start` / `timer finish` / `timer recover` and `status set` /
`status advance` are orchestrator-only via manual CLI. Children (executor,
reviewer, tester) never start timers, advance statuses, edit the issue body, or
label/close the issue. `status_advance` and timer operations are never exposed
over MCP.

## Admin group (never AI roles)

The entire `admin` group (`admin auth setup`, `admin config set/clear`,
`admin config provider set/clear`, `admin workflow bootstrap`) is
human-operator only — including the orchestrator. AI roles use the
top-level read paths (`config show`, `config provider get`, `doctor`)
for self-checks and ask the operator when provisioning is missing.
Audit notes are append-only: there is deliberately no
comment update/delete command; publish a follow-up note instead.
Credentials are never writable outside the admin group, and lease
rows are never deletable (`worktree release --force --reason` is the
attributed override).

## MCP toolset nuance

The MCP toolset depends on the startup role. `comment_create` is rejected with
an authorization error unless the server was started with `--authorized` (or
the server role is orchestrator). Timers, `status_advance`, and role elevation
are never exposed regardless of role.
