# OpenCode API session lifecycle

Where a session runs is owned by the OpenCode host, not by phasegent. This
document records which side owns which piece of that state, how the two sides
reconcile, and where the current design stops.

## The two owners

| State | Owner | Where it lives |
| --- | --- | --- |
| Which worktree a lease points at | phasegent CLI | local lease table (SQLite) |
| Which directory a session runs in | OpenCode host | `session.location.directory` |
| A session's registered worktree target | phasegent adapter | in-memory registry |
| Lease status and release reason | phasegent CLI | local lease table |

A lease records where a session *was*. It is not a promise that the session is
still there, because the host can relocate a session at any time. Every guard
below exists because that gap is real.

## Moving a session: CLI, never the plugin

A session is relocated by `phasegent issue close`, through the OpenCode V2 API
(`opencode api session.move`), and only in one situation: the closing session is
itself still hosted in a worktree that close is about to remove. The target is
the repository's main checkout, resolved from the Git worktree list and verified
to be an inside-work-tree main checkout; a bare or invalid target keeps the
directory instead of guessing a parent.

Four rules keep this narrow:

- **Only the closing session moves.** Another session sharing the directory
  blocks removal; it is never relocated to make room.
- **A lease session without the `ses_` prefix belongs to a plain CLI caller.**
  Such a worktree keeps its pre-existing behaviour and never reaches the API.
- **The close never waits.** A move request that has not taken effect leaves the
  directory in place, and `issue sync` removes it later once the host reports the
  session elsewhere. There is no polling loop.
- **The close's stdout and exit code never change.** Cleanup stays best-effort
  and reports through stderr warnings.

## Occupancy evidence must be complete

Before removing an OpenCode-associated directory, the CLI enumerates the
sessions the host reports for it and requires every associated lease session to
be provably elsewhere. Anything short of complete evidence keeps the directory:

- an unreachable or failed API call;
- a listing whose cursor could not be followed to exhaustion, or a repeated
  cursor;
- invalid JSON, a missing `location`, or a session the selected instance does
  not own — that is uncertainty, not evidence of absence;
- a move that was accepted but has not yet taken effect.

Report mode (`issue sync --no-clean`) contacts the host not at all and reports
every OpenCode-associated directory as `would_keep`.

## The plugin reconciles; it never owns location

The adapter installed by `phasegent plugin install` places a session once per
session, during prompt admission, and after that its registry is a cache that
the host outranks.

- Placement stays a plugin responsibility. A registered, inherited, or leased
  worktree is entered via `session.move`, and a move the host cannot perform
  fails the prompt closed so the turn never runs in the old checkout.
- Before a *settled* placement's cached target is reused, the plugin reads the
  session's directory from the host again. That read is never answered from
  `sessionInfo` — the cache exists to keep ordinary inheritance off the host,
  and answering "is my placement still valid?" from it is the stale answer that
  would send a session back into a removed worktree.
- A settled session the host now reports elsewhere is not moved back. Its
  registry entry, settled marker, in-flight move, cached host record, and the
  shared fallback are cleared together, so neither the session nor a later
  sub-agent re-enters the closed directory.
- An **unreadable** host is not proof the placement still holds. The directory
  may already be gone, so the cached target is dropped rather than re-entered on
  an unproven assumption — a known-closed worktree is never revived.
- A session that has **not** settled is left alone. A session legitimately
  sitting in the main checkout before its first placement is the ordinary case,
  and reconciling it as a relocation would cancel that first placement.
- A child inherits where its parent *actually* is: for a settled parent that is
  a fresh host read, not the parent's registered target.
- The shared remembered-worktree fallback is checked against the converged-lease
  closed marker before it is handed out, so a fallback that outlived its lease
  does not place a new sub-agent in a closed directory. Open inheritance and
  fail-closed placement errors are unchanged, as are absolute-path pass-through
  and `PHASEGENT_WORKTREE_NO_DISCOVER=1` inertness.

The plugin never spawns the `opencode` client and never performs cleanup; it
uses only the host's in-process session APIs.

## Limits

- **`phasegent worktree prune` is a separate, explicit removal path.** The
  close/sync guards above do not cover it; it does not inherit this protection.
- **A session on a standalone/private instance is conservatively retained.**
  Normal API discovery does not find it, and a missing session is treated as
  uncertainty rather than absence.
- **Sync is the retry.** Because close never waits for a move, a directory can
  outlive its issue until a later `issue sync` confirms the session has moved.
- Host authority is only as good as the host's read. If the host cannot report a
  session's directory, the conservative answer is to forget the cached target
  and stay put, not to assume the worktree is intact.