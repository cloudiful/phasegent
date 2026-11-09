---
name: phasegent
description: Phasegent protocol routing — load the shared protocol and the reference for the active role before tracked phase work, delegation, review, or recon. Provider-neutral; the tracking provider comes from user config.
---

# Phasegent

`phasegent` is a role-aware CLI for provider-backed workflow. This skill is the
single entry point; the full protocol and role rules live in real files next to
this one and load on demand.

## Load first

- `references/protocol.md` — shared protocol: tracking modes, role gates, the
  record and marker contracts, result shapes, worktree lease rules, and the
  human-operator-only `admin` boundary.
- `references/orchestrator.md` — orchestrator ownership, delegation, delivery,
  PR, and issue-closure policy.
- `references/executor.md` — one delegated phase.
- `references/reviewer.md` — one independent review round and the VERDICT.
- `references/explore.md` — read-only recon.

Load the role reference that matches the active agent; do not load every role
document. If the references are missing, run `phasegent skill install` from a
managed project shell (or `phasegent skill install --project` for the current
checkout) and retry.

## Core rules

- The issue or plan artifact is the source of truth; a delegation carries only
  the issue number plus deltas.
- The tracking provider comes from user config and is never assumed.
- Agent identity comes from the host broker; never treat `PHASEGENT_ROLE`, a
  prompt, or a command flag as identity.
- `phasegent --help <command>` owns flags for the command you are about to run;
  do not copy flag tables into plans or notes.
- Never commit, push, tag, mutate refs, or create/update/merge a pull request
  unless the orchestrator policy and the task explicitly authorize delivery.
