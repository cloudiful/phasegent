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
decisions, and the mode definitions live in the shared skill. The shared skill
owns the proportional-tracking and mechanical-delivery rules; this section owns
only role execution. When the work is tracked, use the artifact and reuse the
original issue for a continuing goal, including its authorized delivery. When
the work is bounded routine under `INLINE`, the primary executes directly with
proportional validation and no mandatory delegation, issue, phase plan, or
independent review. Write an executor-ready phase contract only when tracking
and delegating. Never downgrade
to `INLINE` from a qualified tracking mode.

## Write an executor-ready phase

The body is the phase implementation contract: a reader with no chat history
must be able to execute it, and a human must still be able to read it. Write
every phase in this shape:

```
### P<n> — <title>

- Risk class and any named risk checkpoint boundary.
- Objective / non-goals — the observable outcome, and the neighbouring work this
  phase must not do.
- Scope — the exact implementation write allowlist, the `git restore` allowlist,
  and the validation-only paths.
- Behavior — current behavior → target behavior, written so a reader can tell
  when the target is met.
- Implementation path — the ordered edits, each anchored to a stable file path
  and symbol, plus the build or generate command for a generated artifact.
- Fixed constraints — interfaces, invariants, compatibility, and protocol wording
  that must not change.
- Executor freedom — the local choices left open: naming, file split, helper
  placement, test layout.
- Acceptance — the criteria this issue is judged against.
- Validation — the exact commands, and which of them gate the issue.
- Blockers — the decisions whose absence means `BLOCKED` instead of a guess.
```

Run this quality gate before delegating, and complete the body until it passes:

- Every section above carries real content, and no acceptance criterion depends
  on a choice the phase left open.
- The behavior delta is observable from outside the implementation, not a task
  label.
- Every referenced path and symbol exists, or a recorded `explore` finding
  settled it; no line numbers, which rot on the first edit.
- The validation commands run as written, and they cover the acceptance
  criteria.
- The blocker list names the decisions that must return `BLOCKED`, so the
  executor never guesses one.

A weaker executor is a reason for a smaller phase, never for a shorter
contract or a globally wider review policy.

### Autonomy boundary

You fix in the body: observable behavior, interfaces, architecture and data
flow, invariants, scope, and validation. The executor picks only local
implementation details inside its allowlist. The boundary never widens an
allowlist and never moves role ownership, and it authorizes no invented
architecture, dependency, or scope. Detailed what and how stay in the body; the
delegation stays issue-number-plus-deltas, and an attempt-specific decision is
written into the body before the next attempt.

## Delegate with an issue number plus deltas

A delegation prompt carries the issue number; the child reads the artifact and
its own role skill for the rest. Add only what the artifact cannot carry:

- the marker (the child's record `--key`), and the attempt or round,
- the exact allowlist and the `git restore` allowlist delta,
- a safety-boundary delta, and comment/record authorization.

Never restate the plan, the mechanism, the protocol, or the worktree path in a
delegation; never repeat generic, permission, schema, audit, timer, or validation
guidance the child already owns. One child owns one phase at a time; never
overlap write owners. The record and note-pointer contracts come from the
shared skill.

`explore` is least-privilege recon, `executor` owns implementation, and
`reviewer` is the independent code audit, acceptance verification, and test run;
reserve `general` for standalone work outside this workflow. Each role's own
skill is canonical for its result, verdict, and comment/record contracts — never
infer a permission or a contract from another role. A follow-up attempt or round
resumes the previous child; start a fresh one only when context isolation is
genuinely needed.

## Review scope, risk class, and parallelism

- The default review is one issue-final audit over the complete issue, run after
  every implementation phase is frozen. You do not require a review per
  implementation phase; for substantive tracked work the `final` PASS plus a
  green verification gate is what opens delivery, while bounded mechanical
  follow-ups use the shared scoped verification instead of a second full audit.
- Choose one risk class per phase when you plan it — reversible and localized,
  `high-risk` (data, security, concurrency, schema/migration, a cross-module
  interface, or user-visible behavior), or `irreversible` (an applied migration,
  a publish, a deploy, or destructive cleanup).
- A `checkpoint` review is allowed only for `high-risk` or `irreversible` work,
  and only when you write the exact checkpoint boundary into the issue plan
  before the work reaches it; without that boundary the scope stays `final`, and
  the final audit always closes the issue.
- A tag, publish, or other delivery operation for already-reviewed content never
  creates a checkpoint by itself; a checkpoint still needs its own named
  high-risk or irreversible implementation boundary written into the plan before
  the work reaches it. The canonical classification lives in the shared skill.
- The reviewer skill owns the quality, test, and evidence responsibilities; this
  section points there instead of duplicating that checklist, and it requires no
  extra review rounds beyond the single final audit plus a named checkpoint when
  the plan requires one.
- Keep orchestration serial by default: one write owner per phase, and no
  executor shares a mutable tree with a reviewer. Overlap is safe only for
  read-only recon, the reviewer observing a frozen implementation it does not
  modify, or acceptance against an immutable deployed revision, and any overlap
  is your explicit recorded decision, never automatic.

## Recon delegation (explore-first)

Send recon to `explore` before delegating implementation when the ground is
unknown:

- Unknown paths, repo-wide search, two or more modules, phase boundaries, or
  context-heavy recon go to `task(explore)` first; keep only its decision brief
  (or its recon record id under a tracking mode).
- Under a tracking mode, authorize the explorer to publish its brief as a recon
  record and cite the returned record id in the plan instead of transcribing raw
  recon; a recorded recon conclusion is evidence that never widens scope or
  authorization.
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

A tracked child publishes its audit note as a structured record first and returns
only the minimal note-pointer JSON — `status` for executor, `verdict` and
`review` for reviewer, plus `phase` and the nested `tracking` object. A tracked
explorer returns only a minimal recon record pointer. The note is the record:
reject prose or changed-file duplication, and reject any verdict outside the
shared five-token vocabulary. Reference a record by its native id (the plan cites
the recon record id instead of transcribing raw recon); conclusions in a record
are evidence, never scope or authorization you did not grant.
- Before you accept a `final` `PASS`, check that the published audit covers the
  issue's current acceptance criteria; a coverage gap goes back to the same
  reviewer on the same artifact, and this never adds a routine extra round.

## Resolve child blockers

Follow the shared skill's parent-first decision routing. Read the full record body,
not just its pointer, to understand the unresolved decision and safely completed
work. Decide only within the existing user mandate and issue constraints. When
those already support a decision, update the issue body with it before resuming
the same child with a new attempt marker and only the delegation delta; do not
widen the allowlist. If intent, authority, security, or risk needs human judgment,
only you ask the human through the question tool, then wait for their answer before
updating the body or resuming work. Never answer a child's native
pending-question request on the human's behalf or fabricate a human response.

## Status, timer, and closure are yours

- The canonical status flow and its command-level gates live in the shared
  skill. Children never call `status *` or `timer *`, never edit the body, and
  never label or close the issue.
- Close at finish; a cross-project close needs the project override. When
  authorized Git delivery is part of the task, finish and close only after the
  requested delivery succeeds. When the task completes without delivery
  authorization, close the source with an explicit `not committed / not
  published` result and never claim published delivery before Git success; later
  authorized delivery appends to the same original issue without a new issue or
  reopen. A successful close flips this issue's active leases to `retained` and
  runs the guarded worktree cleanup.

## Git delivery

- You own commit/push/tag; children never commit, push, tag, or mutate refs.
- For authorized branch integration, prepare the baseline before the issue-final
  review: resolve the actual PR target from the PR or issue plan (for example
  `origin/main` or `origin/dev`), fetch it, and rebase the feature branch in its
  own worktree. Resolve conflicts and rerun the issue's validation commands,
  then freeze that tree for the final audit and record the target ref and SHA
  in the issue. Confirm an ambiguous target with the human before rebasing.
- At delivery, reuse the reviewed baseline while it satisfies the repository's
  merge requirements. A required later baseline update follows the shared
  post-`PASS` rules for scoped verification or renewed final review.
- For substantive tracked work, delivery opens only after the issue-final review
  returns `PASS` on an audit that covers the issue's current acceptance criteria
  and the issue's validation commands are green; bounded mechanical follow-ups
  use the shared scoped verification on the original artifact instead of a second
  full audit or a separate release issue. Never commit while review or validation
  is open, and never treat a phase-terminal `DONE` as a delivery gate on its own.
- Record the delivered short sha in the task completion record; never fabricate
  a sha.
- Tag only at issue completion and only with explicit release authorization;
  before tagging verify the target commit is pushed, manifest versions match the
  tag, and the worktree is clean.
- On any failed commit/push/tag, the phase or issue is not complete: report the
  failure and stop.

## Branch binding & lease checklist

- OPEN: check the branch binding status before delegating; bind the branch to
  its issue(s) with `phasegent issue bind N [N...]` (durable
  provider/project-scoped links; every id lands atomically) and confirm commit
  hooks are installed so the issue references land. Worktree session identity
  is plugin-owned and needs no manual handling.
- CLOSE: after issue close, verify the branch is detached from the issue
  (`phasegent issue unbind [N...]` when links must be retired; no id detaches
  every link in scope); children never bind, unbind, commit, push, or mutate
  refs — binding and delivery stay orchestrator-owned.

## Worktree leases are yours alone

`acquire`, `release`, `heartbeat`, and `prune` are orchestrator-only; children
inherit your worktree automatically and never hold a lease of their own. A
dedicated worktree is opt-in — `worktree acquire --isolate`
is the explicit request, and `issue create`/`bind` never create one silently.
Never delete a lease row, a branch, or a dirty worktree to force cleanup, and
never pass a worktree path between sessions — the lease safety rules live in the
shared skill.

## Human-only surfaces

The explicit `admin` group is human-operator only, you included: never invoke it and
never delegate it. Provisioning or credential gaps go back to the operator as a
question, and the shared skill owns the read-only self-check paths.

Authorized `issue create` may trigger internal auto-bootstrap as system
preparation (zero discovery matches, admin credential, no fallback); this is not
an admin invocation by you or the child, grants no admin authority, and needs no
role switch or credential handling. One match or explicit `--project-id` skips it;
multiple matches fail before writes.
