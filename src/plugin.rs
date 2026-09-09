//! Managed OpenCode plugin installation (issue #239 Phase 3).
//!
//! `phasegent plugin install` writes the worktree adapter into the
//! OpenCode plugin directory: `~/.config/opencode/plugins/` for the
//! global scope, or `.opencode/plugins/` (relative to the current
//! working directory) for the project scope. Files are tagged with
//! the `// phasegent:managed` header marker and never clobber
//! foreign files unless the operator passes `--force`, in which
//! case the foreign file is moved to
//! `phasegent-worktree.js.phasegent-orig`.
//!
//! The installed adapter is `experimental_workspace.register(...)`
//! with the canonical `configure` / `create` / `remove` / `target`
//! shape. `target` runs `phasegent --role orchestrator worktree
//! acquire --issue N --format json` and falls back to the original
//! directory on any failure. No network, no `.env` access, no
//! branch deletion (deletion stays with `phasegent worktree prune`).
//!
//! This module reuses the `hooks.rs` install pattern (marker check,
//! idempotent update, foreign-file backup, atomic temp+rename) so the
//! Phase 3 surface reads the same as Phase 1 / 2 for an operator
//! already familiar with `phasegent hooks install`.

use std::path::{Path, PathBuf};

/// Marker comment embedded at the top of every managed plugin file so
/// later installs can recognise and update their own scripts without
/// touching foreign ones. Mirrors `hooks::MANAGED_MARKER` so the
/// managed-file detection logic stays uniform.
pub const MANAGED_MARKER: &str = "// phasegent:managed";

/// OpenCode plugins directory under `$XDG_CONFIG_HOME` (preferred) or
/// `$HOME/.config/`. Matches the OpenCode convention.
pub const OPENCODE_PLUGIN_DIR: &str = "opencode/plugins";

/// File name of the managed worktree adapter.
pub const PLUGIN_FILENAME: &str = "phasegent-worktree.js";

/// Backup suffix appended to a foreign file that is being displaced
/// under `--force`. Matches the `*.phasegent-orig` shape the parent
/// task requested.
pub const FOREIGN_BACKUP_SUFFIX: &str = ".phasegent-orig";

/// Embedded adapter source. The file lives in `assets/opencode/` so it
/// is also available on the repo source tree (tests assert against the
/// file directly), and `include_str!` makes it a real compile input
/// so an adapter change busts the Cargo fingerprint.
const ADAPTER_JS: &str = include_str!("../assets/opencode/phasegent-worktree.js");

/// Outcome of a single-scope install. The CLI executor merges the
/// global + project outcomes into one JSON envelope so the operator
/// sees the full picture even when both scopes install at once.
#[derive(Debug, Default, Clone)]
pub struct InstallOutcome {
    /// New files written by this run.
    pub installed: Vec<String>,
    /// Pre-existing managed files whose bytes were rewritten because
    /// the embedded template changed.
    pub updated: Vec<String>,
    /// Pre-existing managed files whose bytes already matched the
    /// embedded template (no rewrite was necessary).
    pub skipped: Vec<String>,
    /// Human-readable notes (e.g. foreign-file backup path).
    pub warnings: Vec<String>,
    /// Structured failures (e.g. filesystem errors during install).
    /// Errors that did not block the other scope still surface here
    /// so the operator can see them in the JSON envelope.
    pub errors: Vec<String>,
}

/// One slot of the `plugin status` JSON envelope.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct PluginTargetStatus {
    /// Resolved absolute path for this scope.
    pub path: String,
    /// True when the file exists on disk.
    pub exists: bool,
    /// True when the file exists AND its bytes contain [`MANAGED_MARKER`].
    pub managed: bool,
    /// File size in bytes; `0` when the file does not exist.
    pub size: u64,
    /// Unix-epoch mtime seconds; `None` when the file does not exist.
    pub mtime: Option<i64>,
}

/// Read-only status report covering both scopes.
#[derive(Debug, Default, Clone)]
pub struct StatusReport {
    pub global: PluginTargetStatus,
    pub project: PluginTargetStatus,
}

/// Outcome of a single-scope uninstall.
#[derive(Debug, Default, Clone)]
pub struct UninstallOutcome {
    /// Files that were removed because the marker matched.
    pub removed: Vec<String>,
    /// Notes (e.g. file was not managed and was refused).
    pub warnings: Vec<String>,
    /// Structured failures (e.g. filesystem errors).
    pub errors: Vec<String>,
}

/// Structured error type. Mirrors `hooks::BranchContextError` shape so
/// the CLI layer can reuse `structured_error` + `json()` plumbing.
#[derive(Debug)]
pub struct PluginError {
    pub kind: &'static str,
    pub message: String,
}

impl PluginError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({ "kind": self.kind, "message": self.message })
    }
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for PluginError {}

/// Which scope an install / status / uninstall call targets. Both
/// scopes install by default when no flag is supplied so the
/// operator never has to think about which slot OpenCode is reading.
/// Reserved for callers that want to drive the installer without
/// going through the CLI flag layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum InstallScope {
    Global,
    Project,
}

#[allow(dead_code)]
impl InstallScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

/// Resolve the global OpenCode plugins directory. Honours
/// `$XDG_CONFIG_HOME` first (per the XDG Base Directory spec) and
/// falls back to `$HOME/.config`. Returns a structured error when
/// neither variable points at a usable location.
pub fn resolve_global_dir() -> Result<PathBuf, PluginError> {
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        && !xdg.is_empty()
    {
        return Ok(PathBuf::from(xdg).join(OPENCODE_PLUGIN_DIR));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        PluginError::new(
            "filesystem",
            "HOME is not defined; cannot resolve global OpenCode config directory",
        )
    })?;
    if home.is_empty() {
        return Err(PluginError::new(
            "filesystem",
            "HOME is empty; cannot resolve global OpenCode config directory",
        ));
    }
    Ok(PathBuf::from(home)
        .join(".config")
        .join(OPENCODE_PLUGIN_DIR))
}

/// Resolve the project-scope plugins directory inside the given
/// checkout. Always returned as an absolute path so callers can
/// canonicalise later.
pub fn resolve_project_dir(cwd: &Path) -> PathBuf {
    cwd.join(".opencode").join("plugins")
}

/// Install into the given directory. Idempotent: a re-run against an
/// already-managed file either reports `skipped` (when the bytes
/// already match) or `updated` (when the embedded template changed).
/// Foreign files (no marker) are refused unless `force` is true; in
/// the forced case the foreign file is renamed to
/// `<name>.phasegent-orig` before the managed file is written.
pub fn install_at(dir: &Path, force: bool) -> Result<InstallOutcome, PluginError> {
    ensure_dir(dir)?;
    let target = dir.join(PLUGIN_FILENAME);
    match std::fs::symlink_metadata(&target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_managed(&target)?;
            Ok(InstallOutcome {
                installed: vec![target_label(&target)],
                ..InstallOutcome::default()
            })
        }
        Err(error) => Err(PluginError::new(
            "filesystem",
            format!("cannot inspect plugin path {}: {error}", target.display()),
        )),
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(PluginError::new(
                    "conflict",
                    format!(
                        "refusing to manage {}: it is a symlink; remove or repoint it manually, then re-run plugin install",
                        target.display()
                    ),
                ));
            }
            if !meta.is_file() {
                return Err(PluginError::new(
                    "conflict",
                    format!(
                        "refusing to manage {}: it is not a regular file; inspect it manually",
                        target.display()
                    ),
                ));
            }
            let current = std::fs::read(&target).map_err(|error| {
                PluginError::new(
                    "filesystem",
                    format!(
                        "cannot read existing plugin file {}: {error}",
                        target.display()
                    ),
                )
            })?;
            if contains_marker(&current) {
                if current.as_slice() == ADAPTER_JS.as_bytes() {
                    return Ok(InstallOutcome {
                        skipped: vec![target_label(&target)],
                        ..InstallOutcome::default()
                    });
                }
                write_managed(&target)?;
                return Ok(InstallOutcome {
                    updated: vec![target_label(&target)],
                    ..InstallOutcome::default()
                });
            }
            if !force {
                return Err(PluginError::new(
                    "conflict",
                    format!(
                        "refusing to replace {}: file exists without the {} marker; \
                         move it manually, or re-run with --force to rename the existing file to {}.{}",
                        target.display(),
                        MANAGED_MARKER,
                        PLUGIN_FILENAME,
                        FOREIGN_BACKUP_SUFFIX,
                    ),
                ));
            }
            let backup_path = dir.join(format!("{PLUGIN_FILENAME}{FOREIGN_BACKUP_SUFFIX}"));
            if backup_path.exists() {
                return Err(PluginError::new(
                    "conflict",
                    format!(
                        "refusing to displace {}: backup {} already exists; remove it manually, then re-run plugin install",
                        target.display(),
                        backup_path.display()
                    ),
                ));
            }
            std::fs::rename(&target, &backup_path).map_err(|error| {
                PluginError::new(
                    "filesystem",
                    format!(
                        "cannot move existing plugin {} to {}: {error}",
                        target.display(),
                        backup_path.display()
                    ),
                )
            })?;
            write_managed(&target)?;
            Ok(InstallOutcome {
                installed: vec![target_label(&target)],
                warnings: vec![format!(
                    "moved existing plugin to {}; the managed adapter now owns {}",
                    backup_path.display(),
                    target.display()
                )],
                ..InstallOutcome::default()
            })
        }
    }
}

/// Compute the status of the global + project slots without touching
/// the filesystem beyond the metadata reads. Used by `plugin status`.
pub fn status_at(cwd: &Path) -> StatusReport {
    let global_dir = resolve_global_dir().unwrap_or_else(|_| PathBuf::from(""));
    let project_dir = resolve_project_dir(cwd);
    StatusReport {
        global: inspect_target(&global_dir),
        project: inspect_target(&project_dir),
    }
}

/// Remove the managed file at `dir` only when it exists AND contains
/// the marker. Foreign files are refused with a structured conflict
/// error. Used by `plugin uninstall`.
pub fn uninstall_at(dir: &Path) -> Result<UninstallOutcome, PluginError> {
    let target = dir.join(PLUGIN_FILENAME);
    match std::fs::symlink_metadata(&target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(UninstallOutcome {
            warnings: vec![format!(
                "{} was not present; nothing to remove",
                target.display()
            )],
            ..UninstallOutcome::default()
        }),
        Err(error) => Err(PluginError::new(
            "filesystem",
            format!("cannot inspect plugin path {}: {error}", target.display()),
        )),
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(PluginError::new(
                    "conflict",
                    format!(
                        "refusing to remove {}: it is a symlink; remove or repoint it manually",
                        target.display()
                    ),
                ));
            }
            let current = std::fs::read(&target).map_err(|error| {
                PluginError::new(
                    "filesystem",
                    format!("cannot read plugin file {}: {error}", target.display()),
                )
            })?;
            if !contains_marker(&current) {
                return Err(PluginError::new(
                    "conflict",
                    format!(
                        "refusing to remove {}: file does not contain the {} marker; remove it manually",
                        target.display(),
                        MANAGED_MARKER
                    ),
                ));
            }
            std::fs::remove_file(&target).map_err(|error| {
                PluginError::new(
                    "filesystem",
                    format!("cannot remove plugin file {}: {error}", target.display()),
                )
            })?;
            Ok(UninstallOutcome {
                removed: vec![target_label(&target)],
                ..UninstallOutcome::default()
            })
        }
    }
}

/// Embedded adapter source. `pub(crate)` so the focused test module
/// can assert against the exact bytes that ship with the binary.
#[cfg(test)]
pub(crate) fn adapter_source() -> &'static str {
    ADAPTER_JS
}

fn ensure_dir(dir: &Path) -> Result<(), PluginError> {
    match std::fs::create_dir_all(dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(PluginError::new(
                "filesystem",
                format!("cannot create plugin directory {}: {error}", dir.display()),
            ));
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(dir).map_err(|error| {
            PluginError::new(
                "filesystem",
                format!("cannot stat plugin directory {}: {error}", dir.display()),
            )
        })?;
        let mut permissions = metadata.permissions();
        if permissions.mode() & 0o777 != 0o755 {
            permissions.set_mode(0o755);
            std::fs::set_permissions(dir, permissions).map_err(|error| {
                PluginError::new(
                    "filesystem",
                    format!(
                        "cannot set plugin directory mode {}: {error}",
                        dir.display()
                    ),
                )
            })?;
        }
    }
    Ok(())
}

fn write_managed(path: &Path) -> Result<(), PluginError> {
    atomic_write(path, ADAPTER_JS.as_bytes(), Some(0o644))
}

/// Writes via a sibling temp file plus rename so readers never
/// observe a partially written plugin file. Mirrors the atomic
/// write helper in `hooks.rs` so the install contract stays uniform.
fn atomic_write(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<(), PluginError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    let temp = dir.join(format!(
        ".{PLUGIN_FILENAME}.phasegent-{}-{unique}.tmp",
        std::process::id(),
    ));
    std::fs::write(&temp, bytes).map_err(|error| {
        PluginError::new(
            "filesystem",
            format!("cannot write {}: {error}", temp.display()),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let resolved = mode.unwrap_or_else(|| {
            std::fs::metadata(path)
                .map(|meta| meta.permissions().mode())
                .unwrap_or(0o644)
        });
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(resolved)).map_err(
            |error| {
                PluginError::new(
                    "filesystem",
                    format!("cannot set permissions on {}: {error}", temp.display()),
                )
            },
        )?;
    }
    std::fs::rename(&temp, path).map_err(|error| {
        PluginError::new(
            "filesystem",
            format!("cannot replace {}: {error}", path.display()),
        )
    })?;
    Ok(())
}

fn inspect_target(dir: &Path) -> PluginTargetStatus {
    let path = dir.join(PLUGIN_FILENAME);
    let label = path.to_string_lossy().to_string();
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if meta.is_file() => {
            let managed = std::fs::read(&path)
                .map(|bytes| contains_marker(&bytes))
                .unwrap_or(false);
            let mtime = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs() as i64);
            PluginTargetStatus {
                path: label,
                exists: true,
                managed,
                size: meta.len(),
                mtime,
            }
        }
        Ok(_) => PluginTargetStatus {
            path: label,
            exists: false,
            managed: false,
            size: 0,
            mtime: None,
        },
        Err(_) => PluginTargetStatus {
            path: label,
            exists: false,
            managed: false,
            size: 0,
            mtime: None,
        },
    }
}

fn contains_marker(bytes: &[u8]) -> bool {
    bytes
        .windows(MANAGED_MARKER.len())
        .any(|window| window == MANAGED_MARKER.as_bytes())
}

fn target_label(path: &Path) -> String {
    path.to_string_lossy().to_string()
}
