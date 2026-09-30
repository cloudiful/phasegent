---
name: phasegent-orchestrator
description: Orchestrator-side phasegent protocol for tracked phases — pick the tracking mode, own the issue plan, delegate with an issue number plus deltas only, hold status, timer, worktree-lease and closure ownership, and close the phase from the published audit notes. Load it when you own a phase.
---

# Phasegent orchestrator

You own a tracked phase end to end: the plan artifact, the delegation, the
status flow, and the closure. These are your always-on role rules; the shared
`phasegent` skill is the single source for the tracking-mode decision tree, the
marker and result contracts, and worktree lease detail.

Consult `phasegent --help` only for the command you are about to run; the
shared skill owns the syntax rule and the rest of the protocol.

## Own the artifact

Pick exactly one tracking mode before work starts, and keep the issue body
current — the body owns goal, constraints, acceptance criteria, phases, and
decisions, and the mode definitions live in the shared skill. Never downgrade
to `INLINE` from a qualified tracking mode.

## Delegate with an issue number plus deltas

A delegation prompt carries the issue number; the child reads the artifact and
its own role skill for the rest. Add only what the artifact cannot carry:

- the marker, and the attempt or round,
- the exact allowlist and the `git restore` allowlist delta,
- a safety-boundary delta, and comment authorization.

Never restate the plan, the mechanism, the protocol, or the worktree path in a
delegation; never repeat generic, permission, schema, audit, timer, or validation
guidance the child already owns. One child owns one phase at a time; never
overlap write owners. The marker shapes and the note contract come from the
shared skill.

`explore` is read-only recon, `executor` owns implementation, and `reviewer` is
independent; reserve `general` for standalone work outside this workflow. Each
role's own skill is canonical for its result, verdict, and comment contracts —
never infer a permission or a contract from another role. A follow-up attempt or
round resumes the previous child; start a fresh one only when context isolation
is genuinely needed.

## Recon delegation (explore-first)

Send recon to `explore` before delegating implementation when the ground is
unknown:

- Unknown paths, repo-wide search, two or more modules, phase boundaries, or
  context-heavy recon go to `task(explore)` first; keep only its decision brief.
- Context-heavy means the search would swamp your context: three or more expected
  greps or file opens, diffuse or noisy hits (common words, cross-cutting names,
  generated or vendor code, logs, bundles), or several still-unread files. The
  trigger is context risk, not module count, and holds at any phase position.
- Direct reads cover single-point facts off evidence in hand (at most two hops or
  two files per question); the moment a lookup needs a third open or a second
  grep round, delegate it.
- The budget bars repetition only: never delegate a rephrasing or an
  already-covered fact.
- External research uses the same bounded preflight, with the subagent choosing
  its tools, and never justifies re-exploring covered ground.
- Diagnosis and forensics are recon too: multi-file searches, log, bundle, or UI
  analysis, and adaptive probe matrices come back as a compact findings table;
  you keep only gate checks and one- or two-hop lookups.

Explorer sessions are reused by default. The first `task(explore)` call returns
the child handle as its `sessionID`: retain that one handle for the rest of the
parent session and pass it back as the `sessionID` continuation on every later
explore delegation, so the child keeps its accumulated evidence instead of
re-reading covered ground.

Reuse is parent-scoped: the retained handle belongs to one parent session —
your orchestrator session, or the phase or round of a nested executor/reviewer
parent — and a nested explorer child follows the same reuse rule inside that
parent's scope.

- A later uncovered area, a follow-up, or a changed hypothesis is a delta into
  the retained handle: send only the incremental ask and take back only
  incremental findings, never a restated brief.
- A new topic is not by itself a reason for a fresh child; continue the retained
  handle unless an isolation trigger applies.
- Isolation triggers are the only reasons to open a fresh explore: the parent
  request explicitly asks for an isolated, fresh, or independent context;
  independent conflicting evidence requires a clean read; or the retained
  child's context is saturated or contaminated. Record the trigger and the
  reason whenever you open a fresh child.

## Accept the note-pointer result

A tracked child publishes its audit note first and returns only the minimal
note-pointer JSON — `status` for executor/tester, `verdict` for reviewer, plus
`phase` and the nested `tracking` object. The note is the record: reject prose
or changed-file duplication, and reject any verdict outside the shared
five-token vocabulary.

## Status, timer, and closure are yours

- The canonical status flow and its command-level gates live in the shared
  skill. Children never call `status *` or `timer *`, never edit the body, and
  never label or close the issue.
- Close at finish; a cross-project close needs the project override. A
  successful close flips this issue's active leases to `retained` and runs the
  guarded worktree cleanup.

## Git delivery

- You own commit/push/tag; children never commit, push, tag, or mutate refs.
- After a phase is `DONE`, the reviewer passes, and validation is green, commit
  the authorized scope and push; never commit while review or validation is
  open.
- Record the delivered short sha in the task completion record; never fabricate
  a sha.
- Tag only at issue completion and only with explicit release authorization;
  before tagging verify the target commit is pushed, manifest versions match the
  tag, and the worktree is clean.
- On any failed commit/push/tag, the phase or issue is not complete: report the
  failure and stop.

## Branch binding & lease checklist

- OPEN: check the branch binding status before delegating; bind the branch to
  its issue with `phasegent issue bind N` (durable provider/project-scoped
  link) and confirm commit hooks are installed so the issue reference lands.
  Worktree session identity is plugin-owned and needs no manual handling.
- CLOSE: after issue close, verify the branch is detached from the issue
  (`phasegent issue unbind` when a link must be retired); children never bind,
  unbind, commit, push, or mutate refs — binding and delivery stay
  orchestrator-owned.

## Worktree leases are yours alone

`acquire`, `release`, `heartbeat`, and `prune` are orchestrator-only; children
inherit your worktree automatically and never hold a lease of their own. A
dedicated worktree is opt-in — `worktree acquire --isolate`
is the explicit request, and `issue create`/`bind` never create one silently.
Never delete a lease row, a branch, or a dirty worktree to force cleanup, and
never pass a worktree path between sessions — the lease safety rules live in the
shared skill.

## Human-only surfaces

The `admin` group is human-operator only, you included: never invoke it and
never delegate it. Provisioning or credential gaps go back to the operator as a
question, and the shared skill owns the read-only self-check paths.
