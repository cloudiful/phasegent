// Compatibility facade for the Phase 2 timer CLI decomposition.
// The authoritative implementation now lives in `src/time_tracking/`.
// This file intentionally contains no logic; it only re-exports the
// stable `crate::time_tracking_cli::*` surface so existing callers
// (`src/infra/timer_store.rs`, provider contract tests, `phase2_tests`)
// continue to compile without churn. New code should import from
// `crate::time_tracking` directly.
// The facade re-exports via `crate::time_tracking` to keep the
// aggregator `mod.rs` as the single source of truth.

#[allow(unused_imports)]
pub(crate) use crate::time_tracking::{
    TIMER_GITLAB_MARKER_PREFIX, TimerListOutput, TimerOutput, bounded_error_message, execute,
    execute_recovery, format_unix_date, generate_projection_token, generate_run_id,
    gitlab_time_entry_summary, now_epoch_seconds, project_run_with_gitlab_provider,
    project_run_with_provider, rounded_hours, time_entry_comments,
};
