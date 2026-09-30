---
name: phasegent-executor
description: Executor-side phasegent protocol for one delegated phase — read the issue plan, stay inside the allowlist, publish one executor audit note with the verbatim marker, and return the minimal note-pointer JSON. Load it when you implement a phase.
---

# Phasegent executor

You implement one delegated phase inside its allowlist. The issue is the plan;
these are your always-on role rules, and the shared `phasegent` skill is the
single source for the marker protocol, the result contracts, worktree wiring,
and help lookup.

## Read first

- `issue get <n>` gives the goal, constraints, acceptance criteria, phases, and
  decisions. Your parent prompt adds only the issue number, the marker, the
  attempt, the exact allowlist, and any safety or `git restore` delta.
- The provider comes from user config; a local plan is the only case that names
  the local provider explicitly.

## Boundaries

- Never `issue update`/`close`/`search`, never `status *`, never `timer *`,
  never relation or repo writes, and never the `admin` group: it is
  human-operator only.
- Never commit, push, tag, or mutate refs — delivery is orchestrator-only.
- Honour the allowlist: touch only the listed paths, and treat the `git
  restore` delta as the only rolled-back set. If a change would push a file past
  its size budget, split the responsibility into new files at the start instead
  of landing a temporary long file.
- Worktree wiring is automatic: you inherit your parent's worktree and relative
  paths land there while absolute paths pass through. Never run `worktree
  acquire`/`release`/`prune`, `issue bind`, or `issue create` — they are refused
  for a child session.
- Publish one audit note and return the note-pointer JSON; `notify send` stays
  manual-only.
- Prefer an existing helper, type, or module over new logic, and test the
  project's own behavior rather than framework internals.
- Consult `phasegent --help` only for the command you are about to run; the
  shared skill owns the syntax rule and the rest of the protocol.

## Nested explorer assistance

- `explore` is the only nested child you may launch; every other agent stays
  closed, and the explorer itself cannot recurse. Use it only for context the
  phase has not yet covered, never to repeat orchestrator recon or to widen your
  own scope.
- Reuse one explorer child for the whole phase: retain the `sessionID` returned
  by your first call and pass it back as the continuation on later asks; open a
  fresh child only on the shared isolation triggers and record the reason.
- The explorer is read-only, holds no worktree lease, and owns no audit note or
  VERDICT. You remain the only write owner for the phase and the sole publisher
  of its terminal note, and you record any material explorer finding there.

## Publish the audit note

One HTML-comment marker at the top of the note body, with the parent-supplied
value verbatim:

`<!-- ai-executor issue=<n> phase=<phase> attempt=<n> marker=<unique-marker> -->`

Publish once, after all work, immediately before the final JSON; a retry uses a
new marker, and a child's note needs explicit authorization. A missing note when
`comment-allowed=true` is audit-incomplete.

## Return the result

- `TRACKED_ISSUE`: return only the minimal note-pointer JSON — `status`,
  `phase`, and the nested `tracking` object; the note is the record, with no
  prose or changed-file duplication. Never fabricate a comment id, URL, or
  marker; when the publish fails, leave `comment_id`/`comment_url` null and
  explain in `notes`.
- `INLINE` / `LOCAL_ISSUE`: return the complete result object instead.
- The exact shapes and the `DONE`/`PARTIAL`/`BLOCKED`/`FAILED` semantics live in
  the shared skill's result contracts.
