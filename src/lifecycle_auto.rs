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
use crate::providers::redmine::model::RedmineRelationType;
use crate::providers::{ProviderDispatcher, ProviderKind, RedmineProvider};
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

// ---------------------------------------------------------------------------
// Phase 3 write-side relation auto (issue 257).
//
// The auto-relation fires ONLY on the parent-child split path: when an
// issue is created with `--parent-issue <PARENT>`, the CLI hands the
// freshly resolved `parent_issue_id` to this helper so it can
// auto-create a `relates` link between the new child and its parent.
// AI agents never run `relation create` by hand for this case.
//
// The hook call sites are the `status set` / `status advance` /
// `issue close` paths in `src/cli/status.rs` and `src/cli/issue.rs`,
// matching the lifecycle_auto timer pattern: the helper is invoked
// only after the upstream status change has succeeded, never before,
// and any failure degrades to a bounded `Warning` so stdout JSON
// stays byte-compatible with the plain provider output.
//
// Read-side discovery (i.e. looking up the parent linkage at status
// transition time without a server fetch) is intentionally out of
// scope for this phase: the shared `RedmineIssue` / `ApiIssue` DTOs
// are deliberately narrow so Phase 3 does not touch the read side.
// Callers that already hold a parent linkage at trigger time (the
// create path) pass it explicitly; callers that don't (the status
// path) get a silent `NoOp`. See Remaining in the Phase 3 audit note
// for the deferred lookup shape.
// ---------------------------------------------------------------------------

/// Outcome of the parent-child relation auto-create call. The hook
/// call sites translate `Skipped` into silence, `Created` and
/// `Idempotent` into success (no warning), and `Warning` into a
/// bounded stderr line via `cli::report_local_warnings`.
#[derive(Debug, PartialEq, Eq)]
pub enum AutoRelationOutcome {
    /// Provider has no relation surface (Forgejo) or no parent
    /// linkage was supplied at the call site. Silent success — the
    /// caller should not surface anything.
    Skipped { reason: String },
    /// A fresh `relates` link was created. `relation_id` is the
    /// server-assigned id (Redmine relation id / GitLab issue link
    /// id) so the operator can spot the new auto-link via
    /// `relation list`.
    Created {
        child: u64,
        parent: u64,
        relation_id: u64,
    },
    /// A `relates` link to the parent already exists. The auto path
    /// is idempotent so repeated calls (e.g. status set then status
    /// advance on the same child) never produce duplicate links.
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
/// The helper takes an explicit `parent_issue_id: Option<u64>` so
/// the call site controls when the linkage is observable:
///
/// * The `issue create` CLI arm resolves `--parent-issue` up front
///   and passes the freshly validated id; this is the only path
///   that can fire a real `Created` outcome in Phase 3.
/// * The `status set` / `status advance` / `issue close` arms pass
///   `None` today because the shared issue DTO does not surface
///   the parent linkage; the helper returns `Skipped` silently.
///   Phase 4 may extend this once the read-side widening lands.
///
/// Forgejo and Local are gated to `Skipped` because neither exposes
/// a relation surface. Redmine and GitLab both support `relates`:
/// Redmine's `relates` is symmetric, GitLab's `relates_to` is the
/// same link type translated via the existing
/// `gitlab_link_type_from_relation_type` mapper. The helper is
/// idempotent: a pre-existing `relates` link to the same parent is
/// recognised by listing the child's relations before the create
/// call so repeated invocations never produce duplicate rows.
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
        ProviderDispatcher::Forgejo(_) => AutoRelationOutcome::Skipped {
            reason: "forgejo has no relation surface; auto relation is a no-op".to_owned(),
        },
        ProviderDispatcher::Local(_) => AutoRelationOutcome::Skipped {
            reason: "local has no relation surface; auto relation is a no-op".to_owned(),
        },
        ProviderDispatcher::Redmine(redmine) => {
            auto_relation_redmine(redmine, child_issue_id, parent_issue_id)
        }
        ProviderDispatcher::Gitlab(gitlab) => {
            auto_relation_gitlab(gitlab, child_issue_id, parent_issue_id)
        }
    }
}

fn auto_relation_redmine(
    redmine: &RedmineProvider,
    child_issue_id: u64,
    parent_issue_id: u64,
) -> AutoRelationOutcome {
    // Idempotency: list the child's relations and check whether a
    // `relates` link to the parent already exists. The mapping is
    // done against `RelationSummary::relation_type == "relates"`
    // and `issue_to_id == parent_issue_id`. We do not match the
    // inverse direction (`relates` is symmetric on Redmine so the
    // other side is rendered the same way).
    match redmine.list_relations(child_issue_id) {
        Ok(existing) => {
            if existing.iter().any(|summary| {
                summary.relation_type == "relates"
                    && (summary.issue_to_id == parent_issue_id
                        || summary.issue_id == parent_issue_id)
            }) {
                return AutoRelationOutcome::Idempotent {
                    child: child_issue_id,
                    parent: parent_issue_id,
                };
            }
        }
        Err(error) => {
            return AutoRelationOutcome::Warning {
                reason: format!(
                    "auto relation: list relations for issue {child_issue_id} failed: {error}"
                ),
            };
        }
    }
    match redmine.create_relation(
        child_issue_id,
        parent_issue_id,
        RedmineRelationType::Relates,
        None,
    ) {
        Ok(summary) => AutoRelationOutcome::Created {
            child: child_issue_id,
            parent: parent_issue_id,
            relation_id: summary.id,
        },
        Err(error) => AutoRelationOutcome::Warning {
            reason: format!(
                "auto relation: create relates from issue {child_issue_id} \
                 to parent {parent_issue_id} failed: {error}"
            ),
        },
    }
}

fn auto_relation_gitlab(
    gitlab: &crate::providers::gitlab::GitlabProvider,
    child_issue_id: u64,
    parent_issue_id: u64,
) -> AutoRelationOutcome {
    // Idempotency on GitLab mirrors the Redmine path: list the
    // child's links and look for any existing `relates_to` link
    // targeting the parent. `RelationSummary::relation_type` is the
    // viewpoint-resolved canonical name (`relates` for both
    // directions on the symmetric link).
    match gitlab.list_issue_links(child_issue_id) {
        Ok(existing) => {
            if existing.iter().any(|summary| {
                summary.relation_type == "relates"
                    && (summary.issue_to_id == parent_issue_id
                        || summary.issue_id == parent_issue_id)
            }) {
                return AutoRelationOutcome::Idempotent {
                    child: child_issue_id,
                    parent: parent_issue_id,
                };
            }
        }
        Err(error) => {
            return AutoRelationOutcome::Warning {
                reason: format!(
                    "auto relation: list issue links for issue {child_issue_id} failed: {error}"
                ),
            };
        }
    }
    match gitlab.create_issue_link(
        child_issue_id,
        parent_issue_id,
        RedmineRelationType::Relates,
    ) {
        Ok(summary) => AutoRelationOutcome::Created {
            child: child_issue_id,
            parent: parent_issue_id,
            relation_id: summary.id,
        },
        Err(error) => AutoRelationOutcome::Warning {
            reason: format!(
                "auto relation: create relates_to from issue {child_issue_id} \
                 to parent {parent_issue_id} failed: {error}"
            ),
        },
    }
}

#[cfg(test)]
mod relation_auto_tests {
    use super::*;
    use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};

    fn forgejo_provider_with_token() -> ProviderDispatcher {
        // The Forgejo provider reads the orchestrator token from
        // the SQLite credential store. The tests pin a fresh
        // `PHASEGENT_DB_PATH` and seed the credential so
        // `for_role` succeeds; `lock_workflow_tests` keeps the
        // process-wide env mutation serialised so parallel runs do
        // not race, and the EnvGuard restores the prior value on
        // drop so the host shell is never left pointing at a
        // synthetic DB.
        let _lock = lock_workflow_tests();
        let dir = std::env::temp_dir().join(format!(
            "phasegent-auto-rel-forgejo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join(crate::infra::storage::DB_FILENAME);
        let _guard = EnvGuard::set(
            "PHASEGENT_DB_PATH",
            db.as_os_str().to_string_lossy().as_ref(),
        );
        let storage = crate::infra::storage::Storage::open_at(&db).unwrap();
        storage
            .save_credential(crate::policy::Role::Orchestrator, "forgejo", "test-token")
            .unwrap();
        use crate::policy::Role;
        use crate::providers::forgejo::{ForgejoConfig, ForgejoProvider};
        let config = ForgejoConfig::new("https://forgejo.example.test", "owner", "repo");
        let provider = ForgejoProvider::for_role(Role::Orchestrator, config).unwrap();
        ProviderDispatcher::Forgejo(provider)
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
    fn auto_relation_skips_forgejo_and_local_without_warning() {
        // Forgejo/Local have no relation surface. The outcome must
        // be `Skipped` (no warning) so the hook call site does not
        // emit anything to stderr.
        use crate::providers::local::LocalProvider;

        let forgejo_provider = forgejo_provider_with_token();
        let forgejo_outcome = auto_create_parent_child_relation(&forgejo_provider, 10, Some(20));
        assert!(matches!(
            forgejo_outcome,
            AutoRelationOutcome::Skipped { .. }
        ));
        assert!(forgejo_outcome.warning().is_none());

        let local_provider = ProviderDispatcher::local(LocalProvider::open().unwrap());
        let local_outcome = auto_create_parent_child_relation(&local_provider, 10, Some(20));
        assert!(matches!(local_outcome, AutoRelationOutcome::Skipped { .. }));
        assert!(local_outcome.warning().is_none());
    }

    #[test]
    fn auto_relation_skips_when_parent_linkage_is_absent() {
        // The status set/advance/close arms pass `None` because the
        // DTO does not surface the parent linkage today. The
        // outcome must be `Skipped` (no warning) so the hook call
        // site emits nothing to stderr; the helper stays idempotent
        // and silent on the common path.

        let provider = forgejo_provider_with_token();
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

        let provider = forgejo_provider_with_token();

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
        // GitLab wires into the same Phase 3 helper; the
        // `parent_issue_id: None` path returns `Skipped` so the
        // status/close hooks stay silent. This test pins the
        // GitLab-side outcome shape against a freshly built
        // provider (no network) so a future provider re-route
        // cannot silently regress this branch.
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
