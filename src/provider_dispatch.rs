use super::{
    CiProvider, GitlabProvider, IssueProvider, ProviderCapabilities, ProviderKind,
    RedmineIssueStatus, RedmineMetadataProvider, RedmineProject, RedmineProvider, RedmineVersion,
    RepoProvider,
};
use crate::ci_model::{
    CiInspectOutput, CiInspectRequest, CiJobLogsOutput, CiJobsOutput, CiRunSummary, CiRunsFilter,
    CiRunsOutput,
};
use crate::command::{CiCommand, RepoCommand};
use crate::forgejo::{ForgejoConfig, ForgejoProvider};
use crate::forgejo_model::{CommentOutput, ForgejoError, IssueSummary, RepoSummary};
use crate::policy::Capability;

pub enum ProviderDispatcher {
    Forgejo(ForgejoProvider),
    Redmine(RedmineProvider),
    /// Phase-1 GitLab foundation. The dispatcher already routes every
    /// trait call to GitlabProvider's not-supported stubs so subsequent
    /// phases only need to replace the implementation, not the dispatch
    /// wiring.
    Gitlab(GitlabProvider),
}

impl ProviderDispatcher {
    pub fn for_role(
        role: crate::policy::Role,
        config: ForgejoConfig,
    ) -> Result<Self, ForgejoError> {
        Ok(Self::Forgejo(ForgejoProvider::for_role(role, config)?))
    }

    pub fn redmine(
        role: crate::policy::Role,
        config: crate::provider_config::RedmineConfig,
    ) -> Result<Self, ForgejoError> {
        Ok(Self::Redmine(RedmineProvider::for_role(role, config)?))
    }

    pub fn gitlab(
        role: crate::policy::Role,
        config: crate::provider_config::GitlabConfig,
    ) -> Result<Self, ForgejoError> {
        Ok(Self::Gitlab(GitlabProvider::for_role(role, config)?))
    }

    /// Drive a `RepoCommand::Create` through whichever provider arm
    /// resolved. Phase 3 adds GitLab support; Redmine still surfaces a
    /// structured not-supported error. The provider-side enforcement
    /// of `--private` and namespace resolution stays inside each
    /// provider so this dispatcher stays thin.
    pub fn create_repo_for_command(
        &self,
        command: &RepoCommand,
        _role: crate::policy::Role,
        _api_base: Option<&str>,
        _repository: Option<&str>,
    ) -> Result<RepoSummary, ForgejoError> {
        let RepoCommand::Create {
            target,
            private,
            description,
            auto_init,
        } = command;
        RepoProvider::create_repo(self, target, *private, description, *auto_init)
    }

    /// Drive a `CiCommand` through whichever provider arm resolved.
    /// Phase 3 adds GitLab support; Redmine still surfaces a
    /// structured not-supported error.
    pub fn ci_for_command(&self, command: &CiCommand) -> Result<serde_json::Value, ForgejoError> {
        match command {
            CiCommand::Runs {
                sha,
                ref_name,
                status,
                workflow,
                page,
                limit,
            } => {
                let output = CiProvider::ci_runs(
                    self,
                    &CiRunsFilter {
                        sha: sha.clone(),
                        ref_name: ref_name.clone(),
                        status: status.clone(),
                        workflow: workflow.clone(),
                        page: *page,
                        limit: *limit,
                    },
                )?;
                serde_json::to_value(output)
                    .map_err(|error| ForgejoError::request("ci runs", error.to_string()))
            }
            CiCommand::RunGet { run_id } => {
                let output = CiProvider::ci_run_get(self, *run_id)?;
                serde_json::to_value(output)
                    .map_err(|error| ForgejoError::request("ci run get", error.to_string()))
            }
            CiCommand::RunJobs { run_id } => {
                let output = CiProvider::ci_run_jobs(self, *run_id)?;
                serde_json::to_value(output)
                    .map_err(|error| ForgejoError::request("ci run jobs", error.to_string()))
            }
            CiCommand::JobLogs { job_id, tail } => {
                let output = CiProvider::ci_job_logs(self, *job_id, *tail)?;
                serde_json::to_value(output)
                    .map_err(|error| ForgejoError::request("ci job logs", error.to_string()))
            }
            CiCommand::Inspect {
                sha,
                ref_name,
                wait,
                timeout,
                poll,
            } => {
                let output = CiProvider::ci_inspect(
                    self,
                    &CiInspectRequest {
                        sha: sha.clone(),
                        ref_name: ref_name.clone(),
                        wait: *wait,
                        timeout: *timeout,
                        poll: *poll,
                    },
                )?;
                serde_json::to_value(output)
                    .map_err(|error| ForgejoError::request("ci inspect", error.to_string()))
            }
        }
    }

    pub const fn kind(&self) -> ProviderKind {
        match self {
            Self::Forgejo(_) => ProviderKind::Forgejo,
            Self::Redmine(_) => ProviderKind::Redmine,
            Self::Gitlab(_) => ProviderKind::Gitlab,
        }
    }
}

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
            Capability::RepoCreate => self.capabilities().repository_creation,
            Capability::CommentCreate | Capability::CommentRead | Capability::CommentFindMarker => {
                self.capabilities().comments
            }
            Capability::CiRead => true,
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
        query: Option<&str>,
        state: &str,
    ) -> Result<Vec<IssueSummary>, Self::Error> {
        ForgejoProvider::search_issues(self, query, state)
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
        query: Option<&str>,
        state: &str,
    ) -> Result<Vec<IssueSummary>, Self::Error> {
        RedmineProvider::search_issues(self, query, state)
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

impl RedmineMetadataProvider for ForgejoProvider {
    type Error = ForgejoError;

    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        Err(ForgejoError::not_supported("forgejo", "project list"))
    }

    fn create_project(
        &self,
        _name: &str,
        _identifier: &str,
        _description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error> {
        Err(ForgejoError::not_supported("forgejo", "project create"))
    }

    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        Err(ForgejoError::not_supported("forgejo", "issue status list"))
    }

    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        Err(ForgejoError::not_supported("forgejo", "version list"))
    }
}

impl RedmineMetadataProvider for RedmineProvider {
    type Error = ForgejoError;

    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        RedmineProvider::list_projects(self)
    }

    fn create_project(
        &self,
        name: &str,
        identifier: &str,
        description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error> {
        RedmineProvider::create_project(self, name, identifier, description)
    }

    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        RedmineProvider::list_issue_statuses(self)
    }

    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        RedmineProvider::list_versions(self)
    }
}

// ============================================================================
// Phase-2 GitLab issue / note / label foundation. The dispatch wiring
// forwards every IssueProvider / RedmineMetadataProvider / RepoProvider /
// CiProvider trait call straight to the real
// [`crate::gitlab::GitlabProvider`] implementation. Capability flags
// for not-supported operations stay false so the shared CLI surfaces a
// structured not-supported error before any HTTP traffic.
// ============================================================================

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
        query: Option<&str>,
        state: &str,
    ) -> Result<Vec<IssueSummary>, Self::Error> {
        GitlabProvider::search_issues(self, query, state)
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

impl RedmineMetadataProvider for GitlabProvider {
    type Error = ForgejoError;

    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        Err(ForgejoError::not_supported("gitlab", "project list"))
    }

    fn create_project(
        &self,
        _name: &str,
        _identifier: &str,
        _description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error> {
        Err(ForgejoError::not_supported("gitlab", "project create"))
    }

    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        Err(ForgejoError::not_supported("gitlab", "issue status list"))
    }

    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        Err(ForgejoError::not_supported("gitlab", "version list"))
    }
}

impl RepoProvider for GitlabProvider {
    type Error = ForgejoError;

    fn create_repo(
        &self,
        target: &str,
        private: bool,
        description: &str,
        auto_init: bool,
    ) -> Result<RepoSummary, Self::Error> {
        // Phase 3 wires the GitLab adapter straight through to the
        // inherent `GitlabProvider::create_repo` method so a direct
        // trait call cannot recurse into the adapter. The CLI guard
        // still routes this provider only when the operator asked
        // for GitLab and supplied `--private`; private enforcement
        // and the namespace resolver live inside the provider.
        GitlabProvider::create_repo(self, target, private, description, auto_init)
    }
}

impl CiProvider for GitlabProvider {
    type Error = ForgejoError;

    fn ci_runs(&self, filter: &CiRunsFilter) -> Result<CiRunsOutput, Self::Error> {
        // Phase 3 forwards CI reads to the GitLab provider
        // implementation. Direct trait callers receive GitLab-shaped
        // output without ever bouncing through this adapter.
        GitlabProvider::ci_runs(self, filter)
    }

    fn ci_run_get(&self, run_id: u64) -> Result<CiRunSummary, Self::Error> {
        GitlabProvider::ci_run_get(self, run_id)
    }

    fn ci_run_jobs(&self, run_id: u64) -> Result<CiJobsOutput, Self::Error> {
        GitlabProvider::ci_run_jobs(self, run_id)
    }

    fn ci_job_logs(&self, job_id: u64, tail: usize) -> Result<CiJobLogsOutput, Self::Error> {
        GitlabProvider::ci_job_logs(self, job_id, tail)
    }

    fn ci_inspect(&self, request: &CiInspectRequest) -> Result<CiInspectOutput, Self::Error> {
        GitlabProvider::ci_inspect(self, request)
    }
}

impl RepoProvider for ForgejoProvider {
    type Error = ForgejoError;

    fn create_repo(
        &self,
        target: &str,
        private: bool,
        description: &str,
        auto_init: bool,
    ) -> Result<RepoSummary, Self::Error> {
        ForgejoProvider::create_repo(self, target, private, description, auto_init)
    }
}

impl RepoProvider for RedmineProvider {
    type Error = ForgejoError;

    fn create_repo(
        &self,
        target: &str,
        private: bool,
        description: &str,
        auto_init: bool,
    ) -> Result<RepoSummary, Self::Error> {
        RedmineProvider::create_repo(self, target, private, description, auto_init)
    }
}

impl CiProvider for ForgejoProvider {
    type Error = ForgejoError;

    fn ci_runs(&self, filter: &CiRunsFilter) -> Result<CiRunsOutput, Self::Error> {
        ForgejoProvider::ci_runs(self, filter)
    }

    fn ci_run_get(&self, run_id: u64) -> Result<CiRunSummary, Self::Error> {
        ForgejoProvider::ci_run_get(self, run_id)
    }

    fn ci_run_jobs(&self, run_id: u64) -> Result<CiJobsOutput, Self::Error> {
        ForgejoProvider::ci_run_jobs(self, run_id)
    }

    fn ci_job_logs(&self, job_id: u64, tail: usize) -> Result<CiJobLogsOutput, Self::Error> {
        ForgejoProvider::ci_job_logs(self, job_id, tail)
    }

    fn ci_inspect(&self, request: &CiInspectRequest) -> Result<CiInspectOutput, Self::Error> {
        ForgejoProvider::ci_inspect(self, request)
    }
}

impl CiProvider for RedmineProvider {
    type Error = ForgejoError;

    fn ci_runs(&self, filter: &CiRunsFilter) -> Result<CiRunsOutput, Self::Error> {
        RedmineProvider::ci_runs(self, filter)
    }

    fn ci_run_get(&self, run_id: u64) -> Result<CiRunSummary, Self::Error> {
        RedmineProvider::ci_run_get(self, run_id)
    }

    fn ci_run_jobs(&self, run_id: u64) -> Result<CiJobsOutput, Self::Error> {
        RedmineProvider::ci_run_jobs(self, run_id)
    }

    fn ci_job_logs(&self, job_id: u64, tail: usize) -> Result<CiJobLogsOutput, Self::Error> {
        RedmineProvider::ci_job_logs(self, job_id, tail)
    }

    fn ci_inspect(&self, request: &CiInspectRequest) -> Result<CiInspectOutput, Self::Error> {
        RedmineProvider::ci_inspect(self, request)
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
        query: Option<&str>,
        state: &str,
    ) -> Result<Vec<IssueSummary>, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.search_issues(query, state),
            Self::Redmine(provider) => provider.search_issues(query, state),
            Self::Gitlab(provider) => provider.search_issues(query, state),
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

impl RedmineMetadataProvider for ProviderDispatcher {
    type Error = ForgejoError;

    fn list_projects(&self) -> Result<Vec<RedmineProject>, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.list_projects(),
            Self::Redmine(provider) => provider.list_projects(),
            Self::Gitlab(provider) => provider.list_projects(),
        }
    }

    fn create_project(
        &self,
        name: &str,
        identifier: &str,
        description: Option<&str>,
    ) -> Result<RedmineProject, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.create_project(name, identifier, description),
            Self::Redmine(provider) => provider.create_project(name, identifier, description),
            Self::Gitlab(provider) => provider.create_project(name, identifier, description),
        }
    }

    fn list_issue_statuses(&self) -> Result<Vec<RedmineIssueStatus>, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.list_issue_statuses(),
            Self::Redmine(provider) => provider.list_issue_statuses(),
            Self::Gitlab(provider) => provider.list_issue_statuses(),
        }
    }

    fn list_project_versions(&self) -> Result<Vec<RedmineVersion>, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.list_project_versions(),
            Self::Redmine(provider) => provider.list_project_versions(),
            Self::Gitlab(provider) => provider.list_project_versions(),
        }
    }
}

impl RepoProvider for ProviderDispatcher {
    type Error = ForgejoError;

    fn create_repo(
        &self,
        target: &str,
        private: bool,
        description: &str,
        auto_init: bool,
    ) -> Result<RepoSummary, Self::Error> {
        match self {
            Self::Forgejo(provider) => {
                provider.create_repo(target, private, description, auto_init)
            }
            Self::Redmine(provider) => {
                provider.create_repo(target, private, description, auto_init)
            }
            Self::Gitlab(provider) => provider.create_repo(target, private, description, auto_init),
        }
    }
}

impl CiProvider for ProviderDispatcher {
    type Error = ForgejoError;

    fn ci_runs(&self, filter: &CiRunsFilter) -> Result<CiRunsOutput, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.ci_runs(filter),
            Self::Redmine(provider) => provider.ci_runs(filter),
            Self::Gitlab(provider) => provider.ci_runs(filter),
        }
    }

    fn ci_run_get(&self, run_id: u64) -> Result<CiRunSummary, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.ci_run_get(run_id),
            Self::Redmine(provider) => provider.ci_run_get(run_id),
            Self::Gitlab(provider) => provider.ci_run_get(run_id),
        }
    }

    fn ci_run_jobs(&self, run_id: u64) -> Result<CiJobsOutput, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.ci_run_jobs(run_id),
            Self::Redmine(provider) => provider.ci_run_jobs(run_id),
            Self::Gitlab(provider) => provider.ci_run_jobs(run_id),
        }
    }

    fn ci_job_logs(&self, job_id: u64, tail: usize) -> Result<CiJobLogsOutput, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.ci_job_logs(job_id, tail),
            Self::Redmine(provider) => provider.ci_job_logs(job_id, tail),
            Self::Gitlab(provider) => provider.ci_job_logs(job_id, tail),
        }
    }

    fn ci_inspect(&self, request: &CiInspectRequest) -> Result<CiInspectOutput, Self::Error> {
        match self {
            Self::Forgejo(provider) => provider.ci_inspect(request),
            Self::Redmine(provider) => provider.ci_inspect(request),
            Self::Gitlab(provider) => provider.ci_inspect(request),
        }
    }
}
