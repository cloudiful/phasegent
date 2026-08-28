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
use crate::providers::ProviderDispatcher;
#[allow(unused_imports)]
use crate::providers::api::{CommentOutput, ForgejoError, IssueSummary, RepoSummary};
#[allow(unused_imports)]
use crate::providers::forgejo::ForgejoConfig;
#[allow(unused_imports)]
use crate::providers::forgejo::ForgejoProvider;
#[allow(unused_imports)]
use crate::providers::{
    CiProvider, GitlabProvider, IssueProvider, ProviderCapabilities, ProviderKind,
    RedmineIssueStatus, RedmineMetadataProvider, RedmineProject, RedmineProvider, RedmineVersion,
    RepoProvider,
};

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
