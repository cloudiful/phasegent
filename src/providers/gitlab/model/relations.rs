//! Relation / issue-link DTOs and mapping helpers.

use crate::providers::api::ForgejoError;
use serde::Deserialize;

/// Response payload returned by `GET /projects/:id/issues/:iid/links`
/// and `POST /projects/:id/issues/:iid/links`.
///
/// GitLab 19.x returns two distinct shapes for the same logical
/// resource:
///
/// * `POST /projects/:id/issues/:iid/links` returns
///   `{ "id", "source_issue", "target_issue", "link_type", ... }`.
///   The link id is the top-level `id`; the source/target endpoints
///   carry the `id`, `iid`, and `project_id` of each end.
/// * `GET /projects/:id/issues/:iid/links` returns an array where
///   each element is the **target issue object** plus the link id
///   (`issue_link_id`) and `link_type` attached at the top level:
///   `{ "id", "iid", "project_id", "issue_link_id", "link_type",
///   "title", "state", ... }`.
///
/// Earlier contract fixtures (and the GitLab REST v4 docs for
/// older releases) wrapped the target endpoint under `issue`; the
/// decoder keeps that nested field so the list path stays
/// compatible with the existing fixtures while the live GET
/// response shape is fully supported.
///
/// All id-bearing fields are optional so a partial payload (for
/// example one missing `link_create_user_id`) still decodes
/// instead of producing a `missing field` decode error.
#[derive(Debug, Deserialize)]
pub(crate) struct ApiIssueLink {
    /// Link id returned by `POST /links` on the live instance.
    #[serde(default)]
    pub id: Option<u64>,
    /// Link id attached to the target issue object on
    /// `GET /links` (live shape and the earlier contract
    /// fixtures). The read path prefers this field so the legacy
    /// and live response shapes both decode to the same link id.
    #[serde(default)]
    pub issue_link_id: Option<u64>,
    /// GitLab link-type string (`relates_to`, `blocks`,
    /// `is_blocked_by`). Empty when the server omits the field;
    /// [`ApiIssueLink::into_summary`] surfaces the raw value so an
    /// operator can spot a regression rather than seeing a silent
    /// `relates` default.
    #[serde(default)]
    pub link_type: String,
    /// Target endpoint, only present in the live POST response.
    #[serde(default)]
    pub target_issue: Option<ApiIssueLinkEndpoint>,
    /// Target endpoint, used by the existing contract fixtures and
    /// by older GitLab releases that wrap the target issue inside
    /// a nested `issue` object.
    #[serde(default)]
    pub issue: Option<ApiIssueLinkIssue>,
    /// Target issue iid at the top level of the live GET
    /// response. Mirrors `ApiIssueLinkIssue::iid` and is kept as a
    /// separate field so the flat live shape decodes without
    /// forcing the caller to inspect the nested object.
    #[serde(default)]
    pub iid: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiIssueLinkEndpoint {
    pub iid: u64,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ApiIssueLinkIssue {
    pub iid: u64,
}

/// Whether `relation_type` is accepted by `POST /links` on the live
/// `https://gitlab.example.com/19.2` instance.
///
/// The instance accepts `relates_to` and rejects `blocks` /
/// `is_blocked_by` with `link_type does not have a valid value`
/// even when the request is sent with the documented query
/// parameters. The decision is made locally (no network probe) so
/// the unsupported directions fail with a structured
/// [`crate::providers::api::ForgejoError::NotSupported`] error
/// before any HTTP traffic. The read path still maps every
/// server-returned link type (`blocks`, `is_blocked_by`) so the
/// list output reflects whatever the server already recorded.
pub(crate) fn gitlab_create_supports_relation_type(
    relation_type: crate::providers::redmine::model::RedmineRelationType,
) -> bool {
    use crate::providers::redmine::model::RedmineRelationType;
    matches!(relation_type, RedmineRelationType::Relates)
}

/// Map the orchestrator's Redmine-style canonical relation name to
/// the GitLab `link_type` spelling. `relates` maps to `relates_to`,
/// `blocks` maps to `blocks`, and `Precedes` is rejected before the
/// mapping so the structured not-supported error surfaces from the CLI
/// layer instead of an HTTP 400.
///
/// `Blocked` is GitLab's `is_blocked_by`: when listing links the source
/// issue can carry an `is_blocked_by` link that records the inverse
/// direction. Direct CLI input never uses `Blocked`/`Follows`; the
/// mapping only matters when normalising server responses.
pub(crate) fn gitlab_link_type_from_relation_type(
    relation_type: crate::providers::redmine::model::RedmineRelationType,
) -> Result<&'static str, ForgejoError> {
    use crate::providers::redmine::model::RedmineRelationType;
    match relation_type {
        RedmineRelationType::Relates => Ok("relates_to"),
        RedmineRelationType::Blocks => Ok("blocks"),
        RedmineRelationType::Precedes => Err(ForgejoError::config(
            "GitLab issue links do not support --type precedes",
        )),
        // Inverse direction accepted only on the read path; calling
        // code never passes Blocked/Follows through the CLI parser.
        RedmineRelationType::Blocked | RedmineRelationType::Follows => Err(ForgejoError::config(
            "GitLab issue links accept only the forward canonical names blocks and relates",
        )),
    }
}

/// Map GitLab's `link_type` to the canonical CLI name. Used by
/// `relation list` so the wire format stays GitLab-shaped while the
/// CLI output matches Redmine's vocabulary.
pub(crate) fn gitlab_link_type_to_relation_type(
    link_type: &str,
) -> crate::providers::redmine::model::RedmineRelationType {
    use crate::providers::redmine::model::RedmineRelationType;
    match link_type {
        "relates_to" => RedmineRelationType::Relates,
        "blocks" => RedmineRelationType::Blocks,
        "is_blocked_by" => RedmineRelationType::Blocked,
        // Future GitLab additions are surfaced as the lowercased raw
        // string when emitted through `RelationSummary::relation_type`;
        // the typed enum only carries the known three. The mapper
        // falls back to Relates to keep the typed boundary total so
        // a `RedmineRelationType` value is always producible.
        _ => RedmineRelationType::Relates,
    }
}

#[cfg(test)]
mod tests {
    use super::{gitlab_link_type_from_relation_type, gitlab_link_type_to_relation_type};

    #[test]
    fn gitlab_link_type_maps_canonical_names_and_inverse() {
        use crate::providers::redmine::model::RedmineRelationType;
        assert_eq!(
            gitlab_link_type_from_relation_type(RedmineRelationType::Relates).unwrap(),
            "relates_to",
        );
        assert_eq!(
            gitlab_link_type_from_relation_type(RedmineRelationType::Blocks).unwrap(),
            "blocks",
        );
        let error = gitlab_link_type_from_relation_type(RedmineRelationType::Precedes).unwrap_err();
        assert!(
            error.json()["message"]
                .as_str()
                .unwrap_or_default()
                .contains("precedes")
        );
    }

    #[test]
    fn gitlab_link_type_decode_uses_canonical_inverse_mapping() {
        use crate::providers::redmine::model::RedmineRelationType;
        assert_eq!(
            gitlab_link_type_to_relation_type("relates_to"),
            RedmineRelationType::Relates,
        );
        assert_eq!(
            gitlab_link_type_to_relation_type("blocks"),
            RedmineRelationType::Blocks,
        );
        assert_eq!(
            gitlab_link_type_to_relation_type("is_blocked_by"),
            RedmineRelationType::Blocked,
        );
    }
}
