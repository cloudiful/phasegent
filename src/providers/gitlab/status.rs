use super::{
    GitlabProvider,
    model::{Issue, Project},
};
use crate::providers::api::PhasegentError;
use crate::providers::redmine::model::{RedmineIssueStatus, StatusNextReport, StatusRef};
use reqwest::Method;
use serde_json::json;

/// The native open state of a GitLab issue.
pub(crate) const OPEN: &str = "Open";
/// The native closed state of a GitLab issue.
pub(crate) const CLOSED: &str = "Closed";

pub(crate) const OPEN_ID: u64 = 1;
pub(crate) const CLOSED_ID: u64 = 0;

pub(crate) const NATIVE_POLICY_SOURCE: &str = "phasegent/gitlab-native-state@v1";

pub(crate) const NATIVE_POLICY_CAVEAT: &str = "Native state only: a GitLab issue is open or closed, the phase detail lives in the phasegent record, and no status label is required, read or written.";

impl GitlabProvider {
    fn status_issue(&self, number: u64, project: &Project) -> Result<Issue, PhasegentError> {
        if number == 0 {
            return Err(PhasegentError::config("issue number must be positive"));
        }
        let index = number.to_string();
        let issue: Issue = self.http.get(
            self.http.request(
                Method::GET,
                &["projects", self.config.repository(), "issues", &index],
            )?,
            "status issue get",
        )?;
        self.summary(issue.clone(), project, Some(number))?;
        Ok(issue)
    }

    pub(crate) fn status_next(&self, number: u64) -> Result<StatusNextReport, PhasegentError> {
        let project = self.project()?;
        let issue = self.status_issue(number, &project)?;
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
            recovery: format!("Inspect the native GitLab state of issue {number} and retry."),
        })
    }

    pub(crate) fn transition_status(
        &self,
        number: u64,
        target: &str,
    ) -> Result<crate::providers::api::IssueSummary, PhasegentError> {
        let target = canonical_target(target)?;
        let project = self.project()?;
        let first = self.status_issue(number, &project)?;
        let current = native_state_name(&first.state)?;
        if current == target {
            return self.summary(first, &project, Some(number));
        }
        let fresh = self.status_issue(number, &project)?;
        if first.id != fresh.id || first.state != fresh.state {
            return Err(PhasegentError::config(
                "GitLab issue state changed during preflight; retry",
            ));
        }
        let event = if target == CLOSED { "close" } else { "reopen" };
        let index = number.to_string();
        let request = self
            .http
            .request(
                Method::PUT,
                &["projects", self.config.repository(), "issues", &index],
            )?
            .json(&json!({"state_event": event}));
        let patched: Issue = self.http.write(request, "status transition").map_err(|_| {
            PhasegentError::request(
                "status transition",
                "native state update failed or is unconfirmed; inspect issue before retrying"
                    .to_owned(),
            )
        })?;
        if native_state_name(&patched.state)? != target {
            return Err(PhasegentError::request("status transition", "partial transition: native state response did not confirm target; inspect issue before retrying".to_owned()));
        }
        self.summary(patched, &project, Some(number)).map_err(|_| {
            PhasegentError::request(
                "status transition",
                "native state update is unconfirmed; inspect issue before retrying".to_owned(),
            )
        })
    }

    pub(crate) fn select_native_target(&self, value: &str) -> Result<&'static str, PhasegentError> {
        select_native_target(value)
    }
}

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

pub(super) fn native_state_name(native: &str) -> Result<&'static str, PhasegentError> {
    let trimmed = native.trim();
    if trimmed.eq_ignore_ascii_case("open") || trimmed.eq_ignore_ascii_case("opened") {
        return Ok(OPEN);
    }
    if trimmed.eq_ignore_ascii_case("closed") {
        return Ok(CLOSED);
    }
    Err(PhasegentError::config("invalid GitLab issue state"))
}

pub(super) fn opposite_state(name: &str) -> &'static str {
    if name == CLOSED { OPEN } else { CLOSED }
}

fn native_state_id(name: &str) -> u64 {
    if name == CLOSED { CLOSED_ID } else { OPEN_ID }
}

fn native_state_ref(name: &str) -> StatusRef {
    StatusRef {
        id: Some(native_state_id(name)),
        name: name.to_owned(),
        is_closed: Some(name == CLOSED),
        canonical: true,
    }
}

pub(super) fn canonical_target(target: &str) -> Result<&'static str, PhasegentError> {
    let trimmed = target.trim();
    if trimmed.eq_ignore_ascii_case(OPEN) {
        return Ok(OPEN);
    }
    if trimmed.eq_ignore_ascii_case(CLOSED) {
        return Ok(CLOSED);
    }
    Err(PhasegentError::config(format!(
        "GitLab supports only the native {OPEN} and {CLOSED} states; '{target}' is not a GitLab state"
    )))
}

pub(super) fn select_native_target(value: &str) -> Result<&'static str, PhasegentError> {
    let trimmed = value.trim();
    match trimmed {
        "0" => Ok(CLOSED),
        "1" => Ok(OPEN),
        _ => canonical_target(trimmed),
    }
}

#[cfg(test)]
mod tests {
    use super::{CLOSED, OPEN, native_state_name, select_native_target};

    #[test]
    fn native_states_map_gitlab_wire_names_and_numeric_selectors() {
        assert_eq!(native_state_name("opened").unwrap(), OPEN);
        assert_eq!(native_state_name("closed").unwrap(), CLOSED);
        assert!(native_state_name("locked").is_err());
        assert_eq!(select_native_target("1").unwrap(), OPEN);
        assert_eq!(select_native_target("0").unwrap(), CLOSED);
        assert_eq!(select_native_target("closed").unwrap(), CLOSED);
        assert!(select_native_target("in progress").is_err());
    }
}
