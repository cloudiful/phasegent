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

A session is relocated by the CLI — `phasegent issue close` and
`phasegent worktree prune` — through the OpenCode V2 API
(`opencode api session.move`), and only in one situation: the session is
itself still hosted in a worktree that the command is about to remove. The
target is the repository's main checkout, resolved from the Git worktree list
and verified to be an inside-work-tree main checkout; a bare or invalid target
keeps the directory instead of guessing a parent.

Four rules keep this narrow:

- **Only an associated session moves.** `issue close` moves the closing session
  and only the closing session. `worktree prune` moves the `ses_`-leased
  sessions a lease associates with the directory it removes. A session no lease
  of the repository associates is somebody else's: it keeps the directory and
  is never relocated to make room.
- **A lease session without the `ses_` prefix belongs to a plain CLI caller.**
  Such a session is never moved, and `issue close` never reaches the API for
  it.
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
  registry entry, settled and confirmed markers, in-flight move, and cached host
  record are cleared together, so neither the session nor a later sub-agent
  re-enters the closed directory.
- An **unreadable** host is not proof the placement still holds. The directory
  may already be gone, so the cached target is dropped rather than re-entered on
  an unproven assumption — a known-closed worktree is never revived.
- A session that has **not** settled is left alone. A session legitimately
  sitting in the main checkout before its first placement is the ordinary case,
  and reconciling it as a relocation would cancel that first placement.
- A child inherits where its parent *actually* is: for a settled and
  host-confirmed parent that is a fresh host read; a parent whose move was only
  admitted keeps its registered target, so a lagging host answer is never read
  as an external relocation.
- The registry is **exact per session** (issue 919 P1): a session's worktree is
  the one recorded under its own id, and there is no cross-session fallback. An
  unknown session, an unknown root, or a child whose parent cannot be resolved
  stays where it is rather than being moved or redirected into another session's
  worktree. A decided move whose current location the host cannot report fails
  the prompt (prefixed `PLACEMENT_ERROR_PREFIX`) instead of running the turn in
  an unverified directory, and a fresh own-session directory equal to the target
  settles the placement with no move. Each placement decision emits one bounded
  diagnostic line (session, parent, from, to, reason, outcome) on the existing
  console channel; an accepted move is never labelled a confirmation.
  `PHASEGENT_WORKTREE_NO_DISCOVER=1` stays inert: it adds no host read and never
  auto-moves, since a move whose current location cannot be verified must not
  run.

The plugin never spawns the `opencode` client and never performs cleanup; it
uses only the host's in-process session APIs.

## Limits

- **`phasegent worktree prune` uses a running-session guard, not history
  (issue 928 P1).** It is global and removal-by-default
  (`--dryrun`/`--dry-run` is the read-only preview), and it removes a directory as
  soon as every issue associated with it is confirmed closed on the configured
  provider, no active lease references it, and it is Git-clean — with no waiting
  period (`--stale-days` only affects the explicit stale-active recovery). It
  reuses the close/sync *local* guards — main checkout, active peer paths,
  dirty/untracked contents — and adds its own running-session guard: only a
  session the connected shared service reports as currently running in the
  directory blocks removal, so an inactive historical session record that still
  points at the directory never keeps it. The provider read is the same
  current-configured-provider resolution used elsewhere; a missing, unavailable,
  ambiguous, or mismatched issue is never treated as closed. Within one
  invocation the read-only scan memoizes the running-session answer (the
  `session.active` id set and each active id's `session.get` location, failures
  included) so a global pass does not re-ask across repositories, while every
  removal re-checks the running sessions live and keeps the directory when the
  fresh answer differs. A few progress lines stream to
  stderr, flushed before the running-session and closure checks, so a long scan
  is visibly working; stdout stays reserved for the final text or the single JSON
  document.
- **The removal stage returns the associated idle sessions first (issue 1061
  P1).** The read-only scan never enumerates history and never moves a session.
  The removal stage is different: before it deletes a directory it reads that
  directory's fresh occupancy and the direct answer of every associated
  `ses_`-leased session, and it returns each associated *idle* session to the
  repository's verified main checkout, confirming every move with a fresh
  authoritative read. Those checks describe the one connected service at that
  moment, not a guarantee about session changes after them.
  - Only a lease-associated session is moved, and only out of the directory that
    still hosts it. A session that already runs elsewhere is left untouched,
    and a session's content is never modified — an archived session is neither
    excluded nor unarchived.
  - Every direct answer is checked against the session that was asked about
    before it may authorise a move: an answer that names another session, or
    one whose location is not absolute and therefore cannot be read, is not
    evidence about the requested session, and it never authorises a move of
    the id it happens to carry.
  - A session no lease of this repository associates keeps the directory and is
    never moved.
  - An unavailable or incomplete occupancy listing, an unresolvable associated
    session, an answer that names another session or reports an unusable
    location, a rejected, queued, or unconfirmed move, an unverifiable main
    checkout, a running session, and a new occupant all keep the directory with
    a bounded reason.
  - The coverage limit is unchanged: one connected service, not a whole-machine
    multi-service inventory. The fresh checks cannot promise a globally atomic
    snapshot of every service on the machine, so they are no guarantee about
    sessions that move or appear afterwards.
- **Prune's runtime-host dependency is explicit.** A valid running-session answer
  is required even for a pure-CLI candidate: an unsupported, unavailable,
  malformed, mismatched, or non-absolute answer, or an active id the host will not
  describe, keeps the directory rather than reading as vacancy. Coverage is the
  one connected service's foreground drains, not a whole-machine multi-service
  inventory: other processes or background work the service does not represent
  are outside this contract. A historical record that no lease associates is
  never relocated, so resuming such an old session may require explicitly
  choosing a current checkout.
- **A session on a standalone/private instance is conservatively retained.**
  Normal API discovery does not find it, and a missing session is treated as
  uncertainty rather than absence.
- **Sync is the retry.** Because close never waits for a move, a directory can
  outlive its issue until a later `issue sync` confirms the session has moved.
- Host authority is only as good as the host's read. If the host cannot report a
  session's directory, the conservative answer is to forget the cached target
  and stay put, not to assume the worktree is intact.