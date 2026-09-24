//! Filesystem resolution for the OpenCode plugin directories (issue #591).
//!
//! The installer targets OpenCode directly: it resolves the configured
//! OpenCode plugin location and never probes installed agent binaries or
//! scans other agent hosts. Global resolution follows the OpenCode
//! convention and prefers, in order, a non-empty `$XDG_CONFIG_HOME`
//! (itself a config root), a non-empty `$HOME`, then a non-empty
//! `%USERPROFILE%` on Windows. Unset or empty roots are skipped and an
//! unusable environment yields a structured error instead of a relative
//! current-directory target.

use std::path::{Path, PathBuf};

use super::PluginError;

/// OpenCode plugins directory relative to a config root. Matches the
/// OpenCode convention on every supported platform.
pub const OPENCODE_PLUGIN_DIR: &str = "opencode/plugins";

/// Config roots probed for the global scope, in precedence order.
/// `XDG_CONFIG_HOME` already names a config root, while `HOME` and
/// `USERPROFILE` need the `.config` segment appended.
const GLOBAL_CONFIG_ROOTS: [(&str, &str); 3] = [
    ("XDG_CONFIG_HOME", ""),
    ("HOME", ".config"),
    ("USERPROFILE", ".config"),
];

/// Resolve the global OpenCode plugins directory from the process
/// environment. Returns a structured `filesystem` error when every
/// candidate root is unset or empty, so callers never resolve to a
/// relative path under the current directory.
pub fn resolve_global_dir() -> Result<PathBuf, PluginError> {
    Ok(global_config_root()?.join(OPENCODE_PLUGIN_DIR))
}

fn global_config_root() -> Result<PathBuf, PluginError> {
    for (name, config_segment) in GLOBAL_CONFIG_ROOTS {
        let Some(value) = non_empty_env(name) else {
            continue;
        };
        let root = PathBuf::from(value);
        return Ok(if config_segment.is_empty() {
            root
        } else {
            root.join(config_segment)
        });
    }
    Err(PluginError::new(
        "filesystem",
        "cannot resolve the global OpenCode config directory: XDG_CONFIG_HOME, HOME, \
         and USERPROFILE are all unset or empty",
    ))
}

fn non_empty_env(name: &str) -> Option<std::ffi::OsString> {
    let value = std::env::var_os(name)?;
    if value.is_empty() { None } else { Some(value) }
}

/// Resolve the project-scope plugins directory inside the given
/// checkout. Returned as a path joined onto `cwd` so callers can
/// canonicalise later.
pub fn resolve_project_dir(cwd: &Path) -> PathBuf {
    cwd.join(".opencode").join("plugins")
}
