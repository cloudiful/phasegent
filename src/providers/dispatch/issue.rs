#[allow(unused_imports)]
use crate::policy::Capability;
#[allow(unused_imports)]
use crate::providers::ProviderDispatcher;
#[allow(unused_imports)]
use crate::providers::api::{
    CommentOutput, ForgejoError, IssueSearchOptions, IssueSearchResult, IssueSummary, RepoSummary,
};
#[allow(unused_imports)]
use crate::providers::forgejo::ForgejoConfig;
#[allow(unused_imports)]
use crate::providers::forgejo::ForgejoProvider;
#[allow(unused_imports)]
use crate::providers::{
    GitlabProvider, IssueProvider, ProviderCapabilities, ProviderKind, RedmineIssueStatus,
    RedmineMetadataProvider, RedmineProject, RedmineProvider, RedmineVersion, RepoProvider,
};

impl IssueProvider for ForgejoProvider {
    type Error = ForgejoError;

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            issue_lifecycle: true,
            comments: true,
            repository_creation: true,
        }
    }

    fn supports(&self, capability: Capability) -> bool {
        match capability {
            Capability::IssueRead
            | Capability::IssueSearch
            | Capability::IssueCreate
            | Capability::IssueUpdateBody
            | Capability::IssueClose => self.capabilities().issue_lifecycle,
            Capability::IssueAttachmentUpload => false,
            Capability::RepoCreate => self.capabilities().repository_creation,
            Capability::CommentCreate | Capability::CommentRead | Capability::CommentFindMarker => {
                self.capabilities().comments
            }
            Capability::ProjectRead | Capability::ProjectCreate | Capability::IssueStatusRead => {
                false
            }
            Capability::VersionRead => false,
            Capability::RelationRead | Capability::RelationCreate | Capability::RelationDelete => {
                false
            }
        }
    }

    fn get_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        ForgejoProvider::get_issue(self, number)
    }

    fn search_issues(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<IssueSearchResult, Self::Error> {
        ForgejoProvider::search_issues(self, options)
    }

    fn search_issue_page(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<crate::providers::api::IssueSummaryPage, Self::Error> {
        ForgejoProvider::search_issue_page(self, options)
    }

    fn create_issue(&self, title: &str, body: &str) -> Result<IssueSummary, Self::Error> {
        ForgejoProvider::create_issue(self, title, body)
    }

    fn update_body(&self, number: u64, body: &str) -> Result<IssueSummary, Self::Error> {
        ForgejoProvider::update_body(self, number, body)
    }

    fn close_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        ForgejoProvider::close_issue(self, number)
    }

    fn create_comment(
        &self,
        issue: u64,
        body: &str,
        marker: &str,
    ) -> Result<CommentOutput, Self::Error> {
        ForgejoProvider::create_comment(self, issue, body, marker)
    }

    fn get_comment(&self, issue: u64, comment: u64) -> Result<CommentOutput, Self::Error> {
        ForgejoProvider::get_comment(self, issue, comment)
    }

    fn find_marker(&self, issue: u64, marker: &str) -> Result<CommentOutput, Self::Error> {
        ForgejoProvider::find_marker(self, issue, marker)
    }
}

impl IssueProvider for RedmineProvider {
    type Error = ForgejoError;

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            issue_lifecycle: true,
            comments: true,
            repository_creation: false,
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
}

impl IssueProvider for GitlabProvider {
    type Error = ForgejoError;

    fn capabilities(&self) -> ProviderCapabilities {
        GitlabProvider::capabilities(self)
    }

    fn supports(&self, capability: Capability) -> bool {
        GitlabProvider::supports(self, capability)
    }

    fn get_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        GitlabProvider::get_issue(self, number)
    }

    fn search_issues(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<IssueSearchResult, Self::Error> {
        GitlabProvider::search_issues(self, options)
    }

    fn search_issue_page(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<crate::providers::api::IssueSummaryPage, Self::Error> {
        GitlabProvider::search_issue_page(self, options)
    }

    fn create_issue(&self, title: &str, body: &str) -> Result<IssueSummary, Self::Error> {
        // The shared IssueProvider trait surface does not accept a
        // label list. The CLI layer's planning-aware path forwards
        // tracker labels through `provider.create_issue_with_labels`
        // directly on the GitlabProvider; the trait path is the
        // legacy plain issue body used by Forgejo / Redmine callers
        // that do not need labels.
        GitlabProvider::create_issue(self, title, body)
    }

    fn update_body(&self, number: u64, body: &str) -> Result<IssueSummary, Self::Error> {
        GitlabProvider::update_body(self, number, body)
    }

    fn close_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        GitlabProvider::close_issue(self, number)
    }

    fn create_comment(
        &self,
        issue: u64,
        body: &str,
        marker: &str,
    ) -> Result<CommentOutput, Self::Error> {
        GitlabProvider::create_note(self, issue, body).map(|comment| CommentOutput {
            marker: Some(marker.to_owned()),
            ..comment
        })
    }

    fn get_comment(&self, issue: u64, comment: u64) -> Result<CommentOutput, Self::Error> {
        GitlabProvider::get_note(self, issue, comment)
    }

    fn find_marker(&self, issue: u64, marker: &str) -> Result<CommentOutput, Self::Error> {
        GitlabProvider::find_marker(self, issue, marker)
    }
}

impl IssueProvider for ProviderDispatcher {
    type Error = ForgejoError;

    fn capabilities(&self) -> ProviderCapabilities {
        match self {
            Self::Forgejo(provider) => provider.capabilities(),
            Self::Redmine(provider) => provider.capabilities(),
            Self::Gitlab(provider) => provider.capabilities(),
        }
    }

    fn supports(&self, capability: Capability) -> bool {
        match self {
            Self::Forgejo(provider) => provider.supports(capability),
            Self::Redmine(provider) => provider.supports(capability),
            Self::Gitlab(provider) => provider.supports(capability),
        }
    }

    fn get_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.get_issue(number),
            Self::Redmine(provider) => provider.get_issue(number),
            Self::Gitlab(provider) => provider.get_issue(number),
        }
    }

    fn search_issues(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<IssueSearchResult, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.search_issues(options),
            Self::Redmine(provider) => provider.search_issues(options),
            Self::Gitlab(provider) => provider.search_issues(options),
        }
    }

    fn search_issue_page(
        &self,
        options: &IssueSearchOptions,
    ) -> Result<crate::providers::api::IssueSummaryPage, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.search_issue_page(options),
            Self::Redmine(provider) => provider.search_issue_page(options),
            Self::Gitlab(provider) => provider.search_issue_page(options),
        }
    }

    fn create_issue(&self, title: &str, body: &str) -> Result<IssueSummary, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.create_issue(title, body),
            Self::Redmine(provider) => provider.create_issue(title, body),
            Self::Gitlab(provider) => provider.create_issue(title, body),
        }
    }

    fn update_body(&self, number: u64, body: &str) -> Result<IssueSummary, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.update_body(number, body),
            Self::Redmine(provider) => provider.update_body(number, body),
            Self::Gitlab(provider) => provider.update_body(number, body),
        }
    }

    fn close_issue(&self, number: u64) -> Result<IssueSummary, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.close_issue(number),
            Self::Redmine(provider) => provider.close_issue(number),
            Self::Gitlab(provider) => provider.close_issue(number),
        }
    }

    fn create_comment(
        &self,
        issue: u64,
        body: &str,
        marker: &str,
    ) -> Result<CommentOutput, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.create_comment(issue, body, marker),
            Self::Redmine(provider) => provider.create_comment(issue, body, marker),
            Self::Gitlab(provider) => provider.create_comment(issue, body, marker),
        }
    }

    fn get_comment(&self, issue: u64, comment: u64) -> Result<CommentOutput, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.get_comment(issue, comment),
            Self::Redmine(provider) => provider.get_comment(issue, comment),
            Self::Gitlab(provider) => provider.get_comment(issue, comment),
        }
    }

    fn find_marker(&self, issue: u64, marker: &str) -> Result<CommentOutput, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.find_marker(issue, marker),
            Self::Redmine(provider) => provider.find_marker(issue, marker),
            Self::Gitlab(provider) => provider.find_marker(issue, marker),
        }
    }
}
