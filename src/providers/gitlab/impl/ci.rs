//! Pipeline / job / log / inspect reads.

use crate::ci_model::{
    CiInspectOutput, CiInspectRequest, CiJobLogsOutput, CiJobsOutput, CiRunSummary, CiRunsFilter,
    CiRunsOutput, bound_log, pretty_ref as shared_pretty_ref,
};
use crate::providers::api::ForgejoError;
use crate::providers::gitlab::model::{
    ApiJob, ApiPipeline, pipeline_conclusion_from_gitlab, pipeline_status_from_gitlab,
};

use super::core::GitlabProvider;

impl GitlabProvider {
    /// `GET /projects/:id/pipelines` paginated until GitLab signals
    /// completion. `ref`, `sha`, and `status` are forwarded as
    /// query parameters so the orchestrator's `CiRunsFilter` maps
    /// cleanly onto GitLab's filters.
    pub(crate) fn ci_runs(&self, filter: &CiRunsFilter) -> Result<CiRunsOutput, ForgejoError> {
        let path = self.pipelines_path();
        let pipelines = self.http.paginate("ci runs", |http, page| {
            let mut params = vec![("page", page.to_string())];
            if let Some(sha) = filter.sha.as_deref().filter(|value| !value.is_empty()) {
                params.push(("sha", sha.to_owned()));
            }
            if let Some(ref_name) = filter.ref_name.as_deref().filter(|value| !value.is_empty()) {
                params.push(("ref", ref_name.to_owned()));
            }
            if let Some(status) = filter.status.as_deref().filter(|value| !value.is_empty()) {
                params.push(("status", status.to_owned()));
            }
            http.get_page::<ApiPipeline>(&path, &params, "ci runs")
        })?;
        let runs: Vec<CiRunSummary> = pipelines.into_iter().map(Into::into).collect();
        Ok(CiRunsOutput {
            workflow_runs: runs,
            total_count: None,
            page: filter.page,
            limit: filter.limit,
        })
    }

    /// `GET /projects/:id/pipelines/:pipeline_id` for one pipeline.
    pub(crate) fn ci_run_get(&self, run_id: u64) -> Result<CiRunSummary, ForgejoError> {
        let pipeline: ApiPipeline =
            self.http
                .get(&self.pipeline_path(run_id), &[], "ci run get")?;
        Ok(pipeline.into())
    }

    /// `GET /projects/:id/pipelines/:pipeline_id/jobs` for the jobs
    /// attached to one pipeline. GitLab returns every job in a single
    /// page (no pagination); we still honour the same paginator
    /// envelope so the helper stays symmetrical with the runs list.
    pub(crate) fn ci_run_jobs(&self, run_id: u64) -> Result<CiJobsOutput, ForgejoError> {
        let path = self.pipeline_jobs_path(run_id);
        let jobs = self.http.paginate("ci run jobs", |http, page| {
            http.get_page::<ApiJob>(&path, &[("page", page.to_string())], "ci run jobs")
        })?;
        Ok(CiJobsOutput {
            run_id,
            jobs: jobs.into_iter().map(Into::into).collect(),
        })
    }

    /// `GET /projects/:id/jobs/:job_id/trace` returning the raw
    /// job trace. The orchestrator applies [`bound_log`] so the
    /// shared `CiJobLogsOutput` contract is preserved regardless of
    /// how the provider delivers the bytes.
    pub(crate) fn ci_job_logs(
        &self,
        job_id: u64,
        tail: usize,
    ) -> Result<CiJobLogsOutput, ForgejoError> {
        let raw = self
            .http
            .get_text(&self.job_trace_path(job_id), &[], "ci job logs")?;
        let (log, truncated, bytes) = bound_log(&raw, tail);
        Ok(CiJobLogsOutput {
            job_id,
            log,
            truncated,
            bytes,
        })
    }

    /// GitLab equivalent of [`crate::providers::forgejo::ci::ForgejoProvider::ci_inspect`].
    /// The shared `CiInspectOutput` contract is preserved; only the
    /// GitLab-specific status / conclusion mapping differs from the
    /// Forgejo implementation.
    pub(crate) fn ci_inspect(
        &self,
        request: &CiInspectRequest,
    ) -> Result<CiInspectOutput, ForgejoError> {
        let filter = CiRunsFilter {
            sha: Some(request.sha.clone()),
            ref_name: request.ref_name.clone(),
            status: None,
            workflow: None,
            page: 1,
            limit: 50,
        };
        let mut poll_count = 1_usize;
        let mut selected = self.select_ci_run(&filter)?;
        if selected.is_none() {
            if !request.wait {
                return Ok(self.inspect_output(
                    "no_run",
                    request,
                    None,
                    poll_count,
                    Vec::new(),
                    Vec::new(),
                ));
            }
            if request.timeout == 0 || request.poll == 0 {
                return Ok(self.inspect_output(
                    "timeout",
                    request,
                    None,
                    poll_count,
                    Vec::new(),
                    Vec::new(),
                ));
            }
            let deadline =
                std::time::Instant::now() + std::time::Duration::from_secs(request.timeout);
            while std::time::Instant::now() < deadline {
                std::thread::sleep(
                    std::time::Duration::from_secs(request.poll)
                        .min(deadline.saturating_duration_since(std::time::Instant::now())),
                );
                poll_count += 1;
                selected = self.select_ci_run(&filter)?;
                if selected.is_some() {
                    break;
                }
            }
            if selected.is_none() {
                return Ok(self.inspect_output(
                    "timeout",
                    request,
                    None,
                    poll_count,
                    Vec::new(),
                    Vec::new(),
                ));
            }
        }
        let mut state = run_state(selected.as_ref().expect("selected run exists"));
        if request.wait && state == "running" {
            if request.timeout == 0 || request.poll == 0 {
                state = "timeout";
            } else {
                let deadline =
                    std::time::Instant::now() + std::time::Duration::from_secs(request.timeout);
                while state == "running" && std::time::Instant::now() < deadline {
                    std::thread::sleep(
                        std::time::Duration::from_secs(request.poll)
                            .min(deadline.saturating_duration_since(std::time::Instant::now())),
                    );
                    poll_count += 1;
                    let run_id = selected.as_ref().expect("selected run exists").id;
                    selected = Some(self.ci_run_get(run_id)?);
                    state = run_state(selected.as_ref().expect("selected run exists"));
                }
                if state == "running" {
                    state = "timeout";
                }
            }
        }
        let (failed_jobs, log_excerpts) = if state == "failure" {
            let jobs = selected
                .as_ref()
                .and_then(|run| self.ci_run_jobs(run.id).ok())
                .map(|jobs| jobs.jobs.into_iter().filter(job_failed).collect::<Vec<_>>())
                .unwrap_or_default();
            let excerpts = jobs
                .iter()
                .filter_map(|job| {
                    self.ci_job_logs(job.id, crate::ci_model::DEFAULT_LOG_TAIL)
                        .ok()
                        .map(|log| crate::ci_model::CiLogExcerpt {
                            job_id: job.id,
                            name: job.name.clone(),
                            log: log.log,
                            truncated: log.truncated,
                        })
                })
                .collect();
            (jobs, excerpts)
        } else {
            (Vec::new(), Vec::new())
        };
        Ok(self.inspect_output(
            state,
            request,
            selected,
            poll_count,
            failed_jobs,
            log_excerpts,
        ))
    }

    pub(crate) fn select_ci_run(
        &self,
        filter: &CiRunsFilter,
    ) -> Result<Option<CiRunSummary>, ForgejoError> {
        Ok(self
            .ci_runs(filter)?
            .workflow_runs
            .into_iter()
            .filter(|run| {
                let sha_matches = filter.sha.as_deref().is_none_or(|requested| {
                    run.commit_sha
                        .as_deref()
                        .or(run.head_sha.as_deref())
                        .is_none_or(|actual| actual == requested)
                });
                let ref_matches = filter.ref_name.as_deref().is_none_or(|requested| {
                    run.ref_name
                        .as_deref()
                        .or(run.pretty_ref.as_deref())
                        .is_none_or(|actual| {
                            actual == requested
                                || shared_pretty_ref(actual).as_deref() == Some(requested)
                        })
                });
                sha_matches && ref_matches
            })
            .max_by_key(|run| (run.run_number, run.id)))
    }

    pub(crate) fn inspect_output(
        &self,
        state: &str,
        request: &CiInspectRequest,
        selected: Option<CiRunSummary>,
        poll_count: usize,
        failed_jobs: Vec<crate::ci_model::CiJobSummary>,
        log_excerpts: Vec<crate::ci_model::CiLogExcerpt>,
    ) -> CiInspectOutput {
        let ref_name = request
            .ref_name
            .clone()
            .or_else(|| selected.as_ref().and_then(|run| run.ref_name.clone()));
        let url = selected.as_ref().and_then(|run| run.html_url.clone());
        CiInspectOutput {
            state: state.to_owned(),
            selected_run: selected,
            sha: request.sha.clone(),
            ref_name,
            url,
            failed_jobs,
            log_excerpts,
            checked_at: crate::ci_model::checked_at(),
            poll_count,
        }
    }
}

impl From<ApiPipeline> for CiRunSummary {
    fn from(pipeline: ApiPipeline) -> Self {
        let status = pipeline_status_from_gitlab(&pipeline.status);
        let conclusion = pipeline_conclusion_from_gitlab(&pipeline.status, None);
        let ref_name = pipeline.ref_name.clone();
        let pretty_ref = ref_name.as_deref().and_then(shared_pretty_ref);
        Self {
            id: pipeline.id,
            run_number: pipeline.iid,
            status,
            conclusion,
            head_sha: pipeline.before_sha.clone(),
            commit_sha: pipeline.sha.clone(),
            ref_name,
            pretty_ref,
            workflow_id: None,
            html_url: pipeline.web_url,
            created: pipeline.created_at,
            started: pipeline.started_at,
            stopped: pipeline.finished_at,
        }
    }
}

impl From<ApiJob> for crate::ci_model::CiJobSummary {
    fn from(job: ApiJob) -> Self {
        let status = pipeline_status_from_gitlab(&job.status);
        let conclusion = pipeline_conclusion_from_gitlab(&job.status, job.conclusion.as_deref());
        let run_id = job.pipeline.as_ref().and_then(|pipeline| pipeline.id);
        Self {
            id: job.id,
            name: job.name,
            status,
            conclusion,
            run_id,
            attempt: job.queued_duration.map(|value| serde_json::json!(value)),
            task_id: None,
        }
    }
}

/// Map a `CiRunSummary` status / conclusion to the inspect state's
/// shared vocabulary. Mirrors the Forgejo inspector so the shared
/// CLI consumer does not need provider-specific branches.
///
/// GitLab exposes `skipped` as a terminal pipeline / job state that
/// is distinct from `failure`: a skipped run was deliberately
/// bypassed (for example by a `when: never` rule) and does not
/// represent a regression. The shared Forgejo inspector excludes
/// `skipped` from its failure set, so we mirror that exclusion here
/// and surface a dedicated `skipped` state.
pub(crate) fn run_state(run: &CiRunSummary) -> &'static str {
    let status = run.status.to_ascii_lowercase();
    let conclusion = run
        .conclusion
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if status == "success" || conclusion == "success" {
        return "success";
    }
    if matches!(
        status.as_str(),
        "running" | "pending" | "waiting_for_resource" | "preparing" | "scheduled" | "created"
    ) {
        return "running";
    }
    if status == "skipped" || conclusion == "skipped" {
        return "skipped";
    }
    if matches!(
        status.as_str(),
        "failure" | "failed" | "error" | "cancelled" | "canceled" | "timed_out"
    ) || (!conclusion.is_empty() && conclusion != "success")
    {
        return "failure";
    }
    "unknown"
}

/// True when the job's terminal status indicates failure. Skipped
/// jobs are deliberately excluded: GitLab uses `skipped` for
/// `when: never` and `allow_failure: true` runs that did not
/// actually execute, and they must not block a CI-gated pipeline.
pub(crate) fn job_failed(job: &crate::ci_model::CiJobSummary) -> bool {
    let status = job.status.to_ascii_lowercase();
    let conclusion = job
        .conclusion
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        status.as_str(),
        "failure" | "failed" | "error" | "cancelled" | "canceled" | "timed_out"
    ) || matches!(
        conclusion.as_str(),
        "failure" | "failed" | "error" | "cancelled" | "canceled" | "timed_out"
    )
}
