/// Worktree lease management (issue #239 Phase 2).
///
/// * `acquire` returns a `(lease_id, path, branch, repo_identity,
///   created, reason)` JSON envelope (`reason ∈ {idempotent,
///   no_conflict, new_worktree}`) on stdout; the same session triple
///   reuses the prior lease. An explicit `--base REF` skips the reuse /
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
///   TEXT] [--remove]` is the single pruning entry point (folds the
///   former `release-stale`). With neither `--release-stale` nor
///   `--remove` it is a read-only dry-run that reports stale active
///   leases and prunable worktrees and writes nothing. `--release-stale`
///   requires a non-empty `--reason` and flips exactly the stale active
///   leases to `retained`; `--remove` deletes clean + expired +
///   retained worktrees. Both flags together run the recovery first and
///   then the removal. Branches are never deleted; only
///   `git worktree remove` (no `--force`) is invoked, and only when
///   `is_clean` reports an empty porcelain. Orchestrator-only.
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
        /// defers to `PHASEGENT_SESSION_ID` and then the legacy
        /// `phasegent` fallback at execution time (issue 305 Task 1).
        session: Option<String>,
        /// Explicit `--base REF` request (issue 595). After the
        /// idempotent `(repo, issue, session)` home-coming, a fresh
        /// acquire bases the new worktree/branch on `REF` (a branch,
        /// tag, sha, or `origin/<branch>`) instead of `HEAD` and never
        /// reuses the current checkout. `None` keeps the existing
        /// decision table.
        base: Option<String>,
        format: String,
        /// Per-call isolation override (issue #247). `--isolate` forces
        /// a fresh branch/worktree on a conflict; the resolved
        /// `worktree-auto` switch is OR-ed with it, so the default
        /// (both off) reuses the current checkout with a warning.
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
    },
    List {
        repo: Option<String>,
        no_sync: bool,
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
        remove: bool,
        /// Required (non-empty) when `release_stale` is true so the
        /// recovery stays attributable; rejected on its own.
        reason: Option<String>,
        no_sync: bool,
    },
    Heartbeat {
        lease: String,
        /// Explicit `--session`; `None` defers to
        /// `PHASEGENT_SESSION_ID` and then the legacy fallback.
        session: Option<String>,
    },
}

impl WorktreeCommand {
    /// The reconciliation pass runs before `acquire`, `list`, and `prune`;
    /// every other subcommand returns `None`. The pair is the operation
    /// label used for stderr warnings plus the checkout to reconcile:
    /// `--repo` when the subcommand takes one, otherwise `None` so the
    /// caller falls back to the current working directory.
    pub(crate) fn sync_taxi(&self) -> Option<(&'static str, Option<&str>)> {
        match self {
            Self::Acquire { no_sync: false, .. } => Some(("worktree acquire", None)),
            Self::List {
                repo,
                no_sync: false,
            } => Some(("worktree list", repo.as_deref())),
            Self::Prune {
                repo,
                no_sync: false,
                ..
            } => Some(("worktree prune", repo.as_deref())),
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
