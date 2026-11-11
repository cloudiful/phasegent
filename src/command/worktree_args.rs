/// Worktree directory management.
///
/// * `acquire` returns a `(directory_id, path, branch, repo_identity,
///   created, reason)` envelope (`reason ∈ {idempotent,
///   no_conflict, new_worktree}`) on stdout; the same session triple
///   reuses its registered directory. An explicit `--base REF` skips the
///   reuse/conflict table (after the idempotent home-coming) and creates a
///   fresh worktree/branch from `REF`. Orchestrator-only.
/// * `status --issue N` lists the registered directories associated
///   with the issue. Read-only; available to orchestrator, executor, and
///   reviewer.
/// * `list [--repo PATH]` lists every registered directory for the
///   resolved repo identity; `--repo` defaults to the current working
///   directory. Read-only; available to orchestrator, executor, and reviewer.
/// * `probe [--path PATH | --issue N [--session S]]` reports bounded,
///   read-only filesystem / Git facts about a checkout (or the directory
///   that matches `--issue`). It never calls a provider, writes the
///   registry, or syncs. Read-only; available to orchestrator, executor, and
///   reviewer.
/// * `prune [--repo PATH] [--dryrun] [--json|--format text|json]
///   [--verbose] [--remove] [--no-sync]` is the single pruning entry point.
///   Removal is the default and covers every repository identity the
///   directory registry records; `--repo PATH` restricts the pass to one
///   verified checkout and `--dryrun` (alias `--dry-run`) is the read-only
///   preview that mutates nothing. The output format follows the global
///   `--output auto|text|json` selection; `--verbose` adds the bounded
///   per-directory rows and never changes the machine document. `--remove`
///   is an accepted compatibility no-op and `--no-sync` is accepted but no
///   longer runs a sync pass. Branches are never deleted; only `git worktree
///   remove` (no `--force`) is invoked. Orchestrator-only.
///
/// `acquire` and `list` also accept `--no-sync` (issue 552 Phase 2): the
/// pre-subcommand reconciliation pass described on
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
        /// reuse-current-unless-the-target-path-is-occupied. `--reuse`
        /// states that default explicitly at the parser layer and is mutually
        /// exclusive with `--isolate`; it never bypasses an occupied checkout
        /// path.
        isolate: bool,
        no_sync: bool,
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
        /// Accepted compatibility no-op: removal is the default now, so this
        /// flag changes nothing. `--dryrun`/`--dry-run` is the explicit
        /// preview and wins when both are present.
        remove: bool,
        /// Explicit read-only preview (`--dryrun`/`--dry-run`): classify and
        /// report without mutating any directory or registry row.
        dry_run: bool,
        /// The legacy output selection of the prune: `"text"` (the parse-time
        /// default) or `"json"` (an explicit `--json` or `--format json`).
        /// The effective format follows the resolved global output mode
        /// unless a legacy format was requested explicitly.
        format: String,
        /// The verbose detail selection: the human text lists every bounded
        /// action row instead of the fixed-size summary. It never changes the
        /// machine document, and the JSON output ignores it.
        verbose: bool,
        no_sync: bool,
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
            | Self::Status { .. }
            | Self::Probe { .. } => None,
        }
    }
}
