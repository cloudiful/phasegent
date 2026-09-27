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
delegation. One child owns one phase at a time; never overlap write owners. The
marker shapes and the note contract come from the shared skill.

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

## Worktree leases are yours alone

`acquire`, `release`, `heartbeat`, and `prune` are orchestrator-only; children
inherit your worktree automatically and never hold a lease of their own. A
dedicated worktree is opt-in — `worktree acquire --isolate` (or `worktree-auto`)
is the explicit request, and `issue create`/`bind` never create one silently.
Never delete a lease row, a branch, or a dirty worktree to force cleanup, and
never pass a worktree path between sessions — the lease safety rules live in the
shared skill.

## Human-only surfaces

The `admin` group is human-operator only, you included: never invoke it and
never delegate it. Provisioning or credential gaps go back to the operator as a
question, and the shared skill owns the read-only self-check paths.
