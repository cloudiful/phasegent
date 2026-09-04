use super::status::RedmineIssueStatus;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RedmineProject {
    pub id: u64,
    pub name: String,
    pub identifier: String,
    #[serde(
        default,
        deserialize_with = "deserialize_null_as_empty_string",
        skip_serializing_if = "String::is_empty"
    )]
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_public: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inherit_members: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_on: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_on: Option<String>,
}

fn deserialize_null_as_empty_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|value| value.unwrap_or_default())
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineProjectCollection {
    #[serde(default)]
    pub(crate) projects: Vec<RedmineProject>,
    pub(crate) total_count: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineProjectResponse {
    pub(crate) project: RedmineProject,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewProject<'a> {
    pub(crate) project: RedmineNewProjectFields<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewProjectFields<'a> {
    pub(crate) name: &'a str,
    pub(crate) identifier: &'a str,
    pub(crate) is_public: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<&'a str>,
    /// Project modules enabled at creation. Includes the `repository`
    /// module so the bootstrap-registered Git repository attaches without
    /// a separate `PUT /projects/:id.json` call. Serialized only when set
    /// to preserve the existing request shape for callers that do not opt
    /// into module enablement.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) enabled_modules: Option<Vec<RedmineEnabledModule<'a>>>,
}

impl<'a> RedmineNewProject<'a> {
    pub(crate) fn new(name: &'a str, identifier: &'a str, description: Option<&'a str>) -> Self {
        Self {
            project: RedmineNewProjectFields {
                name,
                identifier,
                is_public: false,
                description,
                enabled_modules: None,
            },
        }
    }

    pub(crate) fn with_repository_module(mut self) -> Self {
        self.project.enabled_modules = Some(vec![RedmineEnabledModule { name: "repository" }]);
        self
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineEnabledModule<'a> {
    pub(crate) name: &'a str,
}

/// Default Redmine role for the orchestrator user identified by the
/// `orchestrator` API key.
pub const DEFAULT_REDMINE_ROLE_ORCHESTRATOR: &str = "Maintainer";
/// Default Redmine role for the executor user identified by the `executor`
/// API key.
pub const DEFAULT_REDMINE_ROLE_EXECUTOR: &str = "Developer";
/// Default Redmine role for the reviewer user identified by the `reviewer`
/// API key.
pub const DEFAULT_REDMINE_ROLE_REVIEWER: &str = "Reporter";
/// Default Redmine role for the tester user identified by the `tester`
/// API key. Tester shares the least-privilege `Reporter` role with
/// reviewer; bootstrap reconciles tester only when its credential is
/// configured.
pub const DEFAULT_REDMINE_ROLE_TESTER: &str = "Reporter";

#[derive(Debug)]
pub struct RedmineBootstrap {
    pub project: RedmineProject,
    pub close_status: RedmineIssueStatus,
    pub created: bool,
}

/// Outcome of reconciling a single user's direct project membership. The
/// `status` mirrors the bootstrap reconciliation vocabulary
/// (`added`/`updated`/`existing`/`warning`) so callers can decide whether the
/// workflow is ready.
#[derive(Debug)]
#[allow(dead_code)]
pub struct RedmineUserMembershipOutcome {
    pub user_id: u64,
    pub user_login: String,
    pub role_id: u64,
    pub role_name: String,
    pub status: String,
    pub warning: Option<String>,
}

/// Identity of the user behind a role-scoped Redmine API key. Returned by
/// `/users/current.json`; used to bind bootstrap output to a concrete user
/// rather than the opaque API key.
#[derive(Clone, Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct RedmineCurrentUser {
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) login: String,
    #[serde(default)]
    pub(crate) firstname: String,
    #[serde(default)]
    pub(crate) lastname: String,
    #[serde(default)]
    pub(crate) mail: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineRoleCollection {
    #[serde(default)]
    pub(crate) roles: Vec<RedmineRole>,
    pub(crate) total_count: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineMembershipCollection {
    #[serde(default)]
    pub(crate) memberships: Vec<RedmineMembership>,
    pub(crate) total_count: Option<usize>,
    pub(crate) limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
pub(crate) struct RedmineRole {
    pub(crate) id: u64,
    pub(crate) name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineMembership {
    pub(crate) id: u64,
    pub(crate) user: Option<RedmineMembershipUser>,
    #[serde(default)]
    pub(crate) roles: Vec<RedmineMembershipRole>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub(crate) struct RedmineMembershipUser {
    pub(crate) id: u64,
    #[serde(default)]
    pub(crate) login: String,
    #[serde(default)]
    pub(crate) name: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineMembershipRole {
    pub(crate) id: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewUserMembership {
    pub(crate) membership: RedmineNewUserMembershipFields,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineNewUserMembershipFields {
    pub(crate) user_id: u64,
    pub(crate) role_ids: Vec<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RedmineCurrentUserResponse {
    pub(crate) user: RedmineCurrentUser,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineUpdateMembership {
    pub(crate) membership: RedmineUpdateMembershipFields,
}

#[derive(Debug, Serialize)]
pub(crate) struct RedmineUpdateMembershipFields {
    pub(crate) role_ids: Vec<u64>,
}
