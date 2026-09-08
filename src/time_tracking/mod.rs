// Thin aggregator: declares the module tree and re-exports the stable
// surface used by `crate::time_tracking_cli` and tests.

pub(crate) mod dispatch;
pub(crate) mod finish;
pub(crate) mod projection_gitlab;
pub(crate) mod projection_redmine;
pub(crate) mod recover;
pub(crate) mod start;
pub(crate) mod util;

// Keep the `crate::time_tracking::*` surface aligned with the historic
// `crate::time_tracking_cli::*` path.
pub(crate) use dispatch::{TimerListOutput, TimerOutput, execute, execute_recovery};
pub(crate) use finish::auto_finish_run;
pub(crate) use projection_gitlab::{
    TIMER_GITLAB_MARKER_PREFIX, gitlab_time_entry_summary, project_run_with_gitlab_provider,
};
pub(crate) use projection_redmine::{project_run_with_provider, time_entry_comments};
pub(crate) use start::auto_start_run;
pub(crate) use util::{
    bounded_error_message, format_unix_date, generate_projection_token, generate_run_id,
    generate_run_id_with_prefix, now_epoch_seconds, rounded_hours,
};
