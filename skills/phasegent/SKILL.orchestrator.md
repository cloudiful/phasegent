---
name: phasegent-orchestrator
description: Orchestrator-side phasegent protocol for tracked phases — pick the tracking mode, own the issue plan, delegate with an issue number plus deltas only, hold status, timer, worktree-lease and closure ownership, and close the phase from the published audit notes. Load it when you own a phase.
---

# Phasegent orchestrator

You own a tracked phase end to end: the plan artifact, the delegation, the
status flow, and the closure. This SKILL carries protocol boundaries only;
`phasegent --help` is the authoritative syntax reference and is never
duplicated here. The provider comes from user config and is never assumed.

## Own the artifact

Pick exactly one tracking mode before work starts:

- `INLINE` — trivial or read-only work. Your prompt carries all context; no
  artifact read and no audit note.
- `TRACKED_ISSUE` (legacy alias `REDMINE_ISSUE`, accept on read, never emit on
  write) — multi-phase, cross-module, API/schema/migration,
  data/security/concurrency, high-risk, or user-visible work. The issue body is
  the plan; comments are append-only audits.
- `LOCAL_ISSUE` — an offline, credential-free plan on the local provider.

A loose plan markdown file is only the fallback when both the remote and the
local provider are unreachable; record that fallback explicitly. Never
downgrade to `INLINE` from a qualified tracking mode.

The issue body owns goal, constraints, acceptance criteria, phases, and
decisions; keep it current with `issue update`. Write the current state only.

## Delegate with an issue number plus deltas

A delegation prompt carries the issue number; the child reads the artifact and
its own role skill for the rest. Add only what the artifact cannot carry:

- the marker, and the attempt or round,
- the exact allowlist and the `git restore` allowlist delta,
- a safety-boundary delta, and comment authorization.

Never restate the plan, the mechanism, the protocol, or the worktree path in a
delegation. One child owns one phase at a time; never overlap write owners.

## Markers the children echo verbatim

- executor — `<!-- ai-executor issue=<n> phase=<phase> attempt=<n> marker=<unique-marker> -->`
- reviewer — `<!-- ai-reviewer issue=<n> phase=<phase> round=<n> marker=<unique-marker> -->`
- tester — `<!-- ai-tester issue=<n> phase=<phase> attempt=<n> marker=<unique-marker> -->`

A retry or fresh child uses a new marker. A missing note when
`comment-allowed=true` is audit-incomplete and forbids a clean finish.

## Results you accept

- `TRACKED_ISSUE` children publish first, then return only the minimal
  note-pointer JSON — `status` (executor/tester) or `verdict` (reviewer),
  `phase`, and the nested `tracking` object with `mode`, `provider`, `issue`,
  `comment`, `comment_id`, `comment_url`, `marker`, `notes`. The note is the
  record; reject prose or changed-file duplication.
- `INLINE` / `LOCAL_ISSUE` children return the complete result object with
  `phase`, `summary`, `changed_files`, `validation`, `remaining_work`,
  `question` (only for `BLOCKED`), `risks`, and the nested `tracking` object.
- Status semantics: `DONE` (criteria met), `PARTIAL` (safe to continue),
  `BLOCKED` (state the smallest concrete decision in `question`), `FAILED`
  (continuing would mislead).
- Reviewer verdicts use exactly one of `PASS` · `FAIL` · `REQUEST_CHANGES` ·
  `BLOCKED` · `AUDIT_FAILED`, matching the note line verbatim;
  `REQUEST_CHANGES` is the legacy alias treated as `FAIL`.

## Status, timer, and closure are yours

- The canonical flow is `New → In Progress → In Review → Resolved → Closed`.
  `Resolved` means AI work is finished and awaits operator verification;
  `Closed` is the verified terminal state. A bare `status transition` takes the
  first allowed next status; resuming implementation is an explicit transition
  back to `In Progress`.
- Status follows the tools automatically: children never call `status *` or
  `timer *`, never edit the body, and never label or close the issue.
- Close at finish; a cross-project close needs `--project-id`. A successful
  close flips this issue's active lease rows to `retained` and runs the guarded
  worktree cleanup (clean, no other active lease, not the main checkout).

## Worktree leases

Leases are keyed by `(repo, issue, session)` and the adapter installed by
`phasegent plugin install` (OpenCode >= 2.0) owns the session identity, so
nothing is minted or passed by hand. A child session inherits its parent's
worktree on its first tool call.

- Relative paths and a bare or relative shell `workdir` land in the worktree;
  absolute paths pass through, so the `external_directory` check still applies.
- Never mint a fresh identity per command or phase, and never pass a worktree
  path between sessions.
- `worktree acquire`/`release`/`heartbeat`/`prune` are yours alone. `prune`
  reports first; `--release-stale --reason TEXT` flips exactly the stale active
  rows, `--remove` deletes only clean, expired, retained worktrees, and neither
  implies the other. Never delete a lease row, a branch, or a dirty worktree to
  force cleanup.
- `release --force` needs a non-empty `--reason`; it is the attributed override
  and rows are never deleted.

## Human-only surfaces

The whole `admin` group (`admin auth setup`, `admin config set/clear`,
`admin config provider set/clear`, `admin workflow bootstrap`) is
human-operator only, you included. Need a credential or setting? Ask the
operator. Check state with `config show`, `config provider get`, or `doctor`,
never by reading the SQLite files or calling provider REST directly:
`comment list` and batch `issue get` cover bulk reads.
