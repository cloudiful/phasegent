//! `admin config migrate`: move the legacy SQLite/TOML configuration into
//! the JSONC configuration file.
//!
//! The command is human-operator only. The registry denies every AI role
//! before the parser accepts it, and a managed session without a role
//! fails closed. Nothing here prints a configuration value or a
//! credential; the report carries paths and row counts only.

use crate::config_migrate::{MigrationOptions, MigrationReport};
use std::path::PathBuf;

/// Run the migration and render the report.
pub(crate) fn execute_migrate(from_sqlite: &str, from_toml: Option<&str>) -> i32 {
    let options = MigrationOptions {
        from_sqlite: PathBuf::from(from_sqlite),
        from_toml: from_toml.map(PathBuf::from),
    };
    match crate::config_migrate::migrate(&options) {
        Ok(report) => print_report(&report),
        Err(message) => {
            super::structured_error(serde_json::json!({"kind":"config", "message":message}), 1)
        }
    }
}

fn print_report(report: &MigrationReport) -> i32 {
    super::print_json(report)
}
