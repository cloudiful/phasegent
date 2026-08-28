// Thin aggregator for the timer CLI decomposition (Phase 2).
// All behavior remains in the focused submodules; this file only
// declares the module tree and re-exports the stable public(crate)
// surface used by `crate::time_tracking_cli` and tests.

pub(crate) mod dispatch;
pub(crate) mod finish;
pub(crate) mod projection_gitlab;
pub(crate) mod projection_redmine;
pub(crate) mod recover;
pub(crate) mod start;
pub(crate) mod util;

// Stable re-exports: keep the `crate::time_tracking::*` surface
// aligned with the historic `crate::time_tracking_cli::*` path.
pub(crate) use dispatch::{TimerListOutput, TimerOutput, execute, execute_recovery};
pub(crate) use projection_gitlab::{
    TIMER_GITLAB_MARKER_PREFIX, gitlab_time_entry_summary, project_run_with_gitlab_provider,
};
pub(crate) use projection_redmine::{project_run_with_provider, time_entry_comments};
pub(crate) use util::{
    bounded_error_message, format_unix_date, generate_projection_token, generate_run_id,
    now_epoch_seconds, rounded_hours,
};
