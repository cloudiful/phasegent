// Compatibility facade: re-exports the `crate::time_tracking` surface so
// existing callers keep compiling. New code should import from
// `crate::time_tracking` directly.

#[allow(unused_imports)]
pub(crate) use crate::time_tracking::{
    TIMER_GITLAB_MARKER_PREFIX, TimerListOutput, TimerOutput, auto_finish_run, auto_start_run,
    bounded_error_message, execute, execute_recovery, format_unix_date, generate_projection_token,
    generate_run_id, generate_run_id_with_prefix, gitlab_time_entry_summary, now_epoch_seconds,
    project_run_with_gitlab_provider, project_run_with_provider, rounded_hours,
    time_entry_comments,
};
