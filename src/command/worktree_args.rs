/// Worktree lease management (issue #239 Phase 2).
///
/// * `acquire` returns a `(lease_id, path, branch, repo_identity,
///   created, reason)` envelope (`reason ∈ {idempotent,
///   no_conflict, new_worktree}`) on stdout; the same session triple
///   reuses the prior lease. The envelope is the machine JSON document or
///   its readable rendering, following the global output mode. An explicit
///   `--base REF` skips the reuse /
///   conflict decision table (after the idempotent home-coming) and
///   creates a fresh worktree/branch from `REF`. Orchestrator-only.
/// * `release` flips the lease to `retained` (default) or `released`.
///   Orchestrator-only. Never deletes the directory or the branch in
///   the current Phase 2 surface; prune is a separate subcommand.
/// * `status --issue N` lists active leases for the issue. Read-only;
///   available to orchestrator, executor, and reviewer.
/// * `list [--repo PATH]` lists every lease (active + terminal) for
///   the resolved repo identity; `--repo` defaults to the current
///   working directory. Read-only; available to orchestrator,
///   executor, and reviewer.
/// * `probe [--path PATH | --issue N [--session S]]` reports bounded,
///   read-only filesystem / Git facts about a checkout (or the lease
///   that matches `--issue`). It never calls a provider, writes a
///   lease, or syncs. Read-only; available to orchestrator, executor,
///   and reviewer.
/// * `prune [--repo PATH] [--stale-days N] [--release-stale --reason
///   TEXT] [--dryrun] [--json|--format text|json] [--remove] [--no-sync]` is
///   the single pruning entry point (folds the former `release-stale`).
///   Removal is the default and covers every repository identity the
///   local lease store records; `--repo PATH` restricts the pass to one
///   verified checkout and `--dryrun` (alias `--dry-run`) is the
///   read-only preview that mutates nothing. `--release-stale` requires
///   a non-empty `--reason` unless it is a preview and flips exactly the
///   stale active leases to `retained`; the directory pass then removes
///   clean + expired + retained worktrees that pass the shared candidate
///   guards (never the main checkout, never an active peer's path, never
///   dirty, never a directory the OpenCode host still occupies). The output
///   format follows the global `--output auto|text|json` selection;
///   `--json` / `--format json` select the machine-readable aggregate and
///   `--format text` the concise English text, and an explicit legacy
///   selection wins over the automatic default. `--remove` is an accepted
///   compatibility no-op and `--no-sync` is accepted but no longer runs a
///   sync pass. Branches are never deleted; only `git worktree remove` (no
///   `--force`) is invoked. Orchestrator-only.
/// * `heartbeat --lease ID [--session SESSION]` refreshes the heartbeat
///   of an active lease, but only when the resolved session owns the
///   row; a foreign session or terminal lease is a structured conflict.
///   Orchestrator-only.
///
/// `acquire`, `list`, and `prune` also accept `--no-sync` (issue 552
/// Phase 2): the pre-subcommand reconciliation pass described on
/// [`WorktreeCommand::sync_taxi`] is skipped for that invocation.
#[derive(Debug)]
#[allow(dead_code)]
pub enum WorktreeCommand {
    Acquire {
        issue: u64,
        /// Explicit `--session` value as supplied by the caller. `None`
        /// defers to `PHASEGENT_SESSION_ID`; without either, execution
        /// fails with a structured `argument` error instead of guessing
        /// an owner (issue 305 Task 1, fallback removed by issue 651 P4).
        session: Option<String>,
        /// Explicit `--base REF` request (issue 595). After the
        /// idempotent `(repo, issue, session)` home-coming, a fresh
        /// acquire bases the new worktree/branch on `REF` (a branch,
        /// tag, sha, or `origin/<branch>`) instead of `HEAD` and never
        /// reuses the current checkout. `None` keeps the existing
        /// decision table.
        base: Option<String>,
        format: String,
        /// Force a fresh worktree (issue #509): `--isolate` skips every
        /// reuse path and acquires an isolated worktree directly, and an
        /// occupied checkout path isolates by default. The default is
        /// reuse-current-unless-the-target-path-is-actively-occupied
        /// (issue 651). `--reuse` states that default explicitly at the
        /// parser layer and is mutually exclusive with `--isolate`; it
        /// never bypasses an active lease on the checkout path.
        isolate: bool,
        no_sync: bool,
    },
    Release {
        lease: String,
        retain: bool,
        /// True when invoked with `--force`: the transition is
        /// recorded with the operator justification in `reason`.
        force: bool,
        reason: Option<String>,
    },
    Status {
        issue: u64,
        page: Option<crate::pagination::PageRequest>,
    },
    List {
        repo: Option<String>,
        no_sync: bool,
        page: Option<crate::pagination::PageRequest>,
    },
    /// Read-only diagnostic (issue 595). Exactly one selector applies:
    /// `--path PATH`, or `--issue N [--session S]`, or neither (probe
    /// the current checkout). Never writes, syncs, or calls a provider.
    Probe {
        path: Option<String>,
        issue: Option<u64>,
        session: Option<String>,
    },
    Prune {
        repo: Option<String>,
        stale_days: u32,
        release_stale: bool,
        /// Accepted compatibility no-op: removal is the default now, so this
        /// flag changes nothing. `--dryrun`/`--dry-run` is the explicit
        /// preview and wins when both are present.
        remove: bool,
        /// Required (non-empty) when `release_stale` is true on a non-preview
        /// run so the recovery stays attributable; optional for a preview and
        /// rejected on its own.
        reason: Option<String>,
        /// Explicit read-only preview (`--dryrun`/`--dry-run`): classify and
        /// report without mutating any directory or lease row.
        dry_run: bool,
        /// The legacy output selection of the prune: `"text"` (the parse-time
        /// default) or `"json"` (an explicit `--json` or `--format json`).
        /// The effective format follows the resolved global output mode
        /// unless a legacy format was requested explicitly.
        format: String,
        no_sync: bool,
    },
    Heartbeat {
        lease: String,
        /// Explicit `--session`; `None` defers to
        /// `PHASEGENT_SESSION_ID`, and a missing identity fails instead
        /// of refreshing under a guessed owner (issue 651 P4).
        session: Option<String>,
    },
}

impl WorktreeCommand {
    /// The reconciliation pass runs before `acquire` and `list`; every
    /// other subcommand returns `None`. `prune` was removed from this set
    /// (issue 913): a prune pass never syncs, in global or repo mode. The
    /// pair is the operation label used for stderr warnings plus the
    /// checkout to reconcile: `--repo` when the subcommand takes one,
    /// otherwise `None` so the caller falls back to the current working
    /// directory.
    pub(crate) fn sync_taxi(&self) -> Option<(&'static str, Option<&str>)> {
        match self {
            Self::Acquire { no_sync: false, .. } => Some(("worktree acquire", None)),
            Self::List {
                repo,
                no_sync: false,
                ..
            } => Some(("worktree list", repo.as_deref())),
            Self::Acquire { .. }
            | Self::List { .. }
            | Self::Prune { .. }
            | Self::Release { .. }
            | Self::Status { .. }
            | Self::Heartbeat { .. }
            | Self::Probe { .. } => None,
        }
    }
}
