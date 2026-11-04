# Worktree reconciliation and cleanup

## Synchronous reconciliation

Authorized Operator/orchestrator `worktree list` and `worktree acquire` callers
run a complete repository-scoped sync before their command continues.
`--no-sync` skips it. Other read roles cannot trigger lease release or removal
by listing leases. A sync failure is a bounded warning for this implicit pass;
a direct `issue sync` reports a failure as an error.

Page flags affect only the subsequent list query, not reconciliation. Sync and
prune always consider complete maintenance collections; they do not accept
user page limits. Prune never runs implicit sync, and its `--no-sync` spelling
is a compatibility no-op.

Repository identity, candidate grouping, the lease store and provider setup are
reused within one invocation. Cleanup reads leases again after release, and
list reads its rows after reconciliation finishes. There is no persistent or
cross-command cache.

## Sync host reads

Sync may reuse successful OpenCode session lookups and listing pages within
its invocation. It never moves a session. Every candidate directory gets a
separate cache-use window:

- A keep verdict remains conservative, including uncertainty or host errors.
- A would-be-free verdict that used any cached answer is checked again through
  the same occupancy predicate with direct, uncached host reads before removal.
- A verdict based entirely on fresh reads is not queried a second time.
- Errors are not cached by the sync memo.

Cached absence or an old lease/session association cannot authorize removal.
Close and read-only report paths use their direct behavior rather than this
sync memo. Report mode does not probe associated historical occupancy or move
sessions.

## Close branch restoration

A successful `issue close` may also return the repository's verified main
checkout to the branch it was on before the automatic issue-branch switch. The
baseline is captured before that switch and keyed by repository identity,
provider/project/issue scope and canonical main-checkout path; an issue without
a matching baseline is a no-op.

Restoration runs only after the remote close, lease release and guarded worktree
cleanup succeed. It requires the main checkout to be clean (untracked files
count as dirty), on the captured issue branch or the already-restored baseline,
with no active repository lease, no other hosted OpenCode session, and a
verifiable closing session identity. Pending capture rows are recovered only
from the captured issue branch; an unchanged baseline becomes `aborted` and any
other state is skipped with a warning. A third-party branch change is never
overridden, and a captured local branch that no longer exists is skipped with a
warning rather than recreated from a matching remote-tracking namesake. If the
verified main checkout cannot be resolved while recorded baseline evidence
exists for the known repository and provider/project scope, close emits a
bounded restoration warning; an absent baseline remains a quiet no-op, and an
unknown scope never searches another scope.

The captured local branch is restored without force, stash, reset, clean,
branch deletion or merge commit. When that branch has a configured non-local
remote upstream, the close fetches only that branch ref without tags, verifies
that the local head is an ancestor, rechecks the guards, and fast-forwards. A
missing or unsupported upstream restores only and marks the update skipped;
local ahead/diverged, unavailable remote/ref, failed fetch or refused
fast-forward preserves the branch and commits and marks the update failed.
Branch restoration and upstream update are recorded independently: repeating
`issue close` retries only an incomplete update, and a completed or
no-upstream restoration is a no-op without another fetch. This is best-effort
and deliberately not atomic against arbitrary external Git clients; it does not
claim a global lock, and restoration problems never reopen or fail a successful
remote close. `issue sync` and `worktree prune` do not restore branches.

## Prune scan and removal

Prune's scan is read-only. It groups lease rows by canonical directory and
reuses canonical path answers and issue/link setup within that scan. Its host
scan reads the connected service's running-session set and session locations;
it neither enumerates historical sessions nor moves them. Unknown or unusable
answers keep the directory.

Each actual removal has a new verification window, independent of scan memo:

1. Read current lease rows and rebuild the target's full issue/session set.
2. Resolve paths again; a recorded alias that now names a different directory
   keeps the candidate.
3. Invalidate the target issue closures only. Unrelated issue answers and
   repository setup remain reusable.
4. Recheck active leases, main-checkout identity, dirty/untracked files and
   running sessions with current evidence, then require every target issue to
   be confirmed closed through fresh branch-link and provider reads.
5. Read fresh complete occupancy and the lineage the host reports. Only the
   idle members of the directory's lease family — the sessions its leases name
   and the descendants whose bounded `parentID` chain reaches them — may be
   returned to the verified main checkout. A foreign occupant is never moved
   and keeps the directory.
6. Check running state immediately before every request and again afterwards.
   Every safe member of the lease family is requested once before any wait; an
   immediately pending request does not suppress a later sibling. The first read
   after each request is immediate and typed pending (old location), confirmed
   (destination), or unsafe (stop new requests and waiting without rollback).
   Only then does an actual prune start one invocation-shared 3-second wait
   phase (100 ms round-robin, never reset per directory or session, never
   reissuing a move), while close uses the same batch requests with no waiting
   and sync/report/preview never move. After all pending requests confirm, fresh
   family reads and a complete listing must agree the directory is free before
   removal proceeds.

An unavailable or incomplete listing, missing/mismatched session answer,
unusable location, unreadable/mismatched/cyclic lineage,
rejected/queued/unconfirmed move or unverifiable main
checkout keeps the directory. The `--dryrun` preview runs the same occupancy
and lineage reads and reports the same blockers and move eligibility without
moving, deleting, or writing anything. Local Git guards run before host reads,
so a
main checkout or dirty path does not need host access to be kept. One shared
dirty exception is a verified root `.cache`-only worktree: `.cache` is a real
root directory (never a file, symlink, `.cache2`, or nested `other/.cache`),
Git tracks nothing below it, porcelain status names only `??` paths under that
exact root, and a bounded symlink-free inventory (at most 4096 entries) of
reliable device/inode and high-resolution change evidence matches the initial
eligibility baseline across session movement and re-reads identically
immediately before the guarded cache removal. A platform without reliable
identity fails closed for this exception, and a same-length replacement or root
replacement keeps. After that removal Git status is checked again, every
applicable lease, local, closure, and fresh host gate is re-checked once more,
and then the ordinary no-force `git worktree remove`
runs; every other path is preserved. Removal uses `git worktree remove` without
force. Branches and lease rows are retained as history.

A recorded worktree path that no longer exists on disk is counted as absent; its
still-open lease rows converge once to `released`, and a repeated run leaves an
already-released row and its heartbeat untouched. A filesystem error or a
symlink entry is never read as absence.

These checks cover the one connected OpenCode service at the time of the
reads, not a whole-machine inventory or a guarantee against later session
changes. No scan cache replaces the final freshness checks. See
`phasegent --help worktree prune` for preview and stale-lease recovery options,
and [explicit pagination](cli-pagination.md) for read collection limits.
