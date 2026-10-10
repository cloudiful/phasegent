#[derive(Debug)]
pub enum IssueCommand {
    Get {
        number: u64,
    },
    /// Batch fetch of 2–20 issues in one invocation. The parser keeps
    /// single-number invocations on `Get` so the legacy single-object
    /// output shape is preserved; batches return an
    /// `{issues, errors}` envelope instead of failing fast.
    GetBatch {
        numbers: Vec<u64>,
    },
    Search {
        query: Option<String>,
        state: String,
        page: usize,
        limit: usize,
        all: bool,
        include_body: bool,
    },
    Create {
        title: String,
        body: String,
        /// Optional one-shot Markdown file input (`--body-file`); mutually
        /// exclusive with `--body`, deleted after a successful write unless
        /// `--keep-body-file` was supplied.
        body_file: Option<String>,
        keep_body_file: bool,
        /// Optional Redmine tracker selector (validated name or id) resolved
        /// against `/trackers.json` at execution time.
        tracker: Option<String>,
        planning: PlanningOptions,
        assignee: AssigneeOption,
        branch: BranchOption,
        /// Optional start point for `--branch` (`--base REF`); `None`
        /// defaults to `HEAD` at execution time.
        base: Option<String>,
        /// Optional explicit worktree session id (`--session`). `None`
        /// defers to `PHASEGENT_SESSION_ID`; when neither names a session
        /// the auto-acquire hook stays silent and no lease is booked.
        /// After a successful create the shared auto-acquire hook runs
        /// best-effort; stdout JSON is unchanged (issue 18). Boxed so the
        /// `Command` enum stays under the `large_enum_variant` threshold.
        session: Option<Box<str>>,
    },
    /// former `update-body`). The body plus optional tracker/planning
    /// fields are applied in one PUT.
    Update {
        number: u64,
        body: String,
        /// Optional one-shot Markdown file input (`--body-file`); mutually
        /// exclusive with `--body`, deleted after a successful write unless
        /// `--keep-body-file` was supplied.
        body_file: Option<String>,
        keep_body_file: bool,
        tracker: Option<String>,
        planning: PlanningOptions,
    },
    Close {
        number: u64,
        /// Optional explicit worktree session id (`--worktree-session`).
        /// `None` defers to `PHASEGENT_SESSION_ID`; without either, the
        /// close still succeeds and the leases flip with the plain
        /// `issue closed` reason — no closer is ever guessed
        /// (issue 575 Phase 1, issue 651 P4).
        worktree_session: Option<String>,
    },
    /// Redmine-only orchestrator attachment upload. Validates the local
    /// file (exists, regular, non-empty, bounded 25 MiB, valid filename)
    /// then performs the raw `POST /uploads.json?filename=...` plus
    /// `PUT /issues/<id>.json` with `uploads` protocol.
    UploadAttachment {
        number: u64,
        path: String,
        description: Option<String>,
    },
    /// `issue sync [--all] [--no-clean]` (issue 552 Phase 2): reconcile
    /// local worktree leases and worktree directories against the
    /// provider's issue state. Default scope is the current repository;
    /// `--all` scans every repository recorded in the lease table.
    /// Remotely closed issues converge their active leases to `retained`
    /// and then run the `issue close` guarded cleanup; `--no-clean`
    /// reports the same verdicts without writing anything.
    /// Orchestrator-only at execution time.
    Sync {
        all: bool,
        no_clean: bool,
    },
    /// Local branch context operations (no provider access). `bind`
    /// records a durable provider/project-scoped branch link in the local
    /// link store; the resolved scope comes from global options or stored
    /// role config and is never guessed.
    Bind {
        issue_id: u64,
    },
    Unbind,
    StatusBranch,
    /// Read-only reverse lookup `issue branches N` (issue 628 P5):
    /// every branch linked to numeric issue `N` in this repository
    /// across all provider/project scopes, with last-known cached
    /// state. No provider access, no writes.
    Branches {
        number: u64,
    },
}

/// Raw planning option values captured by the parser. Numeric ranges,
/// date shapes, and `--fixed-version` resolution are validated at
/// execution time in `redmine_planning_cli` so the parser stays purely
/// structural.
#[derive(Debug, Default)]
pub struct PlanningOptions {
    pub parent_issue: Option<String>,
    pub fixed_version: Option<String>,
    pub start_date: Option<String>,
    pub due_date: Option<String>,
    pub estimated_hours: Option<String>,
    pub done_ratio: Option<String>,
}

impl PlanningOptions {
    /// True when no planning flag was supplied; such invocations keep the
    /// exact legacy execution path and payload.
    pub fn is_empty(&self) -> bool {
        self.parent_issue.is_none()
            && self.fixed_version.is_none()
            && self.start_date.is_none()
            && self.due_date.is_none()
            && self.estimated_hours.is_none()
            && self.done_ratio.is_none()
    }
}

/// Raw assignee selector for `issue create`.
///
/// * `Unset` — no `--assignee`/`--no-assign` flag. The payload keeps no
///   assignee field.
/// * `Unassigned` — `--no-assign`; never attach an assignee (a no-op that
///   keeps the legacy payload).
/// * `Explicit` — `--assignee` value. No provider supports an explicit
///   assignee, so it is rejected before any write.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum AssigneeOption {
    #[default]
    Unset,
    Unassigned,
    Explicit(String),
}

/// Explicit local branch request for `issue create` (`--branch [NAME]`).
///
/// * `Unset` — no `--branch` flag. The default path derives the
///   provider-scoped branch, links it durably, and switches only when
///   safe (never creating a branch for another issue implicitly).
/// * `Auto` — bare `--branch`. Generate `<type>/<id>` from the tracker
///   (`Bug` -> `fix`, everything else -> `feat`, e.g. `feat/452`).
/// * `Named` — `--branch NAME` (or `--branch=NAME`). Use `NAME` verbatim.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum BranchOption {
    #[default]
    Unset,
    Auto,
    Named(String),
}
