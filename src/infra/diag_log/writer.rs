//! The appending diagnostic writer: bounded rotation, lazy retention, and one
//! stable advisory lock.
//!
//! The writer owns exactly five log files and one lock file:
//!
//! * `prune.jsonl` — the active NDJSON file;
//! * `prune.jsonl.1` … `prune.jsonl.4` — the archives, `.1` the newest;
//! * `prune.log.lock` — the stable advisory lock, never renamed and never
//!   unlinked.
//!
//! Nothing else in the directory is ever written, rotated, or deleted. A file
//! that is not one of those exact names is invisible to this module, and a
//! symbolic link or a non-regular file is never followed, rotated, or removed.
//!
//! Logging is best effort. A busy lock, an unwritable directory, or a failed
//! append drops the event and reports one bounded warning for the run; it
//! never fails the work the caller was doing, and it never writes to the
//! caller's standard output.

use std::cell::Cell;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;

use super::event::{
    COMMAND_WORKTREE_PRUNE, DiagEvent, EventKind, RunCounts, RunTimings, SCHEMA_VERSION,
    is_known_operation, is_known_outcome, is_known_reason, is_known_stage, is_safe_token,
};

/// The active log file.
pub(crate) const ACTIVE_LOG: &str = "prune.jsonl";
/// The number of archives kept beside the active file.
pub(crate) const ARCHIVE_COUNT: usize = 4;
/// The size cap of every log file, including the event that fills it.
pub(crate) const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// The size cap of all log files together.
pub(crate) const MAX_TOTAL_BYTES: u64 = MAX_FILE_BYTES * (ARCHIVE_COUNT as u64 + 1);
/// The age after which an owned log file is deleted on writer startup.
pub(crate) const RETENTION_DAYS: u64 = 14;
/// The stable advisory lock beside the log files.
pub(crate) const LOCK_FILE: &str = "prune.log.lock";

/// The names of the owned log files, the active one first.
pub(super) fn owned_log_names() -> Vec<String> {
    let mut names = vec![ACTIVE_LOG.to_owned()];
    for index in 1..=ARCHIVE_COUNT {
        names.push(archive_name(index));
    }
    names
}

/// The name of one archive.
pub(super) fn archive_name(index: usize) -> String {
    format!("{ACTIVE_LOG}.{index}")
}

/// The resolved log directory. Logs are operational state: the platform state
/// directory is preferred and the local application data directory is the
/// fallback for the platforms that have no state directory.
pub(crate) fn log_dir() -> Result<PathBuf, String> {
    let dirs = ProjectDirs::from("com", "Cloud1ful", "phasegent")
        .ok_or_else(|| "could not resolve the phasegent log directory".to_owned())?;
    let base = dirs.state_dir().unwrap_or_else(|| dirs.data_local_dir());
    Ok(base.join("logs"))
}

/// Why one event was dropped.
#[derive(Debug)]
pub(super) enum AppendError {
    /// Another writer holds the lock. The event is dropped.
    Busy,
    /// The log could not be rotated. Further writes of this logger instance
    /// are disabled, so a half-rotated state is never touched again.
    Rotation,
    /// The log could not be written.
    Io,
    /// The required retention sweep failed. The logger stops writing for the
    /// rest of the run: an event is never appended while the required
    /// maintenance is incomplete.
    Maintenance,
}

/// The retention sweep of one log directory. The production sweep removes the
/// owned files that are older than the retention age; the unit tests inject a
/// failing sweep to cover the maintenance-failure path.
pub(super) type SweepFn = fn(&Path) -> Result<(), AppendError>;

/// The run context every event of one run carries.
pub(crate) struct RunState {
    run_id: String,
    mode: &'static str,
    seq: Cell<u64>,
    started: Instant,
    repo: Cell<Option<u64>>,
    candidate: Cell<Option<u64>>,
}

impl RunState {
    fn new(mode: &'static str) -> Self {
        Self {
            run_id: new_run_id(),
            mode,
            seq: Cell::new(0),
            started: Instant::now(),
            repo: Cell::new(None),
            candidate: Cell::new(None),
        }
    }
}

/// The appending diagnostic logger of one run.
pub(crate) struct DiagLogger {
    dir: PathBuf,
    warn: fn(&str),
    warned: Cell<bool>,
    disabled: Cell<bool>,
    /// The retention sweep is deferred while the lock is busy at startup. The
    /// sweep runs before the first append that owns the lock, so a transient
    /// contention costs the delay, never the rest of the run.
    pending_retention: Cell<bool>,
    /// The retention sweep this logger performs. The production sweep removes
    /// the owned files that are older than the retention age.
    sweep: SweepFn,
    run: RunState,
}

impl DiagLogger {
    /// Open the logger for one run.
    ///
    /// The directory is created owner-only when it is missing, the retained
    /// files are pruned under the lock, and the logger is ready to append. A
    /// directory that cannot be prepared yields no logger and one bounded
    /// warning; the caller keeps working.
    pub(crate) fn open(mode: &'static str, warn: fn(&str)) -> Option<Self> {
        let dir = match log_dir() {
            Ok(dir) => dir,
            Err(_) => {
                warn("diagnostic logging is unavailable");
                return None;
            }
        };
        Self::open_in(&dir, mode, warn)
    }

    /// Open the logger in one known directory. The production path resolves
    /// the platform directory and delegates here; the unit tests use this
    /// seam so no test ever writes into the real log directory.
    pub(crate) fn open_in(dir: &Path, mode: &'static str, warn: fn(&str)) -> Option<Self> {
        Self::open_in_with(dir, mode, warn, apply_retention)
    }

    /// Open the logger with an explicit retention sweep. The unit tests inject
    /// a failing sweep here to cover the maintenance-failure path; the
    /// production path uses the real sweep.
    pub(super) fn open_in_with(
        dir: &Path,
        mode: &'static str,
        warn: fn(&str),
        sweep: SweepFn,
    ) -> Option<Self> {
        if let Err(reason) = prepare_directory(dir) {
            warn(reason);
            return None;
        }
        let logger = Self {
            dir: dir.to_path_buf(),
            warn,
            warned: Cell::new(false),
            disabled: Cell::new(false),
            pending_retention: Cell::new(false),
            sweep,
            run: RunState::new(mode),
        };
        // The retained files are pruned under the same lock the appends use,
        // so a reader never sees a half-pruned set. A busy lock at startup
        // only defers the sweep: the logger stays usable and performs the
        // sweep before the first append that owns the lock. A sweep that
        // actually fails is a different matter: the required maintenance is
        // incomplete, so the logger stops writing for the rest of the run.
        match logger.with_lock(|dir| (logger.sweep)(dir)) {
            Ok(()) => {}
            Err(AppendError::Busy) => {
                logger.pending_retention.set(true);
                logger.warn_once(
                    "the diagnostic retention sweep was deferred: another writer holds the log lock",
                );
            }
            Err(AppendError::Rotation) => {
                logger.fail_maintenance();
            }
            Err(AppendError::Io | AppendError::Maintenance) => {
                logger.fail_maintenance();
            }
        }
        Some(logger)
    }

    /// The identifier of this run.
    #[cfg(test)]
    pub(crate) fn run_id(&self) -> &str {
        &self.run.run_id
    }

    /// The directory this logger writes to.
    #[cfg(test)]
    pub(crate) fn directory(&self) -> &Path {
        &self.dir
    }

    /// The mode of this run.
    #[cfg(test)]
    pub(crate) fn mode(&self) -> &'static str {
        self.run.mode
    }

    /// Set the ordinal of the repository the run is working on.
    pub(crate) fn set_repo(&self, ordinal: u64) {
        self.run.repo.set(Some(ordinal));
        self.run.candidate.set(None);
    }

    /// Set the ordinal of the candidate the run is working on.
    pub(crate) fn set_candidate(&self, ordinal: u64) {
        self.run.candidate.set(Some(ordinal));
    }

    /// Clear the candidate ordinal.
    pub(crate) fn clear_candidate(&self) {
        self.run.candidate.set(None);
    }

    /// Start one event of this run.
    pub(crate) fn event(&self, kind: EventKind) -> DiagEventRef<'_> {
        let seq = self.run.seq.get();
        self.run.seq.set(seq + 1);
        DiagEventRef {
            logger: self,
            event: DiagEvent {
                schema_version: SCHEMA_VERSION,
                run_id: self.run.run_id.clone(),
                seq,
                ts_ms: unix_millis(),
                elapsed_ms: self.run.started.elapsed().as_secs_f64() * 1000.0,
                command: COMMAND_WORKTREE_PRUNE.to_owned(),
                mode: self.run.mode.to_owned(),
                repo: self.run.repo.get(),
                candidate: self.run.candidate.get(),
                kind,
                stage: None,
                operation: None,
                outcome: None,
                reason: None,
                duration_ms: None,
                calls: None,
                counts: None,
                timings: None,
            },
        }
    }

    /// Report one bounded warning for this run. Only the first warning of a
    /// run is reported.
    fn warn_once(&self, reason: &str) {
        if self.warned.get() {
            return;
        }
        self.warned.set(true);
        (self.warn)(reason);
    }

    /// The shared handling of a failed maintenance sweep. The required
    /// retention did not complete, so this logger stops writing for the rest
    /// of the run: no event and no later event is appended while the
    /// maintenance is incomplete. The next invocation starts fresh and may
    /// retry the sweep normally.
    fn fail_maintenance(&self) {
        self.disabled.set(true);
        self.warn_once("diagnostic logging stopped: the log retention sweep failed");
    }

    /// Run the deferred retention sweep under the owned lock. The sweep must
    /// succeed before the event is appended: a failed sweep stops the logger
    /// and drops the event.
    fn run_pending_retention(&self, dir: &Path) -> Result<(), AppendError> {
        if !self.pending_retention.get() {
            return Ok(());
        }
        match (self.sweep)(dir) {
            Ok(()) => {
                self.pending_retention.set(false);
                Ok(())
            }
            Err(_) => {
                self.fail_maintenance();
                Err(AppendError::Maintenance)
            }
        }
    }

    /// Append one event. A failure drops the event only.
    fn append(&self, event: DiagEvent) -> bool {
        if self.disabled.get() {
            return false;
        }
        let mut line = match serde_json::to_string(&event) {
            Ok(line) => line.into_bytes(),
            Err(_) => return false,
        };
        line.push(b'\n');
        if line.len() as u64 > MAX_FILE_BYTES {
            // A single event that cannot fit is dropped rather than written
            // past the file cap.
            self.warn_once("a diagnostic event was dropped: it exceeds the log file cap");
            return false;
        }
        match self.with_lock(|dir| {
            // A sweep that was deferred at startup runs here, under the same
            // lock, before the event is appended. The event is never written
            // before the required retention has succeeded.
            self.run_pending_retention(dir)?;
            append_line(dir, &line)
        }) {
            Ok(()) => true,
            Err(AppendError::Busy) => {
                self.warn_once("a diagnostic event was dropped: another writer holds the log lock");
                false
            }
            Err(AppendError::Rotation) => {
                self.disabled.set(true);
                self.warn_once("diagnostic logging stopped: the log could not be rotated");
                false
            }
            Err(AppendError::Maintenance) => {
                // The maintenance failure already reported the bounded warning
                // and disabled this logger.
                false
            }
            Err(AppendError::Io) => {
                self.warn_once("a diagnostic event was dropped: the log could not be written");
                false
            }
        }
    }

    /// Run `body` under the exclusive advisory lock of this log directory.
    fn with_lock<T>(
        &self,
        body: impl FnOnce(&Path) -> Result<T, AppendError>,
    ) -> Result<T, AppendError> {
        let path = self.dir.join(LOCK_FILE);
        // A symbolic link is never followed: the lock is a real file in the
        // log directory, and a link cannot redirect a writer elsewhere.
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(AppendError::Io);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(AppendError::Io),
        }
        let file = match open_private(&path, false) {
            Ok(file) => file,
            Err(_) => return Err(AppendError::Io),
        };
        tighten_path(&path);
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(AppendError::Busy),
            Err(std::fs::TryLockError::Error(_)) => return Err(AppendError::Io),
        }
        let outcome = body(&self.dir);
        drop(file);
        outcome
    }
}

/// Open one owned file with owner-only permissions. A file that already
/// exists keeps its content; a file that is created here is never briefly
/// readable by another user.
fn open_private(path: &Path, append: bool) -> std::io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    if append {
        options.append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// One event under construction. Every categorical field is checked against
/// its fixed vocabulary before it is written, so an event can never carry a
/// value the schema does not name.
pub(crate) struct DiagEventRef<'a> {
    logger: &'a DiagLogger,
    event: DiagEvent,
}

impl DiagEventRef<'_> {
    /// Name the pipeline stage of this event.
    pub(crate) fn stage(mut self, stage: &str) -> Self {
        if is_known_stage(stage) {
            self.event.stage = Some(stage.to_owned());
        }
        self
    }

    /// Name the boundary operation of this event.
    pub(crate) fn operation(mut self, operation: &str) -> Self {
        if is_known_operation(operation) {
            self.event.operation = Some(operation.to_owned());
        }
        self
    }

    /// Name the outcome of this event.
    pub(crate) fn outcome(mut self, outcome: &str) -> Self {
        if is_known_outcome(outcome) {
            self.event.outcome = Some(outcome.to_owned());
        }
        self
    }

    /// Name the safe reason of this event.
    pub(crate) fn reason(mut self, reason: &str) -> Self {
        if is_known_reason(reason) {
            self.event.reason = Some(reason.to_owned());
        }
        self
    }

    /// Carry the measured duration of this event.
    pub(crate) fn duration_ms(mut self, duration_ms: f64) -> Self {
        self.event.duration_ms = Some(duration_ms);
        self
    }

    /// Carry the measured call count of this event.
    pub(crate) fn calls(mut self, calls: u64) -> Self {
        self.event.calls = Some(calls);
        self
    }

    /// Carry the aggregate counts of the run.
    pub(crate) fn counts(mut self, counts: RunCounts) -> Self {
        self.event.counts = Some(counts);
        self
    }

    /// Carry the measured timings of the run.
    pub(crate) fn timings(mut self, timings: RunTimings) -> Self {
        self.event.timings = Some(timings);
        self
    }

    /// Write this event.
    pub(crate) fn write(self) -> bool {
        self.logger.append(self.event)
    }
}

/// Prepare the log directory: it must be a real directory, never a symbolic
/// link, and it is created owner-only when it is missing.
pub(super) fn prepare_directory(dir: &Path) -> Result<(), &'static str> {
    match fs::symlink_metadata(dir) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err("diagnostic logging is unavailable: the log path is not a directory");
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {
            return Err("diagnostic logging is unavailable: the log path could not be inspected");
        }
    }
    crate::infra::sqlite_file::create_private_dir(dir, "diagnostic log", false)
        .map_err(|_| "diagnostic logging is unavailable: the log directory could not be created")
}

/// Append one complete line to the active log, rotating first when the file
/// cap or the daily rollover asks for it.
pub(super) fn append_line(dir: &Path, line: &[u8]) -> Result<(), AppendError> {
    // A line that cannot fit into an empty file is refused: the size cap
    // covers the event itself, so no file ever exceeds it.
    if line.len() as u64 > MAX_FILE_BYTES {
        return Err(AppendError::Io);
    }
    let active = dir.join(ACTIVE_LOG);
    // A symbolic link or a non-regular active file is never followed or
    // written: the writer refuses the append instead of touching a file
    // outside the log set.
    match fs::symlink_metadata(&active) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(AppendError::Io);
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(AppendError::Io),
    }
    if let Some((size, modified)) = owned_file_stat(&active)
        && (size + line.len() as u64 > MAX_FILE_BYTES || earlier_utc_day(modified))
    {
        rotate(dir).map_err(|_| AppendError::Rotation)?;
    }
    let mut file = open_private(&active, true).map_err(|_| AppendError::Io)?;
    tighten_path(&active);
    file.write_all(line).map_err(|_| AppendError::Io)
}

/// The size and modification time of one owned file, or `None` when the file
/// is missing, a symbolic link, or not a regular file.
pub(super) fn owned_file_stat(path: &Path) -> Option<(u64, SystemTime)> {
    let metadata = fs::symlink_metadata(path).ok()?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return None;
    }
    Some((metadata.len(), metadata.modified().ok()?))
}

/// Rotate the owned files: the oldest archive is dropped, the others shift one
/// step back, and the active file becomes the newest archive.
///
/// Every source and every destination is validated before the first mutation.
/// An existing owned name that is not a regular file — a symbolic link, a
/// directory, or any other non-regular target — makes the rotation fail
/// closed: nothing is unlinked, overwritten, or renamed, so a link is never
/// replaced and no partial sequence precedes a discovered unsafe target.
pub(super) fn rotate(dir: &Path) -> Result<(), ()> {
    if !owned_targets_are_regular(dir) {
        return Err(());
    }
    let oldest = dir.join(archive_name(ARCHIVE_COUNT));
    if owned_file_stat(&oldest).is_some() {
        fs::remove_file(&oldest).map_err(|_| ())?;
    }
    for index in (1..ARCHIVE_COUNT).rev() {
        let from = dir.join(archive_name(index));
        let to = dir.join(archive_name(index + 1));
        if owned_file_stat(&from).is_some() {
            fs::rename(&from, &to).map_err(|_| ())?;
        }
    }
    let active = dir.join(ACTIVE_LOG);
    if owned_file_stat(&active).is_some() {
        fs::rename(&active, dir.join(archive_name(1))).map_err(|_| ())?;
    }
    Ok(())
}

/// Whether every existing owned name is a regular file. A missing name is
/// regular enough: it is simply not there.
fn owned_targets_are_regular(dir: &Path) -> bool {
    for name in owned_log_names() {
        match fs::symlink_metadata(dir.join(&name)) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return false;
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return false,
        }
    }
    true
}

/// Delete the owned log files that are older than the retention age. A file
/// that is a symbolic link or not a regular file is never deleted, and no
/// other name in the directory is ever considered.
pub(super) fn apply_retention(dir: &Path) -> Result<(), AppendError> {
    let cutoff = Duration::from_secs(RETENTION_DAYS * 24 * 60 * 60);
    for name in owned_log_names() {
        let path = dir.join(&name);
        let Some((_, modified)) = owned_file_stat(&path) else {
            continue;
        };
        // A future timestamp is not an ancient one: the elapsed duration
        // saturates at zero instead of wrapping.
        let ancient = modified.elapsed().map(|age| age >= cutoff).unwrap_or(false);
        if ancient {
            fs::remove_file(&path).map_err(|_| AppendError::Io)?;
        }
    }
    Ok(())
}

/// Whether the modification time lies on an earlier UTC day than now.
pub(super) fn earlier_utc_day(modified: SystemTime) -> bool {
    let modified = match modified.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs() as i64,
        // A timestamp before the epoch is on an earlier day than any now.
        Err(error) => -(error.duration().as_secs() as i64),
    };
    utc_day(modified) < utc_day(unix_seconds())
}

/// The UTC day index of a Unix timestamp.
pub(super) fn utc_day(seconds: i64) -> i64 {
    seconds.div_euclid(24 * 60 * 60)
}

/// The current Unix time in seconds.
fn unix_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// The current Unix time in milliseconds.
fn unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// A run identifier that is unique per process and per run, and carries no
/// session identity.
pub(super) fn new_run_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos())
        .unwrap_or(0);
    let candidate = format!("{pid}-{nanos:x}-{counter:x}");
    if is_safe_token(&candidate) {
        candidate
    } else {
        "run-0".to_owned()
    }
}

/// Force the file mode to 0600 on Unix.
#[cfg(unix)]
fn tighten_path(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        let _ = fs::set_permissions(path, permissions);
    }
}

/// The file mode is owned by the platform default off Unix.
#[cfg(not(unix))]
fn tighten_path(_path: &Path) {}
