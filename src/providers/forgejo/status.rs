use super::{
    ForgejoProvider,
    model::{Dependency, Issue, Label, Repository},
};
use crate::providers::api::PhasegentError;
use crate::providers::redmine::model::{
    RedmineIssueStatus, STATUS_POLICY_SOURCE, StatusNextReport, StatusRef, canonical_allowed_next,
};
use reqwest::Method;
use serde_json::json;

impl ForgejoProvider {
    pub(super) fn required_status_labels(&self) -> Result<Vec<Label>, PhasegentError> {
        let labels = self.collect(&["labels"], &[], "issue status list")?;
        require_labels(&labels)?;
        Ok(labels)
    }

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

    pub(crate) fn status_next(&self, number: u64) -> Result<StatusNextReport, PhasegentError> {
        let repo = self.repository()?;
        let issue = self.status_issue(number, &repo)?;
        let name = current_status(&issue.state, &issue.labels)?;
        let current_id = if name == "Closed" {
            Some(0)
        } else {
            issue
                .labels
                .iter()
                .find(|label| label.name == format!("status/{name}"))
                .map(|label| label.id)
        };
        let current = StatusRef {
            id: current_id,
            name: name.to_owned(),
            is_closed: Some(matches!(name, "Closed" | "Cancelled")),
            canonical: true,
        };
        let labels: Vec<Label> = self.collect(&["labels"], &[], "issue status list")?;
        let catalogue = statuses(&labels);
        let mut allowed_next = Vec::new();
        let mut missing = Vec::new();
        for next in canonical_allowed_next(name).unwrap_or(&[]) {
            match catalogue.iter().find(|status| status.name == *next) {
                Some(status) => allowed_next.push(StatusRef::from_installation(status)),
                None => missing.push((*next).to_owned()),
            }
        }
        Ok(StatusNextReport {
            issue: number,
            current,
            allowed_next,
            allowed_next_missing_on_server: missing,
            policy_source: STATUS_POLICY_SOURCE,
            advisory: false,
            caveat: "The Forgejo repository must have its canonical status labels provisioned before a transition.",
            recovery: format!("Inspect status labels and retry issue {number}."),
        })
    }

    pub(crate) fn transition_status(
        &self,
        number: u64,
        target: &str,
    ) -> Result<crate::providers::api::IssueSummary, PhasegentError> {
        let repo = self.repository()?;
        let first = self.status_issue(number, &repo)?;
        let current = current_status(&first.state, &first.labels)?;
        validate_transition(current, target)?;
        if current == target {
            return self.summary(first, &repo, Some(number));
        }
        let catalogue = self.required_status_labels()?;
        let desired = if target == "Closed" {
            None
        } else {
            Some(
                catalogue
                    .iter()
                    .find(|label| label.name == format!("status/{target}"))
                    .ok_or_else(|| PhasegentError::config("invalid Forgejo target status"))?
                    .id,
            )
        };
        if matches!(target, "Closed" | "Cancelled") {
            self.require_closed_dependencies(number)?;
        }
        let fresh = self.status_issue(number, &repo)?;
        if first.id != fresh.id
            || first.state != fresh.state
            || label_identity(&first.labels) != label_identity(&fresh.labels)
        {
            return Err(PhasegentError::config(
                "Forgejo issue status or labels changed during preflight; retry",
            ));
        }
        let mut label_ids: Vec<u64> = fresh
            .labels
            .iter()
            .filter(|label| !is_status_label(&label.name))
            .map(|label| label.id)
            .collect();
        if let Some(id) = desired {
            label_ids.push(id);
        }
        if label_ids.contains(&0) {
            return Err(PhasegentError::config("invalid Forgejo issue label id"));
        }
        label_ids.sort_unstable();
        label_ids.dedup();
        let native = if matches!(target, "Closed" | "Cancelled") {
            "closed"
        } else {
            "open"
        };
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
        let summary = self
            .summary(patched.clone(), &repo, Some(number))
            .map_err(|_| {
                PhasegentError::request(
                    "status transition",
                    "native state update is unconfirmed; inspect issue before retrying".to_owned(),
                )
            })?;
        if patched.state != native {
            return Err(PhasegentError::request("status transition", "partial transition: native state response did not confirm target; inspect issue before retrying".to_owned()));
        }
        if label_identity(&patched.labels) != label_identity(&fresh.labels) {
            return Err(PhasegentError::request("status transition", "partial transition: labels changed during native state update; inspect issue before retrying".to_owned()));
        }
        let request = self
            .http
            .request(Method::PUT, &["issues", &index, "labels"])?
            .json(&json!({"labels": label_ids}));
        let actual: Vec<Label> = self.http.write(request, "status labels").map_err(|_| PhasegentError::request("status transition", "partial transition: native state changed but label update failed; inspect issue before retrying".to_owned()))?;
        let mut actual_ids: Vec<_> = actual.iter().map(|label| label.id).collect();
        actual_ids.sort_unstable();
        actual_ids.dedup();
        if actual_ids != label_ids || current_status(native, &actual).ok() != Some(target) {
            return Err(PhasegentError::request("status transition", "partial transition: label response did not confirm target; inspect issue before retrying".to_owned()));
        }
        Ok(summary)
    }
}

fn is_status_label(name: &str) -> bool {
    name.strip_prefix("status/")
        .is_some_and(|suffix| CANONICAL.contains(&suffix))
}

fn label_identity(labels: &[Label]) -> Vec<(u64, String)> {
    let mut values: Vec<_> = labels
        .iter()
        .map(|label| (label.id, label.name.clone()))
        .collect();
    values.sort_unstable();
    values
}

const CANONICAL: [&str; 7] = [
    "New",
    "In Progress",
    "In Review",
    "Resolved",
    "Blocked",
    "Changes Requested",
    "Cancelled",
];

pub(super) fn statuses(labels: &[Label]) -> Vec<RedmineIssueStatus> {
    let mut result: Vec<_> = labels
        .iter()
        .filter_map(|label| {
            label
                .name
                .strip_prefix("status/")
                .filter(|name| CANONICAL.contains(name))
                .map(|name| RedmineIssueStatus {
                    id: label.id,
                    name: name.to_owned(),
                    is_closed: name == "Cancelled",
                })
        })
        .collect();
    result.push(RedmineIssueStatus {
        id: 0,
        name: "Closed".to_owned(),
        is_closed: true,
    });
    result
}

pub(super) fn require_labels(labels: &[Label]) -> Result<(), PhasegentError> {
    for name in CANONICAL {
        if labels
            .iter()
            .filter(|label| label.name == format!("status/{name}") && label.id > 0)
            .count()
            != 1
        {
            return Err(PhasegentError::config(format!(
                "Forgejo status label status/{name} must exist exactly once; ask an operator to configure it"
            )));
        }
    }
    Ok(())
}

pub(super) fn current_status<'a>(
    native: &str,
    labels: &'a [Label],
) -> Result<&'a str, PhasegentError> {
    let selected: Vec<_> = labels
        .iter()
        .filter_map(|label| {
            label
                .name
                .strip_prefix("status/")
                .filter(|name| CANONICAL.contains(name))
        })
        .collect();
    match native {
        "closed" => Ok(if selected.contains(&"Cancelled") {
            "Cancelled"
        } else {
            "Closed"
        }),
        "open" if selected.len() == 1 && selected[0] != "Cancelled" => Ok(selected[0]),
        "open" => Err(PhasegentError::config(
            "open Forgejo issue must have exactly one non-cancelled canonical status label",
        )),
        _ => Err(PhasegentError::config("invalid Forgejo issue state")),
    }
}

pub(super) fn validate_transition(current: &str, target: &str) -> Result<(), PhasegentError> {
    if current == target {
        return Ok(());
    }
    if canonical_allowed_next(current).is_some_and(|allowed| allowed.contains(&target)) {
        return Ok(());
    }
    Err(PhasegentError::config(
        "Forgejo status transition is not allowed by canonical workflow",
    ))
}
