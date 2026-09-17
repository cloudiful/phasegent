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
pub fn classify_redmine_error(error: &ForgejoError) -> RedmineErrorKind {
    match error {
        ForgejoError::Auth(_) => RedmineErrorKind::Auth,
        ForgejoError::Http {
            status, message, ..
        } => classify_http(*status, message),
        _ => RedmineErrorKind::Other,
    }
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
