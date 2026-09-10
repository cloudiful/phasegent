# Command contract

Source of truth: `src/cli/help/` role-filter plus `src/policy.rs`.
`phasegent --role <role> --help <topic> [<command>]` is the authoritative
syntax for every command below; no command here is invented. Provider defaults:
Forgejo is the default; `--provider redmine|gitlab|local` selects another.
Credentials are never accepted as CLI values (`auth setup` reads a secure
prompt or `--stdin`).

## Commands and role gates

| Command | Capability / gate | Roles allowed | Provider / surface notes |
|---|---|---|---|
| `issue get` | IssueRead | orchestrator, executor, reviewer, tester | — |
| `issue search` | IssueSearch | orchestrator only | provider-fresh; auto-bootstraps project on no match; scoped local-index fallback on failure |
| `issue create` | IssueCreate | orchestrator only | planning flags Redmine/GitLab; Forgejo rejects every planning flag |
| `issue update-body` | IssueUpdateBody | orchestrator only | tracker/planning flags in same PUT |
| `issue close` | IssueClose | orchestrator only | — |
| `issue upload-attachment` | IssueAttachmentUpload | orchestrator, tester | Redmine-only; file ≤25 MiB; upload token never printed |
| `issue bind` / `issue unbind` / `issue status` | IssueRead | orchestrator, executor, reviewer, tester | local branch–issue binding; no provider/network |
| `comment create` | CommentCreate | orchestrator, executor, reviewer, tester | `--authorized` required unless orchestrator (CLI) |
| `comment get` | CommentRead | orchestrator, executor, reviewer, tester | — |
| `comment find-marker` | CommentFindMarker | orchestrator, executor, reviewer, tester | marker matched verbatim |
| `project list` | ProjectRead | orchestrator, admin, executor, reviewer | Redmine-only; does not need `--project-id` |
| `project create` | ProjectCreate | orchestrator, admin | Redmine-only; requires `--confirm` |
| `status list` | IssueStatusRead | orchestrator, admin, executor, reviewer | Redmine and local; Forgejo/GitLab return not-supported |
| `status next` | IssueStatusRead | orchestrator, admin, executor, reviewer | read-only; current + policy-allowed next + recovery command; Redmine and local |
| `status set` | role == orchestrator | orchestrator only | validated name/id; Redmine, GitLab (workflow label), and local |
| `status advance` | role == orchestrator | orchestrator only | policy preflight; idempotent no-op; Redmine and local |
| `version list` | VersionRead | orchestrator, admin, executor, reviewer | Redmine-only; never auto-bootstraps |
| `relation list` | RelationRead | orchestrator, executor, reviewer | Redmine/GitLab; Forgejo rejects |
| `relation create` | role == orchestrator | orchestrator only | Redmine/GitLab; Forgejo rejects |
| `relation delete` | role == orchestrator | orchestrator only | Redmine/GitLab; Forgejo rejects |
| `timer start/finish/list/get/recover` | role == orchestrator | orchestrator only | local ledger; finish/recover project to Redmine/GitLab (Forgejo rejects); list/get never reach a provider |
| `worktree acquire/release/prune` | role == orchestrator | orchestrator only | per-(repo, issue, session) leases; prune only clean + expired + retained, never deletes a branch or a dirty worktree |
| `worktree status/list` | command-level read gate | orchestrator, executor, reviewer | read-only lease inspection; tester denied |
| `plugin install/status/uninstall` | no role gate | any | local OpenCode adapter file; managed-marker ownership, foreign-file refusal; never touches worktrees or branches |
| `workflow bootstrap` | role == admin | admin only | Redmine-only; needs only the admin key |
| `repo create` | RepoCreate | orchestrator only | Forgejo/GitLab; `--private` required; Redmine/local reject as not-supported |
| `notify send` | role gate | orchestrator, executor, reviewer, tester (admin denied) | manual-only; bounded envelope; never automatic |
| `mcp serve` | startup `--role` | role-scoped toolset | tools: `capabilities`, `issue_get`, `issue_search`, `status_next`, `comment_create` (needs server-side `--authorized` unless orchestrator), `notify_send`; excludes `status_advance`, timer start/finish, role elevation |
| `auth setup` | all roles | admin, orchestrator, executor, reviewer, tester | credentials never a CLI value |
| `config show` / `config provider get·set·clear` | machine-wide | any (no `--role` required) | redacted snapshot; secret values only as presence/length |
| `config set` / `config clear` | global or role-scoped | any; role-scoped settings need `--role` | SQLite only; secret settings require `--stdin` |
| `hooks install` | no role gate | any | local checkout; managed prepare-commit-msg/commit-msg hooks |
| `gui` | none | any | desktop single-binary shell |

## Orchestrator-owned primitives (never client roles)

`timer start` / `timer finish` / `timer recover` and `status set` /
`status advance` are orchestrator-only via manual CLI. Children (executor,
reviewer, tester) never start timers, advance statuses, edit the issue body, or
label/close the issue. `status_advance` and timer operations are never exposed
over MCP.

## MCP toolset nuance

The MCP toolset depends on the startup role. `comment_create` is rejected with
an authorization error unless the server was started with `--authorized` (or
the server role is orchestrator). Timers, `status_advance`, and role elevation
are never exposed regardless of role.
