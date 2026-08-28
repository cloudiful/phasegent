use crate::providers::api::{ForgejoError, IssueSummary};
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::{
    RedmineIssue, RedmineIssueResponse, RedmineIssueStatus, RedmineIssueStatusCollection,
    RedmineStatus, RedmineTracker, RedmineTrackerCollection, STATUS_POLICY_CAVEAT,
    STATUS_POLICY_SOURCE, StatusNextReport, StatusRef, StatusTransitionOutcome, TransitionVerdict,
    canonical_allowed_next, evaluate_transition,
};

/// Outcome of a close-status verification step. The provider turns an
/// observed status into one of these variants so the two `close_issue`
/// verification paths (PUT response and follow-up `GET`) share the same
/// decision logic.
enum CloseVerification {
    /// The observed status confirms the close (matches the configured
    /// close status id or carries `is_closed=true`).
    Confirmed,
    /// The observed status contradicts the close (different id, not
    /// closed). The caller surfaces a structured request error.
    Mismatch,
    /// The observed status carries no usable signal (legacy response
    /// shape with no id and no closed flag). The caller falls back to a
    /// follow-up `GET` for the final verdict.
    Indeterminate,
}

impl RedmineProvider {
    pub fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, ForgejoError> {
        let response: RedmineIssueStatusCollection =
            self.http
                .get("issue_statuses.json", &[], "issue status list")?;
        Ok(response.issue_statuses)
    }

    /// List every tracker visible to this API key (`/trackers.json`). The
    /// configured workflow only uses Bug and Feature, but resolution stays
    /// generic so the server remains the source of truth.
    pub fn list_trackers(&self) -> Result<Vec<RedmineTracker>, ForgejoError> {
        let response: RedmineTrackerCollection =
            self.http.get("trackers.json", &[], "tracker list")?;
        Ok(response.trackers)
    }

    /// Move an issue to any status resolved by validated name or id via
    /// [`RedmineProvider::select_status_by_value`]. Unlike `close_issue`
    /// this is not restricted to closed statuses.
    ///
    /// The PUT response (or a follow-up `GET` when the response body is
    /// empty) must confirm `status_id` actually landed on the issue. A
    /// server that returns `200 OK` while the remote state remains
    /// unchanged produces a structured request error so callers never
    /// see a false success. When the response carries no usable status
    /// id (older Redmine versions, legacy mock fixtures), the PUT
    /// response is accepted as authoritative because no verification
    /// signal is available; the follow-up `GET` still runs whenever the
    /// PUT returns no body so empty / `204 No Content` responses do not
    /// mask a silently-failed status change.
    pub fn set_issue_status(
        &self,
        number: u64,
        status_id: u64,
    ) -> Result<IssueSummary, ForgejoError> {
        let payload = crate::providers::redmine::model::RedmineUpdateIssue::status(status_id);
        let response: Option<RedmineIssueResponse> =
            self.http
                .put(&self.issue_path(number), &payload, "issue status update")?;
        match response {
            Some(response) => {
                if let Some(status) = response.issue.status.as_ref()
                    && let Some(observed) = status.known_id()
                {
                    if observed == status_id {
                        return Ok(self.issue_summary(response.issue));
                    }
                    return Err(ForgejoError::request(
                        "issue status update",
                        format!(
                            "Redmine did not confirm status_id={status_id}; observed status_id={observed} ('{}')",
                            status.name,
                        ),
                    ));
                }
                // The PUT response is present but does not carry a usable
                // status id (older Redmine, legacy fixtures). Trust it and
                // skip the follow-up GET so those environments keep working.
                Ok(self.issue_summary(response.issue))
            }
            None => {
                // The PUT response is empty; re-read the issue so a server that
                // silently ignored the status change cannot return success.
                let issue = self.issue_with_journals(number, "issue status update")?;
                if let Some(status) = issue.status.as_ref()
                    && let Some(observed) = status.known_id()
                    && observed != status_id
                {
                    return Err(ForgejoError::request(
                        "issue status update",
                        format!(
                            "Redmine did not confirm status_id={status_id}; observed status_id={observed} ('{}')",
                            status.name,
                        ),
                    ));
                }
                Ok(self.issue_summary(issue))
            }
        }
    }

    /// Read the issue's current status without the surrounding summary
    /// so status policy checks can run before any write.
    fn current_status(&self, number: u64, operation: &str) -> Result<RedmineStatus, ForgejoError> {
        let issue = self.issue_with_journals(number, operation)?;
        issue.status.ok_or_else(|| {
            ForgejoError::request(
                operation,
                format!("Redmine issue {number} response carried no status"),
            )
        })
    }

    /// Answer "where can this issue go next" from the centralized
    /// canonical policy, resolving policy names to this installation's
    /// status ids. Read-only: no transition is attempted.
    pub fn status_next(&self, number: u64) -> Result<StatusNextReport, ForgejoError> {
        let operation = "issue status next";
        let statuses = self.list_issue_statuses()?;
        let current = self.current_status(number, operation)?;
        let policy_names = canonical_allowed_next(&current.name);
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
            current: StatusRef::from_issue_status(&current),
            allowed_next,
            allowed_next_missing_on_server: missing,
            policy_source: STATUS_POLICY_SOURCE,
            advisory: policy_names.is_none(),
            caveat: STATUS_POLICY_CAVEAT,
            recovery: recovery_hint(number),
        })
    }

    /// Move an issue to `target_value` after a policy preflight.
    /// Same-status requests are an idempotent no-op, canonical illegal
    /// edges fail before the PUT with structured guidance, and unknown
    /// or custom statuses are forwarded to the server as advisory so a
    /// custom workflow keeps working.
    pub fn advance_issue_status(
        &self,
        number: u64,
        target_value: &str,
    ) -> Result<StatusTransitionOutcome, ForgejoError> {
        let operation = "issue status advance";
        let statuses = self.list_issue_statuses()?;
        let target = RedmineProvider::select_status_by_value(&statuses, target_value)?;
        let current = self.current_status(number, operation)?;
        let from = StatusRef::from_issue_status(&current);
        let to = StatusRef::from_installation(target);
        let verdict = evaluate_transition(&current.name, &target.name);
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
                    forbidden_message(number, &current.name, &target.name, allowed_next),
                ));
            }
            TransitionVerdict::Allowed | TransitionVerdict::Advisory { .. } => {}
        }
        let summary = self.set_issue_status(number, target.id).map_err(|error| {
            annotate_transition_error(error, &current.name, &target.name, number)
        })?;
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

    /// Close an issue using the configured close status id. The PUT
    /// response (or a follow-up `GET`) must confirm the issue is now in
    /// a closed state — either by matching the configured close status
    /// id or by reporting `is_closed=true` so an operator who renames
    /// the close status id still sees a correct close verification.
    pub fn close_issue(&self, number: u64) -> Result<IssueSummary, ForgejoError> {
        let status_id = self.config.require_close_status_id()?;
        let payload = crate::providers::redmine::model::RedmineUpdateIssue::status(status_id);
        let response: Option<RedmineIssueResponse> =
            self.http
                .put(&self.issue_path(number), &payload, "issue close")?;
        if let Some(response) = response {
            match Self::evaluate_close_response(&response.issue, status_id) {
                CloseVerification::Confirmed => return Ok(self.issue_summary(response.issue)),
                CloseVerification::Mismatch => {
                    let status = response
                        .issue
                        .status
                        .as_ref()
                        .expect("mismatch evaluation observed a present status");
                    return Err(Self::close_mismatch_error(status, status_id));
                }
                CloseVerification::Indeterminate => {
                    // Legacy response shape (no status id, no closed
                    // flag, or no status at all). Trust the PUT response
                    // and return so environments without an explicit id
                    // in the PUT body keep working.
                    return Ok(self.issue_summary(response.issue));
                }
            }
        }
        let issue = self.issue_with_journals(number, "issue close")?;
        if let Some(status) = issue.status.as_ref() {
            match Self::verify_close_status(status, status_id) {
                CloseVerification::Confirmed => return Ok(self.issue_summary(issue)),
                CloseVerification::Mismatch => {
                    return Err(Self::close_mismatch_error(status, status_id));
                }
                CloseVerification::Indeterminate => {}
            }
        }
        Ok(self.issue_summary(issue))
    }

    /// Evaluate a single PUT response against the configured close
    /// status. Returns `Indeterminate` whenever the response carries no
    /// usable status signal at all so the caller can fall back to a
    /// follow-up `GET`.
    fn evaluate_close_response(issue: &RedmineIssue, expected_status_id: u64) -> CloseVerification {
        match issue.status.as_ref() {
            Some(status) => Self::verify_close_status(status, expected_status_id),
            None => CloseVerification::Indeterminate,
        }
    }

    fn verify_close_status(status: &RedmineStatus, expected_status_id: u64) -> CloseVerification {
        if let Some(observed) = status.known_id() {
            if observed == expected_status_id {
                CloseVerification::Confirmed
            } else {
                CloseVerification::Mismatch
            }
        } else if status.is_closed.unwrap_or(false) {
            CloseVerification::Confirmed
        } else {
            CloseVerification::Indeterminate
        }
    }

    fn close_mismatch_error(status: &RedmineStatus, expected_status_id: u64) -> ForgejoError {
        ForgejoError::request(
            "issue close",
            format!(
                "Redmine did not confirm close (status_id={expected_status_id}); observed status_id={:?} ('{}', is_closed={:?})",
                status.known_id(),
                status.name,
                status.is_closed,
            ),
        )
    }
}

/// Concrete recovery command an AI can run to see the current status and
/// the policy-allowed next statuses for one issue.
fn recovery_hint(number: u64) -> String {
    format!("phasegent --role orchestrator --provider redmine status next {number}")
}

/// Structured, self-describing message for a policy-rejected
/// transition. It names the current status, the target status, the
/// allowed next statuses, the policy identifier, and the recovery
/// command so the caller never has to guess the workflow.
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
        recovery_hint(number)
    )
}

/// Preserve a server-side rejection while appending bounded
/// current/target/recovery context. The original provider error keeps
/// its kind and operation; only the message gains guidance, and the
/// appended text is length-bounded so no full remote response or
/// credential can be echoed.
fn annotate_transition_error(
    error: ForgejoError,
    current: &str,
    target: &str,
    number: u64,
) -> ForgejoError {
    let context = bounded(&format!(
        "current status '{current}' -> target status '{target}'; server rejected a policy-allowed or custom transition, so the Redmine workflow is authoritative; recovery: {}",
        recovery_hint(number)
    ));
    match error {
        ForgejoError::Http {
            operation,
            status,
            message,
        } => ForgejoError::Http {
            operation,
            status,
            message: format!("{}; {context}", bounded(&message)),
        },
        ForgejoError::Request { operation, message } => {
            ForgejoError::request(&operation, format!("{}; {context}", bounded(&message)))
        }
        other => other,
    }
}

const CONTEXT_BOUND: usize = 400;

fn bounded(message: &str) -> String {
    let collapsed = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > CONTEXT_BOUND {
        let truncated = collapsed.chars().take(CONTEXT_BOUND).collect::<String>();
        format!("{truncated}...")
    } else {
        collapsed
    }
}
