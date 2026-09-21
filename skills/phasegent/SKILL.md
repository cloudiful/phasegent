---
name: phasegent
description: Role-aware, provider-backed workflow protocol for phasegent issue/plan work plus the OpenCode worktree adapter — pick a tracking mode (INLINE/TRACKED_ISSUE/LOCAL_ISSUE), delegate to executor/reviewer/tester, enforce the marker, VERDICT, note-pointer, and orchestrator-owned timer/status contracts, and run the (repo, issue, session) worktree lease through the adapter's session identity, agent-role injection, path redirect, and prune recovery. Provider-neutral — the tracking provider comes from user config and is never assumed by this skill. Load when starting or interpreting a multi-phase task, delegating to a child role, reading or publishing a phase-terminal audit note, when a session needs its worktree lease, when the adapter is not redirecting tool calls, or when leases must be inspected or released.
---

# Phasegent

`phasegent` is a role-aware CLI for provider-backed workflow. `--role` selects the
capability/routing policy; the tracking provider comes from user config. This
SKILL defines **protocol boundaries** only. `phasegent --help` is the
authoritative **syntax** reference and is never duplicated here.

## OpenCode adaptation

- The `orchestrator` agent is `mode: primary` and loads this skill with
  `use_skill phasegent`; it dispatches `explore`, `executor`, `reviewer`, and
  `tester` as subagents, and repeatable prompts run via `/orchestrate`,
  `/review`, and `/test`. Agent, skill, command, and plugin files load once at
  startup, so editing any of them needs a new session.
- `phasegent plugin install` writes the worktree adapter to
  `$XDG_CONFIG_HOME/opencode/plugins/phasegent-worktree.js` (project slot:
  `.opencode/plugins/`). The adapter owns the session identity, so nothing has
  to mint or pass a session id by hand. After a session acquires a worktree its
  `tool.execute.before` hook redirects relative file paths and a bare or
  relative shell workdir into it; absolute paths pass through unchanged and the
  `external_directory` permission check is never bypassed.
- The same adapter registers this skill through `skill.transform` (`id`/`name`
  `phasegent`, path `/builtin/phasegent.md`, body and description embedded from
  `skills/phasegent/SKILL.md`), so the plugin install is the skill's only
  deployment channel and the skill is visible on any host the adapter is
  installed on.
- The loose plan-markdown fallback lives in `.opencode/plans/*.md`.

## When to use this skill

Load it when one of these is true:

- You are starting or interpreting a multi-phase, cross-module, or user-visible
  task carried on a tracking artifact.
- You are delegating to an `executor`, `reviewer`, or `tester`, or receiving a
  delegation delta from an `orchestrator`.
- A tracking issue or plan is the source of truth and you must decide a tracking
  mode, read the artifact, or publish a phase-terminal audit note.
- You need the marker, note-pointer JSON, or VERDICT vocabulary, or need to
  confirm which primitives a role may call.
- A session needs its `(repo, issue, session)` worktree lease, the adapter is not
  redirecting tool calls, or leases must be inspected or released.

## Tracking modes (decision tree)

Pick exactly one before work starts; the artifact owns goal, constraints,
acceptance criteria, phases, and decisions. A delegation parent prompt overrides
only safety boundaries, the exact allowlist, the `git restore` allowlist, the
attempt/round, and comment authorization. The provider always comes from user
config; this skill never picks one.

1. **`INLINE`** — trivial or read-only work, no plan and no issue. The parent
   prompt carries the full context; no artifact read and no audit comment.
   Result shape: the complete result object.
2. **`TRACKED_ISSUE`** (legacy alias `REDMINE_ISSUE`, accept on read, never emit
   on write) — multi-phase, cross-module, API/schema/migration,
   data/security/concurrency, high-risk, or user-visible work. The issue body on
   the configured tracking provider is the plan; comments are append-only audits.
   The child publishes exactly one authorized phase-terminal audit note.
   Result shape: the minimal note-pointer JSON.
3. **`LOCAL_ISSUE`** — tracking when the remote provider is unavailable or you
   want an offline, credential-free plan. The plan lives as a **local provider
   issue** (`--provider local`, explicit, no credential, no network), which
   replaces loose plan markdown files.
   Result shape: always the complete result object; when an audit comment is
   authorized its ids go into `tracking.comment_id`/`comment_url`.

- A loose plan markdown file is only a fallback when **both** the remote provider
  and the local provider are unreachable; record that fallback explicitly.
- Never downgrade to `INLINE` from a qualified tracking mode.

## Roles

The role capability matrix below is the human-readable mirror of `src/policy.rs`
(`Role::allows`); command-level gates live in *Command contract*. Summary:

- `orchestrator` owns issue write/search/close, repo create, relation write,
  `status transition`, `timer`, `worktree` leases, phase ordering,
  delegation, review, and final closure. Status follows the tools
  automatically — child roles never call status; the orchestrator closes the
  issue at finish.
- `executor`/`reviewer` are read/comment plus project/status/version/
  relation-read; `tester` is issue-read plus comment read/find/create plus
  attachment upload; `admin` is bootstrap-only.
- Capability is how-much-can-it-do, not who; credentials stay role-scoped and
  least-privilege, and a role never claims another's.

## Role capability matrix

Source of truth: `src/policy.rs` (`Role::allows`). The five roles are
`admin`, `orchestrator`, `executor`, `reviewer`, `tester`. `--role` is a
capability/routing policy, not identity isolation; each role's credential stays
least-privilege and never crosses roles.

Legend: `✓` allowed, `—` denied.

| Capability | Operation | admin | orchestrator | executor | reviewer | tester |
|---|---|---|---|---|---|---|
| IssueRead | issue read | — | ✓ | ✓ | ✓ | ✓ |
| IssueSearch | issue search | — | ✓ | — | — | — |
| IssueCreate | issue create | — | ✓ | — | — | — |
| IssueUpdateBody | issue update | — | ✓ | — | — | — |
| IssueClose | issue close | — | ✓ | — | — | — |
| IssueAttachmentUpload | issue upload-attachment | — | ✓ | — | — | ✓ |
| RepoCreate | repo create | — | ✓ | — | — | — |
| CommentCreate | comment create | — | ✓ | ✓ | ✓ | ✓ |
| CommentRead | comment get | — | ✓ | ✓ | ✓ | ✓ |
| CommentFindMarker | comment find-marker | — | ✓ | ✓ | ✓ | ✓ |
| ProjectRead | project list | ✓ | ✓ | ✓ | ✓ | — |
| ProjectCreate | project create | ✓ | ✓ | — | — | — |
| IssueStatusRead | issue status list | ✓ | ✓ | ✓ | ✓ | — |
| VersionRead | version list | ✓ | ✓ | ✓ | ✓ | — |
| RelationRead | relation list | — | ✓ | ✓ | ✓ | — |
| RelationCreate | relation create | — | ✓ | — | — | — |
| RelationDelete | relation delete | — | ✓ | — | — | — |

### Role notes

- **orchestrator** allows every capability, and is the only role with issue
  write/search/close, repo create, relation write, and the only non-admin
  status-transition and `timer` role (status flow is automatic; command-level
  gates live in *Command contract*).
- **admin** is bootstrap-only: project list/create, status list/next, version
  list, and `workflow bootstrap`.
- **executor** and **reviewer** share the read/comment/project/status/version/
  relation-read surface; executor alone can write to its own audit note, but
  both are barred from issue write/close/search, relation write, repo create,
  and timer; status flows automatically (command-level gates in
  *Command contract*).
- **tester** is comment + attachment read/write surface only: issue read,
  comment read/find/create, and attachment upload. It never sees project,
  status, version, or relation data.
- Capability-level entries above are authoritative; command-level gates such as
  `status transition`, `timer *`, and `workflow bootstrap` are keyed
  to the role, not a capability, so they are listed in *Command contract*.

## Command contract

Source of truth: `src/cli/help/` role-filter plus `src/policy.rs`.
`phasegent --help <topic> [<command>]` is the authoritative syntax for every
command below, filtered by the session's role; no command here is invented. The
provider is resolved from configuration: an explicit `--provider` wins, otherwise
the configured default (role or global setting, `phasegent.toml`, or environment)
applies, and Forgejo is the final fallback. This section records role gates, not
flag tables. Credentials are never accepted as CLI values (`admin auth setup`
reads a secure prompt or `--stdin`).

Provisioning lives under the human-operator `admin` group (`admin auth
setup`, `admin config set/clear`, `admin config provider set/clear`,
`admin workflow bootstrap`). AI roles must never invoke the `admin`
token; agent permission rules deny that single prefix.

### Commands and role gates

| Command | Capability / gate | Roles allowed | Provider / surface notes |
|---|---|---|---|
| `issue get` | IssueRead | orchestrator, executor, reviewer, tester | one number returns the single-issue object; 2–20 return an `{issues, errors}` envelope (exit 1 unless every fetch succeeds) |
| `doctor` | none (read-only self-check) | any (no `--role` required) | credential presence (fingerprint, never values), index backend state, masked PG URL; approved replacement for schema dumps and raw setting reads |
| `issue search` | IssueSearch | orchestrator only | provider-fresh; auto-bootstraps project on no match; scoped local-index fallback on failure |
| `issue create` | IssueCreate | orchestrator only | planning flags Redmine/GitLab; Forgejo rejects every planning flag |
| `issue update` | IssueUpdateBody | orchestrator only | tracker/planning flags in same PUT |
| `issue close` | IssueClose | orchestrator only | orchestrator closes at finish; auto-climbs to the closed status; cross-project close needs `--project-id` |
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
| `status set` | role == orchestrator | orchestrator only | validated name/id; Redmine, GitLab (managed workflow label), and local |
| `status advance` | role == orchestrator | orchestrator only | policy preflight; idempotent no-op on the same status; Redmine and local |
| `status transition` | role == orchestrator | orchestrator only | preferred status write; `--to`/`--status` names the target, bare auto-routes to the first allowed next; Redmine and local |
| `version list` | VersionRead | orchestrator, admin, executor, reviewer | Redmine (native) and GitLab (GET /projects/:id/milestones); local returns an empty catalogue (no versions table); Forgejo returns not-supported; never auto-bootstraps |
| `relation list` | RelationRead | orchestrator, executor, reviewer | Redmine/GitLab; Forgejo and local reject (no relation surface) |
| `relation create` | role == orchestrator | orchestrator only | Redmine/GitLab; Forgejo and local reject; Phase 3 lifecycle helper auto-creates a `relates` link on `issue create --parent-issue <ID>` (idempotent, bounded warning on failure) |
| `relation delete` | role == orchestrator | orchestrator only | Redmine/GitLab; Forgejo and local reject |
| `timer start/finish/list/get/recover` | role == orchestrator | orchestrator only | local ledger; finish/recover project to Redmine/GitLab (Forgejo rejects); list/get never reach a provider |
| `worktree acquire/release/heartbeat/prune` | role == orchestrator | orchestrator only | per-(repo, issue, session) leases; `prune` is read-only unless `--release-stale --reason TEXT` or `--remove` is given, removes only clean + expired + retained worktrees, and never deletes a branch or a dirty worktree; `release --force` requires non-empty `--reason`, persisted on the row and visible in status/list (rows are never deleted) |
| `worktree status/list` | command-level read gate | orchestrator, executor, reviewer | read-only lease inspection; tester denied |
| `plugin install/status/uninstall` | no role gate | any | worktree adapter for the OpenCode host (`$XDG_CONFIG_HOME/opencode/plugins/`, project slot `.opencode/plugins/`); managed-marker ownership, foreign-file refusal; never touches worktrees or branches |
| `admin workflow bootstrap` | role == admin | admin only | Redmine-only; needs only the admin key |
| `repo create` | RepoCreate | orchestrator only | Forgejo/GitLab; `--private` required; Redmine/local reject as not-supported |
| `notify send` | role gate | orchestrator, executor, reviewer, tester (admin denied) | manual-only; bounded envelope; never automatic |
| `mcp serve` | startup `--role` | role-scoped toolset | tools: `capabilities`, `issue_get`, `issue_search`, `status_next`, `comment_create` (needs server-side `--authorized` unless orchestrator), `notify_send`; excludes status writes (`status transition` is CLI-only), timer start/finish, role elevation |
| `admin auth setup` | all roles | admin, orchestrator, executor, reviewer, tester | credentials never a CLI value |
| `config show` / `config provider get` | machine-wide | any (no `--role` required) | redacted snapshot; secrets as presence/length/fingerprint, never values |
| `admin config set` / `admin config clear` | global or role-scoped | any; role-scoped settings need `--role` | SQLite only; secret settings require `--stdin` |
| `admin config provider set` / `admin config provider clear` | machine-wide | any (no `--role` required) | SQLite only |
| `hooks install` | no role gate | any | local checkout; managed prepare-commit-msg/commit-msg hooks |
| `gui` | none | any | desktop single-binary shell |

### Orchestrator-owned primitives (never client roles)

`timer start` / `timer finish` / `timer recover` and `status transition` are
orchestrator-only via manual CLI. Children (executor, reviewer, tester) never
touch timers or status, edit the issue body, or label/close the issue.
`status transition` and timer operations are never exposed over MCP.

### Admin group (never AI roles)

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

### MCP toolset nuance

The MCP toolset depends on the startup role. `comment_create` is rejected with
an authorization error unless the server was started with `--authorized` (or
the server role is orchestrator). Status writes stay CLI-only (`status transition`
has no MCP tool), as do timers and role elevation — never exposed regardless
of role.

## Worktree leases

Worktree leases are keyed by `(repo, issue, session)`, and the session identity
must stay stable within one agent session so two concurrent sessions never
collide on one issue. The managed adapter installed by `phasegent plugin
install` owns the lease side of a session: it requires **OpenCode >= 2.0** and
loads as a v2 `export default { id, setup }` module; the v1 plugin contract is
rejected by the v2 module loader.

What the adapter does:

- Registers `tool.execute.before`: relative file paths and a bare or relative
  shell `workdir` are rewritten into the acquired worktree. Absolute paths pass
  through untouched, so the `external_directory` permission check still applies.
- Rewrites the shell's `phasegent` invocations before they run: the session role
  is injected, a claimed orchestrator/admin role is downgraded, `--session` is
  appended to an `issue create|bind` segment, and those two commands are refused
  outside an orchestrator session. See *Agent role injection*.
- Claims the `worktree.transform` strategy only when the checkout already
  carries a phasegent issue binding; otherwise the host git strategy stays in
  place, and a failed acquire falls back to a plain git worktree.
- Moves the session into the acquired worktree with `session.move`, and
  registers the `phasegent` skill through `skill.transform`.
- Degrades gracefully: a missing binding, a failed acquire, or a failed
  `session.move` keeps the original directory, warns, and never blocks a tool
  call.

Rules:

- Never mint a fresh session id per command or per phase. A successful
  `issue create` and an `issue bind` that changes the binding auto-acquire a
  worktree (best-effort stderr warning only) when the checkout conflicts with
  another lease, so later tool calls land there; a repeated bind reports
  `already_bound` and does not acquire again. The adapter's lazy discovery in
  `tool.execute.before` stays as the idempotent fallback.
- `issue bind` and `issue unbind` are orchestrator-only writes: an explicit
  non-orchestrator `--role` is refused with a structured `permission` error,
  while a role-less call (Git hooks, manual repair, legacy scripts) keeps the
  historical passthrough. The read-only `issue status` stays unrestricted.
- Role resolution at the CLI is `--role` first, then the `PHASEGENT_ROLE`
  environment variable: an explicit flag always wins, a blank value means "no
  role", and a non-empty invalid value is an error rather than a silent
  role-less run. The managed adapter resolves the role from the session's agent
  name (`explore` counts as `reviewer`), injects it into every shell `phasegent`
  invocation, injects nothing for an unknown agent, and downgrades a sub-agent
  that claims `orchestrator`/`admin` by flag or by `PHASEGENT_ROLE=` back to its
  own role. Sub-agent sessions cannot run `issue create`/`issue bind` at all.
- Never delete lease rows, branches, or a dirty worktree to force cleanup.

`phasegent --help worktree` owns the exact flags for these commands.

### Agent role injection

The adapter reads the agent name from the hook event and never asks the model to
type `--role` by hand.

- `orchestrator`, `executor`, `reviewer` and `tester` resolve to their own role
  and `explore` resolves to `reviewer`; the agent name is matched
  case-insensitively against those hints. The role is injected right after the
  `phasegent` token unless the segment already carries `--role`.
- An unknown agent name injects nothing: the adapter never guesses a role, and a
  session with no agent name is treated as role-less.
- A sub-agent session — any resolved role other than `orchestrator` — that
  claims `--role orchestrator`, `--role admin`, `PHASEGENT_ROLE=orchestrator` or
  `PHASEGENT_ROLE=admin` is downgraded to the session's own role and warned
  about. Only code spans are rewritten, so the same text inside a quoted value
  stays byte-for-byte.
- A sub-agent session running `issue create` or `issue bind` is refused: the
  whole command is replaced by a stub that prints the orchestrator-only hint on
  stderr and exits non-zero. Both commands are orchestrator-only.
- `--session` is appended at the end of the `issue create`/`issue bind` segment
  (after any trailing redirection, before the `;`, `&`, `|` or newline
  separator) and only when that segment carries no `--session` yet. No other
  command is touched.
- Segmentation and flag detection are quote-aware: a separator or a flag inside
  single or double quotes is data, and a segment with an unterminated quote is
  left byte-for-byte. A rewrite therefore requires a `phasegent` invocation at
  the start of a segment, optionally behind env assignments or a `path/` prefix,
  with the command name as a whole word — a mention such as
  `grep -rn phasegent src` or a quoted path is never rewritten.

### Relative path redirect

- A confirmed `session.move` already placed the session's working directory in
  the worktree, so relative paths resolve there on their own and the adapter
  stops rewriting `path`/`workdir` for the rest of the session. The call that
  triggers the move still runs in the old directory and is redirected.
- When the host exposes no `session.move`, the move fails, or it has not run
  yet, per-tool path rewriting stays the fallback. Command rewriting is
  independent of it and always runs, with or without a worktree.

### Environment

- `PHASEGENT_SESSION_ID` — the only hard session guarantee on a host without the
  adapter. Export one value per session and reuse it for every worktree call;
  `worktree acquire --session` resolves the flag, then this variable, then the
  legacy `phasegent` fallback.
- `PHASEGENT_ROLE` — the CLI-level role fallback for a host outside the adapter
  (scripts, wrappers, Git hooks). It is consulted only when no `--role` flag is
  present, so an explicit flag always wins; a blank value means "no role" while
  a non-empty invalid value is an error rather than a silent role-less run.
- `PHASEGENT_WORKTREE_NO_DISCOVER=1` — keeps the adapter from running the CLI at
  all: no discovery, no acquire, no strategy claim. The skill registration stays
  inert metadata. Paths then stay relative to the session directory.

### Acquire

- Manual: `phasegent worktree acquire --issue N [--session S] --format json`
  (orchestrator-only). Idempotent per `(repo, issue, session)`; re-running
  refreshes the heartbeat instead of creating a second lease, and the managed
  adapter then moves the session into the returned path.
- Failure is a warning, never a delete: no branch, lease row, or dirty worktree
  is removed by the adapter.

### Prune and release

- `phasegent worktree prune` reports stale active leases and removable
  worktrees (read-only).
- `phasegent worktree prune --release-stale --reason TEXT` flips exactly the
  stale active leases to `retained`; `--remove` deletes only clean, expired,
  retained worktrees. Neither action implies the other, and an owner is never
  guessed.
- `phasegent --help worktree` owns the exact flags.

## Branch binding lifecycle

Work happens on `<type>/<id>` branches (e.g. `feat/452`) and `bind` is only a
fallback repair when the name cannot resolve. A successful `issue create`
auto-acquires a worktree when the checkout conflicts with another lease; a `bind`
that changes the binding does the same, and an `already_bound` repeat is an
idempotent no-op (see Worktree leases).

## Marker protocol

One HTML-comment marker at the top of the note body; the parent-supplied value
appears **verbatim** as `marker=<unique-marker>`:

- executor — `<!-- ai-executor issue=<n> phase=<phase> attempt=<n> marker=<unique-marker> -->`
- reviewer — `<!-- ai-reviewer issue=<n> phase=<phase> round=<n> marker=<unique-marker> -->`
- tester — `<!-- ai-tester issue=<n> phase=<phase> attempt=<n> marker=<unique-marker> -->`

Rules:

- One note per phase-terminal; publish once after all work, immediately before
  the final JSON. A retry or fresh child uses a **new** marker.
- The JSON top-level `status` (executor/tester) or `verdict` (reviewer) must
  match the note's labelled line verbatim.
- Publish under the child's own role: `phasegent comment create <ISSUE>
  --marker <MARKER> --authorized` (children require `--authorized`; orchestrator
  does not). The role is implicit — a managed session supplies it, and any other
  host exports `PHASEGENT_ROLE` once per session. Pass the note body with
  `--body` or a one-shot `--body-file` (mutually exclusive); always pass
  `--provider local` for the local provider. `phasegent --help comment create`
  owns the body-file lifecycle and cleanup flags.
- A missing note when `comment-allowed=true` is audit-incomplete and forbids a
  clean finish.

## Result contracts

- `TRACKED_ISSUE` (with `comment-allowed=true`): publish first, then return
  **only** the minimal note-pointer JSON — the note is the record, no prose or
  changed-file/validation duplication:

  ```json
  {
    "status": "DONE | PARTIAL | BLOCKED | FAILED",
    "phase": "phase id",
    "tracking": {
      "mode": "TRACKED_ISSUE",
      "provider": "configured provider name",
      "issue": 123,
      "comment": "posted | failed",
      "comment_id": 456,
      "comment_url": "issue URL with comment anchor, or null",
      "marker": "exact marker value supplied by the parent",
      "notes": "short failure note when comment=failed, otherwise empty"
    }
  }
  ```

  Never fabricate a comment id, URL, or marker. On `comment=failed` leave
  `comment_id`/`comment_url` null and explain in `notes`.

- `INLINE` / `LOCAL_ISSUE`: return the complete result object with `phase`,
  `summary`, `changed_files`, `validation`, `remaining_work`, `question`
  (required only for `BLOCKED`), `risks`, and nested `tracking` (`mode`
  `INLINE` or `LOCAL_ISSUE`, `provider` when bound, `issue` number or null,
  `comment` `posted|failed|skipped`, `comment_id`/`comment_url`, `marker`,
  `notes`).

- **VERDICT vocabulary** (reviewer only). Use exactly one of these five
  case-sensitive tokens on the note's `VERDICT:` line and in the JSON `verdict`;
  the two must match verbatim:

  `PASS` · `FAIL` · `REQUEST_CHANGES` · `BLOCKED` · `AUDIT_FAILED`

  - `PASS` — no confirmed P0-P2 (P3 nits may exist).
  - `FAIL` — at least one confirmed P0-P2; blocks the phase until repaired
    (`REQUEST_CHANGES` is the legacy alias the orchestrator treats as `FAIL`).
  - `BLOCKED` — review cannot complete (missing context/tooling/artifact).
  - `AUDIT_FAILED` — the mandatory `ai-reviewer` comment could not be published.
  - `APPROVE`, `ACCEPT`, `OK`, `LGTM`, etc. are protocol violations; reselect a
    token from the vocabulary.

- Status semantics: `DONE` (all acceptance criteria met), `PARTIAL` (useful
  work done, criteria remain, safe to continue), `BLOCKED`
  (decision/prerequisite missing — state the smallest concrete decision in
  `question`), `FAILED` (execution failed; continuing would mislead).

## Primitives (who may call what)

- Orchestrator-only writes: issue body/search/create/close, `status transition`,
  `timer`, `worktree` leases, repo/relation write. Status follows the tools
  automatically — children never call status; the orchestrator closes the
  issue at finish (cross-project close needs `--project-id`). Children never
  edit the body, label, close, commit, push, or mutate refs.
- `status list`/`next` are read-only for IssueStatusRead roles.
- `comment create`/`get`/`find-marker` per the role matrix; non-orchestrator
  `comment create` needs `--authorized` (CLI) or server-side `--authorized`
  (MCP).
- `notify send` is manual-only, never automatic; orchestrator/executor/reviewer/
  tester allowed, admin denied.
- `mcp serve` exposes only the startup role's toolset (`capabilities`,
  `issue_get`, `issue_search`, `status_next`, `comment_create`, `notify_send`);
  `status transition`, timer start/finish, and role elevation stay CLI-only
  and are never exposed.

## Syntax vs boundaries

- `phasegent --help <topic> [<command>]` owns syntax, and its output reflects
  the session's role; this SKILL owns boundaries (what may be published, note
  shape, verdict vocabulary, who owns timer/status/closure).
- Recommended commands: `issue update` (issue body/planning writes) and
  `worktree prune` (stale-lease recovery and worktree cleanup), each with its
  current flags; `phasegent --help` carries the flag tables this SKILL never
  reproduces.
- A child acts only under its own role and never claims another.
- The `admin` group (`admin auth setup`, `admin config set/clear`,
  `admin config provider set/clear`, `admin workflow bootstrap`) is
  human-operator only — no AI role ever invokes it, and orchestrator
  never delegates it. Need a credential or setting? Ask the operator.
  Need to check state? Use `config show`, `config provider get`, or
  `doctor`. Never read the SQLite files or call provider REST
  directly: `comment list` and batch `issue get` cover bulk reads.
