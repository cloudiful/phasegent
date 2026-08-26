use crate::ci_model::{
    CiInspectOutput, CiInspectRequest, CiJobLogsOutput, CiJobsOutput, CiRunSummary, CiRunsFilter,
    CiRunsOutput,
};
use crate::forgejo_model::{CommentOutput, IssueSummary, RepoSummary};
use crate::policy::Capability;

pub use crate::provider_config::{
    GitlabConfig, GitlabProvider, ProviderKind, RedmineConfig, RedmineProvider,
};
pub use crate::redmine_model::{RedmineIssueStatus, RedmineProject, RedmineVersion};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub struct ProviderCapabilities {
    pub issue_lifecycle: bool,
    pub comments: bool,
    pub repository_creation: bool,
}

#[allow(dead_code)]
pub trait IssueProvider {
    type Error;

    fn capabilities(&self) -> ProviderCapabilities;
    fn supports(&self, capability: Capability) -> bool;
    fn get_issue(&self, number: u64) -> Result<IssueSummary, Self::Error>;
    fn search_issues(
        &self,
        query: Option<&str>,
        state: &str,
    ) -> Result<Vec<IssueSummary>, Self::Error>;
    fn create_issue(&self, title: &str, body: &str) -> Result<IssueSummary, Self::Error>;
    fn update_body(&self, number: u64, body: &str) -> Result<IssueSummary, Self::Error>;
    fn close_issue(&self, number: u64) -> Result<IssueSummary, Self::Error>;
    fn create_comment(
        &self,
        issue: u64,
        body: &str,
        marker: &str,
    ) -> Result<CommentOutput, Self::Error>;
    fn get_comment(&self, issue: u64, comment: u64) -> Result<CommentOutput, Self::Error>;
    fn find_marker(&self, issue: u64, marker: &str) -> Result<CommentOutput, Self::Error>;
}

pub trait RedmineMetadataProvider {
    type Error;

    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error>;
    fn create_project(
        &self,
        name: &str,
        identifier: &str,
        description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error>;
    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error>;
    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error>;
}

pub trait RepoProvider {
    type Error;

    fn create_repo(
        &self,
        target: &str,
        private: bool,
        description: &str,
        auto_init: bool,
    ) -> Result<RepoSummary, Self::Error>;
}

pub trait CiProvider {
    type Error;

    fn ci_runs(&self, filter: &CiRunsFilter) -> Result<CiRunsOutput, Self::Error>;
    fn ci_run_get(&self, run_id: u64) -> Result<CiRunSummary, Self::Error>;
    fn ci_run_jobs(&self, run_id: u64) -> Result<CiJobsOutput, Self::Error>;
    fn ci_job_logs(&self, job_id: u64, tail: usize) -> Result<CiJobLogsOutput, Self::Error>;
    fn ci_inspect(&self, request: &CiInspectRequest) -> Result<CiInspectOutput, Self::Error>;
}

#[path = "provider_dispatch.rs"]
mod provider_dispatch;

pub use provider_dispatch::ProviderDispatcher;
