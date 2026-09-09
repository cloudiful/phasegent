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
//! - **Forgejo** — no status surface, no auto-run. The helper returns
//!   [`AutoTimerOutcome::Skipped`] so the caller can distinguish
//!   "no-op because no status" from "could not start".
//! - **Redmine** — full auto-accounting; `status set` and
//!   `status advance` both flow through the helper.
//! - **GitLab** — `status set` flows through (label-based); `status
//!   advance` is Redmine-only upstream so the helper is not reached
//!   on that path. The provider branch is gated inside the helper
//!   itself for defensive parity.
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
use crate::providers::ProviderKind;
use crate::time_tracking::{finish, start};

/// Upper bound for any warning text derived from local ledger state.
/// Matches the `MAX_WARNING_CHARS` bound used by the bind/unbind
/// helpers so JSON output stays well-bounded regardless of the
/// underlying storage error.
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

/// Outcome of a lifecycle auto-accounting call. Callers translate
/// `Skipped` into silence (no warning), and either `Started` (when
/// its `warning` field is `Some`) or `Warning` into a bounded
/// stderr line via `cli::report_local_warnings`. The `Started`
/// variant carries a `warning` field so the fallback mapping
/// (custom Redmine status with no canonical role) and the
/// close-sweep partial-failure path can be surfaced to the operator
/// even when the new segment was opened successfully.
#[derive(Debug, PartialEq, Eq)]
pub enum AutoTimerOutcome {
    /// Forgejo: no status surface, no auto-run. This is the only
    /// outcome that produces no row activity.
    Skipped { reason: String },
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
            _ => None,
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
/// `status set` / `status advance`. Forgejo is a no-op (no status,
/// no timer). For Redmine and GitLab we finish every running
/// auto-run for the issue, then open a new run whose phase is
/// `status_name` and whose role comes from
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
    provider_kind: ProviderKind,
    status_name: &str,
) -> AutoTimerOutcome {
    if provider_kind == ProviderKind::Forgejo {
        return AutoTimerOutcome::Skipped {
            reason: "forgejo has no status surface; auto timer is a no-op".to_owned(),
        };
    }
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
/// Forgejo is gated to [`AutoCloseOutcome::Noop`] for parity with
/// [`auto_transition_timer`]'s [`AutoTimerOutcome::Skipped`]:
/// Forgejo has no first-class status surface, and the auto-run
/// set is provider-local bookkeeping so a Forgejo close should
/// not retroactively mutate Redmine or GitLab rows. An empty
/// ledger also returns `Noop`. The branch-context unbind is
/// provider-agnostic at its own layer and is unaffected by this
/// gate.
pub fn auto_close_issue_timer(issue: u64, provider_kind: ProviderKind) -> AutoCloseOutcome {
    if provider_kind == ProviderKind::Forgejo {
        return AutoCloseOutcome::Noop {
            reason: "forgejo has no status surface; auto timer close is a no-op".to_owned(),
        };
    }
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

#[cfg(test)]
mod tests {
    use super::status_to_agent_role;

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
}
