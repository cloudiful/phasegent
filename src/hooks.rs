//! Managed native Git hook installation and commit-message behavior.
//!
//! `phasegent hooks install` discovers the checkout's hooks directory via
//! `git rev-parse --git-path hooks` and writes self-contained POSIX shell
//! wrappers that call `phasegent hooks run <hook> "$@"`, resolving the
//! binary through PATH at runtime. Foreign hooks are never clobbered: they
//! are moved to `.git/hooks/phasegent-original/<name>` and the managed
//! wrapper chains to them. Issue IDs live only in local Git config; they are
//! never baked into hook files, and message contents are never printed.

use crate::branch_context::{self, BranchContextError, GitRunner};
use std::path::{Path, PathBuf};

pub const PREPARE_COMMIT_MSG_HOOK: &str = "prepare-commit-msg";
pub const COMMIT_MSG_HOOK: &str = "commit-msg";
/// Marker comment embedded in managed hook files so later installs can
/// recognize and update their own scripts without touching foreign ones.
pub const MANAGED_MARKER: &str = "# phasegent:managed";
/// Directory inside the hooks directory that preserves displaced foreign hooks.
pub const ORIGINAL_BACKUP_DIR: &str = "phasegent-original";

/// Message sources Git passes to `prepare-commit-msg`.
const KNOWN_SOURCES: [&str; 6] = ["", "message", "template", "merge", "squash", "commit"];
/// Git-generated sources whose messages must never be rewritten.
const SKIP_SOURCES: [&str; 3] = ["merge", "squash", "commit"];
/// Keywords recognized as Redmine reference tokens (checked case-insensitively).
const REF_KEYWORDS: [&str; 6] = ["refs", "references", "ref", "fixes", "closes", "closed"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookKind {
    PrepareCommitMsg,
    CommitMsg,
}

impl HookKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::PrepareCommitMsg => PREPARE_COMMIT_MSG_HOOK,
            Self::CommitMsg => COMMIT_MSG_HOOK,
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            PREPARE_COMMIT_MSG_HOOK => Some(Self::PrepareCommitMsg),
            COMMIT_MSG_HOOK => Some(Self::CommitMsg),
            _ => None,
        }
    }
}

/// Parsed shape of the internal `phasegent hooks run ...` forms invoked by
/// generated hook scripts. These forms need neither a role nor credentials.
#[derive(Debug)]
pub enum HooksCommand {
    Install,
    Run {
        hook: HookKind,
        message_file: String,
        source: Option<String>,
    },
}

#[derive(Debug, Default)]
pub struct InstallOutcome {
    pub installed: Vec<&'static str>,
    pub updated: Vec<&'static str>,
    pub warnings: Vec<String>,
}

pub fn install() -> Result<InstallOutcome, BranchContextError> {
    let working_dir = std::env::current_dir().map_err(|error| {
        BranchContextError::new(
            "filesystem",
            format!("cannot resolve working directory: {error}"),
        )
    })?;
    install_in(&branch_context::ProcessGitRunner::new(), &working_dir)
}

pub fn install_in(
    runner: &dyn GitRunner,
    working_dir: &Path,
) -> Result<InstallOutcome, BranchContextError> {
    let output = runner.run(&["rev-parse", "--git-path", "hooks"])?;
    if output.status != 0 {
        return Err(BranchContextError::new(
            "git",
            "git rev-parse --git-path hooks failed; run `phasegent hooks install` inside a Git checkout",
        ));
    }
    let discovered = output.stdout.trim();
    if discovered.is_empty() {
        return Err(BranchContextError::new(
            "git",
            "git rev-parse --git-path hooks returned an empty path",
        ));
    }
    let hooks_dir = working_dir.join(discovered);
    std::fs::create_dir_all(&hooks_dir).map_err(|error| {
        BranchContextError::new(
            "filesystem",
            format!(
                "cannot create hooks directory {}: {error}",
                hooks_dir.display()
            ),
        )
    })?;
    let hooks_dir = std::fs::canonicalize(&hooks_dir).map_err(|error| {
        BranchContextError::new(
            "filesystem",
            format!(
                "cannot resolve hooks directory {}: {error}",
                hooks_dir.display()
            ),
        )
    })?;

    let mut outcome = InstallOutcome::default();
    install_hook(&hooks_dir, HookKind::PrepareCommitMsg, &mut outcome)?;
    install_hook(&hooks_dir, HookKind::CommitMsg, &mut outcome)?;
    Ok(outcome)
}

fn install_hook(
    hooks_dir: &Path,
    hook: HookKind,
    outcome: &mut InstallOutcome,
) -> Result<(), BranchContextError> {
    let name = hook.name();
    let path = hooks_dir.join(name);
    // The backup decision is derived from on-disk state, not history, so
    // repeated installs converge without nesting wrappers or overwriting
    // a preserved original.
    let backup_path = hooks_dir.join(ORIGINAL_BACKUP_DIR).join(name);

    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_script(
                &path,
                &render_script(
                    hook,
                    backup_exists(&backup_path).then_some(backup_path.as_path()),
                ),
            )?;
            outcome.installed.push(name);
        }
        Err(error) => {
            return Err(BranchContextError::new(
                "filesystem",
                format!("cannot inspect hook {}: {error}", path.display()),
            ));
        }
        Ok(meta) => {
            if meta.file_type().is_symlink() {
                return Err(BranchContextError::new(
                    "conflict",
                    format!(
                        "refusing to manage {}: it is a symlink; remove or repoint it manually, then re-run hooks install",
                        path.display()
                    ),
                ));
            }
            if !meta.is_file() {
                return Err(BranchContextError::new(
                    "conflict",
                    format!(
                        "refusing to manage {}: it is not a regular file; inspect it manually",
                        path.display()
                    ),
                ));
            }
            let current = std::fs::read(&path).map_err(|error| {
                BranchContextError::new(
                    "filesystem",
                    format!("cannot read existing hook {}: {error}", path.display()),
                )
            })?;
            if current
                .windows(MANAGED_MARKER.len())
                .any(|w| w == MANAGED_MARKER.as_bytes())
            {
                let desired = render_script(
                    hook,
                    backup_exists(&backup_path).then_some(backup_path.as_path()),
                );
                if current != desired.as_bytes() {
                    write_script(&path, &desired)?;
                }
                outcome.updated.push(name);
                return Ok(());
            }
            displace_foreign_hook(&path, &backup_path, name, outcome)?;
            write_script(&path, &render_script(hook, Some(&backup_path)))?;
            outcome.installed.push(name);
        }
    }
    Ok(())
}

fn backup_exists(backup_path: &Path) -> bool {
    std::fs::symlink_metadata(backup_path).is_ok()
}

/// Moves an unrelated existing hook to the deterministic backup location,
/// preserving its bytes and mode via rename. When a backup already exists the
/// current hook cannot be displaced safely, so installation fails before any
/// mutation rather than silently overwriting the live foreign hook.
fn displace_foreign_hook(
    path: &Path,
    backup_path: &Path,
    name: &str,
    outcome: &mut InstallOutcome,
) -> Result<(), BranchContextError> {
    if backup_exists(backup_path) {
        return Err(BranchContextError::new(
            "conflict",
            format!(
                "refusing to replace {}: an original {name} hook is already preserved at {}; \
                 restore it over the current hook or remove one of them manually, then re-run hooks install",
                path.display(),
                backup_path.display()
            ),
        ));
    }
    let parent = backup_path
        .parent()
        .ok_or_else(|| BranchContextError::new("filesystem", "backup path has no parent"))?;
    std::fs::create_dir_all(parent).map_err(|error| {
        BranchContextError::new(
            "filesystem",
            format!(
                "cannot create backup directory {}: {error}",
                parent.display()
            ),
        )
    })?;
    std::fs::rename(path, backup_path).map_err(|error| {
        BranchContextError::new(
            "filesystem",
            format!(
                "cannot move existing {name} hook to {}: check permissions; {}",
                backup_path.display(),
                error
            ),
        )
    })?;
    outcome.warnings.push(format!(
        "moved existing {name} hook to {}; the managed wrapper chains to it",
        backup_path.display()
    ));
    Ok(())
}

/// Renders a self-contained POSIX shell wrapper. `phasegent` is resolved
/// through PATH at runtime; no absolute developer path, credential, or issue
/// value is ever embedded.
fn render_script(hook: HookKind, backup: Option<&Path>) -> String {
    let mut script = String::from("#!/bin/sh\n");
    script.push_str(MANAGED_MARKER);
    script.push_str("\n# Installed by `phasegent hooks install`; safe to reinstall or remove.\n");
    if let Some(backup) = backup {
        script.push_str(
            "# The preserved original hook runs first; phasegent runs only if it succeeds.\n",
        );
        script.push_str(&format!(
            "PHASEGENT_ORIGINAL_HOOK={}\n",
            shell_quote(&backup.to_string_lossy())
        ));
        script.push_str("\"$PHASEGENT_ORIGINAL_HOOK\" \"$@\" || exit $?\n");
    }
    script.push_str(&format!(
        "exec phasegent hooks run {} \"$@\"\n",
        hook.name()
    ));
    script
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Test-only rendering hook so tests can assert on script contents without
/// touching the filesystem.
#[cfg(test)]
pub fn render_script_for_tests(hook: HookKind, backup: Option<&Path>) -> String {
    render_script(hook, backup)
}

fn write_script(path: &Path, contents: &str) -> Result<(), BranchContextError> {
    atomic_write(path, contents.as_bytes(), Some(0o755))
}

/// Writes via a sibling temp file plus rename so readers never observe a
/// partially written hook or message file.
fn atomic_write(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<(), BranchContextError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned());
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    let temp = dir.join(format!(
        ".{file_name}.phasegent-{}-{unique}.tmp",
        std::process::id()
    ));
    std::fs::write(&temp, bytes).map_err(|error| {
        BranchContextError::new(
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
                BranchContextError::new(
                    "filesystem",
                    format!("cannot set permissions on {}: {error}", temp.display()),
                )
            },
        )?;
    }
    std::fs::rename(&temp, path).map_err(|error| {
        BranchContextError::new(
            "filesystem",
            format!("cannot replace {}: {error}", path.display()),
        )
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal `hooks run` execution used by the generated scripts.
// ---------------------------------------------------------------------------

pub fn run(
    hook: HookKind,
    message_file: &str,
    source: Option<&str>,
) -> Result<serde_json::Value, BranchContextError> {
    run_with(
        &branch_context::ProcessGitRunner::new(),
        hook,
        message_file,
        source,
    )
}

pub fn run_with(
    runner: &dyn GitRunner,
    hook: HookKind,
    message_file: &str,
    source: Option<&str>,
) -> Result<serde_json::Value, BranchContextError> {
    if let Some(source) = source
        && !KNOWN_SOURCES.contains(&source)
    {
        return Err(BranchContextError::new(
            "argument",
            format!(
                "unsupported prepare-commit-msg source '{source}'; expected one of: message, template, merge, squash, commit, or empty"
            ),
        ));
    }
    let path = PathBuf::from(message_file);
    // Read lossily but never echo the contents anywhere.
    let bytes = std::fs::read(&path).map_err(|error| {
        BranchContextError::new(
            "argument",
            format!(
                "message file '{}' is missing or unreadable: {error}",
                path.display()
            ),
        )
    })?;
    match hook {
        HookKind::PrepareCommitMsg => prepare_commit_msg(runner, &path, &bytes, source),
        HookKind::CommitMsg => commit_msg(runner, &path, &bytes),
    }
}

/// Detached HEAD carries no branch section, so it behaves like "unbound":
/// hooks stay silent instead of blocking rebases and cherry-pick workflows.
fn bound_issue_id(runner: &dyn GitRunner) -> Result<Option<u64>, BranchContextError> {
    match branch_context::current_branch(runner) {
        Ok(branch) => branch_context::read_issue_id(runner, &branch),
        Err(error) if error.kind == "branch" => Ok(None),
        Err(error) => Err(error),
    }
}

fn noop(hook: HookKind, reason: &str) -> serde_json::Value {
    serde_json::json!({ "hook": hook.name(), "action": "noop", "reason": reason })
}

fn prepare_commit_msg(
    runner: &dyn GitRunner,
    path: &Path,
    bytes: &[u8],
    source: Option<&str>,
) -> Result<serde_json::Value, BranchContextError> {
    if source.is_some_and(|source| SKIP_SOURCES.contains(&source)) {
        return Ok(noop(HookKind::PrepareCommitMsg, "git-generated source"));
    }
    let Some(issue_id) = bound_issue_id(runner)? else {
        return Ok(noop(HookKind::PrepareCommitMsg, "no branch binding"));
    };
    let text = String::from_utf8_lossy(bytes);
    if has_any_issue_reference(&text) {
        return Ok(noop(
            HookKind::PrepareCommitMsg,
            "message already references an issue",
        ));
    }
    let body = text.trim_end_matches('\n');
    let updated = format!("{body}\n\nRefs #{issue_id}\n");
    atomic_write(path, updated.as_bytes(), None)?;
    Ok(serde_json::json!({
        "hook": HookKind::PrepareCommitMsg.name(),
        "action": "appended",
        "trailer": format!("Refs #{issue_id}"),
    }))
}

fn commit_msg(
    runner: &dyn GitRunner,
    _path: &Path,
    bytes: &[u8],
) -> Result<serde_json::Value, BranchContextError> {
    let Some(issue_id) = bound_issue_id(runner)? else {
        return Ok(noop(HookKind::CommitMsg, "no branch binding"));
    };
    let text = String::from_utf8_lossy(bytes);
    let conflicts: Vec<u64> = issue_references(&text)
        .into_iter()
        .filter(|found| *found != issue_id)
        .collect();
    if !conflicts.is_empty() {
        return Err(BranchContextError::new(
            "conflict",
            format!(
                "message references Redmine issue(s) {conflicts:?} but this branch is bound to issue {issue_id}; \
                 fix the message or commit with --no-verify"
            ),
        ));
    }
    // Reject duplicated generated trailers while leaving free-form body
    // mentions alone: only exact `Refs #<id>` lines count as generated.
    let generated = format!("Refs #{issue_id}");
    let duplicates = text.lines().filter(|line| line.trim() == generated).count();
    if duplicates > 1 {
        return Err(BranchContextError::new(
            "conflict",
            format!(
                "message contains {duplicates} identical '{generated}' trailers; keep exactly one"
            ),
        ));
    }
    Ok(serde_json::json!({
        "hook": HookKind::CommitMsg.name(),
        "action": "valid",
    }))
}

fn has_any_issue_reference(text: &str) -> bool {
    !issue_references(text).is_empty()
}

/// Collects every issue ID referenced through a Redmine keyword token such as
/// `Refs #12` or `fixes:#34`, case-insensitive, requiring word boundaries on
/// both sides so identifiers like `Xrefs #1` or `#12abc` do not match.
fn issue_references(text: &str) -> Vec<u64> {
    let lowered = text.to_lowercase();
    let bytes = lowered.as_bytes();
    let mut ids: Vec<u64> = Vec::new();
    for keyword in REF_KEYWORDS {
        let mut search_from = 0;
        while let Some(offset) = lowered[search_from..].find(keyword) {
            let at = search_from + offset;
            search_from = at + keyword.len();
            if at > 0 && bytes[at - 1].is_ascii_alphanumeric() {
                continue;
            }
            let mut cursor = search_from;
            while cursor < bytes.len() && (bytes[cursor] == b' ' || bytes[cursor] == b'\t') {
                cursor += 1;
            }
            if cursor >= bytes.len() || bytes[cursor] != b'#' {
                continue;
            }
            let digits_start = cursor + 1;
            let mut digits_end = digits_start;
            while digits_end < bytes.len() && bytes[digits_end].is_ascii_digit() {
                digits_end += 1;
            }
            if digits_end == digits_start
                || (digits_end < bytes.len() && bytes[digits_end].is_ascii_alphanumeric())
            {
                continue;
            }
            if let Ok(found) = lowered[digits_start..digits_end].parse::<u64>()
                && !ids.contains(&found)
            {
                ids.push(found);
            }
        }
    }
    ids
}
