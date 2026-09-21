---
name: phasegent-workflow
description: Role-aware, provider-backed workflow protocol for phasegent issue/plan work — pick a tracking mode (INLINE/TRACKED_ISSUE/LOCAL_ISSUE), delegate to executor/reviewer/tester, and enforce the marker, VERDICT, note-pointer, and orchestrator-owned timer/status contracts. Provider-neutral — the tracking provider comes from user config and is never assumed by this skill. Load when starting or interpreting a multi-phase task, delegating to a child role, or reading/publishing a phase-terminal audit note.
---

# Phasegent Workflow

`phasegent` is a role-aware CLI for provider-backed workflow. `--role` selects the
capability/routing policy; the tracking provider comes from user config. This
SKILL defines **protocol boundaries** only. `phasegent --help` is the
authoritative **syntax** reference and is never duplicated here.

## OpenCode adaptation

- The `orchestrator` agent is `mode: primary` and loads this skill with
  `use_skill phasegent-workflow`; it dispatches `explore`, `executor`,
  `reviewer`, and `tester` as subagents, and repeatable prompts run via
  `/orchestrate`, `/review`, and `/test`. Agent, skill, command, and plugin
  files load once at startup, so editing any of them needs a new session.
- `phasegent plugin install` writes the worktree adapter to
  `$XDG_CONFIG_HOME/opencode/plugins/phasegent-worktree.js` (project slot:
  `.opencode/plugins/`). That adapter owns the session identity, so nothing has
  to mint or pass a session id by hand. After a session acquires a worktree its
  `tool.execute.before` hook redirects relative file paths and a bare or
  relative shell workdir into it; absolute paths pass through unchanged and the
  `external_directory` permission check is never bypassed.
- The loose plan-markdown fallback lives in `.opencode/plans/*.md`.
- This file in the phasegent repository is the source of truth; chezmoi mirrors
  it to `~/.config/opencode/skills/phasegent-workflow/`.

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

Detail matrix (from `src/policy.rs`) is in
[`references/roles.md`](references/roles.md); command gates are in
[`references/contracts.md`](references/contracts.md). Summary:

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

## Worktree leases

Worktree leases are keyed by `(repo, issue, session)`, and the session identity
must stay stable within one agent session so two concurrent sessions never
collide on one issue. The OpenCode host plugin (`phasegent plugin install`)
requires **OpenCode >= 2.0** and loads as a v2 `export default { id, setup }`
module: `tool.execute.before` redirects relative tool paths, `worktree.transform`
claims the phasegent strategy, `session.move` relocates the session, and
`skill.transform` registers the `phasegent-worktree-v2` skill. No slash command
is registered — the v2 command draft only accepts an Effect-returning `execute`
callback, which a promise plugin cannot build, so `phasegent worktree acquire`
stays the manual path. The v1 plugin contract is no longer supported.

That adapter owns the session identity and injects it automatically, so nothing
has to mint a session id by hand. Two environment variables are the only hard
guarantees, on any host: export a single `PHASEGENT_SESSION_ID` per session and
reuse it for every worktree call on a host without the adapter, and set
`PHASEGENT_WORKTREE_NO_DISCOVER=1` to keep the adapter from running the CLI at
all (no discovery, no acquire, no strategy claim; the skill registration stays
inert metadata). The adapter targets the OpenCode binary's
runtime plugin context, not the npm `@opencode-ai/plugin` type package, which
can lag it (1.18.25 exposes no `tool`, `worktree`, `session`, or `location`); a
missing registration surface degrades to a console warning.

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
- Stale recovery is read-only by default. `worktree prune` reports stale active
  leases and removable worktrees; `worktree prune --release-stale --reason TEXT`
  flips exactly those stale active leases to `retained`, and
  `worktree prune --remove` deletes only clean, expired, retained worktrees.
  Neither action is implied by the other; never guess an owner.
- Never delete lease rows, branches, or a dirty worktree to force cleanup.

`phasegent --help worktree` owns the exact flags for these commands.

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
