---
name: phasegent-worktree-v2
description: phasegent worktree adapter for OpenCode v2 — the PHASEGENT_SESSION_ID and PHASEGENT_WORKTREE_NO_DISCOVER escape hatches, the worktree acquire flow, and the phasegent worktree prune recovery. Load when a session needs its (repo, issue, session) worktree lease, when the adapter is not redirecting tool calls, or when leases must be inspected or released.
---

# phasegent worktree (OpenCode v2 adapter)

The managed adapter installed by `phasegent plugin install` owns the session's
`(repo, issue, session)` worktree lease. It requires **OpenCode >= 2.0** and the
v2 plugin shape `export default { id, setup }`; the v1 plugin contract is
rejected by the v2 module loader.

## What the adapter does

- Registers `tool.execute.before`: relative file paths and a bare or relative
  shell `workdir` are rewritten into the acquired worktree. Absolute paths pass
  through untouched, so the `external_directory` permission check still applies.
- Claims the `worktree.transform` strategy only when the checkout already
  carries a phasegent issue binding; otherwise the host git strategy stays in
  place.
- Moves the session into the acquired worktree with `session.move`, and
  registers this skill through `skill.transform`.
- Degrades gracefully: a missing binding, a failed acquire, or a failed
  `session.move` keeps the original directory, warns, and never blocks a tool
  call.

The adapter registers no slash command. The OpenCode v2 command draft only
accepts `execute` callbacks that return an Effect, which a promise plugin cannot
build, so there is no `/phasegent-worktree-acquire`: use `phasegent worktree
acquire` directly.

The npm `@opencode-ai/plugin` type package can lag the binary it ships with:
`tool`, `worktree`, `session` and `location` are missing from 1.18.25 even
though the binary exposes them. The adapter relies on the binary's runtime
context, not on the type package, so a missing registration surface only warns.

## Environment

- `PHASEGENT_SESSION_ID` — the only hard session guarantee on a host without the
  adapter. Export one value per session and reuse it for every worktree call;
  `worktree acquire --session` resolves the flag, then this variable, then the
  legacy `phasegent` fallback.
- `PHASEGENT_WORKTREE_NO_DISCOVER=1` — keeps the adapter from running the CLI at
  all: no discovery, no acquire, no strategy claim. The skill registration stays
  inert metadata. Paths then stay relative to the session directory.

## Acquire

- Manual: `phasegent --role orchestrator worktree acquire --issue N [--session S]
  --format json`. Idempotent per `(repo, issue, session)`; re-running refreshes
  the heartbeat instead of creating a second lease, and the managed adapter then
  moves the session into the returned path.
- Failure is a warning, never a delete: no branch, lease row, or dirty worktree
  is removed by the adapter.

## Prune and release

- `phasegent worktree prune` reports stale active leases and removable
  worktrees (read-only).
- `phasegent worktree prune --release-stale --reason TEXT` flips exactly the
  stale active leases to `retained`; `--remove` deletes only clean, expired,
  retained worktrees. Neither action implies the other, and an owner is never
  guessed.
- `phasegent --help worktree` owns the exact flags.
