---
name: phasegent-workflow
description: Role-aware, provider-backed workflow protocol for phasegent issue/plan work — pick a tracking mode (INLINE/REDMINE_ISSUE/LOCAL_ISSUE), delegate to executor/reviewer/tester, and enforce the marker, VERDICT, note-pointer, and orchestrator-owned timer/status/notify contracts. Load this skill when starting or interpreting a multi-phase phasegent task, when delegating to a subagent, or when a tracking context or Redmine phase audit is involved.
---

# Phasegent Workflow

`phasegent` is a role-aware CLI for provider-backed workflow. `--role` selects the
capability/routing policy; `--provider` selects Forgejo (default), Redmine,
GitLab, or `local`. This SKILL defines the **protocol boundaries** (tracking
modes, delegation contracts, markers, verdict vocabulary, primitives, and who
owns what). `phasegent --help` is the authoritative **syntax** reference for
every command and never duplicates this document.

## When to use this skill

Load it when one of these is true:

- You are about to start or interpret a multi-phase, cross-module, or
  user-visible phasegent task carried on a tracking artifact.
- You are delegating to an `executor`, `reviewer`, or `tester`, or receiving a
  delegation delta from an `orchestrator`.
- A Redmine/local issue or plan is the source of truth and you must decide a
  tracking mode, read the artifact, or publish a phase-terminal audit note.
- You need the marker, note-pointer JSON, or VERDICT vocabulary, or need to
  confirm which primitives a role may call.

## Tracking modes (decision tree)

Pick exactly one mode before work starts; the tracking artifact (issue body or
plan) owns goal, constraints, acceptance criteria, phases, and decisions. A
delegation parent prompt overrides only safety boundaries, the exact allowlist,
the `git restore` allowlist, the attempt/round, and comment authorization.

1. **`INLINE`** — trivial or read-only work, no plan and no issue. The parent
   prompt carries the full context; no artifact read and no audit comment.
   Handled directly or delegated. Result shape: the complete result object.
2. **`REDMINE_ISSUE`** — multi-phase, cross-module, API/schema/migration,
   data/security/concurrency, high-risk, or user-visible work. The Redmine issue
   body is the plan; comments are append-only audits; Forgejo handles code/CI.
   The executor/reviewer/tester publishes exactly one authorized phase-terminal
   audit note. Result shape: the minimal note-pointer JSON.
3. **`LOCAL_ISSUE`** — tracking when the remote workflow provider is unavailable
   or you want an offline, credential-free plan. The plan lives as a **local
   provider issue** (`--provider local`), which replaces `.opencode/plans/*.md`
   markdown. `--provider local` needs no credential and no network and is seeded
   with a default project, issues, comments, and canonical status transitions.
   Read the body with `phasegent --role <role> --provider local issue get <N>`;
   write the plan with `issue create`. Result shape: the complete result object
   (no Redmine comment). Optional audit via a local-issue comment create when
   the orchestrator authorizes a marker.
   - `.opencode/plans/*.md` markdown is only a fallback when **both** the remote
     workflow provider and the local provider are unreachable; record that
     fallback explicitly rather than treating it as the default.
   - Never downgrade to `INLINE` from a qualified tracking mode.

## Roles

The five roles — `admin`, `orchestrator`, `executor`, `reviewer`, `tester` — and
their exact capability matrix (from `src/policy.rs`) are in
[`references/roles.md`](references/roles.md). The two tables you will actually
use day-to-day:

- `orchestrator` is the only role with issue write/search/close, repo create,
  relation write, and is the only non-admin `status set`/`advance` and `timer`
  role. It owns phase ordering, delegation, review, timer/status transitions,
  and final closure.
- `executor` and `reviewer` share the read/comment/project/status/version/
  relation-read surface; `tester` is comment + attachment read/write only;
  `admin` is bootstrap-only.

Role capability is a how-much-can-it-do policy, not who; credentials stay
role-scoped and least-privilege, and a role never invokes another role's
`--role`.

## Marker protocol

A phase-terminal audit note anchors on a single HTML-comment marker at the top
of the body. The unique `marker` value supplied by the parent must appear
**verbatim** as `marker=<unique-marker>`. The same syntax is used for every
child role:

- executor — `<!-- ai-executor issue=<issue> phase=<phase> attempt=<attempt> marker=<unique-marker> -->`
- reviewer — `<!-- ai-reviewer issue=<issue> phase=<phase> round=<round> marker=<unique-marker> -->`
- tester — `<!-- ai-tester issue=<issue> phase=<phase> attempt=<attempt> marker=<marker> -->`

Rules:

- One note per phase-terminal; publish once after all work and immediately before
  the final JSON. A continuation or fresh child must use a **new** marker so the
  previous note is not duplicated.
- The top-level `status` (executor/tester) and `verdict` (reviewer) in the JSON
  must match the corresponding labelled line in the note verbatim.
- Publish with the child's own role key: `phasegent --role <child> comment create
  <ISSUE> --body <BODY> --marker <MARKER> --authorized` (executor, reviewer, and
  tester must pass `--authorized`; the orchestrator does not need it).
- A missing/unpublished note for a role that was told `comment-allowed=true` is
  audit incomplete and forbids a clean verdict/finish.

## Result contracts

- `REDMINE_ISSUE` (with `comment-allowed=true`): publish the note first, then
  return **only** the minimal note-pointer JSON that locates it. No prose, no
  changed-file/validation duplication in the response — the note is the record.

  ```json
  {
    "status": "DONE | PARTIAL | BLOCKED | FAILED",
    "tracking": {
      "mode": "REDMINE_ISSUE",
      "issue": "issue number",
      "comment": "posted | failed",
      "comment_id": "comment id or null",
      "comment_url": "issue URL with the #note-<id> anchor, or null",
      "marker": "exact marker value supplied by the parent",
      "notes": "short failure note when comment=failed, otherwise empty"
    }
  }
  ```

  Never fabricate a comment id, URL, or marker. On `comment=failed` leave
  `comment_id`/`comment_url` null and explain in `notes`.

- `INLINE` / `LOCAL_ISSUE`: return the complete result object with `phase`,
  `summary`, `changed_files`, `validation`, `remaining_work`, `question`
  (required only for `BLOCKED`), `risks`, and the nested `tracking` (`mode`
  `INLINE` or `LOCAL_ISSUE`, `issue` null except when bound to a local issue,
  `comment` `posted|failed|skipped`, `notes`).

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

- Status semantics: `DONE` (all acceptance criteria met), `PARTIAL` (useful work
  done, criteria remain, safe to continue), `BLOCKED` (decision/prerequisite
  missing — state the smallest concrete decision in `question`), `FAILED`
  (execution failed; continuing would mislead).

## Primitives (who may call what)

- **status** — `status list`/`status next` are read-only and available to the
  IssueStatusRead roles; `status set`/`status advance` are orchestrator-only.
  Children never advance statuses.
- **notify** — `phasegent --role <role> notify send --event <EVENT> --title
  <TITLE> [--body <BODY>] [--issue <ID>] [--phase <PHASE>]`. Manual-only,
  never automatic. Available to orchestrator/executor/reviewer/tester (admin
  denied). Events: `completion`, `blocked`, `failure`,
  `interruption_suspected`, `publish_failed`.
- **comment** — `comment create`, `comment get`, `comment find-marker` as per
  the role matrix. `comment create` needs `--authorized` unless the role is
  orchestrator (or server `--authorized` for MCP).
- **mcp** — `phasegent --role <role> mcp serve [--transport stdio|http]
  [--bind 127.0.0.1:3000] [--authorized]`. Tools:
  `capabilities`, `issue_get`, `issue_search`, `status_next`, `comment_create`
  (needs server-side `--authorized` unless orchestrator), `notify_send`.
  `status_advance`, timer start/finish, and role elevation are never exposed.

## Syntax vs boundaries

- `phasegent --role <role> --help <topic> [<command>]` is the authoritative
  syntax source; run it when you are unsure rather than guessing.
- This SKILL is the protocol boundary source: what may be published, the note
  shape, the verdict vocabulary, who owns timers/statuses/closure, and which
  role is responsible for each step.
- Each child calls `phasegent` with its own `--role` and never another role's;
  `--role executor`, `--role reviewer`, `--role tester`,
  `--role orchestrator`, and `--role admin` stay distinct. Use the configured
  provider default and omit `--provider` unless a different provider is needed.
- The orchestrator owns the issue body, phase progression, disposition, timer,
  status transitions, checkpoint/push, tag, and final close; children never
  edit the body, label, close, commit, push, or mutate Git refs.

## Commands

Every command and its role gate / provider surface is in
[`references/contracts.md`](references/contracts.md). It is the detail table;
the body does not expand it.

## Install

Copy this directory tree to `~/.config/opencode/skills/`:

```sh
cp -r skills/phasegent-workflow ~/.config/opencode/skills/
```

The skill then loads by its `name: phasegent-workflow`.
