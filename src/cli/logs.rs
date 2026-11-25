//! The `phasegent logs` command: a bounded, read-only view of the prune
//! diagnostic log.
//!
//! The command reports the resolved log directory, the fixed retention
//! limits, the matching typed events, how many lines were skipped as
//! malformed, and whether a writer held the log lock while the query ran. It
//! writes nothing: no configuration is prepared, no provider is resolved, no
//! credential is used, no registry is opened, and the query itself is never
//! logged.

use serde::Serialize;

use crate::command::{DEFAULT_LIMIT, LogsCommand};
use crate::infra::diag_log::{
    self, ACTIVE_LOG, ARCHIVE_COUNT, DiagEvent, LOCK_FILE, MAX_FILE_BYTES, MAX_TOTAL_BYTES,
    RETENTION_DAYS,
};

/// The fixed retention limits the log directory follows.
#[derive(Debug, Serialize)]
pub(crate) struct LogsRetention {
    /// The size cap of one log file.
    pub max_file_bytes: u64,
    /// How many log files the directory holds: the active file and its
    /// archives.
    pub files: usize,
    /// The size cap of all log files together.
    pub max_total_bytes: u64,
    /// The age after which an owned log file is deleted on writer startup.
    pub retention_days: u64,
    /// Whether the active file rolls over on every UTC day.
    pub daily_rollover: bool,
    /// The active log file name.
    pub active_file: &'static str,
    /// The archive count beside the active file.
    pub archives: usize,
    /// The stable advisory lock file name.
    pub lock_file: &'static str,
}

impl LogsRetention {
    fn new() -> Self {
        Self {
            max_file_bytes: MAX_FILE_BYTES,
            files: ARCHIVE_COUNT + 1,
            max_total_bytes: MAX_TOTAL_BYTES,
            retention_days: RETENTION_DAYS,
            daily_rollover: true,
            active_file: ACTIVE_LOG,
            archives: ARCHIVE_COUNT,
            lock_file: LOCK_FILE,
        }
    }
}

/// The query envelope.
#[derive(Debug, Serialize)]
pub(crate) struct LogsEnvelope {
    /// The resolved log directory.
    pub directory: String,
    /// The fixed retention limits.
    pub retention: LogsRetention,
    /// The run identifier the query filtered for.
    pub run: Option<String>,
    /// The largest number of events the query returns.
    pub limit: usize,
    /// The matching typed events, oldest first.
    pub entries: Vec<DiagEvent>,
    /// How many lines were rejected as malformed.
    pub skipped_malformed: usize,
    /// How many files were rejected as unreadable.
    pub skipped_files: usize,
    /// Whether a writer held the log lock while the query ran.
    pub busy: bool,
}

/// The `--path` envelope: the resolved directory and its policy, without
/// reading or creating anything.
#[derive(Debug, Serialize)]
pub(crate) struct LogsPathEnvelope {
    /// The resolved log directory.
    pub directory: String,
    /// The fixed retention limits.
    pub retention: LogsRetention,
}

/// Run the `logs` command.
pub(crate) fn execute_logs(command: &LogsCommand) -> i32 {
    let directory = match diag_log::log_dir() {
        Ok(directory) => directory,
        Err(message) => {
            return crate::cli::structured_error(
                serde_json::json!({"kind":"config", "message":message}),
                1,
            );
        }
    };
    if command.path {
        let envelope = LogsPathEnvelope {
            directory: directory.to_string_lossy().into_owned(),
            retention: LogsRetention::new(),
        };
        return crate::cli::print_json(&envelope);
    }
    let limit = command.limit.unwrap_or(DEFAULT_LIMIT);
    let result = diag_log::query(&diag_log::LogQuery {
        run: command.run.clone(),
        limit,
    });
    let envelope = LogsEnvelope {
        directory: result.directory.to_string_lossy().into_owned(),
        retention: LogsRetention::new(),
        run: command.run.clone(),
        limit,
        entries: result.entries,
        skipped_malformed: result.skipped_malformed,
        skipped_files: result.skipped_files,
        busy: result.busy,
    };
    crate::cli::print_json(&envelope)
}
