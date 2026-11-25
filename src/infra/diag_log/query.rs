//! The read-only diagnostic query.
//!
//! A query never writes: it resolves the log directory, reads the owned files
//! that exist, and reports what it found. It creates no directory, no file,
//! and no lock, and it never deletes or rotates anything.
//!
//! The reader takes the existing shared lock of the log directory. A writer
//! that holds the exclusive lock makes the query report `busy` with an empty
//! entry list instead of blocking, so a query never waits for a run.
//!
//! Only the owned files are read, each at most once and at most its size cap.
//! A symbolic link, a non-regular file, or an oversized file is skipped and
//! counted; an event that does not match the typed schema is counted as
//! malformed and never echoed.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use super::event::DiagEvent;
use super::writer::{ACTIVE_LOG, ARCHIVE_COUNT, LOCK_FILE, MAX_FILE_BYTES, archive_name, log_dir};

/// One read-only query.
pub(crate) struct LogQuery {
    /// The exact run identifier to filter for, when the caller named one.
    pub run: Option<String>,
    /// How many events the query returns at most.
    pub limit: usize,
}

/// The result of one read-only query.
pub(crate) struct LogQueryResult {
    /// The resolved log directory.
    pub directory: PathBuf,
    /// The matching events, oldest first.
    pub entries: Vec<DiagEvent>,
    /// How many lines were rejected as malformed.
    pub skipped_malformed: usize,
    /// How many files were rejected as unreadable.
    pub skipped_files: usize,
    /// Whether a writer held the log lock while the query ran.
    pub busy: bool,
}

impl LogQueryResult {
    fn empty(directory: PathBuf) -> Self {
        Self {
            directory,
            entries: Vec::new(),
            skipped_malformed: 0,
            skipped_files: 0,
            busy: false,
        }
    }
}

/// The owned files in read order: the oldest archive first, the active file
/// last.
fn read_order() -> Vec<String> {
    let mut names = Vec::with_capacity(ARCHIVE_COUNT + 1);
    for index in (1..=ARCHIVE_COUNT).rev() {
        names.push(archive_name(index));
    }
    names.push(ACTIVE_LOG.to_owned());
    names
}

/// Read the owned diagnostic events. The query creates nothing and blocks for
/// nothing.
pub(crate) fn query(query: &LogQuery) -> LogQueryResult {
    let directory = match log_dir() {
        Ok(directory) => directory,
        Err(_) => return LogQueryResult::empty(PathBuf::new()),
    };
    query_in(&directory, query)
}

/// Read the owned diagnostic events of one known directory. The production
/// path resolves the platform directory and delegates here; the unit tests
/// use this seam so no test reads or writes the real log directory.
pub(crate) fn query_in(directory: &Path, query: &LogQuery) -> LogQueryResult {
    let mut result = LogQueryResult::empty(directory.to_path_buf());
    // The reader never creates the lock: a missing lock means no run has
    // written yet, which is an empty success. A symbolic link is never
    // followed, so a link cannot redirect the reader to another file.
    let lock_path = directory.join(LOCK_FILE);
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return result;
        }
        Ok(_) => {}
        Err(_) => return result,
    }
    let lock = match File::open(&lock_path) {
        Ok(lock) => lock,
        Err(_) => return result,
    };
    match lock.try_lock_shared() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            result.busy = true;
            return result;
        }
        Err(std::fs::TryLockError::Error(_)) => {
            result.busy = true;
            return result;
        }
    }
    let mut events: Vec<DiagEvent> = Vec::new();
    for name in read_order() {
        match read_file(&directory.join(&name)) {
            Ok(Some(lines)) => {
                for line in lines {
                    match parse_event(&line) {
                        Ok(event) => events.push(event),
                        Err(()) => result.skipped_malformed += 1,
                    }
                }
            }
            Ok(None) => {}
            Err(()) => result.skipped_files += 1,
        }
    }
    drop(lock);
    if let Some(run) = &query.run {
        events.retain(|event| &event.run_id == run);
    }
    events.sort_by(|left, right| left.ts_ms.cmp(&right.ts_ms).then(left.seq.cmp(&right.seq)));
    let first = events.len().saturating_sub(query.limit);
    result.entries = events.split_off(first);
    result
}

/// The lines of one owned file, or `None` when the file does not exist.
fn read_file(path: &Path) -> Result<Option<Vec<String>>, ()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(());
    }
    if metadata.len() > MAX_FILE_BYTES {
        return Err(());
    }
    let content = fs::read(path).map_err(|_| ())?;
    let text = String::from_utf8(content).map_err(|_| ())?;
    Ok(Some(
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_owned)
            .collect(),
    ))
}

/// Parse one typed event, rejecting anything the schema does not describe.
fn parse_event(line: &str) -> Result<DiagEvent, ()> {
    let event: DiagEvent = serde_json::from_str(line).map_err(|_| ())?;
    if !event.is_well_formed() {
        return Err(());
    }
    Ok(event)
}
