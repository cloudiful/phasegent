//! CLI surface for the global all-projects bootstrap.
//!
//! Routes `admin workflow bootstrap --all-projects` (with its optional
//! `--dry-run` preview) to the workflow entry point and renders the complete
//! serializable report. The exit code derives from the same report that is
//! printed, so a partial failure stays visible with a nonzero code.

mod report;

use crate::workflow;

/// Execute the global bootstrap. Admin-only gating, provider selection, and
/// flag-conflict rejection already happened in the caller, before any network
/// access.
pub(crate) fn execute(api_base: Option<&str>, dry_run: bool) -> i32 {
    match workflow::bootstrap_all_projects(api_base, dry_run) {
        Ok(report) => report::emit(&report),
        Err(error) => super::provider_error(error),
    }
}
