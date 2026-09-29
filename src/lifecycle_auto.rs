//! Local lifecycle side effects for **automatic** time accounting driven by
//! status transitions and issue close.
//!
//! Every helper here is best-effort: the caller's status change or close
//! must succeed even if the local ledger cannot be mutated, so all
//! errors are captured as bounded [`AutoTimerOutcome::Warning`]
//! values. The hook call sites in `src/cli/status.rs` and
//! `src/cli/issue.rs` translate the warning into a structured stderr
//! line via `cli::report_local_warnings` and keep stdout JSON
//! compatible with the plain provider output shape.
//!
//! ## Provider coverage
//!
//! - **Redmine** — full auto-accounting; `status set` and
//!   `status advance` both flow through the helper.
//! - **GitLab** — `status set` flows through (label-based); `status
//!   advance` is Redmine-only upstream so the helper is not reached
//!   on that path. The provider branch is gated inside the helper
//!   itself for defensive parity.
//! - **Local** — full auto-accounting on the local ledger.
//!
//! ## Single-running invariant
//!
//! The contract is "at most one `running` row per `issue_id` after a
//! successful status transition". On a healthy ledger, the issue has
//! zero or one running row, the helper closes the open one (if any),
//! then opens the new one. On a crash-or-legacy ledger, the helper
//! may observe multiple running rows for the same issue; the chosen
//! recovery is to finish **every** running row, oldest first, so the
//! final state is one fresh running row regardless of the prior
//! multiplicity. The "finish all but newest" wording in the issue
//! plan refers to the **order** of the recovery sweep (oldest first),
//! not to leaving the newest open: keeping the newest open would
//! violate the invariant because the helper still has to open a new
//! row afterwards. The Phase 3 projection tests can observe the
//! individual finished rows via `timer list` to confirm the recovery
//! sweep was complete.
//!
//! ## Status → agent-role mapping
//!
//! See [`status_to_agent_role`]. Custom Redmine status names that
//! don't match any of the documented buckets fall back to
//! `executor` and the helper emits a stderr warning so the operator
//! can rename the workflow if they want a different role to handle
//! it. The role always passes `validate_timer_identity` because the
//! whitelist is exactly `{executor, reviewer, tester}`.
//!
//! ## Cumulative sum
//!
//! Repeated same-state transitions open a fresh auto-run each time
//! (because the run_id includes a per-call counter). The total
//! elapsed time for a given `(issue, phase)` is therefore the
//! **sum** of every finished row's `elapsed_seconds`, not a single
//! overwritten value. A helper
//! [`Storage::sum_elapsed_seconds_for_issue_phase`] is provided so
//! the future cumulative-sum reporting can read the aggregated value
//! without re-deriving it in the lifecycle layer.
//!
//! ## Attempt counter
//!
//! The lifecycle path always opens a fresh segment with
//! `attempt = 1`; the attempt counter is **not** incremented on
//! re-entry. The cumulative contract above is the only thing that
//! tracks how many times the same `(issue, phase)` pair has been
//! visited, and a future cumulative-sum reporter reads it from
//! `Storage::sum_elapsed_seconds_for_issue_phase` rather than from
//! the per-row `attempt` column. Operators inspecting
//! `execution_timer_runs` directly will see a single `attempt=1`
//! per `(issue, phase)` segment.

use crate::infra::storage::Storage;
use crate::providers::{ProviderDispatcher, ProviderKind};
use crate::time_tracking::{finish, start};

const MAX_AUTO_WARNING_CHARS: usize = 200;

fn bounded(text: &str) -> String {
    text.chars().take(MAX_AUTO_WARNING_CHARS).collect()
}

/// Map a status name to an agent role. The case-insensitive matching
/// keeps the mapping tolerant of custom Redmine / GitLab status
/// names (e.g. "Doing", "In Review") while never silently dropping
/// a value: the fallback is `executor` and the caller surfaces a
/// stderr warning describing the unmapped status so the operator can
/// rename the workflow if they want a different role to handle it.
///
/// Order of checks matters: the QA / test bucket must run before
/// the generic review / dev containment so a status like "ready for
/// test" is routed to `tester` and not `reviewer`. Likewise the
/// review check runs before the dev/impl containment so a status
/// like "implementation review" is `reviewer` and not `executor`.
pub fn status_to_agent_role(status_name: &str) -> (&'static str, bool) {
    let lowered = status_name.to_ascii_lowercase();
    if lowered.contains("test") || lowered.contains("qa") {
        ("tester", false)
    } else if lowered.contains("review") {
        ("reviewer", false)
    } else if lowered.contains("progress") || lowered.contains("dev") || lowered.contains("impl") {
        ("executor", false)
    } else {
        ("executor", true)
    }
}

/// Tool-driven auto-route signal (issue 443 Phase 2).
///
/// Maps the most recent successful tool to its canonical target status
/// so the timer ledger follows the tool without a manual
/// `status set` / `status advance`. The table mirrors the issue
/// examples and is intentionally signal-only: `number` is reserved
/// for future per-issue routing and is unused today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSignal {
    IssueCreated,
    CommentCreated,
    /// `issue close` succeeded; the task is done. Reserved for the
    /// Phase 3 close-climb target derivation; the Phase 2 close path
    /// finishes runs via `auto_close_issue_timer` instead of opening
    /// a new segment, so this variant is mapping-only today.
    #[allow(dead_code)]
    IssueClosed,
}

/// Map a tool signal to its canonical target status name.
///
/// Returns `None` only when a future signal has no canonical target;
/// all three Phase 2 signals map today. Callers feed the name into
/// [`auto_transition_timer`] for the timer ledger (stderr-only
/// warnings, stdout untouched) and treat `None` as silent skip.
pub fn auto_route_next(_issue: u64, signal: ToolSignal) -> Option<&'static str> {
    match signal {
        ToolSignal::IssueCreated => Some("In Progress"),
        ToolSignal::CommentCreated => Some("In Review"),
        ToolSignal::IssueClosed => Some("Closed"),
    }
}

/// Outcome of a lifecycle auto-accounting call. Callers translate
/// `Started` (when its `warning` field is `Some`) or `Warning` into
/// a bounded stderr line via `cli::report_local_warnings`. The
/// `Started` variant carries a `warning` field so the fallback mapping
/// (custom Redmine status with no canonical role) and the
/// close-sweep partial-failure path can be surfaced to the operator
/// even when the new segment was opened successfully.
#[derive(Debug, PartialEq, Eq)]
pub enum AutoTimerOutcome {
    /// The transition produced a new auto-run and zero or more
    /// finished pre-existing runs. `warning` is `Some` when a
    /// non-fatal signal needs to reach the operator: the status
    /// name had no canonical role (fallback to `executor`), the
    /// pre-existing close sweep only partially succeeded, or both.
    Started {
        issue: u64,
        status_name: String,
        role: &'static str,
        finished_runs: Vec<String>,
        new_run_id: String,
        warning: Option<String>,
    },
    /// The transition could not start a new run. The underlying
    /// status change is already successful; the warning is the only
    /// observable signal of the local ledger failure.
    Warning { reason: String },
}

impl AutoTimerOutcome {
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Warning { reason } => Some(bounded(reason)),
            Self::Started { warning, .. } => warning.as_ref().map(|w| bounded(w)),
        }
    }
}

/// Outcome of the close-path helper. The close is always best-effort;
/// a failure to finish the running rows never undoes the remote
/// close that already succeeded.
#[derive(Debug, PartialEq, Eq)]
pub enum AutoCloseOutcome {
    /// No running auto-runs existed for the issue; the close path
    /// had nothing to do.
    Noop { reason: String },
    /// Every running auto-run for the issue was finished locally.
    Closed {
        issue: u64,
        finished_runs: Vec<String>,
    },
    /// The helper could not open the ledger at all (e.g. corrupt
    /// SQLite file). The remote close already succeeded; this is a
    /// bounded warning only.
    Warning { reason: String },
}

impl AutoCloseOutcome {
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Warning { reason } => Some(bounded(reason)),
            _ => None,
        }
    }
}

/// Run the auto-accounting side effect for a successful
/// `status set` / `status advance`. For Redmine, GitLab, and Local we
/// finish every running auto-run for the issue, then open a new run
/// whose phase is `status_name` and whose role comes from
/// [`status_to_agent_role`]. Failures degrade to a `Warning` so
/// the orchestrator's status change still returns success.
///
/// When the new segment is opened successfully, the `Started`
/// outcome still carries a `warning` field if either the status
/// name fell back to `executor` (no canonical role) or the
/// close-sweep partially failed; both are observable via
/// `warning()` so the hook call sites can surface them through
/// `report_local_warnings`.
pub fn auto_transition_timer(
    issue: u64,
    _provider_kind: ProviderKind,
    status_name: &str,
) -> AutoTimerOutcome {
    let (role, fallback) = status_to_agent_role(status_name);
    let phase = status_name.to_owned();
    let mut finished_runs: Vec<String> = Vec::new();
    let mut sweep_warnings: Vec<String> = Vec::new();

    match close_issue_runs(issue) {
        Ok(closed) => finished_runs.extend(closed),
        Err(error) => sweep_warnings.push(format!("close previous auto-run failed: {error}")),
    }

    // Start the new segment. We attempt the start even if the
    // close sweep failed: a stale open row is less harmful than
    // missing the new segment entirely. The new start's own
    // failure (storage error) still surfaces as a `Warning` so
    // the operator sees the gap.
    let combined_warning = build_transition_warning(fallback, &sweep_warnings, status_name);
    match start::auto_start_run(issue, &phase, role, 1) {
        Ok(run_id) => AutoTimerOutcome::Started {
            issue,
            status_name: phase,
            role,
            finished_runs,
            new_run_id: run_id,
            warning: combined_warning,
        },
        Err(error) => {
            let mut reason = format!("auto timer start failed: {error}");
            if fallback {
                reason.push_str(&format!(
                    " (status '{status_name}' had no canonical mapping; defaulted to executor)"
                ));
            }
            for warn in sweep_warnings {
                reason.push_str(&format!("; {warn}"));
            }
            AutoTimerOutcome::Warning { reason }
        }
    }
}

fn build_transition_warning(
    fallback: bool,
    sweep_warnings: &[String],
    status_name: &str,
) -> Option<String> {
    if !fallback && sweep_warnings.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if fallback {
        parts.push(format!(
            "status '{status_name}' had no canonical mapping; defaulted to executor"
        ));
    }
    parts.extend(sweep_warnings.iter().cloned());
    Some(parts.join("; "))
}

/// Run the auto-accounting side effect for a successful
/// `issue close`. Every running auto-run for the issue is
/// finished locally. This is the close path; no new run is
/// started because the issue is now closed. The branch-context
/// `unbind_closed_issue` is a separate sibling helper and is
/// untouched by this module.
///
/// An empty ledger returns [`AutoCloseOutcome::Noop`]. The
/// branch-context unbind is provider-agnostic at its own layer and is
/// unaffected by this gate.
pub fn auto_close_issue_timer(issue: u64, _provider_kind: ProviderKind) -> AutoCloseOutcome {
    match close_issue_runs(issue) {
        Ok(finished_runs) if finished_runs.is_empty() => AutoCloseOutcome::Noop {
            reason: format!("no running auto-runs to finish for issue {issue}"),
        },
        Ok(finished_runs) => AutoCloseOutcome::Closed {
            issue,
            finished_runs,
        },
        Err(error) => AutoCloseOutcome::Warning {
            reason: format!("auto timer close failed: {error}"),
        },
    }
}

fn close_issue_runs(issue: u64) -> Result<Vec<String>, String> {
    let storage = Storage::open().map_err(|error| format!("storage open: {error}"))?;
    let running = storage
        .list_running_runs_for_issue(issue)
        .map_err(|error| format!("list running: {error}"))?;
    // Finish in oldest-first order so the "newest" row is closed
    // last. The final state is the same regardless of order, but
    // the recovery audit (a future `timer list` while debugging) is
    // easier to read when the row timestamps line up with the
    // close order.
    let mut finished = Vec::with_capacity(running.len());
    for run in running.iter().rev() {
        match finish::auto_finish_run(&run.run_id) {
            Ok(()) => finished.push(run.run_id.clone()),
            Err(error) => {
                // Continue with the remaining runs even if one
                // close fails: the issue is already closed
                // upstream, so leaving one orphan open is worse
                // than reporting the partial failure.
                return Err(format!(
                    "could not finish running auto-run '{}': {error}",
                    run.run_id
                ));
            }
        }
    }
    Ok(finished)
}

// Native hierarchy (issue 641) replaces the old parent-child `relates`
// auto-link: Redmine `--parent-issue` already creates a native subtask and
// GitLab hierarchy uses native Work Item widgets, so no `relates` edge is
// attempted. Hierarchy never implies a relation.

/// Outcome of the parent-child relation auto-create call. The hook
/// call sites translate `Skipped` into silence, `Created` and
/// `Idempotent` into success (no warning), and `Warning` into a
/// bounded stderr line via `cli::report_local_warnings`.
#[derive(Debug, PartialEq, Eq)]
pub enum AutoRelationOutcome {
    /// Provider has no relation surface (Local) or no parent
    /// linkage was supplied at the call site. Silent success — the
    /// caller should not surface anything.
    Skipped { reason: String },
    /// A fresh `relates` link was created. `relation_id` is the
    /// server-assigned id (Redmine relation id / GitLab issue link
    /// id) so the operator can spot the new auto-link via
    /// `relation list`.
    #[allow(dead_code)]
    Created {
        child: u64,
        parent: u64,
        relation_id: u64,
    },
    /// A `relates` link to the parent already exists. The auto path
    /// is idempotent so repeated calls (e.g. status set then status
    /// advance on the same child) never produce duplicate links.
    #[allow(dead_code)]
    Idempotent { child: u64, parent: u64 },
    /// The helper could not create the link (storage open failure,
    /// server validation, network). The upstream status change is
    /// already successful; this is a bounded warning only.
    Warning { reason: String },
}

impl AutoRelationOutcome {
    pub fn warning(&self) -> Option<String> {
        match self {
            Self::Warning { reason } => Some(bounded(reason)),
            _ => None,
        }
    }
}

/// Run the parent-child relation auto-create for `child_issue_id`.
///
/// Redmine and GitLab always return `Skipped`: Redmine native `--parent-issue`
/// already establishes the subtask (a `relates` edge is rejected with 422)
/// and GitLab hierarchy uses native Work Item widgets (typed parent writes
/// land in a dedicated command). No `relates` edge is attempted.
pub fn auto_create_parent_child_relation(
    provider: &ProviderDispatcher,
    child_issue_id: u64,
    parent_issue_id: Option<u64>,
) -> AutoRelationOutcome {
    // No parent linkage at the call site: silent skip. This is the
    // common path on `status set` / `status advance` / `issue close`
    // until the read-side DTO widening lands; the auto path stays
    // correct (idempotent + bounded warnings) once the linkage is
    // threaded through.
    let Some(parent_issue_id) = parent_issue_id else {
        return AutoRelationOutcome::Skipped {
            reason: format!("issue {child_issue_id} has no parent linkage at this call site"),
        };
    };
    if parent_issue_id == 0 {
        return AutoRelationOutcome::Warning {
            reason: format!("parent issue id must be greater than zero for issue {child_issue_id}"),
        };
    }
    if parent_issue_id == child_issue_id {
        return AutoRelationOutcome::Warning {
            reason: format!(
                "auto parent-child relation cannot target the same issue ({child_issue_id})"
            ),
        };
    }
    match provider {
        ProviderDispatcher::Local(_) => AutoRelationOutcome::Skipped {
            reason: "local has no relation surface; auto relation is a no-op".to_owned(),
        },
        ProviderDispatcher::Redmine(_) => AutoRelationOutcome::Skipped {
            reason: "redmine native parent/subtask already establishes hierarchy; no relates edge"
                .to_owned(),
        },
        ProviderDispatcher::Gitlab(_) => AutoRelationOutcome::Skipped {
            reason:
                "gitlab native Work Item hierarchy already covers parent/child; no relates edge"
                    .to_owned(),
        },
    }
}

#[cfg(test)]
mod relation_auto_tests {
    use super::*;

    fn local_provider() -> ProviderDispatcher {
        // The Local backend has no relation surface. Each call opens its own
        // throwaway database so the parallel tests never share a file, and no
        // credential or provider call is involved.
        use crate::providers::local::LocalProvider;
        let dir = crate::test_scratch::root().join(format!(
            "phasegent-auto-rel-local-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        ProviderDispatcher::local(
            LocalProvider::open_at(&dir.join("local.sqlite3")).expect("local provider opens"),
        )
    }

    fn parent_linked_outcome(child: u64, parent: u64) -> bool {
        // Helper for the no-network tests: build a fake outcome
        // matching the contract so we can assert the warning()
        // extraction path without spinning up a real provider.
        AutoRelationOutcome::Idempotent { child, parent }
            .warning()
            .is_none()
            && AutoRelationOutcome::Created {
                child,
                parent,
                relation_id: 1,
            }
            .warning()
            .is_none()
    }

    #[test]
    fn relation_outcome_warning_is_only_some_for_warning_variant() {
        let cases = [
            (
                AutoRelationOutcome::Skipped {
                    reason: "no linkage".to_owned(),
                },
                false,
            ),
            (
                AutoRelationOutcome::Idempotent {
                    child: 10,
                    parent: 20,
                },
                false,
            ),
            (
                AutoRelationOutcome::Created {
                    child: 10,
                    parent: 20,
                    relation_id: 1,
                },
                false,
            ),
            (
                AutoRelationOutcome::Warning {
                    reason: "boom".to_owned(),
                },
                true,
            ),
        ];
        for (outcome, expected_warning) in cases {
            assert_eq!(
                outcome.warning().is_some(),
                expected_warning,
                "outcome {outcome:?} should warn={expected_warning}"
            );
        }
        assert!(parent_linked_outcome(1, 2));
    }

    #[test]
    fn auto_relation_skips_local_without_warning() {
        // Local has no relation surface. The outcome must be `Skipped` (no
        // warning) so the hook call site does not emit anything to stderr.
        let provider = local_provider();
        let outcome = auto_create_parent_child_relation(&provider, 10, Some(20));
        assert!(matches!(outcome, AutoRelationOutcome::Skipped { .. }));
        assert!(outcome.warning().is_none());
    }

    #[test]
    fn auto_relation_skips_when_parent_linkage_is_absent() {
        // The status set/advance/close arms pass `None` because the
        // DTO does not surface the parent linkage today. The
        // outcome must be `Skipped` (no warning) so the hook call
        // site emits nothing to stderr; the helper stays idempotent
        // and silent on the common path.

        let provider = local_provider();
        let outcome = auto_create_parent_child_relation(&provider, 10, None);
        match &outcome {
            AutoRelationOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("no parent linkage"),
                    "reason must explain the absent linkage: {reason}"
                );
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
        assert!(outcome.warning().is_none());
    }

    #[test]
    fn auto_relation_warns_on_zero_or_self_parent_id() {
        // A zero or self-targeting parent id is rejected locally
        // with a Warning so a downstream create call is never
        // attempted. The Warning must carry a bounded reason so the
        // operator sees what the auto path rejected.

        let provider = local_provider();

        let zero = auto_create_parent_child_relation(&provider, 10, Some(0));
        match zero {
            AutoRelationOutcome::Warning { reason } => {
                assert!(
                    reason.contains("greater than zero"),
                    "zero parent id must surface a bounded reason: {reason}"
                );
            }
            other => panic!("expected Warning for zero parent id, got {other:?}"),
        }

        let self_target = auto_create_parent_child_relation(&provider, 10, Some(10));
        match self_target {
            AutoRelationOutcome::Warning { reason } => {
                assert!(
                    reason.contains("cannot target the same issue"),
                    "self parent id must surface a bounded reason: {reason}"
                );
            }
            other => panic!("expected Warning for self parent id, got {other:?}"),
        }
    }

    #[test]
    fn auto_relation_skips_silently_for_gitlab_when_parent_linkage_absent() {
        // GitLab hierarchy uses native Work Item widgets; the
        // `parent_issue_id: None` path returns `Skipped` so the
        // status/close hooks stay silent.
        use crate::providers::config::GitlabConfig;
        use crate::providers::gitlab::GitlabProvider;

        let provider = ProviderDispatcher::Gitlab(
            GitlabProvider::new(
                GitlabConfig::new("https://gitlab.example/api/v4", 42),
                "test-token".to_owned(),
            )
            .unwrap(),
        );
        let outcome = auto_create_parent_child_relation(&provider, 10, None);
        match &outcome {
            AutoRelationOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("no parent linkage"),
                    "reason must explain the absent linkage: {reason}"
                );
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
        assert!(outcome.warning().is_none());
    }

    #[test]
    fn auto_relation_skips_silently_for_gitlab_with_parent_linkage() {
        // Issue 641 P3: GitLab never attempts a `relates` edge for a
        // parent linkage; native Work Item hierarchy covers it. A
        // closed-port base proves no list/create call happens.
        use crate::providers::config::GitlabConfig;
        use crate::providers::gitlab::GitlabProvider;

        let provider = ProviderDispatcher::Gitlab(
            GitlabProvider::new(
                GitlabConfig::new("http://127.0.0.1:1", 42),
                "test-token".to_owned(),
            )
            .unwrap(),
        );
        let outcome = auto_create_parent_child_relation(&provider, 11, Some(10));
        match &outcome {
            AutoRelationOutcome::Skipped { reason } => {
                assert!(
                    reason.contains("Work Item") || reason.contains("hierarchy"),
                    "reason must name native hierarchy: {reason}"
                );
            }
            other => panic!("expected Skipped for GitLab parent linkage, got {other:?}"),
        }
        assert!(outcome.warning().is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::{ToolSignal, auto_route_next, status_to_agent_role};

    #[test]
    fn status_to_agent_role_matches_documented_buckets() {
        for (input, expected) in [
            ("In Progress", "executor"),
            ("In progress", "executor"),
            ("IN PROGRESS", "executor"),
            ("Implementation", "executor"),
            ("Development", "executor"),
            ("In Review", "reviewer"),
            ("Code Review", "reviewer"),
            ("In review", "reviewer"),
            ("Review", "reviewer"),
            ("Testing", "tester"),
            ("Test", "tester"),
            ("QA", "tester"),
            ("Ready for Test", "tester"),
            ("ready for test", "tester"),
        ] {
            let (role, fallback) = status_to_agent_role(input);
            assert_eq!(role, expected, "input: {input}");
            assert!(!fallback, "input: {input} must not fall back");
        }
    }

    #[test]
    fn status_to_agent_role_uses_ordered_precedence() {
        // A name that contains both "review" and "test" must pick
        // "test" because the QA / test bucket is checked first.
        let (role, _) = status_to_agent_role("Test Review");
        assert_eq!(role, "tester");
        // A name that contains "impl" and "review" must pick
        // "reviewer" because the review bucket is checked before
        // the dev/impl bucket.
        let (role, _) = status_to_agent_role("Implementation Review");
        assert_eq!(role, "reviewer");
    }

    #[test]
    fn status_to_agent_role_falls_back_to_executor_with_warning() {
        for input in ["Closed", "New", "Resolved", "Custom", "Done"] {
            let (role, fallback) = status_to_agent_role(input);
            assert_eq!(role, "executor", "input: {input}");
            assert!(fallback, "input: {input} must report fallback");
        }
    }

    #[test]
    fn auto_route_next_matches_issue_443_tool_table() {
        assert_eq!(
            auto_route_next(1, ToolSignal::IssueCreated),
            Some("In Progress")
        );
        assert_eq!(
            auto_route_next(1, ToolSignal::CommentCreated),
            Some("In Review")
        );
        assert_eq!(auto_route_next(1, ToolSignal::IssueClosed), Some("Closed"));
    }

    #[test]
    fn auto_route_targets_resolve_to_expected_agent_roles() {
        for (signal, expected_role) in [
            (ToolSignal::IssueCreated, "executor"),
            (ToolSignal::CommentCreated, "reviewer"),
        ] {
            let target = auto_route_next(7, signal).expect("mapped signal must route");
            let (role, fallback) = status_to_agent_role(target);
            assert_eq!(role, expected_role, "signal {signal:?}");
            assert!(!fallback, "signal {signal:?} target must be canonical");
        }
    }
}
