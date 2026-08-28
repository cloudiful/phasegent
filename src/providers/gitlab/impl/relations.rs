//! Issue links list/create/delete and conversions.

use crate::providers::api::ForgejoError;
use crate::providers::gitlab::model::{
    ApiIssueLink, gitlab_create_supports_relation_type, gitlab_link_type_from_relation_type,
};
use crate::providers::redmine::model::{RedmineRelationType, RelationSummary};

use super::core::GitlabProvider;

impl GitlabProvider {
    /// `GET /projects/:id/issues/:iid/links` paginated until GitLab
    /// signals completion. The CLI only needs a single page for
    /// realistic graph sizes, but the paginated helper stays
    /// symmetrical with the issue and note paths. The returned
    /// summaries are rendered from the queried issue's viewpoint so
    /// `is_blocked_by` shows up as `blocked` (the inverse Redmine
    /// name).
    pub(crate) fn list_issue_links(&self, iid: u64) -> Result<Vec<RelationSummary>, ForgejoError> {
        if iid == 0 {
            return Err(ForgejoError::config(
                "GitLab issue iid must be greater than zero",
            ));
        }
        let path = self.issue_links_path(iid);
        let links = self.http.paginate("relation list", |http, page| {
            http.get_page::<ApiIssueLink>(&path, &[("page", page.to_string())], "relation list")
        })?;
        Ok(links
            .into_iter()
            .map(|link| link.into_summary(iid))
            .collect())
    }

    /// `POST /projects/:id/issues/:iid/links` with the canonical CLI
    /// `--type` mapped to GitLab's `link_type` spelling. The
    /// `RedmineRelationType` enum is the single source of truth so the
    /// parser already rejected any invalid names.
    ///
    /// The live `https://gitlab.example.com/19.2` instance
    /// rejects the body-shape payload and only accepts the request
    /// when `target_project_id`, `target_issue_iid`, and the
    /// optional `link_type` are sent as URL query parameters. We
    /// route the call through the [`GitlabHttp::post_with_query`]
    /// helper so the credentials stay in the `PRIVATE-TOKEN` header
    /// rather than the URL, and the body is left empty.
    ///
    /// The same instance only accepts `relates_to` for create:
    /// `blocks` and `is_blocked_by` come back with
    /// `link_type does not have a valid value` even when the query
    /// parameters are correct. We gate the create path locally via
    /// [`crate::providers::gitlab::model::gitlab_create_supports_relation_type`] so
    /// the unsupported directions fail with a structured
    /// [`ForgejoError::NotSupported`] error before any network
    /// traffic. The list mapping still decodes every
    /// server-returned direction (`blocks`, `is_blocked_by`).
    pub(crate) fn create_issue_link(
        &self,
        issue_iid: u64,
        target_iid: u64,
        relation_type: RedmineRelationType,
    ) -> Result<RelationSummary, ForgejoError> {
        if issue_iid == 0 {
            return Err(ForgejoError::config(
                "GitLab issue iid must be greater than zero",
            ));
        }
        if target_iid == 0 {
            return Err(ForgejoError::config(
                "GitLab target issue iid must be greater than zero",
            ));
        }
        if issue_iid == target_iid {
            return Err(ForgejoError::config(
                "GitLab issue link cannot target the same issue",
            ));
        }
        if !gitlab_create_supports_relation_type(relation_type) {
            // The live instance rejects every direction other than
            // `relates_to` with a structured validation error. Fail
            // before any HTTP traffic so the client sees a
            // consistent not-supported error regardless of which
            // direction the caller asked for, and so the read path
            // remains the only place that decodes `blocks` /
            // `is_blocked_by` from a server response.
            return Err(ForgejoError::not_supported(
                "gitlab",
                "relation create with the requested link_type",
            ));
        }
        let link_type = gitlab_link_type_from_relation_type(relation_type)?;
        let query = vec![
            ("target_project_id", self.project_id().to_string()),
            ("target_issue_iid", target_iid.to_string()),
            ("link_type", link_type.to_owned()),
        ];
        let link: ApiIssueLink = self.http.post_with_query(
            &self.issue_links_path(issue_iid),
            None::<&()>,
            &query,
            "relation create",
        )?;
        Ok(link.into_summary(issue_iid))
    }

    /// `DELETE /projects/:id/issues/:issue_iid/links/:link_id`. The
    /// source issue iid is required by the GitLab REST v4 contract
    /// (the endpoint is scoped per source issue and there is no
    /// single-link GET that resolves the source from a link id).
    /// `source_issue_iid` is therefore an explicit parameter; callers
    /// that have no source context must surface a structured config
    /// error rather than silently guessing the source.
    pub(crate) fn delete_issue_link(
        &self,
        source_issue_iid: Option<u64>,
        link_id: u64,
    ) -> Result<u64, ForgejoError> {
        let source_issue_iid = source_issue_iid.ok_or_else(|| {
            ForgejoError::config(
                "GitLab relation delete requires the source issue iid; \
                  the DELETE endpoint is scoped per source issue and the \
                  GitLab REST v4 API exposes no single-link GET that \
                  resolves the source from a link id. Forward --issue \
                  <SOURCE_ISSUE_IID> from the relation delete CLI once \
                  it is wired through the parser.",
            )
        })?;
        if source_issue_iid == 0 {
            return Err(ForgejoError::config(
                "GitLab source issue iid must be greater than zero",
            ));
        }
        if link_id == 0 {
            return Err(ForgejoError::config(
                "GitLab issue link id must be greater than zero",
            ));
        }
        let path = self.issue_link_path(source_issue_iid, link_id);
        let _: Option<serde_json::Value> = self.http.delete(&path, "relation delete")?;
        Ok(link_id)
    }
}

impl ApiIssueLink {
    /// Render the GitLab link as the orchestrator's shared
    /// [`RelationSummary`] shape, resolving the relation type from
    /// the queried issue's viewpoint.
    ///
    /// The link id is `issue_link_id` on `GET` responses and `id`
    /// on `POST` responses; the mapper prefers the explicit
    /// `issue_link_id` field and falls back to `id` so both
    /// contract fixtures and the live shapes decode to the same
    /// id. The queried issue's iid becomes `issue_id`. The
    /// linked issue's iid is read from `target_issue` (POST
    /// response), `issue` (legacy GET fixtures), or the top-level
    /// `iid` (live GET response) in that order so the flat live
    /// shape and the legacy nested shape both surface the right
    /// `issue_to_id`. `delay` is always `None` for GitLab because
    /// the API has no notion of a precedence lag.
    pub(crate) fn into_summary(self, queried_issue_iid: u64) -> RelationSummary {
        let link_id = self.issue_link_id.or(self.id).unwrap_or(0);
        let linked_iid = self
            .target_issue
            .as_ref()
            .map(|endpoint| endpoint.iid)
            .or_else(|| self.issue.as_ref().map(|issue| issue.iid))
            .or(self.iid)
            .unwrap_or(0);
        let relation_type = if self.link_type.is_empty() {
            // GitLab 19.x always reports a `link_type`; an empty
            // value surfaces as `unknown` so an operator can spot
            // the regression rather than seeing a silent `relates`
            // default.
            "unknown".to_owned()
        } else {
            crate::providers::gitlab::model::gitlab_link_type_to_relation_type(&self.link_type)
                .as_str()
                .to_owned()
        };
        RelationSummary {
            id: link_id,
            relation_type,
            issue_id: queried_issue_iid,
            issue_to_id: linked_iid,
            delay: None,
        }
    }
}
