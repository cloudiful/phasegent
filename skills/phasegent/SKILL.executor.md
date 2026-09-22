---
name: phasegent-executor
description: Executor-side phasegent protocol for one delegated phase — read the issue plan, stay inside the allowlist, publish one executor audit note with the verbatim marker, and return the minimal note-pointer JSON. Load it when you implement a phase.
---

# Phasegent executor

You implement one delegated phase inside its allowlist. The issue is the plan;
this SKILL is your whole protocol surface, and `phasegent --help` is the
authoritative syntax reference.

## Read first

- `issue get <n>` gives the goal, constraints, acceptance criteria, phases, and
  decisions. Your parent prompt adds only the issue number, the marker, the
  attempt, the exact allowlist, and any safety or `git restore` delta.
- The provider comes from user config; pass `--provider local` only for a local
  plan.

## Boundaries

- Never `issue update`/`close`/`search`, never `status *`, never `timer *`,
  never relation or repo writes, and never the `admin` group.
- Never commit, push, tag, or mutate refs — delivery is orchestrator-only.
- Honour the allowlist: touch only the listed paths, and treat the
  `git restore` delta as the only rolled-back set. If a change would push a
  file past its size budget, split the responsibility into new files at the
  start instead of landing a temporary long file.
- Worktree wiring is automatic: a child session inherits its parent's worktree
  and relative paths land there while absolute paths pass through. Never run
  `worktree acquire`/`release`/`prune`, `issue bind`, or `issue create` — they
  are refused for a child session.
- `notify send` is manual-only, never automatic.
- Prefer an existing helper, type, or module over new logic, and test the
  project's own behavior rather than framework internals.

## Publish the audit note

One HTML-comment marker at the top of the note body, with the parent-supplied
value verbatim:

`<!-- ai-executor issue=<n> phase=<phase> attempt=<n> marker=<unique-marker> -->`

Publish once, after all work, immediately before the final JSON:

`phasegent comment create <ISSUE> --marker <MARKER> --authorized`

Children need `--authorized`; the session supplies the role. Pass the body with
`--body` or a one-shot `--body-file` (mutually exclusive). A retry uses a new
marker, and a missing note when `comment-allowed=true` is audit-incomplete.

## Return the result

- `TRACKED_ISSUE`: publish first, then return only the minimal note-pointer
  JSON — the note is the record, with no prose or changed-file duplication:

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

  The top-level `status` must match the note's labelled line verbatim. Never
  fabricate a comment id, URL, or marker; on `comment=failed` leave
  `comment_id`/`comment_url` null and explain in `notes`.
- `INLINE` / `LOCAL_ISSUE`: return the complete result object with `phase`,
  `summary`, `changed_files`, `validation`, `remaining_work`, `question`
  (required only for `BLOCKED`), `risks`, and nested `tracking`.
- `DONE` means every acceptance criterion is met; `PARTIAL` means useful work
  remains safe to continue; `BLOCKED` needs the smallest concrete decision in
  `question`; `FAILED` means continuing would mislead.
