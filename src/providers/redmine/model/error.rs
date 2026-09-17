use crate::providers::api::ForgejoError;

/// Server-side failure classification for Redmine PUT rejections.
///
/// Phase 3 (issue 443): distinguishes a workflow refusal (the Redmine
/// installation's transition rules rejected a `PUT status_id`) from auth,
/// validation, and transport failures so `close` can climb only on the
/// workflow case. Pure string matching on the already-redacted provider
/// error; no network access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RedmineErrorKind {
    Auth,
    Validation,
    WorkflowNotAllowed,
    Other,
}

/// Classify an already-surfaced provider error.
///
/// A silent-200 close mismatch (`issue close` request error `Redmine did
/// not confirm close ...`, e.g. dogfood `issue close 443` observing `New`
/// after a `200 OK`) is a server workflow refusal without an HTTP error
/// status, so it classifies as `WorkflowNotAllowed` and drives the same
/// stepwise close climb as a 403/422 workflow rejection.
pub fn classify_redmine_error(error: &ForgejoError) -> RedmineErrorKind {
    match error {
        ForgejoError::Auth(_) => RedmineErrorKind::Auth,
        ForgejoError::Http {
            status, message, ..
        } => classify_http(*status, message),
        ForgejoError::Request { operation, message } if is_close_mismatch(operation, message) => {
            RedmineErrorKind::WorkflowNotAllowed
        }
        _ => RedmineErrorKind::Other,
    }
}

/// True for the provider's own close-verification mismatch: a `200 OK`
/// PUT/GET pair whose observed status contradicts the requested close.
fn is_close_mismatch(operation: &str, message: &str) -> bool {
    operation == "issue close"
        && message
            .to_ascii_lowercase()
            .contains("did not confirm close")
}

/// Classify a raw HTTP status plus redacted message body.
pub fn classify_http(status: u16, message: &str) -> RedmineErrorKind {
    let haystack = message.to_ascii_lowercase();
    if status == 401 || has_auth_marker(&haystack) {
        return RedmineErrorKind::Auth;
    }
    if (status == 403 || status == 422) && has_workflow_marker(&haystack) {
        return RedmineErrorKind::WorkflowNotAllowed;
    }
    if status == 422 {
        return RedmineErrorKind::Validation;
    }
    RedmineErrorKind::Other
}

fn has_auth_marker(haystack: &str) -> bool {
    [
        "unauthorized",
        "authentication",
        "access denied",
        "not authorized",
        "forbidden",
        "api key",
        "invalid key",
    ]
    .iter()
    .any(|marker| haystack.contains(marker))
}

fn has_workflow_marker(haystack: &str) -> bool {
    [
        "status is invalid",
        "status invalid",
        "invalid status",
        "workflow",
        "not allowed",
        "cannot be changed",
        "status cannot",
        "transition",
        "issue status",
        "status_id",
    ]
    .iter()
    .any(|marker| haystack.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_markers_win_on_403_and_422() {
        assert_eq!(
            classify_http(422, "Status is invalid"),
            RedmineErrorKind::WorkflowNotAllowed
        );
        assert_eq!(
            classify_http(403, "You cannot change status via workflow"),
            RedmineErrorKind::WorkflowNotAllowed
        );
        assert_eq!(
            classify_http(422, "Transition is not allowed"),
            RedmineErrorKind::WorkflowNotAllowed
        );
    }

    #[test]
    fn auth_markers_win_even_with_workflow_status() {
        assert_eq!(
            classify_http(401, "Status is invalid"),
            RedmineErrorKind::Auth
        );
        assert_eq!(
            classify_http(403, "Access denied: not authorized"),
            RedmineErrorKind::Auth
        );
    }

    #[test]
    fn plain_422_is_validation_and_plain_403_is_other() {
        assert_eq!(
            classify_http(422, "Invalid project"),
            RedmineErrorKind::Validation
        );
        assert_eq!(classify_http(403, ""), RedmineErrorKind::Other);
        assert_eq!(classify_http(500, "boom"), RedmineErrorKind::Other);
    }

    #[test]
    fn silent_200_close_mismatch_classifies_as_workflow() {
        let mismatch = ForgejoError::request(
            "issue close",
            "Redmine did not confirm close (status_id=5); observed status_id=Some(1) ('New', is_closed=Some(false))".to_owned(),
        );
        assert_eq!(
            classify_redmine_error(&mismatch),
            RedmineErrorKind::WorkflowNotAllowed
        );
        let unrelated = ForgejoError::request("issue close", "boom".to_owned());
        assert_eq!(classify_redmine_error(&unrelated), RedmineErrorKind::Other);
    }

    #[test]
    fn error_classifier_maps_variants() {
        let workflow = ForgejoError::Http {
            operation: "issue close".to_owned(),
            status: 422,
            message: "Status is invalid".to_owned(),
        };
        assert_eq!(
            classify_redmine_error(&workflow),
            RedmineErrorKind::WorkflowNotAllowed
        );
        assert_eq!(
            classify_redmine_error(&ForgejoError::auth("x")),
            RedmineErrorKind::Auth
        );
        assert_eq!(
            classify_redmine_error(&ForgejoError::config("x")),
            RedmineErrorKind::Other
        );
    }
}
