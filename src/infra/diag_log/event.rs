//! The typed diagnostic event of schema version 1.
//!
//! An event carries metadata only: no path, no session or issue identifier, no
//! credential, no URL, no request or response body, and no raw error text.
//! Every categorical field is a closed vocabulary, so an event can never carry
//! an arbitrary string that a caller could fill with something private.
//!
//! The vocabulary is deliberately small and fixed:
//!
//! * the event kinds name the lifecycle of one run;
//! * the stage and operation names are the prune pipeline's own names;
//! * the outcome codes are the prune result codes;
//! * the reason codes are the prune keep-reason codes.
//!
//! A reader rejects any event that carries a field outside this schema, so an
//! unknown or unsafe field is never echoed back to a caller.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The schema version every event of this contract carries.
pub(crate) const SCHEMA_VERSION: u32 = 1;

/// The only command this logger writes for.
pub(crate) const COMMAND_WORKTREE_PRUNE: &str = "worktree_prune";

/// The run mode of a dry-run pass.
pub(crate) const MODE_DRY_RUN: &str = "dry_run";
/// The run mode of a removal pass.
pub(crate) const MODE_REMOVE: &str = "remove";

/// The lifecycle of one event inside a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum EventKind {
    /// The run started; the boundary before any work.
    RunStart,
    /// The run ended, successfully or with a typed error.
    RunEnd,
    /// A pipeline stage started.
    StageStart,
    /// A pipeline stage ended.
    StageEnd,
    /// A boundary operation started.
    OperationStart,
    /// A boundary operation ended.
    OperationEnd,
    /// One candidate directory was decided.
    CandidateResult,
}

impl EventKind {
    /// The event kind's stable name.
    #[cfg(test)]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::RunStart => "run_start",
            Self::RunEnd => "run_end",
            Self::StageStart => "stage_start",
            Self::StageEnd => "stage_end",
            Self::OperationStart => "operation_start",
            Self::OperationEnd => "operation_end",
            Self::CandidateResult => "candidate_result",
        }
    }
}

/// The pipeline stages, in the fixed order the report lists them.
pub(crate) const STAGES: [&str; 5] = [
    "scope_registry",
    "scan",
    "family_move",
    "verification",
    "cache_delete_reclaim",
];

/// The boundary operations, in the fixed order the report lists them.
pub(crate) const OPERATIONS: [&str; 5] = [
    "closure",
    "host_active",
    "host_get",
    "host_list",
    "host_move",
];

/// The outcome codes: the run outcomes and the candidate result codes.
pub(crate) const OUTCOMES: [&str; 9] = [
    "ok",
    "error",
    "dry_run",
    "pruned",
    "prunable",
    "absent",
    "skipped_protected",
    "skipped_issue",
    "failed",
];

/// The safe reason codes a kept candidate may carry.
pub(crate) const REASONS: [&str; 32] = [
    "absent",
    "cache_changed",
    "cache_cleanup_failed",
    "cache_reappeared",
    "checkout_subdirectory",
    "cleanliness_probe_failed",
    "closure_authentication_denied",
    "closure_missing_configuration",
    "closure_provider_unavailable",
    "closure_scope_mismatch",
    "directory_vanished",
    "dirty",
    "foreign_checkout",
    "host_lookup_error",
    "issue_open",
    "main_checkout",
    "main_subdirectory",
    "operation_lock",
    "other_protected",
    "path_changed",
    "path_probe_error",
    "path_reappeared",
    "probe_error",
    "registration_changed",
    "registry_cleanup_failed",
    "registry_read_failed",
    "registry_record_missing",
    "repository_unavailable",
    "running_session",
    "session_family",
    "unattributed_legacy",
    "worktree_remove_failed",
];

/// Whether `name` is one of the fixed stage names.
pub(crate) fn is_known_stage(name: &str) -> bool {
    STAGES.contains(&name)
}

/// Whether `name` is one of the fixed operation names.
pub(crate) fn is_known_operation(name: &str) -> bool {
    OPERATIONS.contains(&name)
}

/// Whether `name` is one of the fixed outcome codes.
pub(crate) fn is_known_outcome(name: &str) -> bool {
    OUTCOMES.contains(&name)
}

/// Whether `name` is one of the fixed reason codes.
pub(crate) fn is_known_reason(name: &str) -> bool {
    REASONS.contains(&name)
}

/// One measured name of the safe timing report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunTimingEntry {
    /// How many calls the measurement counted.
    pub calls: u64,
    /// The summed duration of those calls.
    pub duration_ms: f64,
}

/// The safe timing report of one run: the wall clock, the pipeline stages and
/// the nested boundary operations. It carries names, counts and durations only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunTimings {
    /// The wall clock of the run.
    pub total_ms: f64,
    /// The measured pipeline stages.
    pub stages: BTreeMap<String, RunTimingEntry>,
    /// The measured boundary operations.
    pub operations: BTreeMap<String, RunTimingEntry>,
}

/// The aggregate counts of one run. They are counts, never the paths the run
/// touched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunCounts {
    /// The scanned directories.
    pub scanned: u64,
    /// The removed directories.
    pub removed: u64,
    /// The eligible directories of a preview.
    pub eligible: u64,
    /// The absent registrations.
    pub absent: u64,
    /// The directories kept by a local guard.
    pub kept_protected: u64,
    /// The directories kept by an issue state.
    pub kept_issue: u64,
    /// The failed candidates.
    pub failed: u64,
}

/// One typed diagnostic event.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DiagEvent {
    /// The schema version of this event.
    pub schema_version: u32,
    /// The identifier of the run this event belongs to.
    pub run_id: String,
    /// The position of this event inside its run.
    pub seq: u64,
    /// The wall-clock time of the event, in Unix milliseconds.
    pub ts_ms: i64,
    /// The milliseconds since the run started.
    pub elapsed_ms: f64,
    /// The command the run belongs to.
    pub command: String,
    /// The mode of the run.
    pub mode: String,
    /// The ordinal of the repository inside the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<u64>,
    /// The ordinal of the candidate inside the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate: Option<u64>,
    /// The lifecycle of this event.
    pub kind: EventKind,
    /// The pipeline stage this event belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// The boundary operation this event belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// The outcome code of this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    /// The safe reason code of this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The measured duration of this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<f64>,
    /// The measured call count of this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls: Option<u64>,
    /// The aggregate counts of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<RunCounts>,
    /// The measured timings of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<RunTimings>,
}

impl DiagEvent {
    /// Whether the event carries only the metadata this schema allows: a known
    /// version, command and mode, a safe run id, and categorical fields from
    /// the fixed vocabularies.
    pub(crate) fn is_well_formed(&self) -> bool {
        if self.schema_version != SCHEMA_VERSION {
            return false;
        }
        if self.command != COMMAND_WORKTREE_PRUNE {
            return false;
        }
        if self.mode != MODE_DRY_RUN && self.mode != MODE_REMOVE {
            return false;
        }
        if !is_safe_token(&self.run_id) {
            return false;
        }
        if let Some(stage) = &self.stage
            && !is_known_stage(stage)
        {
            return false;
        }
        if let Some(operation) = &self.operation
            && !is_known_operation(operation)
        {
            return false;
        }
        if let Some(outcome) = &self.outcome
            && !is_known_outcome(outcome)
        {
            return false;
        }
        if let Some(reason) = &self.reason
            && !is_known_reason(reason)
        {
            return false;
        }
        // The nested timing maps carry fixed names only. A map key is a
        // string, so an unknown one could hold a path, a secret, or a raw
        // error; the whole event is rejected instead of echoing it.
        if let Some(timings) = &self.timings
            && (timings.stages.keys().any(|name| !is_known_stage(name))
                || timings
                    .operations
                    .keys()
                    .any(|name| !is_known_operation(name)))
        {
            return false;
        }
        true
    }
}

/// Whether `value` is an ASCII-safe token: 1..=128 characters of letters,
/// digits, dot, underscore, colon, or hyphen.
pub(crate) fn is_safe_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | ':' | '-')
        })
}
