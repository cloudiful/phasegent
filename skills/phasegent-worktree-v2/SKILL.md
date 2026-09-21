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
- Rewrites the shell's `phasegent` invocations before they run: the session role
  travels as a `PHASEGENT_ROLE=<role>` assignment prefixed to the invocation, a
  claimed orchestrator/admin role is rewritten to the session's own role,
  `--session` is appended to an `issue create|bind` segment, and those two
  commands are refused outside an orchestrator session. See *Agent role
  injection*.
- Claims the `worktree.transform` strategy only when the checkout already
  carries a phasegent issue binding; otherwise the host git strategy stays in
  place.
- Registers the `/phasegent-acquire` command: it reports this session's
  worktree, acquiring the lease for the branch-bound issue when it is not
  claimed yet. Failures warn instead of throwing.
- Moves the session into the acquired worktree with `session.move`, and
  registers this skill through `skill.transform`.
- Degrades gracefully: a missing binding, a failed acquire, or a failed
  `session.move` keeps the original directory, warns, and never blocks a tool
  call.

The adapter registers both the embedded skill and `/phasegent-acquire` through
their `transform` drafts, and only through `add`: a draft surface without `add`,
or a rejection from it, warns and leaves the rest of the adapter — path
redirection included — in place, because a throw inside a transform callback
disables the whole plugin.

The npm `@opencode-ai/plugin` type package can lag the binary it ships with:
`tool`, `worktree`, `session` and `location` are missing from 1.18.25 even
though the binary exposes them. The adapter relies on the binary's runtime
context, not on the type package, so a missing registration surface only warns.

## Agent role injection

The adapter reads the agent name from the hook event and never asks the model to
type `--role` by hand.

- `orchestrator`, `executor`, `reviewer` and `tester` resolve to their own role
  and `explore` resolves to `reviewer`; the agent name is matched
  case-insensitively against those hints. The role travels to the CLI as a
  `PHASEGENT_ROLE=<role>` assignment prefixed to the invocation, which is the
  CLI's environment fallback; an explicit `--role` flag in the segment stays
  untouched and wins. A segment that already assigns that same role is left as
  it is, while a blank or different assignment still gets the prefix, which wins
  because it comes last.
- An unknown agent name injects nothing: the adapter never guesses a role, and a
  session with no agent name is treated as role-less.
- A sub-agent session — any resolved role other than `orchestrator` — that
  claims `--role orchestrator`, `--role admin`, `PHASEGENT_ROLE=orchestrator` or
  `PHASEGENT_ROLE=admin` never keeps that role: a claimed flag is rewritten in
  place, an elevated `PHASEGENT_ROLE` value is rewritten to the session's own
  role, and the session is warned about the takeover.
- The elevated-value rewrite is independent of invocation recognition, so it also
  covers a claim behind a wrapper (`env …`, `/usr/bin/env …`, `time …`), a
  continued line, or a here-doc body handed to a shell, and it reads a value with
  its quotes and surrounding padding removed.
- A sub-agent session running `issue create` or `issue bind` is refused: the
  whole command is replaced by a stub that prints the orchestrator-only hint on
  stderr and exits non-zero. Both commands are orchestrator-only.
- `--session` is appended at the end of the `issue create`/`issue bind` segment
  (after any trailing redirection, before the `;`, `&`, `|` or newline
  separator) and only when that segment carries no `--session` yet. No other
  command is touched.
- Segmentation and flag detection are quote-aware: a separator or a flag inside
  single or double quotes is data, and a segment with an unterminated quote is
  left byte-for-byte. A rewrite therefore requires a `phasegent` invocation at
  the start of a segment, optionally behind env assignments or a `path/` prefix,
  with the command name as a whole word — a mention such as
  `grep -rn phasegent src` or a quoted path is never rewritten.

## Relative path redirect

- A confirmed `session.move` already placed the session's working directory in
  the worktree, so relative paths resolve there on their own and the adapter
  stops rewriting `path`/`workdir` for the rest of the session. The call that
  triggers the move still runs in the old directory and is redirected.
- When the host exposes no `session.move`, the move fails, or it has not run
  yet, per-tool path rewriting stays the fallback. Command rewriting is
  independent of it and always runs, with or without a worktree.

## Environment

- `PHASEGENT_SESSION_ID` — the only hard session guarantee on a host without the
  adapter. Export one value per session and reuse it for every worktree call;
  `worktree acquire --session` resolves the flag, then this variable, then the
  legacy `phasegent` fallback.
- `PHASEGENT_ROLE` — the CLI-level role fallback the adapter prefixes to every
  shell invocation, and the fallback for a host outside the adapter (scripts,
  wrappers, Git hooks). It is consulted only when no `--role` flag is present, so
  an explicit flag always wins; a blank value means "no role" while a non-empty
  invalid value is an error rather than a silent role-less run.
- `PHASEGENT_WORKTREE_NO_DISCOVER=1` — keeps the adapter from running the CLI at
  all: no discovery, no acquire, no strategy claim. The skill and command
  registrations stay inert metadata. Paths then stay relative to the session
  directory.

## Acquire

- `/phasegent-acquire` reports this session's worktree: it reuses the worktree
  the adapter already claimed, otherwise acquires the lease for the branch-bound
  issue, moves the session into the returned path, and reports that path on the
  adapter's console channel. A missing binding or a failed acquire only warns.
- Manual: `phasegent worktree acquire --issue N [--session S] --format json`
  (orchestrator-only). Idempotent per `(repo, issue, session)`; re-running
  refreshes the heartbeat instead of creating a second lease, and the managed
  adapter then moves the session into the returned path.
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
