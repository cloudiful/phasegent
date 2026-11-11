# Worktree registry, reconciliation and cleanup

## Directory registry

`worktree_directories` records each physical worktree directory by repository
identity, canonical path, main checkout, branch and creation time. Its child
tables associate a directory with durable tasks, unknown legacy issue identities
and observed sessions. A directory can reference many tasks and sessions.

The registry carries no lease state: no status, heartbeat, expiration or release
reason. Live ownership comes from fresh evidence — a task that is still open, or
a session the connected OpenCode service currently reports in the directory.
Task and session provenance stays recorded after an issue closes until the
directory is really removed.

A legacy `worktree_leases`/`worktree_lease_scopes` store is upgraded once, in a
single immediate transaction, into the registry. Every repository, path, issue,
session and known scope is preserved; unknown legacy provenance stays unknown.
The lease tables are dropped only after the rows are represented. A marker
beside an unconsumed legacy table never lets those rows be dropped unimported:
the upgrade re-imports idempotently. An unsupported old schema, or any
contradictory task/scope evidence, refuses the whole transaction and leaves the
old tables and rows intact. A preview or
any other pure read never creates, migrates, drops, writes or chmods a store; an
absent store reads as an empty registry, and a legacy store is projected in
memory without being modified.

## Operation lock

Cooperating acquire, issue-switch, cleanup, removal and restore operations take
one advisory lock per canonical repository under the existing worktree cache.
The lock is a stable `File::try_lock` on a read/write-opened file; dropping the
handle releases it on success, failure or crash, and the file is never unlinked.
Contention fails with a bounded, actionable error. The lock is held from the
fresh occupancy and registry check through the Git mutation and the short
registry transaction. Provider and host I/O never run inside a SQLite
transaction. Dry-run, probe, list and status create no lock file.

The lock protects cooperating phasegent operations only. It makes no claim
about arbitrary Git clients or sessions that start after a check.

## Acquire ownership

`worktree acquire` is owned by the current repository's durable task when one is
bound, for every provider and without repeating `--provider` or `--repository`.
A same-number task on another instance is a different scope that is never
reused, and a contradicting explicit selection refuses before any registry write
or sync pass. Without a bound task, an explicit `--provider forgejo` keeps the
explicit `owner/repo` scope, an implicit Forgejo default fails closed, and a
provider that owns legacy numeric issues keeps the unscoped legacy route.

Reuse is the default. The current checkout is reused whether it is clean, dirty,
unbound or linked to a historical issue. Fresh current occupancy, or an
associated task that is still open, protects a different idle task's directory.
A confirmed closed historical task does not veto reuse by itself. An unresolved
occupation of the checkout path is reported as an `isolation` error naming
`phasegent worktree acquire --issue N --isolate`; a worktree is created only
when `--isolate` is passed explicitly. `--base REF` only chooses the start point
of that explicit worktree; without `--isolate` it is rejected and never creates
one. `--reuse` states the preference and never bypasses an occupied checkout. Dirty state is advisory only. An unknown
`git status` probe is never silently treated as clean.

## Synchronous reconciliation

Authorized Operator/orchestrator `worktree list` and `worktree acquire` callers
run a complete repository-scoped `issue sync` before their command continues.
`--no-sync` skips it. A sync failure is a bounded warning for this implicit
pass; a direct `issue sync` reports a failure as an error.

Page flags affect only the subsequent list query, not reconciliation. Sync and
prune always consider complete maintenance collections; they do not accept user
page limits. Prune never runs implicit sync, and its `--no-sync` spelling is a
compatibility no-op.

Repository identity, candidate grouping, registry setup and provider setup are
reused within one invocation. Removal reads current registry evidence again, and
list reads its rows after reconciliation finishes. There is no persistent or
cross-command cache.

`issue sync --no-clean` writes nothing. It classifies each existing associated
directory with the shared local guards; an OpenCode-associated directory is
reported `would_keep` without contacting the host, and a pure CLI directory is
`would_clean` when locally eligible. Clean mode runs the shared removal guards.

## Prune removal

Prune's scan is read-only. It groups registry records by canonical directory and
reuses canonical path answers within that scan. Its host scan reads the connected
service's occupancy and lineage; it neither enumerates historical sessions nor
moves them. Unknown or unusable answers keep the directory.

A directory is removed only when all of these hold:

1. It is a verified non-main, clean directory. The narrow verified root `.cache`
   exception is preserved.
2. Every associated task is confirmed closed through its own durable task, or a
   legacy number through a proven sidecar, link or trusted manual route. A
   confirmed closed historical task never keeps the directory; a still-open
   associated task does.
3. A fresh host occupancy check reports no running or unassociated session.
4. Every present idle session family the registry associates with the directory
   is safely relocated to the verified main checkout.
5. The operation lock is held and the local, closure and fresh host guards are
   rechecked immediately before removal.

An unavailable or incomplete listing, a missing or mismatched session answer, an
unusable location, broken lineage, a rejected, queued or unconfirmed move, or an
unverifiable main checkout keeps the directory. A missing associated idle
session needs no move and is not a permanent keep unless the host's current
listing still names it, which is contradictory evidence and keeps. Only the
connected service is covered; no whole-machine inventory is claimed.

The `--dryrun` preview runs the same occupancy and lineage reads and reports the
same blockers and move eligibility without moving, deleting or writing anything,
and without creating a store, schema or operation lock. Local Git guards run
before host reads, so a main checkout or dirty path is kept without host access.

After a successful `git worktree remove` (never with `--force`), the registry row
is deleted. Branches and Git history are retained. A recorded path that no
longer exists is reported as absent: the fact is independent of task or provider
identity, registry evidence is preserved, and no provider read or session move
is required.

The default output follows the global `--output auto` selection: the compact
human summary on an interactive terminal and the machine aggregate envelope on a
pipe or in CI. `--verbose` adds bounded per-directory rows without changing the
machine document. The aggregate envelope is the complete-detail inspection
channel. Progress is a human affordance only: one stderr line is redrawn in
place, never carries a directory path, counts completed repositories and
directories, and is cleared before the result.

See `phasegent --help worktree prune` for the preview and action flags, and
[explicit pagination](cli-pagination.md) for read collection limits.
