//! Per-invocation timing diagnostics for `worktree prune`.
//!
//! The measurements are observations of one invocation, not performance
//! guarantees and not a benchmark: nothing here decides a removal, changes a
//! guard, or fails a run. The statistics live in the invocation that owns
//! them — there is no global, no shared cache and no persisted log — and they
//! are dropped with the pass.
//!
//! Two measurements are kept apart:
//!
//! * **stages** are the prune pipeline's own steps: the scope registry read,
//!   the candidate scan, the session-family move, the verification re-reads and
//!   the reclamation work of the removal stage — the root `.cache` clearance
//!   (or the cleanliness probe that replaces it when the tree has no root
//!   `.cache`), the `git worktree remove` and the directory registry row
//!   removal, together with the reclamation of an absent registration. Every
//!   call of that stage is counted and timed, including the ones that fail.
//! * **operations** are the narrow boundary calls those stages make: closure
//!   evaluations and the host's `active`, `get`, `list` and `move` calls.
//!
//! The two families overlap; operations may also occur outside a measured stage,
//! such as during dry-run preview. Durations are not an additive partition of
//! the wall clock. The total is the wall clock of the pass; each other duration
//! is the summed time of the calls for that stage or operation.
//!
//! A closure measurement counts *closure evaluations*, not network requests:
//! one evaluation may read one provider answer, several, or none at all, and
//! it may retry. It is therefore never presented as a request count.
//!
//! Every completed call is counted, including a call that returned an error:
//! a failed boundary call is part of the pass's cost. Nothing measured here
//! carries a parameter, a session id, a credential or a provider body.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::cli::issue_closure::{IssueClosure, IssueStateSource, TaskScopeRef};
use crate::worktree::opencode::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};

/// The per-scope registry read.
pub(crate) const SCOPE_REGISTRY: &str = "scope_registry";
/// The candidate scan.
pub(crate) const SCAN: &str = "scan";
/// The session-family relocation.
pub(crate) const FAMILY_MOVE: &str = "family_move";
/// The fresh verification re-reads before a destructive step.
pub(crate) const VERIFICATION: &str = "verification";
/// The cache clearance, the Git worktree removal and the registry row
/// removal, plus the reclamation of an absent registration.
pub(crate) const CACHE_DELETE_RECLAIM: &str = "cache_delete_reclaim";

/// The stages, in the fixed order the report lists them.
pub(crate) const STAGES: [&str; 5] = [
    SCOPE_REGISTRY,
    SCAN,
    FAMILY_MOVE,
    VERIFICATION,
    CACHE_DELETE_RECLAIM,
];

/// A closure evaluation.
pub(crate) const CLOSURE: &str = "closure";
/// The host's running-session query.
pub(crate) const HOST_ACTIVE: &str = "host_active";
/// The host's single-session read.
pub(crate) const HOST_GET: &str = "host_get";
/// The host's session listing.
pub(crate) const HOST_LIST: &str = "host_list";
/// The host's session relocation.
pub(crate) const HOST_MOVE: &str = "host_move";

/// The operations, in the fixed order the report lists them.
pub(crate) const OPERATIONS: [&str; 5] = [CLOSURE, HOST_ACTIVE, HOST_GET, HOST_LIST, HOST_MOVE];

/// One measured stage or operation.
#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct TimingEntry {
    pub calls: u64,
    pub duration_ms: f64,
}

/// The measured timings of one invocation.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct TimingReport {
    /// The wall clock of the whole pass.
    pub total_ms: f64,
    /// The pipeline stages, each with its own call count and duration.
    pub stages: BTreeMap<String, TimingEntry>,
    /// The nested boundary calls, each with its own call count and duration.
    pub operations: BTreeMap<String, TimingEntry>,
}

/// One measured name.
#[derive(Debug, Clone, Copy, Default)]
struct Accumulator {
    calls: u64,
    duration: Duration,
}

/// The invocation-owned measurement sink.
pub(crate) struct TimingStats {
    started: Instant,
    stages: RefCell<BTreeMap<&'static str, Accumulator>>,
    operations: RefCell<BTreeMap<&'static str, Accumulator>>,
}

impl TimingStats {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
            stages: RefCell::new(BTreeMap::new()),
            operations: RefCell::new(BTreeMap::new()),
        }
    }

    /// Measure one stage call. The call is counted and timed even when it
    /// fails: a failing stage is part of the pass.
    pub(crate) fn measure_stage<T>(&self, stage: &'static str, call: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let outcome = call();
        record(self.stages.borrow_mut(), stage, started.elapsed());
        outcome
    }

    /// Measure one nested boundary call, with the same counting rule.
    pub(crate) fn measure_operation<T>(
        &self,
        operation: &'static str,
        call: impl FnOnce() -> T,
    ) -> T {
        let started = Instant::now();
        let outcome = call();
        record(self.operations.borrow_mut(), operation, started.elapsed());
        outcome
    }

    /// The measured report. Every known stage and operation is present, so a
    /// pass that never reached one reports a valid zero instead of omitting it.
    pub(crate) fn report(&self) -> TimingReport {
        let stages = self.stages.borrow();
        let operations = self.operations.borrow();
        TimingReport {
            total_ms: millis(self.started.elapsed()),
            stages: STAGES
                .iter()
                .map(|stage| {
                    (
                        (*stage).to_owned(),
                        entry(stages.get(stage).copied().unwrap_or_default()),
                    )
                })
                .collect(),
            operations: OPERATIONS
                .iter()
                .map(|operation| {
                    (
                        (*operation).to_owned(),
                        entry(operations.get(operation).copied().unwrap_or_default()),
                    )
                })
                .collect(),
        }
    }
}

impl Default for TimingStats {
    fn default() -> Self {
        Self::new()
    }
}

/// Record one completed call.
fn record(
    mut sink: std::cell::RefMut<'_, BTreeMap<&'static str, Accumulator>>,
    name: &'static str,
    duration: Duration,
) {
    let accumulator = sink.entry(name).or_default();
    accumulator.calls += 1;
    accumulator.duration += duration;
}

/// One measured name as it is reported.
fn entry(accumulator: Accumulator) -> TimingEntry {
    TimingEntry {
        calls: accumulator.calls,
        duration_ms: millis(accumulator.duration),
    }
}

/// A duration in milliseconds with three decimals.
fn millis(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 100_000.0).round() / 100.0
}

/// The closure source with its evaluations measured. Every method is
/// forwarded unchanged, including the invalidation, so a decorated source
/// behaves exactly like the source it wraps.
pub(crate) struct TimedIssueStateSource<'a> {
    inner: &'a dyn IssueStateSource,
    timing: &'a TimingStats,
}

impl<'a> TimedIssueStateSource<'a> {
    pub(crate) fn new(inner: &'a dyn IssueStateSource, timing: &'a TimingStats) -> Self {
        Self { inner, timing }
    }
}

impl IssueStateSource for TimedIssueStateSource<'_> {
    fn closure(&self, repo_path: &Path, issue: u64) -> IssueClosure {
        self.timing
            .measure_operation(CLOSURE, || self.inner.closure(repo_path, issue))
    }

    fn selection(&self) -> Option<&crate::cli::provider_context::ProviderContext> {
        self.inner.selection()
    }

    fn refresh(&self) {
        self.inner.refresh();
    }

    fn own_task_closure(
        &self,
        repo_path: &Path,
        task: TaskScopeRef<'_>,
        issue: u64,
    ) -> IssueClosure {
        self.timing.measure_operation(CLOSURE, || {
            self.inner.own_task_closure(repo_path, task, issue)
        })
    }

    fn associated_closure(&self, repo_path: &Path, issue: u64) -> IssueClosure {
        self.timing
            .measure_operation(CLOSURE, || self.inner.associated_closure(repo_path, issue))
    }

    fn invalidate(&self, repo_path: &Path, issues: &[u64]) {
        self.inner.invalidate(repo_path, issues);
    }

    fn scoped_closure(
        &self,
        repo_path: &Path,
        issue: u64,
        scope: Option<&crate::worktree::WorktreeScope>,
    ) -> IssueClosure {
        self.timing.measure_operation(CLOSURE, || {
            self.inner.scoped_closure(repo_path, issue, scope)
        })
    }
}

/// The host client with its calls measured. Every method is forwarded
/// unchanged, so a decorated client answers exactly like the client it wraps.
pub(crate) struct TimedOpenCodeApi<'a> {
    inner: &'a dyn OpenCodeApi,
    timing: &'a TimingStats,
}

impl<'a> TimedOpenCodeApi<'a> {
    pub(crate) fn new(inner: &'a dyn OpenCodeApi, timing: &'a TimingStats) -> Self {
        Self { inner, timing }
    }
}

impl OpenCodeApi for TimedOpenCodeApi<'_> {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.timing
            .measure_operation(HOST_GET, || self.inner.session(id))
    }

    fn list(&self, request: &ListRequest) -> Result<SessionPage, ApiError> {
        self.timing
            .measure_operation(HOST_LIST, || self.inner.list(request))
    }

    fn move_session(&self, id: &str, directory: &Path) -> Result<(), ApiError> {
        self.timing
            .measure_operation(HOST_MOVE, || self.inner.move_session(id, directory))
    }

    fn active_sessions(&self) -> Result<Vec<String>, ApiError> {
        self.timing
            .measure_operation(HOST_ACTIVE, || self.inner.active_sessions())
    }
}
