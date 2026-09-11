---
name: phasegent-workflow
description: Role-aware, provider-backed workflow protocol for phasegent issue/plan work — pick a tracking mode (INLINE/TRACKED_ISSUE/LOCAL_ISSUE), delegate to executor/reviewer/tester, and enforce the marker, VERDICT, note-pointer, and orchestrator-owned timer/status contracts. Provider-neutral: the tracking provider comes from user config or --provider, never assumed by this skill. Load when starting or interpreting a multi-phase task, delegating to a child role, or reading/publishing a phase-terminal audit note.
---

# Phasegent Workflow

`phasegent` is a role-aware CLI for provider-backed workflow. `--role` selects the
capability/routing policy; `--provider` selects the tracking provider
(`forgejo|redmine|gitlab|local`, default from user config). This SKILL defines
**protocol boundaries** only. `phasegent --help` is the authoritative **syntax**
reference and is never duplicated here.

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
config or an explicit `--provider`; this skill never picks one.

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
   replaces `.opencode/plans/*.md` markdown.
   Result shape: always the complete result object; when an audit comment is
   authorized its ids go into `tracking.comment_id`/`comment_url`.

- `.opencode/plans/*.md` markdown is only a fallback when **both** the remote
  provider and the local provider are unreachable; record that fallback
  explicitly.
- Never downgrade to `INLINE` from a qualified tracking mode.

## Roles

Detail matrix (from `src/policy.rs`) is in
[`references/roles.md`](references/roles.md); command gates are in
[`references/contracts.md`](references/contracts.md). Summary:

- `orchestrator` owns issue write/search/close, repo create, relation write,
  `status set`/`advance`, `timer`, `worktree` leases, phase ordering,
  delegation, review, and final closure.
- `executor`/`reviewer` are read/comment plus project/status/version/
  relation-read; `tester` is issue-read plus comment read/find/create plus
  attachment upload; `admin` is bootstrap-only.
- Capability is how-much-can-it-do, not who; credentials stay role-scoped and
  least-privilege, and a role never invokes another role's `--role`.

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
- Publish with the child's own role key: `phasegent --role <child>
  [--provider <p>] comment create <ISSUE> --body-file <PATH> --marker <MARKER>
  --authorized` (children require `--authorized`; orchestrator does not). Omit
  `--provider` on the configured default; `--provider local` is always explicit.
- Default note flow: write the note to a temporary Markdown file and pass it with
  `--body-file`, so long Markdown never goes through the shell; `--body-file` and
  `--body` are mutually exclusive. After a successful publish the CLI deletes the
  file; any read, validation, marker, authorization, or provider failure keeps it
  for diagnosis. Use `--keep-body-file` only when the file must survive success,
  and never rely on it to clean up permanent files.
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

- Orchestrator-only writes: issue body/search/create/close, `status set`/
  `advance`, `timer`, `worktree` leases, repo/relation write. Children never
  edit the body, label, close, commit, push, or mutate refs.
- `status list`/`next` are read-only for IssueStatusRead roles.
- `comment create`/`get`/`find-marker` per the role matrix; non-orchestrator
  `comment create` needs `--authorized` (CLI) or server-side `--authorized`
  (MCP).
- `notify send` is manual-only, never automatic; orchestrator/executor/reviewer/
  tester allowed, admin denied.
- `mcp serve` exposes only the startup role's toolset (`capabilities`,
  `issue_get`, `issue_search`, `status_next`, `comment_create`, `notify_send`);
  `status_advance`, timer start/finish, and role elevation are never exposed.

## Syntax vs boundaries

- `phasegent --role <role> --help <topic> [<command>]` owns syntax; this SKILL
  owns boundaries (what may be published, note shape, verdict vocabulary, who
  owns timer/status/closure).
- Each child uses its own `--role` only. Omit `--provider` on the configured
  default; pass it only to override (and always for `local`).
- The `admin` group (`admin auth setup`, `admin config set/clear`,
  `admin config provider set/clear`, `admin workflow bootstrap`) is
  human-operator only — no AI role ever invokes it, and orchestrator
  never delegates it. Need a credential or setting? Ask the operator.
  Need to check state? Use `config show`, `config provider get`, or
  `doctor`. Never read the SQLite files or call provider REST
  directly: `comment list` and batch `issue get` cover bulk reads.
