---
name: phasegent
description: Role-aware, provider-backed workflow protocol for phasegent issue/plan work plus the OpenCode worktree adapter — tracking modes (INLINE/TRACKED_ISSUE/LOCAL_ISSUE), executor/reviewer/tester delegation, marker and VERDICT contracts, note-pointer results, and the automatic (repo, issue, session) worktree lease. Provider-neutral — the tracking provider comes from user config and is never assumed; load it when starting, delegating, or publishing a phase-terminal audit note.
---

# Phasegent

`phasegent` is a role-aware CLI for provider-backed workflow. The session role
selects the capability/routing policy; the tracking provider comes from user
config and is never assumed.

This SKILL is the single source of the shared protocol: tracking modes, role
gates, the marker protocol, result contracts, worktree lease safety, and the
human-operator-only `admin` boundary. The role skills
(`phasegent-orchestrator`, `phasegent-executor`, `phasegent-reviewer`,
`phasegent-tester`, and the read-only `phasegent-explore`) carry only their
role-specific always-on rules and defer here for the shared detail.

Commands are not protocol. `phasegent --help <command>` owns flags, usage, and
the provider surface, filtered by the session role: consult `phasegent --help
<command>` only for the command you are about to run, and defer every other
command's help until it is selected. Never copy a flag table into a plan, a
note, or a delegation; a child acts only under its own role and never claims
another.

## OpenCode adaptation

- The `orchestrator` agent is `mode: primary` and loads this skill with
  `use_skill phasegent`; it dispatches `explore`, `executor`, `reviewer`, and
  `tester` as subagents, and repeatable prompts run via `/orchestrate`,
  `/review`, and `/test`. Agent, skill, command, and plugin files load once at
  startup, so editing any of them needs a new session.
- `phasegent plugin install` is this skill's only deployment channel: the
  adapter registers it through `skill.transform` (`id`/`name` `phasegent`, path
  `/builtin/phasegent.md`, body and description embedded from
  `skills/phasegent/SKILL.md`) together with the five slim role skills
  `phasegent-orchestrator`, `phasegent-executor`, `phasegent-reviewer`,
  `phasegent-tester`, and `phasegent-explore` (embedded from
  `skills/phasegent/SKILL.<role>.md`), and
  prepends each protocol agent's role skill to its `system`, so those boundaries
  are always on and every skill is visible on any host the adapter is installed
  on.
- Session and worktree wiring is automatic — the adapter owns the session
  identity, a child session inherits its parent's worktree on its first call,
  and relative paths land there while absolute paths pass through untouched.
  Never run `worktree acquire`, `issue bind`, or `issue create` by hand from a
  child role, and never pass a worktree path between sessions.
- The loose plan-markdown fallback lives in `.opencode/plans/*.md`.

## Research delegation backends

Research is delegated, never inlined, and there are two backends that are not
equivalent. Prefer the phasegent research backend when it is available; fall
back to the native `explore` subagent otherwise. Both are read-only, and the
choice never changes what you are allowed to do — only who does the reading.

- **phasegent research backend** — the `phasegent` MCP server's research tools
  (`research_start`, then `research_wait` or `research_status`, plus
  `research_cancel` and `research_resume`). The caller supplies a user prompt
  and a bounded runtime budget; the server adds a fixed read-only research
  system instruction it owns, so a caller cannot weaken it. One call starts a
  run and returns a run id immediately, so a long read is never one synchronous
  call. Each run executes on its own in a private server-created scratch
  directory that never holds the phasegent repository or a resolved worktree,
  and the run is owned by the delegating session: only that session can read,
  wait for, cancel, or resume it. No issue, worktree, checkout, repository,
  session, or scratch path is named in the request or the result.
- **native backend** — the host's own `task(explore)` subagent with a retained
  `sessionID`. This is a separate native capability, not an equivalent
  phasegent backend: it reads inside the OpenCode session's own directory under
  the host's own read-only tool policy, not in a phasegent scratch sandbox, and
  it never calls the phasegent MCP research tools or any phasegent
  issue/worktree binding. Use it when the MCP server is unregistered, when your
  role's surface has no research tools, or when the delegation is refused, and
  say so in your note.

`phasegent --help mcp` lists the research tools for your role. The research
delegation is available to the orchestrator, executor, and reviewer; `tester`
and `admin` never have it.

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
acceptance criteria, phases, and decisions. A delegation prompt carries the
issue number; children read the artifact and this skill for the rest, and a
delegation adds only what the artifact cannot carry — the marker, the
attempt/round, a safety-boundary or allowlist delta (including the
`git restore` allowlist), and comment authorization. Never restate the
mechanism, the plan, or the worktree path in a delegation. The provider always
comes from user config; this skill never picks one.

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

## Role capability matrix

Source of truth: `src/policy.rs` (`Role::allows`); command-level gates keyed to
a role rather than a capability live in *Command contract*. The five roles are
`admin`, `orchestrator`, `executor`, `reviewer`, `tester`. The session role is
a capability/routing policy, not identity isolation: each role's credential
stays least-privilege and never crosses roles, and status follows the tools
automatically.

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
| Notify | notify send | — | ✓ | ✓ | ✓ | ✓ |
| ProjectRead | project list | ✓ | ✓ | ✓ | ✓ | — |
| ProjectCreate | project create | ✓ | ✓ | — | — | — |
| IssueStatusRead | issue status list | ✓ | ✓ | ✓ | ✓ | — |
| VersionRead | version list | ✓ | ✓ | ✓ | ✓ | — |
| RelationRead | relation list | — | ✓ | ✓ | ✓ | — |
| RelationCreate | relation create | — | ✓ | — | — | — |
| RelationDelete | relation delete | — | ✓ | — | — | — |

### Role notes

- **orchestrator** allows every capability and is the only role with issue
  write/search/close, repo create, relation write, and the only non-admin role
  with the status-transition and `timer` commands.
- **admin** is bootstrap-only — project list/create, status list/next, version
  list, and `workflow bootstrap` — and is never an AI role.
- **executor** and **reviewer** share the read/comment/project/status/version/
  relation-read surface and `notify send`; both are barred from issue
  write/close/search, relation write, repo create, status, and timer.
- **tester** is the independent code-level verification role: issue-read plus
  comment read/find/create, attachment upload, and `notify send`, with the
  `phasegent-tester` role skill owning its test-only write boundary; it never
  sees project, status, version, or relation data.
- Capability entries above are authoritative; command-level gates such as
  `status transition`, `timer *`, and `workflow bootstrap` are keyed to the role,
  not a capability, and are listed in *Command contract*.

## Risk classes and reviewer policy

Every tracked phase carries one risk class, chosen when the phase is planned,
and a `reviewer_policy` derived from it. A risk class is a planning label, never
a new capability: it does not widen a role's allowlist, its write ownership, or
its CLI gates.

- `standard` — reversible, localized work with no data, security, concurrency,
  schema/migration, cross-module interface, or user-visible surface. The policy
  is `final-only`: one independent audit after the phase's work is frozen.
- `high-risk` — data, security, concurrency, schema/migration, a cross-module
  interface, or user-visible behavior. The policy may be `checkpoint-and-final`
  only when the issue plan names the exact checkpoint boundary to review.
- `irreversible` — a step that cannot be undone, such as an applied migration, a
  publish, a deploy, or destructive cleanup. The policy is `checkpoint-and-final`
  with the checkpoint placed before that step.

`final-only` is the default for `standard` work, and a `checkpoint-and-final`
phase always keeps its final audit: a checkpoint review never replaces the final
one. Without a named checkpoint boundary in the issue plan the policy stays
`final-only`. The reviewer's five-token VERDICT vocabulary is unchanged; the
note and the pointer label the review `final` or `checkpoint` beside the same
token.

## Bounded parallelism (serial by default)

Orchestration is serial by default: one write owner per phase, and an executor
and a reviewer never work the same mutable tree at the same time.

- Reviewer and a subsequent executor may overlap only when the reviewer reads an
  immutable revision or snapshot in a separate worktree and the two allowlists
  do not overlap; the shared-worktree flow stays serial.
- Safe overlap is limited to independent read-only recon, a tester observing a
  frozen implementation without writing the executor's allowlist, and live
  acceptance against an immutable deployed revision.
- Overlapping write owners are never allowed, and nothing schedules them
  automatically: any overlap is an explicit orchestrator decision carried in the
  delegation.

## Test disposition and compact evidence

- An executor note declares a test disposition — what it added or updated, or why
  it added none — and its tests are implementation evidence that never
  substitutes for the tester's independent verification.
- A reviewer owns the final static audit and does not repeat the tester report:
  it reports its own confirmed findings with file and line and the bounded
  targeted command it ran, if any.
- Notes stay compact. Evidence supports the verdict or status instead of
  restating logs or duplicating another role's report, and the note remains the
  record under the result contracts.

## Command contract

Source of truth: `src/cli/help/` role-filter plus `src/policy.rs`.
`phasegent --help <command>` is the authoritative syntax for the command in
hand, filtered by the session's role; this section records role gates and
boundaries, never flag tables. The provider resolves from configuration at
runtime — an explicit override wins, then the configured default (role or
global setting, `phasegent.toml`, or environment), with Forgejo as the final
fallback — and a session never hard-codes one.

### Role gates

The canonical status flow is
`New → In Progress → In Review → Resolved → Closed`: `Resolved` means AI work
is finished and awaits the operator's verification, and `Closed` is the
verified terminal state whose guarded cleanup may remove worktrees. A bare
`status transition` takes the first policy-allowed next status, so it walks the
`In Review → Resolved → Closed` chain; resuming implementation after a reviewed
phase is an explicit transition back to `In Progress`.

- Issue body/search/create/close writes, relation and repo writes,
  `status transition`, every `timer *` command, and `worktree` lease writes are
  orchestrator-only. Children never edit the body, label, search, or close an
  issue, never call `status *` or `timer *`, never commit, push, tag, or mutate refs,
  and never claim another role's credential.
- `comment create` writes under the session role: a child's note needs explicit
  authorization (the CLI flag, or server-side authorization for MCP unless the
  server role is orchestrator).
- `issue get` batch-reads up to 20 issues as an `{issues, errors}` envelope, and
  `comment list` is the bulk note read.
- `notify send` is manual-only and never automatic.
- `issue upload-attachment` rejects uniformly as not-supported, before any file,
  network, or credential access.

### Admin group (never AI roles)

The entire `admin` group (`admin auth setup`, `admin config set/clear`,
`admin config provider set/clear`, `admin workflow bootstrap`) is
human-operator only — for every AI role, orchestrator included. Agent
permission rules deny the prefix, no role ever delegates it, and provisioning,
credentials, and settings stay with the operator: a missing one goes back as a
question, and credentials never travel as CLI values. Only when a configuration
or provider problem actually blocks the task do the read-only self-checks come
in — `doctor`, `config show`, and `config provider get` carry no role gate.
Never read the local SQLite files or call provider REST directly, since
`comment list` and batch `issue get` cover bulk reads. Audit notes are
append-only: there is deliberately no comment update/delete command, so publish
a follow-up note instead.

### MCP toolset nuance

`mcp serve` exposes only the startup role's toolset (`capabilities`,
`issue_get`, `issue_search`, `status_next`, `comment_create`, `notify_send`);
status writes (`status transition` stays CLI-only), timers, and role elevation
are never exposed. Clients never supply a role, and `comment_create` needs
server-side authorization unless the server role is orchestrator.

## Worktree leases

Leases are keyed by `(repo, issue, session)` and the adapter installed by
`phasegent plugin install` (OpenCode >= 2.0, v2 `export default { id, setup }`)
owns the session identity and the lease side, so no session id, worktree path,
or lease call travels between sessions. Wiring is automatic: never mint a fresh
session id per command or per phase, and a child session inherits its parent's
worktree on its first tool call. When a session has a known worktree, placement
uses `session.move` before the tool executes; no-worktree and already-placed
sessions continue normally. If the required move is unavailable, fails, or is
still landing at the runner's next step boundary, the `tool.execute.before`
hook throws so OpenCode cancels that invocation instead of running it in the
old checkout; the next invocation retries and proceeds once the host confirms
the session sits in the target. Only ordinary discovery failures keep the
original directory.

Creating a worktree is opt-in: `issue create` and the adapter's lazy path
reuse an existing lease, an inherited worktree, or the current checkout, and a
conflict surfaces the explicit choice instead of a new directory — `phasegent
worktree acquire --issue N --isolate` is how a dedicated worktree is requested.

Boundaries:

- Relative paths and a bare or relative shell `workdir` land in the worktree.
  Absolute paths pass through untouched, so an explicit escape and the
  `external_directory` permission check are never rewritten.
- Never delete a lease row, a branch, or a dirty or untracked worktree to force
  cleanup.
- An acquire against a base ref that does not resolve fails locally before any
  worktree, branch, or lease is written.
- `phasegent worktree prune` reports stale active leases and removable
  worktrees (read-only). Releasing stale leases and removing worktrees are
  separate explicit actions, and removal only ever touches clean, expired,
  retained worktrees; an owner is never guessed.
- Forced release is the attributed override: it requires a reason and the lease
  row stays (rows are never deleted).
- `worktree probe` is read-only: it reports existence,
  Git-worktree/clean/branch/`HEAD`/main-checkout facts and any matching lease
  as bounded JSON, and never calls a provider, writes a lease, syncs, deletes,
  or repairs. No matching lease yields a stable empty result instead of a
  guessed path.
- A successful `issue close` flips this issue's active leases to `retained` and
  then removes a worktree directory only when it is clean, no active lease of
  another session points at it, and it is not the repository's main checkout.
  Branches and lease rows are never deleted, and cleanup never changes the
  close exit code or its stdout. `issue sync` runs the same guards for issues
  the provider already closed.
- Environment: `PHASEGENT_SESSION_ID` is the only hard session guarantee on a
  host without the adapter (one value per session, reused for every worktree
  call), and `PHASEGENT_WORKTREE_NO_DISCOVER=1` keeps the adapter inert beyond
  the in-memory registry.
- `phasegent --help worktree` owns the exact flags for these commands.

## Branch binding lifecycle

Work happens on `<type>/<id>` branches (e.g. `feat/452`) and the durable provider/project-scoped link is the sole branch/issue association; `bind` records that link explicitly and the branch name is the only fallback when the name carries an id and no link resolves. A successful `issue create` reuses or books the current checkout; it never creates a worktree implicitly, and an occupied checkout path surfaces guidance naming `phasegent worktree acquire --issue N --isolate` instead, so an `already_bound` repeat stays an idempotent no-op (see Worktree leases). `issue status` shows the current branch with its compatible single issue (a durable link, else the branch name; only when unambiguous and not the detected default), how it resolved (`linked`/`named`/`none`), the durable linked issues with last-known local-index state/source/indexed time (`unknown` when missing), and the reverse branches of the active issue; `issue branches N` lists every branch linked to issue N in this repository across all provider/project scopes with the same cached state, where same-number rows from distinct scopes stay distinct and set `ambiguous=true`. Both reads are read-only, never call a provider, and never guess (`phasegent --help issue` owns the exact flags).

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
- Publish under the child's own role; the role is implicit, and a child's note
  needs explicit authorization. A LOCAL_ISSUE note uses the local provider
  explicitly, and `phasegent --help comment create` owns the body-file
  lifecycle.
- A reviewer note labels its review `final` or `checkpoint` on a `REVIEW:` line
  beside the unchanged `VERDICT:` line, so a checkpoint round and the final audit
  stay distinguishable without a new token.
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

- A `tester` result reuses that same `status` vocabulary and adds no new verdict
  token; its note carries the explicit test-result evidence — the exact commands
  run, the observed pass/fail outcome, and each behavioral failure's signature.

- A `reviewer` pointer keeps that same minimal shape, with `verdict` instead of
  `status`, and adds a top-level `review` field, `"final"` or `"checkpoint"`,
  matching the note's `REVIEW:` line; the five VERDICT tokens and their note
  `VERDICT:` line are unchanged.

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

- Nested explorer assistance changes no contract: an `explore` child launched by
  the orchestrator, an executor, or a reviewer stays read-only, non-audited, and
  unable to recurse, so it publishes no marker or VERDICT and owns no result. The
  owning executor/reviewer still publishes its own single phase-terminal note
  with any material explorer finding recorded there, and the orchestrator alone
  owns plan, status, timer, worktree, and closure.
- Status semantics: `DONE` (all acceptance criteria met), `PARTIAL` (useful
  work done, criteria remain, safe to continue), `BLOCKED`
  (decision/prerequisite missing — state the smallest concrete decision in
  `question`), `FAILED` (execution failed; continuing would mislead).
