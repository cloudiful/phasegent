//! Shared helpers for the private SQLite files phasegent owns.
//!
//! The config database, the issue index, and the local provider database
//! are opened the same way: create the parent directory owner-only, open
//! the file, then force the file itself to mode 0600 on Unix so a
//! pre-existing database with broader permissions is tightened too. The
//! `label` argument keeps each caller's error wording intact.

use directories::ProjectDirs;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};

/// Create `path` for a database file, applying mode 0700 on Unix.
///
/// `only_when_created` preserves each caller's existing behaviour: the
/// phasegent config directory is re-tightened on every open, while the
/// provider/index directories are only tightened by the call that created
/// them.
pub(crate) fn create_private_dir(
    path: &Path,
    label: &str,
    only_when_created: bool,
) -> Result<(), String> {
    let missing = !path.exists();
    fs::create_dir_all(path)
        .map_err(|error| format!("could not create {label} directory: {error}"))?;
    if missing || !only_when_created {
        tighten_directory(path, label)?;
    }
    Ok(())
}

#[cfg(unix)]
fn tighten_directory(path: &Path, label: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("could not secure {label} directory: {error}"))
}

/// Non-Unix platforms keep the default directory mode; only the SQLite
/// file-mode pragma handles visibility there.
#[cfg(not(unix))]
fn tighten_directory(_path: &Path, _label: &str) -> Result<(), String> {
    Ok(())
}

/// Open `path` and force the file to mode 0600 on Unix.
pub(crate) fn open_private_connection(path: &Path, label: &str) -> Result<Connection, String> {
    let connection = Connection::open(path)
        .map_err(|error| format!("could not open {label} database: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(path)
            .map_err(|error| format!("could not stat {label} database: {error}"))?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(path, permissions)
            .map_err(|error| format!("could not secure {label} database: {error}"))?;
    }
    Ok(connection)
}

/// Resolve `<platform config dir>/<filename>`; the single place the
/// `com`/`Cloud1ful`/`phasegent` project identity is mapped to a path.
/// The qualifier / organisation / application tuple maps to the
/// platform-standard config directory:
/// - Linux: `$XDG_CONFIG_HOME/phasegent` (defaults to `~/.config/phasegent`)
/// - macOS: `~/Library/Application Support/com.Cloud1ful.phasegent`
/// - Windows: `%APPDATA%\Cloud1ful\phasegent\config`
pub(crate) fn project_dirs_config_path(filename: &str) -> Result<PathBuf, String> {
    let dirs = ProjectDirs::from("com", "Cloud1ful", "phasegent")
        .ok_or_else(|| "could not resolve phasegent config directory".to_owned())?;
    Ok(dirs.config_dir().join(filename))
}
