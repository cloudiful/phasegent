use super::{
    ForgejoProvider,
    model::{Dependency, Issue, Repository},
};
use crate::providers::api::PhasegentError;
use crate::providers::redmine::model::{RedmineIssueStatus, StatusNextReport, StatusRef};
use reqwest::Method;
use serde_json::json;

/// The native open state of a Forgejo issue.
pub(crate) const OPEN: &str = "Open";
/// The native closed state of a Forgejo issue.
pub(crate) const CLOSED: &str = "Closed";

/// Provider-local numeric selectors for the two synthetic states. `Closed`
/// keeps the historical `0` and `Open` uses `1`. They are Forgejo-local
/// selectors, not server identifiers, and they are not stable.
pub(crate) const OPEN_ID: u64 = 1;
pub(crate) const CLOSED_ID: u64 = 0;

/// The policy identity of the native two-state contract, so a caller can
/// tell it apart from the canonical Redmine/Local phase graph.
pub(crate) const NATIVE_POLICY_SOURCE: &str = "phasegent/forgejo-native-state@v1";

/// The caveat attached to every native-state answer: a Forgejo issue is
/// open or closed, the phase detail lives in the phasegent record, and no
/// status label is required, read or written.
pub(crate) const NATIVE_POLICY_CAVEAT: &str = "Native state only: a Forgejo issue is open or closed, the phase detail lives in the phasegent record, and no status label is required, read or written.";

impl ForgejoProvider {
    pub(super) fn require_closed_dependencies(&self, number: u64) -> Result<(), PhasegentError> {
        if number == 0 {
            return Err(PhasegentError::config("issue number must be positive"));
        }
        let index = number.to_string();
        let dependencies: Vec<Dependency> = self.collect(
            &["issues", &index, "dependencies"],
            &[],
            "issue dependencies",
        )?;
        if dependencies
            .iter()
            .any(|dependency| dependency.state != "closed")
        {
            return Err(PhasegentError::config(
                "Forgejo issue has open or unverified dependencies; close them before retrying",
            ));
        }
        Ok(())
    }

    fn status_issue(&self, number: u64, repo: &Repository) -> Result<Issue, PhasegentError> {
        if number == 0 {
            return Err(PhasegentError::config("issue number must be positive"));
        }
        let index = number.to_string();
        let issue: Issue = self.http.get(
            self.http.request(Method::GET, &["issues", &index])?,
            "status issue get",
        )?;
        self.summary(issue.clone(), repo, Some(number))?;
        Ok(issue)
    }

    /// The native state of one issue and the single opposite state it may
    /// move to. No label is read and no phase graph is consulted.
    pub(crate) fn status_next(&self, number: u64) -> Result<StatusNextReport, PhasegentError> {
        let repo = self.repository()?;
        let issue = self.status_issue(number, &repo)?;
        let name = native_state_name(&issue.state)?;
        let opposite = opposite_state(name);
        Ok(StatusNextReport {
            issue: number,
            current: native_state_ref(name),
            allowed_next: vec![native_state_ref(opposite)],
            allowed_next_missing_on_server: Vec::new(),
            policy_source: NATIVE_POLICY_SOURCE,
            advisory: false,
            caveat: NATIVE_POLICY_CAVEAT,
            recovery: format!("Inspect the native Forgejo state of issue {number} and retry."),
        })
    }

    /// Move one issue to the native state a target names.
    ///
    /// The target is validated first, so an unsupported state refuses before
    /// any request. Closing runs the dependency preflight, reopening does
    /// not, and the same state is a read-only no-op. The write is a single
    /// native `PATCH {"state": ...}`: no label is read, written or removed,
    /// and the response must confirm the target.
    pub(crate) fn transition_status(
        &self,
        number: u64,
        target: &str,
    ) -> Result<crate::providers::api::IssueSummary, PhasegentError> {
        let target = canonical_target(target)?;
        let repo = self.repository()?;
        let first = self.status_issue(number, &repo)?;
        let current = native_state_name(&first.state)?;
        if current == target {
            return self.summary(first, &repo, Some(number));
        }
        if target == CLOSED {
            self.require_closed_dependencies(number)?;
        }
        let fresh = self.status_issue(number, &repo)?;
        if first.id != fresh.id || first.state != fresh.state {
            return Err(PhasegentError::config(
                "Forgejo issue state changed during preflight; retry",
            ));
        }
        let native = native_state_value(target);
        let index = number.to_string();
        let request = self
            .http
            .request(Method::PATCH, &["issues", &index])?
            .json(&json!({"state": native}));
        let patched: Issue = self.http.write(request, "status transition").map_err(|_| {
            PhasegentError::request(
                "status transition",
                "native state update failed or is unconfirmed; inspect issue before retrying"
                    .to_owned(),
            )
        })?;
        if !patched.state.trim().eq_ignore_ascii_case(native) {
            return Err(PhasegentError::request("status transition", "partial transition: native state response did not confirm target; inspect issue before retrying".to_owned()));
        }
        self.summary(patched, &repo, Some(number)).map_err(|_| {
            PhasegentError::request(
                "status transition",
                "native state update is unconfirmed; inspect issue before retrying".to_owned(),
            )
        })
    }

    /// The native state a status selector names.
    ///
    /// The catalogue is synthetic, so the selector is resolved locally: a
    /// numeric `0`/`1` and the names `Open`/`Closed` (trimmed and case
    /// insensitive) select a state, and anything else refuses with the two
    /// supported names before any request.
    pub(crate) fn select_native_target(&self, value: &str) -> Result<&'static str, PhasegentError> {
        select_native_target(value)
    }
}

/// The synthetic native catalogue: the two states a Forgejo issue can have.
pub(super) fn native_states() -> Vec<RedmineIssueStatus> {
    vec![
        RedmineIssueStatus {
            id: OPEN_ID,
            name: OPEN.to_owned(),
            is_closed: false,
        },
        RedmineIssueStatus {
            id: CLOSED_ID,
            name: CLOSED.to_owned(),
            is_closed: true,
        },
    ]
}

/// The canonical name of the native state an issue carries.
pub(super) fn native_state_name(native: &str) -> Result<&'static str, PhasegentError> {
    let trimmed = native.trim();
    if trimmed.eq_ignore_ascii_case("open") {
        return Ok(OPEN);
    }
    if trimmed.eq_ignore_ascii_case("closed") {
        return Ok(CLOSED);
    }
    Err(PhasegentError::config("invalid Forgejo issue state"))
}

/// The native wire value of one canonical state.
pub(super) fn native_state_value(name: &str) -> &'static str {
    if name == CLOSED { "closed" } else { "open" }
}

/// The provider-local selector of one canonical state.
pub(super) fn native_state_id(name: &str) -> u64 {
    if name == CLOSED { CLOSED_ID } else { OPEN_ID }
}

/// The single state an issue may move to from the state it carries.
pub(super) fn opposite_state(name: &str) -> &'static str {
    if name == CLOSED { OPEN } else { CLOSED }
}

fn native_state_ref(name: &str) -> StatusRef {
    StatusRef {
        id: Some(native_state_id(name)),
        name: name.to_owned(),
        is_closed: Some(name == CLOSED),
        canonical: true,
    }
}

/// The canonical state a target name selects.
pub(super) fn canonical_target(target: &str) -> Result<&'static str, PhasegentError> {
    let trimmed = target.trim();
    if trimmed.eq_ignore_ascii_case(OPEN) {
        return Ok(OPEN);
    }
    if trimmed.eq_ignore_ascii_case(CLOSED) {
        return Ok(CLOSED);
    }
    Err(PhasegentError::config(format!(
        "Forgejo supports only the native {OPEN} and {CLOSED} states; '{target}' is not a Forgejo state"
    )))
}

/// The canonical state a numeric or named selector selects.
pub(super) fn select_native_target(value: &str) -> Result<&'static str, PhasegentError> {
    let trimmed = value.trim();
    match trimmed {
        "0" => Ok(CLOSED),
        "1" => Ok(OPEN),
        _ => canonical_target(trimmed),
    }
}
