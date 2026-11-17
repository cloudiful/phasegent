//! The `phasegent admin config upgrade` executor.
//!
//! The upgrade is a human-operator surface: the parser gate already denies
//! every AI role and every managed session without a role before this
//! executor runs. The command never reads the environment, never contacts a
//! provider, and never touches the business database.

/// Upgrade the configuration file to schema version 2.
pub(crate) fn execute_config_upgrade() -> i32 {
    match crate::config_upgrade::upgrade() {
        Ok(report) => crate::cli::print_json(&report),
        Err(message) => {
            crate::cli::structured_error(serde_json::json!({"kind":"config", "message":message}), 1)
        }
    }
}
