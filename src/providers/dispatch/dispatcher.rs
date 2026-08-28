#[allow(unused_imports)]
use crate::ci_model::{
    CiInspectOutput, CiInspectRequest, CiJobLogsOutput, CiJobsOutput, CiRunSummary, CiRunsFilter,
    CiRunsOutput,
};
#[allow(unused_imports)]
use crate::command::{CiCommand, RepoCommand};
#[allow(unused_imports)]
use crate::policy::Capability;
#[allow(unused_imports)]
use crate::providers::api::{CommentOutput, ForgejoError, IssueSummary, RepoSummary};
use crate::providers::forgejo::{ForgejoConfig, ForgejoProvider};
#[allow(unused_imports)]
use crate::providers::{
    CiProvider, GitlabProvider, IssueProvider, ProviderCapabilities, ProviderKind,
    RedmineIssueStatus, RedmineMetadataProvider, RedmineProject, RedmineProvider, RedmineVersion,
    RepoProvider,
};

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
        config: crate::providers::config::RedmineConfig,
    ) -> Result<Self, ForgejoError> {
        Ok(Self::Redmine(RedmineProvider::for_role(role, config)?))
    }

    pub fn gitlab(
        role: crate::policy::Role,
        config: crate::providers::config::GitlabConfig,
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
