use crate::ci_model::{
    CiInspectOutput, CiInspectRequest, CiJobSummary, CiLogExcerpt, CiRunSummary, CiRunsFilter,
    DEFAULT_LOG_TAIL, checked_at, pretty_ref,
};
use crate::forgejo::ForgejoProvider;
use crate::forgejo_model::ForgejoError;
use std::time::{Duration, Instant};

impl ForgejoProvider {
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
        let mut poll_count = 1;
        let mut selected = self.select_ci_run(&filter)?;
        if selected.is_none() {
            if !request.wait {
                return Ok(inspect_output(
                    "no_run",
                    request,
                    None,
                    poll_count,
                    Vec::new(),
                    Vec::new(),
                ));
            }
            if request.timeout == 0 || request.poll == 0 {
                return Ok(inspect_output(
                    "timeout",
                    request,
                    None,
                    poll_count,
                    Vec::new(),
                    Vec::new(),
                ));
            }
            let deadline = Instant::now() + Duration::from_secs(request.timeout);
            while Instant::now() < deadline {
                sleep_until_next_poll(deadline, request.poll);
                poll_count += 1;
                selected = self.select_ci_run(&filter)?;
                if selected.is_some() {
                    break;
                }
            }
            if selected.is_none() {
                return Ok(inspect_output(
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
                let deadline = Instant::now() + Duration::from_secs(request.timeout);
                while state == "running" && Instant::now() < deadline {
                    sleep_until_next_poll(deadline, request.poll);
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
                    self.ci_job_logs(job.id, DEFAULT_LOG_TAIL)
                        .ok()
                        .map(|log| CiLogExcerpt {
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
        Ok(inspect_output(
            state,
            request,
            selected,
            poll_count,
            failed_jobs,
            log_excerpts,
        ))
    }

    fn select_ci_run(&self, filter: &CiRunsFilter) -> Result<Option<CiRunSummary>, ForgejoError> {
        Ok(self
            .ci_runs(filter)?
            .workflow_runs
            .into_iter()
            .filter(|run| {
                let sha_matches = filter.sha.as_deref().is_none_or(|requested| {
                    run.head_sha
                        .as_deref()
                        .or(run.commit_sha.as_deref())
                        .is_none_or(|actual| actual == requested)
                });
                let ref_matches = filter.ref_name.as_deref().is_none_or(|requested| {
                    run.ref_name
                        .as_deref()
                        .or(run.pretty_ref.as_deref())
                        .is_none_or(|actual| {
                            actual == requested || pretty_ref(actual).as_deref() == Some(requested)
                        })
                });
                sha_matches && ref_matches
            })
            .max_by_key(|run| (run.run_number, run.id)))
    }
}

fn inspect_output(
    state: &str,
    request: &CiInspectRequest,
    selected: Option<CiRunSummary>,
    poll_count: usize,
    failed_jobs: Vec<CiJobSummary>,
    log_excerpts: Vec<CiLogExcerpt>,
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
        checked_at: checked_at(),
        poll_count,
    }
}

fn run_state(run: &CiRunSummary) -> &'static str {
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
        "queued" | "waiting" | "requested" | "pending" | "in_progress" | "running"
    ) {
        return "running";
    }
    if matches!(
        status.as_str(),
        "failure" | "failed" | "error" | "cancelled" | "timed_out"
    ) || (!conclusion.is_empty() && conclusion != "success")
    {
        return "failure";
    }
    "unknown"
}

fn job_failed(job: &CiJobSummary) -> bool {
    let status = job.status.to_ascii_lowercase();
    let conclusion = job
        .conclusion
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        status.as_str(),
        "failure" | "failed" | "error" | "cancelled" | "timed_out"
    ) || matches!(
        conclusion.as_str(),
        "failure" | "failed" | "error" | "cancelled" | "timed_out"
    )
}

fn sleep_until_next_poll(deadline: Instant, poll: u64) {
    let remaining = deadline.saturating_duration_since(Instant::now());
    std::thread::sleep(Duration::from_secs(poll).min(remaining));
}
