#[allow(unused_imports)]
use crate::policy::Capability;
#[allow(unused_imports)]
use crate::providers::ProviderDispatcher;
#[allow(unused_imports)]
use crate::providers::api::{
    CommentOutput, IssueSearchOptions, IssueSearchResult, IssueSummary, PhasegentError,
};
use crate::providers::local::LocalProvider;
#[allow(unused_imports)]
use crate::providers::{IssueProvider, ProviderCapabilities, ProviderKind, RedmineProvider};

impl IssueProvider for RedmineProvider {
    type Error = PhasegentError;

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            issue_lifecycle: true,
            comments: true,
        }
    }

    fn supports(&self, capability: Capability) -> bool {
        RedmineProvider::supports(self, capability)
    }

    fn get_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        RedmineProvider::get_issue(self, number)
    }

    fn search_issues(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<IssueSearchResult, Self::Error> {
        RedmineProvider::search_issues(self, options)
    }

    fn search_issue_page(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<crate::providers::api::IssueSummaryPage, Self::Error> {
        RedmineProvider::search_issue_page(self, options)
    }

    fn create_issue(&self, title: &str, body: &str) -> Result<IssueSummary, Self::Error> {
        RedmineProvider::create_issue(self, title, body)
    }

    fn update_body(&self, number: u64, body: &str) -> Result<IssueSummary, Self::Error> {
        RedmineProvider::update_body(self, number, body)
    }

    fn close_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        RedmineProvider::close_issue(self, number)
    }

    fn create_comment(
        &self,
        issue: u64,
        body: &str,
        marker: &str,
    ) -> Result<CommentOutput, Self::Error> {
        RedmineProvider::create_comment(self, issue, body, marker)
    }

    fn get_comment(&self, issue: u64, comment: u64) -> Result<CommentOutput, Self::Error> {
        RedmineProvider::get_comment(self, issue, comment)
    }

    fn find_marker(&self, issue: u64, marker: &str) -> Result<CommentOutput, Self::Error> {
        RedmineProvider::find_marker(self, issue, marker)
    }

    fn list_comments(&self, issue: u64) -> Result<Vec<CommentOutput>, Self::Error> {
        RedmineProvider::list_comments(self, issue)
    }
}

impl IssueProvider for ProviderDispatcher {
    type Error = PhasegentError;

    fn capabilities(&self) -> ProviderCapabilities {
        match self {
            Self::Redmine(provider) => provider.capabilities(),
            Self::Local(provider) => provider.capabilities(),
        }
    }

    fn supports(&self, capability: Capability) -> bool {
        match self {
            Self::Redmine(provider) => provider.supports(capability),
            Self::Local(provider) => provider.supports(capability),
        }
    }

    fn get_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.get_issue(number),
            Self::Local(provider) => provider.get_issue(number),
        }
    }

    fn search_issues(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<IssueSearchResult, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.search_issues(options),
            Self::Local(provider) => provider.search_issues(options),
        }
    }

    fn search_issue_page(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<crate::providers::api::IssueSummaryPage, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.search_issue_page(options),
            Self::Local(provider) => provider.search_issue_page(options),
        }
    }

    fn create_issue(&self, title: &str, body: &str) -> Result<IssueSummary, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.create_issue(title, body),
            Self::Local(provider) => provider.create_issue(title, body),
        }
    }

    fn update_body(&self, number: u64, body: &str) -> Result<IssueSummary, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.update_body(number, body),
            Self::Local(provider) => provider.update_body(number, body),
        }
    }

    fn close_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.close_issue(number),
            Self::Local(provider) => provider.close_issue(number),
        }
    }

    fn create_comment(
        &self,
        issue: u64,
        body: &str,
        marker: &str,
    ) -> Result<CommentOutput, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.create_comment(issue, body, marker),
            Self::Local(provider) => provider.create_comment(issue, body, marker),
        }
    }

    fn get_comment(&self, issue: u64, comment: u64) -> Result<CommentOutput, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.get_comment(issue, comment),
            Self::Local(provider) => provider.get_comment(issue, comment),
        }
    }

    fn find_marker(&self, issue: u64, marker: &str) -> Result<CommentOutput, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.find_marker(issue, marker),
            Self::Local(provider) => provider.find_marker(issue, marker),
        }
    }

    fn list_comments(&self, issue: u64) -> Result<Vec<CommentOutput>, Self::Error> {
        match self {
            Self::Redmine(provider) => provider.list_comments(issue),
            Self::Local(provider) => provider.list_comments(issue),
        }
    }
}

impl IssueProvider for LocalProvider {
    type Error = PhasegentError;

    fn capabilities(&self) -> ProviderCapabilities {
        LocalProvider::capabilities(self)
    }

    fn supports(&self, capability: Capability) -> bool {
        LocalProvider::supports(self, capability)
    }

    fn get_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        LocalProvider::get_issue(self, number)
    }

    fn search_issues(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<IssueSearchResult, Self::Error> {
        LocalProvider::search_issues(self, options)
    }

    fn search_issue_page(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<crate::providers::api::IssueSummaryPage, Self::Error> {
        LocalProvider::search_issue_page(self, options)
    }

    fn create_issue(&self, title: &str, body: &str) -> Result<IssueSummary, Self::Error> {
        LocalProvider::create_issue(self, title, body)
    }

    fn update_body(&self, number: u64, body: &str) -> Result<IssueSummary, Self::Error> {
        LocalProvider::update_body(self, number, body)
    }

    fn close_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        LocalProvider::close_issue(self, number)
    }

    fn create_comment(
        &self,
        issue: u64,
        body: &str,
        marker: &str,
    ) -> Result<CommentOutput, Self::Error> {
        LocalProvider::create_comment(self, issue, body, marker)
    }

    fn get_comment(&self, issue: u64, comment: u64) -> Result<CommentOutput, Self::Error> {
        LocalProvider::get_comment(self, issue, comment)
    }

    fn find_marker(&self, issue: u64, marker: &str) -> Result<CommentOutput, Self::Error> {
        LocalProvider::find_marker(self, issue, marker)
    }

    fn list_comments(&self, issue: u64) -> Result<Vec<CommentOutput>, Self::Error> {
        LocalProvider::list_comments(self, issue)
    }
}
