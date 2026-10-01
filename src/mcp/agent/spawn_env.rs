//! Child-process environment for the ACP explorer worker.
//!
//! A model-driven turn can read whatever its own process can read, and
//! the phasegent server's environment is a credential store:
//! `PHASEGENT_MCP_AUTH_TOKEN`, `PHASEGENT_REDMINE_GIT_MIRROR_API_KEY`
//! and the notify tokens all live there. The explorer therefore
//! inherits an explicit allowlist and nothing else.
//!
//! `HOME` stays on the list because MCode resolves its own
//! configuration and credentials from it; dropping it would break the
//! handshake this adapter exists to perform. `PATH` stays because the
//! child needs to find `node`/`bun` internals, which is also why the
//! program is resolved to an absolute path before the spawn instead
//! of relying on the child environment to resolve it.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use super::error::{AgentError, AgentResult};

/// The only variables the ACP child inherits. Every other name,
/// including all `PHASEGENT_*` settings, is dropped.
pub const CHILD_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "TMPDIR",
    "TEMP",
    "TMP",
    "SYSTEMROOT",
    "COMSPEC",
    "PATHEXT",
    "USERPROFILE",
    "APPDATA",
    "LOCALAPPDATA",
];

/// Searched when the parent process has no `PATH` to pass down.
const FALLBACK_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

/// Whether a variable name may cross into the child.
pub fn is_inherited_env(name: &str) -> bool {
    CHILD_ENV_ALLOWLIST.contains(&name)
}

/// The exact `(name, value)` pairs handed to the child.
pub fn child_env() -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = Vec::new();
    for name in CHILD_ENV_ALLOWLIST {
        if let Some(value) = std::env::var_os(name)
            && !value.is_empty()
        {
            env.push((OsStr::new(name).to_owned(), value));
        }
    }
    if !env.iter().any(|(name, _)| name == OsStr::new("PATH")) {
        env.push((
            OsStr::new("PATH").to_owned(),
            OsStr::new(FALLBACK_PATH).to_owned(),
        ));
    }
    env
}

/// The `PATH` entries a spawned child would search.
fn search_path() -> Vec<PathBuf> {
    let raw = child_env()
        .into_iter()
        .find(|(name, _)| name == OsStr::new("PATH"))
        .map(|(_, value)| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| FALLBACK_PATH.to_owned());
    raw.split(if cfg!(windows) { ';' } else { ':' })
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Resolve `program` to an absolute path before spawning. With a
/// cleared environment the child cannot be trusted to find a bare
/// program name, so the lookup happens in the parent against the same
/// `PATH` the child would have used.
pub fn resolve_program(program: &str) -> AgentResult<PathBuf> {
    if program.trim().is_empty() {
        return Err(AgentError::spawn("no ACP program configured"));
    }
    if program.contains('/') || program.contains('\\') {
        let candidate = Path::new(program);
        if candidate.is_absolute() {
            return Ok(candidate.to_path_buf());
        }
        let base = std::env::current_dir()
            .map_err(|error| AgentError::spawn(format!("no working directory: {error}")))?;
        return Ok(base.join(candidate));
    }
    for directory in search_path() {
        let hit = directory.join(program);
        if hit.is_file() {
            return Ok(hit);
        }
    }
    Err(AgentError::spawn(format!(
        "could not spawn the ACP process: '{program}' is not on PATH"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_drops_every_phasegent_and_secret_variable() {
        for name in [
            "PHASEGENT_MCP_AUTH_TOKEN",
            "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY",
            "PHASEGENT_NOTIFY_DINGTALK_WEBHOOK_TOKEN",
            "PHASEGENT_INDEX_PG_URL",
            "PHASEGENT_DEFAULT_PROVIDER",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
            "MINIMAX_API_KEY",
            "SSH_AUTH_SOCK",
        ] {
            assert!(
                !is_inherited_env(name),
                "{name} must not reach the explorer process"
            );
        }
        for name in ["PATH", "HOME", "LANG", "TZ"] {
            assert!(is_inherited_env(name), "{name} is required to start MCode");
        }
    }

    #[test]
    fn child_env_carries_only_allowlisted_names_and_always_a_path() {
        let env = child_env();
        assert!(!env.is_empty());
        assert!(
            env.iter().any(|(name, _)| name == OsStr::new("PATH")),
            "a PATH entry is mandatory for the spawn"
        );
        for (name, value) in &env {
            let name = name.to_string_lossy();
            assert!(is_inherited_env(&name), "{name} is not allowlisted");
            assert!(!value.is_empty(), "{name} must not be empty");
        }
    }

    #[test]
    fn program_resolution_rejects_unknown_names_and_passes_paths_through() {
        let error = resolve_program("phasegent-definitely-not-a-binary").expect_err("unknown name");
        assert_eq!(error.kind.as_str(), "spawn");
        assert!(resolve_program("  ").is_err());

        let resolved = resolve_program("sh").expect("sh is on PATH");
        assert!(resolved.is_absolute(), "{resolved:?} must be absolute");
        let relative = resolve_program("some/where/mcode").expect("relative path passes through");
        assert!(relative.is_absolute(), "{relative:?} must be absolute");
    }
}
