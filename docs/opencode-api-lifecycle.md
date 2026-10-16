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

- **`phasegent worktree prune` reuses the same guards.** It is global and
  removal-by-default (`--dryrun`/`--dry-run` is the read-only preview), and it
  removes a directory as soon as every issue associated with it is confirmed
  closed on the configured provider, no active lease references it, and it is
  Git-clean — with no waiting period (`--stale-days` only affects the explicit
  stale-active recovery). It runs the close/sync candidate protection in `Sync`
  semantics: main checkout, active peer paths, dirty/untracked contents, and
  host-occupied directories are all kept. The provider read is the same
  current-configured-provider resolution used elsewhere; a missing, unavailable,
  ambiguous, or mismatched issue is never treated as closed. Within one
  invocation the read-only scan memoizes a repeated host answer (each
  `session.get` and each paginated listing page, keyed by the request that
  produced it, failures included) so a global pass does not re-ask across
  repositories, while every removal re-checks the host live and keeps the
  directory when the fresh answer differs. A few progress lines stream to
  stderr, flushed before the host and closure checks, so a long scan is visibly
  working; stdout stays reserved for the final text or the single JSON document.
- **Prune's one relocation exception is narrow and user-authorized (issues 918
  P2, 923 P1).** The shared guard still never moves a session, but prune itself
  may relocate an individually proven *associated* child so a child-occupied
  worktree can be cleaned. A child qualifies only when its `parentID` chain (at
  most 8 hops, cycle- and error-guarded) reaches a persisted `ses_` lease session
  for that canonical directory and the child is not itself a lease owner. Its
  execution outcome is never a cleanup criterion: `succeeded`, `failed`,
  `interrupted`, `cancelled`, absent, or unknown all follow the same association
  rule, because outcome is not ownership and carries no filesystem safety.
  Every other occupant — a foreign, unreadable, parentless, cyclic, or over-deep
  session, or a hosted lease owner — keeps the whole directory with zero moves.
  The target is the existing `TargetResolver`'s verified main checkout, never a
  path guessed from the current directory or a parent chain; a failed or merely
  accepted move that a fresh read cannot confirm stops the cleanup with the
  already-moved children left at the safe target (no rollback). Only after every
  planned move is confirmed does prune re-prove vacancy through the raw, uncached
  host and fresh lease rows before removing the directory. A preview performs
  the same read-only discovery and reports the proposed relocations without
  moving or releasing anything.
- **Local guards run before any host read (issue 919 P2).** Prune decides the
  local, Git-only guards — never the main checkout, never a path an `active`
  lease points at, never dirty or untracked — before it consults the host, so a
  main-checkout or dirty directory is excluded without a single host or provider
  request. Reporting is faithful: a removal pass lists exactly the children whose
  move it confirmed, and keeps listing them when a later occupancy check still
  keeps the directory; a preview lists only proposed relocations in a distinct
  field and never labels a proposal actual; and the human failure count scopes to
  failed `git worktree remove` calls, not to kept/protected results, which can
  include unproven host answers.
- **A session on a standalone/private instance is conservatively retained.**
  Normal API discovery does not find it, and a missing session is treated as
  uncertainty rather than absence.
- **Sync is the retry.** Because close never waits for a move, a directory can
  outlive its issue until a later `issue sync` confirms the session has moved.
- Host authority is only as good as the host's read. If the host cannot report a
  session's directory, the conservative answer is to forget the cached target
  and stay put, not to assume the worktree is intact.