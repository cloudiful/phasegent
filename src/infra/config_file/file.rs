use super::jsonc::ConfigDocument;
use directories::ProjectDirs;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const CONFIG_FILENAME: &str = "phasegent.jsonc";
pub const CONFIG_PATH_ENV: &str = "PHASEGENT_CONFIG_PATH";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn default_config_path() -> Result<PathBuf, String> {
    let dirs = ProjectDirs::from("com", "Cloud1ful", "phasegent")
        .ok_or_else(|| "could not resolve phasegent config directory".to_owned())?;
    Ok(dirs.config_dir().join(CONFIG_FILENAME))
}

pub fn resolve_config_path() -> Result<PathBuf, String> {
    match std::env::var(CONFIG_PATH_ENV) {
        Ok(raw) => {
            let trimmed = raw.trim().to_owned();
            if trimmed.is_empty() {
                default_config_path()
            } else {
                let path = PathBuf::from(&trimmed);
                if !path.is_absolute() {
                    return Err(format!(
                        "{CONFIG_PATH_ENV} must be an absolute path for test isolation"
                    ));
                }
                Ok(path)
            }
        }
        Err(std::env::VarError::NotPresent) => default_config_path(),
        Err(error) => Err(format!("could not read {CONFIG_PATH_ENV}: {error}")),
    }
}

/// Private configuration file store. Reads never create the file; updates take
/// the sibling advisory lock, re-read the current bytes, and refuse to rename
/// over an external edit detected after the read.
///
/// Unix enforcement: the directory must not grant group/other access and the
/// file must be a regular non-symlink file with no group/other bits. New
/// directories use 0700 and new files use 0600. Windows keeps the user-local
/// `ProjectDirs` location but does not model Unix modes; its protection is the
/// inherited user-profile ACL boundary, not an explicit file ACL check.
///
/// Every replacement renames the prepared temporary file over the live file,
/// so the live configuration is never removed before its replacement exists.
pub struct ConfigFileStore {
    path: PathBuf,
}

/// An exclusive advisory claim on the configuration sidecar lock.
///
/// The lock is held for the lifetime of the value, so a caller that needs
/// the configuration file to stay unchanged across several steps (the
/// migration importer exports, verifies, backs up, and purges) can take the
/// lock once and keep every normal configuration writer out for the whole
/// sequence.
pub struct ConfigLock {
    _file: File,
}

impl ConfigFileStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn resolve() -> Result<Self, String> {
        Ok(Self::new(resolve_config_path()?))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the raw bytes of the configuration file without creating or
    /// locking it. Returns `None` when the file does not exist.
    ///
    /// The migration importer uses the bytes to notice an edit that a
    /// direct editor made while it ran: the advisory lock only keeps
    /// cooperating phasegent writers out.
    pub fn read_bytes(&self) -> Result<Option<Vec<u8>>, String> {
        read_optional(&self.path)
    }

    /// Read the document without creating or locking the file.
    pub fn load(&self) -> Result<ConfigDocument, String> {
        match read_optional(&self.path)? {
            Some(bytes) => parse_bytes(&bytes),
            None => Ok(ConfigDocument::new()),
        }
    }

    /// Claim the sibling advisory lock. The lock stays held until the
    /// returned value is dropped, so a caller can keep every normal
    /// configuration writer out across a multi-step sequence.
    pub fn lock(&self) -> Result<ConfigLock, String> {
        acquire_lock(&self.path)
    }

    /// Create the configuration file while the caller already holds the
    /// lock. The lock is not taken again, so a caller that must keep the
    /// configuration stable across several steps does not deadlock against
    /// its own claim. The file must not exist yet: an existing file is
    /// never overwritten here, so the migration importer can refuse to
    /// replace a destination that holds different values.
    pub fn create_locked(
        &self,
        _lock: &ConfigLock,
        document: &ConfigDocument,
    ) -> Result<(), String> {
        match fs::symlink_metadata(&self.path) {
            Ok(_) => {
                return Err(format!(
                    "configuration file at {} already exists; refusing to overwrite it",
                    self.path.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect configuration file at {}: {error}",
                    self.path.display()
                ));
            }
        }
        write_atomic(&self.path, None, document.as_str())
    }

    /// Locked read-modify-write. The closure mutates the CST-backed document;
    /// the result is written atomically and the lock is held until the rename
    /// and parent sync finish.
    pub fn update(
        &self,
        update: impl FnOnce(&mut ConfigDocument) -> Result<(), String>,
    ) -> Result<ConfigDocument, String> {
        let _lock = acquire_lock(&self.path)?;
        let original = read_optional(&self.path)?;
        let mut document = match original.as_deref() {
            Some(bytes) => parse_bytes(bytes)?,
            None => ConfigDocument::new(),
        };
        update(&mut document)?;
        write_atomic(&self.path, original.as_deref(), document.as_str())?;
        Ok(document)
    }
}

fn parse_bytes(bytes: &[u8]) -> Result<ConfigDocument, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "configuration file is not valid UTF-8".to_owned())?;
    ConfigDocument::parse(text)
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure_private_regular_file(path, &metadata)?;
            fs::read(path)
                .map(Some)
                .map_err(|error| format!("could not read configuration file: {error}"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "could not inspect configuration file at {}: {error}",
            path.display()
        )),
    }
}

fn ensure_private_regular_file(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "configuration file at {} must not be a symlink",
            path.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!(
            "configuration path at {} must be a regular file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(format!(
                "configuration file at {} must not be accessible by group or others",
                path.display()
            ));
        }
    }
    Ok(())
}

fn prepare_parent_dir(parent: &Path) -> Result<(), String> {
    match fs::symlink_metadata(parent) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "configuration directory at {} must not be a symlink",
                    parent.display()
                ));
            }
            if !metadata.is_dir() {
                return Err(format!(
                    "configuration directory at {} must be a directory",
                    parent.display()
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(format!(
                        "configuration directory at {} must not be accessible by group or others",
                        parent.display()
                    ));
                }
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "could not create configuration directory at {}: {error}",
                    parent.display()
                )
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(
                    |error| {
                        format!(
                            "could not set permissions on configuration directory at {}: {error}",
                            parent.display()
                        )
                    },
                )?;
            }
            Ok(())
        }
        Err(error) => Err(format!(
            "could not inspect configuration directory at {}: {error}",
            parent.display()
        )),
    }
}

fn acquire_lock(path: &Path) -> Result<ConfigLock, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "configuration path has no parent directory".to_owned())?;
    prepare_parent_dir(parent)?;
    let lock_path = lock_path(path);
    match fs::symlink_metadata(&lock_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(format!(
                    "configuration lock at {} must be a regular non-symlink file",
                    lock_path.display()
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "could not inspect configuration lock at {}: {error}",
                lock_path.display()
            ));
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&lock_path).map_err(|error| {
        format!(
            "could not open configuration lock at {}: {error}",
            lock_path.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            format!(
                "could not secure configuration lock at {}: {error}",
                lock_path.display()
            )
        })?;
    }
    match file.try_lock() {
        Ok(()) => Ok(ConfigLock { _file: file }),
        Err(std::fs::TryLockError::WouldBlock) => Err(
            "another phasegent configuration write is in progress; retry after it finishes"
                .to_owned(),
        ),
        Err(std::fs::TryLockError::Error(error)) => Err(format!(
            "could not lock the configuration file at {}: {error}",
            path.display()
        )),
    }
}

/// Write the new content to a sibling temporary file and replace the
/// destination with it.
///
/// The temporary file is created exclusively, so a leftover file with the
/// same name is never overwritten and never removed by this call: only a
/// temporary file this invocation actually created is unlinked again when
/// the write fails.
fn write_atomic(path: &Path, expected: Option<&[u8]>, content: &str) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "configuration path has no parent directory".to_owned())?;
    prepare_parent_dir(parent)?;
    let mut last_error = None;
    for attempt in 0..8 {
        let temp_path = sibling_name(path, "tmp", attempt);
        match write_temp_and_rename(path, &temp_path, expected, content) {
            Ok(()) => return Ok(()),
            Err(error) => {
                if is_temp_collision(&error) {
                    last_error = Some(error);
                    continue;
                }
                return Err(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| "could not create a temporary configuration file".to_owned()))
}

/// Whether the failure was caused by an existing temporary file rather
/// than by the write itself.
fn is_temp_collision(error: &str) -> bool {
    error.starts_with("could not create temporary configuration file")
}

fn write_temp_and_rename(
    path: &Path,
    temp_path: &Path,
    expected: Option<&[u8]>,
    content: &str,
) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let temp = options.open(temp_path).map_err(|error| {
        format!(
            "could not create temporary configuration file at {}: {error}",
            temp_path.display()
        )
    })?;
    // The temporary file belongs to this call from here on: it is removed
    // again when the write fails. A file this call did not create (a
    // leftover from an earlier run) is never touched.
    let result = write_temp_body(path, temp_path, expected, content, temp);
    if result.is_err() {
        let _ = fs::remove_file(temp_path);
    }
    result
}

fn write_temp_body(
    path: &Path,
    temp_path: &Path,
    expected: Option<&[u8]>,
    content: &str,
    mut temp: File,
) -> Result<(), String> {
    temp.write_all(content.as_bytes())
        .map_err(|error| format!("could not write temporary configuration file: {error}"))?;
    temp.sync_all()
        .map_err(|error| format!("could not sync temporary configuration file: {error}"))?;
    drop(temp);

    match fs::read(path) {
        Ok(current) => {
            if expected != Some(current.as_slice()) {
                return Err(
                    "configuration file changed concurrently; refusing to overwrite it".to_owned(),
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if expected.is_some() {
                return Err(
                    "configuration file was removed concurrently; refusing to recreate it"
                        .to_owned(),
                );
            }
        }
        Err(error) => {
            return Err(format!(
                "could not re-check configuration file before writing: {error}"
            ));
        }
    }

    rename_over(path, temp_path)?;

    #[cfg(unix)]
    if let Ok(directory) = File::open(
        path.parent()
            .ok_or_else(|| "configuration path has no parent directory".to_owned())?,
    ) {
        let _ = directory.sync_all();
    }
    Ok(())
}

/// Replace the destination with the temporary file.
///
/// `std::fs::rename` replaces an existing destination on every supported
/// platform, so the live configuration file is never removed before the
/// replacement exists. On Windows the replacement is performed by the
/// `MoveFileEx` replace-existing path of the standard library; the file is
/// therefore never missing between the write and the rename.
fn rename_over(path: &Path, temp_path: &Path) -> Result<(), String> {
    fs::rename(temp_path, path).map_err(|error| {
        format!(
            "could not replace configuration file at {}: {error}",
            path.display()
        )
    })
}

fn sibling_name(path: &Path, kind: &str, attempt: u64) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "phasegent.jsonc".to_owned());
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(
        "{file_name}.{kind}.{}.{counter}.{attempt}",
        std::process::id()
    ))
}

fn lock_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "phasegent.jsonc".to_owned());
    path.with_file_name(format!("{file_name}.lock"))
}
