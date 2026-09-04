pub mod api;
pub mod config;
pub mod dispatch;
pub mod forgejo;
pub mod gitlab;
pub mod redmine;

use crate::policy::Capability;

pub use api::{
    CommentOutput, IssueSearchItem, IssueSearchOptions, IssueSearchResult, IssueSummary,
    RepoSummary, ISSUE_SEARCH_DEFAULT_LIMIT, ISSUE_SEARCH_DEFAULT_PAGE,
    ISSUE_SEARCH_MAX_BODY_BYTES, ISSUE_SEARCH_MAX_LIMIT,
};
pub use config::{GitlabConfig, GitlabProvider, ProviderKind, RedmineConfig, RedmineProvider};
pub use dispatch::ProviderDispatcher;
pub use redmine::model::{RedmineIssueStatus, RedmineProject, RedmineVersion};

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
        options: &IssueSearchOptions,
    ) -> Result<IssueSearchResult, Self::Error>;
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
