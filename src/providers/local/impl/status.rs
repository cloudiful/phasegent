//! Local status catalogue + transition policy.

use super::LocalProvider;
use super::model::{is_closed_status, local_sql, local_statuses, now_epoch_seconds};
use crate::providers::RedmineIssueStatus;
use crate::providers::api::{ForgejoError, IssueSummary};
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::{
    STATUS_POLICY_CAVEAT, STATUS_POLICY_SOURCE, StatusNextReport, StatusRef,
    StatusTransitionOutcome, TransitionVerdict, canonical_allowed_next, canonical_status_name,
    evaluate_transition,
};

/// Allowed next statuses for a canonical status, derived from
/// `infra::local_schema::STATUS_TRANSITION_SEED` so the local
/// workflow and the seeds share one literal.
pub(crate) fn allowed_next_for(status: &str) -> Vec<&'static str> {
    crate::infra::local_schema::STATUS_TRANSITION_SEED
        .iter()
        .filter(|(from, _)| *from == status)
        .map(|(_, to)| *to)
        .collect()
}

/// True for same-status no-ops and seeded edges; false otherwise.
pub(crate) fn is_transition_allowed(from: &str, to: &str) -> bool {
    if from == to {
        return true;
    }
    crate::infra::local_schema::STATUS_TRANSITION_SEED
        .iter()
        .any(|(seed_from, seed_to)| *seed_from == from && *seed_to == to)
}

impl LocalProvider {
    pub fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, ForgejoError> {
        Ok(local_statuses())
    }

    fn current_status_name(&self, number: u64) -> Result<String, ForgejoError> {
        if number == 0 {
            return Err(ForgejoError::config(
                "issue number must be greater than zero",
            ));
        }
        self.with_conn("issue status next", |conn| {
            conn.query_row(
                local_sql("get_issue_status"),
                rusqlite::params![number as i64],
                |row| row.get(0),
            )
        })
        .map_err(|error| {
            let message = error.to_string();
            if message.contains("QueryReturnedNoRows") || message.contains("no rows") {
                ForgejoError::not_found(
                    "issue status next",
                    &format!("issue {number} was not found"),
                )
            } else {
                error
            }
        })
    }

    /// Answer "where can this issue go next" from the canonical policy,
    /// resolved against the static local catalogue. Read-only.
    pub fn status_next(&self, number: u64) -> Result<StatusNextReport, ForgejoError> {
        let statuses = self.list_issue_statuses()?;
        let current_name = self.current_status_name(number)?;
        let current = status_ref_for_name(&statuses, &current_name);
        let policy_names = canonical_allowed_next(&current_name);
        let mut allowed_next = Vec::new();
        let mut missing = Vec::new();
        for name in policy_names.unwrap_or(&[]) {
            match statuses
                .iter()
                .find(|status| status.name.eq_ignore_ascii_case(name))
            {
                Some(status) => allowed_next.push(StatusRef::from_installation(status)),
                None => missing.push((*name).to_owned()),
            }
        }
        Ok(StatusNextReport {
            issue: number,
            current,
            allowed_next,
            allowed_next_missing_on_server: missing,
            policy_source: STATUS_POLICY_SOURCE,
            advisory: policy_names.is_none(),
            caveat: STATUS_POLICY_CAVEAT,
            recovery: local_recovery_hint(number),
        })
    }

    /// Move a local issue to any status resolved by validated name or id.
    /// Same-status is an idempotent read-back; closed targets stamp
    /// `closed_at`, open targets clear it.
    pub fn set_issue_status(
        &self,
        number: u64,
        status_id: u64,
    ) -> Result<IssueSummary, ForgejoError> {
        if number == 0 {
            return Err(ForgejoError::config(
                "issue number must be greater than zero",
            ));
        }
        let statuses = self.list_issue_statuses()?;
        let target = statuses
            .iter()
            .find(|status| status.id == status_id)
            .ok_or_else(|| {
                ForgejoError::config(format!("local status id {status_id} was not found"))
            })?;
        let target_name = target.name.clone();
        let target_closed = target.is_closed;
        // Friendly existence check so a missing issue is not-found.
        self.current_status_name(number).map_err(|error| {
            let message = error.to_string();
            if message.contains("was not found") {
                ForgejoError::not_found(
                    "issue status update",
                    &format!("issue {number} was not found"),
                )
            } else {
                error
            }
        })?;
        let now = now_epoch_seconds();
        let closed_at: Option<i64> = if target_closed { Some(now) } else { None };
        let target_owned = target_name.clone();
        self.with_conn("issue status update", |conn| {
            let changed = conn.execute(
                local_sql("update_issue_status"),
                rusqlite::params![target_owned, now, closed_at, number as i64],
            )?;
            if changed == 0 {
                return Err(rusqlite::Error::QueryReturnedNoRows);
            }
            Ok(())
        })
        .map_err(|error| {
            let message = error.to_string();
            if message.contains("QueryReturnedNoRows") || message.contains("no rows") {
                ForgejoError::not_found(
                    "issue status update",
                    &format!("issue {number} was not found"),
                )
            } else {
                error
            }
        })?;
        self.get_issue(number)
    }

    /// Move a local issue to `target_value` after a policy preflight,
    /// mirroring the Redmine `advance` semantics on the static catalogue.
    pub fn advance_issue_status(
        &self,
        number: u64,
        target_value: &str,
    ) -> Result<StatusTransitionOutcome, ForgejoError> {
        let operation = "issue status advance";
        let statuses = self.list_issue_statuses()?;
        let target = RedmineProvider::select_status_by_value(&statuses, target_value)?;
        let current_name = self.current_status_name(number).map_err(|error| {
            let message = error.to_string();
            if message.contains("was not found") {
                ForgejoError::not_found(operation, &format!("issue {number} was not found"))
            } else {
                error
            }
        })?;
        let from = status_ref_for_name(&statuses, &current_name);
        let to = StatusRef::from_installation(target);
        let verdict = evaluate_transition(&current_name, &target.name);
        let advisory = matches!(verdict, TransitionVerdict::Advisory { .. });
        match &verdict {
            TransitionVerdict::NoOp => {
                return Ok(StatusTransitionOutcome {
                    issue: number,
                    changed: false,
                    from,
                    to,
                    policy_source: STATUS_POLICY_SOURCE,
                    advisory: false,
                    caveat: None,
                    issue_summary: None,
                });
            }
            TransitionVerdict::Forbidden { allowed_next } => {
                return Err(ForgejoError::request(
                    operation,
                    forbidden_message(number, &current_name, &target.name, allowed_next),
                ));
            }
            TransitionVerdict::Allowed | TransitionVerdict::Advisory { .. } => {}
        }
        let summary = self.set_issue_status(number, target.id)?;
        Ok(StatusTransitionOutcome {
            issue: number,
            changed: true,
            from,
            to,
            policy_source: STATUS_POLICY_SOURCE,
            advisory,
            caveat: advisory.then_some(STATUS_POLICY_CAVEAT),
            issue_summary: Some(summary),
        })
    }
}

fn status_ref_for_name(statuses: &[RedmineIssueStatus], name: &str) -> StatusRef {
    match statuses.iter().find(|status| status.name == name) {
        Some(status) => StatusRef::from_installation(status),
        None => StatusRef {
            id: None,
            name: name.to_owned(),
            is_closed: Some(is_closed_status(name)),
            canonical: canonical_status_name(name).is_some(),
        },
    }
}

fn local_recovery_hint(number: u64) -> String {
    format!("phasegent --role orchestrator --provider local status next {number}")
}

fn forbidden_message(
    number: u64,
    current: &str,
    target: &str,
    allowed_next: &[&'static str],
) -> String {
    let allowed = if allowed_next.is_empty() {
        "<none: terminal status>".to_owned()
    } else {
        allowed_next.join(", ")
    };
    format!(
        "transition rejected before any write: current status '{current}' -> target status '{target}' is not allowed by policy {STATUS_POLICY_SOURCE}; allowed_next=[{allowed}]; {STATUS_POLICY_CAVEAT} recovery: {}",
        local_recovery_hint(number)
    )
}
