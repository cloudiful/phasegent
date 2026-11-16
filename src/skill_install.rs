//! Bundled `phasegent` skill installation (issue #22 P2).
//!
//! The skill is one directory (`phasegent/`) containing the concise entry
//! `SKILL.md` and on-demand references under `references/`. The installed bytes
//! are compiled into the binary, so an operator never needs the source checkout:
//! `bun` is not required and no file is fetched.
//!
//! Safety contract:
//! * the default root is `~/.agents/skills` (`HOME`, then `USERPROFILE`), and
//!   `--project` selects `.agents/skills` in the current checkout;
//! * the target directory is owned by a `.phasegent-managed` marker, so a
//!   foreign `phasegent` directory is refused without `--force`;
//! * `--force` first renames foreign content to `phasegent.phasegent-orig`, so
//!   no user content is lost;
//! * symlinks at the root, target directory, or any managed file are refused
//!   rather than followed;
//! * files are written atomically (sibling temp file + rename) and an install
//!   never removes a pre-existing file it does not own.

use std::path::{Path, PathBuf};

pub const SKILL_DIR_NAME: &str = "phasegent";
pub const SKILL_FILENAME: &str = "SKILL.md";
pub const REFERENCES_DIR: &str = "references";
pub const MANAGED_MARKER_FILE: &str = ".phasegent-managed";
pub const FOREIGN_BACKUP_SUFFIX: &str = ".phasegent-orig";

const MANAGED_MARKER_CONTENT: &str = "phasegent managed skill directory\n";

struct BundledFile {
    relative: &'static str,
    content: &'static str,
}

const BUNDLED_FILES: &[BundledFile] = &[
    BundledFile {
        relative: SKILL_FILENAME,
        content: include_str!("../skills/phasegent/SKILL.md"),
    },
    BundledFile {
        relative: "references/protocol.md",
        content: include_str!("../skills/phasegent/references/protocol.md"),
    },
    BundledFile {
        relative: "references/orchestrator.md",
        content: include_str!("../skills/phasegent/references/orchestrator.md"),
    },
    BundledFile {
        relative: "references/executor.md",
        content: include_str!("../skills/phasegent/references/executor.md"),
    },
    BundledFile {
        relative: "references/reviewer.md",
        content: include_str!("../skills/phasegent/references/reviewer.md"),
    },
    BundledFile {
        relative: "references/explore.md",
        content: include_str!("../skills/phasegent/references/explore.md"),
    },
];

#[derive(Debug, Default, Clone)]
pub struct SkillInstallOutcome {
    pub installed: Vec<String>,
    pub updated: Vec<String>,
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct SkillInstallError {
    pub kind: &'static str,
    pub message: String,
}

impl SkillInstallError {
    fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({ "kind": self.kind, "message": self.message })
    }
}

impl std::fmt::Display for SkillInstallError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for SkillInstallError {}

/// Resolve the default agent-home skills root. `HOME` wins; Windows without
/// `HOME` falls back to `%USERPROFILE%`. Empty values are skipped.
pub fn resolve_global_skills_root() -> Result<PathBuf, SkillInstallError> {
    for name in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(name)
            && !value.is_empty()
        {
            return Ok(PathBuf::from(value).join(".agents").join("skills"));
        }
    }
    Err(SkillInstallError::new(
        "filesystem",
        "cannot resolve the agent home skills directory: HOME and USERPROFILE are both unset or empty",
    ))
}

/// `--project` root: `.agents/skills` in the checkout.
pub fn resolve_project_skills_root(cwd: &Path) -> PathBuf {
    cwd.join(".agents").join("skills")
}

/// Install the single bundled skill under `root`. `root` is the skills root
/// (the directory that contains `phasegent/`).
pub fn install_at(root: &Path, force: bool) -> Result<SkillInstallOutcome, SkillInstallError> {
    prepare_root(root)?;
    let target = root.join(SKILL_DIR_NAME);
    // Refuse an ancestor symlink below the trusted anchor before any write,
    // including the update and forced-replacement paths.
    refuse_symlinked_descendants(&target)?;
    match std::fs::symlink_metadata(&target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            write_fresh(&target)?;
            Ok(SkillInstallOutcome {
                installed: vec![target_label(&target)],
                ..SkillInstallOutcome::default()
            })
        }
        Err(error) => Err(SkillInstallError::new(
            "filesystem",
            format!("cannot inspect skill path {}: {error}", target.display()),
        )),
        Ok(meta) if meta.file_type().is_symlink() => Err(SkillInstallError::new(
            "conflict",
            format!(
                "refusing to manage {}: it is a symlink; remove or repoint it manually, then re-run skill install",
                target.display()
            ),
        )),
        Ok(meta) if meta.is_dir() => {
            if is_managed(&target) {
                update_managed(&target)
            } else if !force {
                Err(foreign_conflict(&target))
            } else {
                replace_foreign(root, &target)
            }
        }
        Ok(_) => {
            if !force {
                return Err(foreign_conflict(&target));
            }
            replace_foreign(root, &target)
        }
    }
}

fn foreign_conflict(target: &Path) -> SkillInstallError {
    SkillInstallError::new(
        "conflict",
        format!(
            "refusing to replace {}: it exists without the {} marker; move it manually, or re-run with --force to preserve it as {}{}",
            target.display(),
            MANAGED_MARKER_FILE,
            SKILL_DIR_NAME,
            FOREIGN_BACKUP_SUFFIX,
        ),
    )
}

fn replace_foreign(root: &Path, target: &Path) -> Result<SkillInstallOutcome, SkillInstallError> {
    let backup = root.join(format!("{SKILL_DIR_NAME}{FOREIGN_BACKUP_SUFFIX}"));
    if backup.exists() || std::fs::symlink_metadata(&backup).is_ok() {
        return Err(SkillInstallError::new(
            "conflict",
            format!(
                "refusing to displace {}: backup {} already exists; remove it manually, then re-run skill install",
                target.display(),
                backup.display()
            ),
        ));
    }
    std::fs::rename(target, &backup).map_err(|error| {
        SkillInstallError::new(
            "filesystem",
            format!(
                "cannot preserve existing {} as {}: {error}",
                target.display(),
                backup.display()
            ),
        )
    })?;
    write_fresh(target)?;
    Ok(SkillInstallOutcome {
        installed: vec![target_label(target)],
        warnings: vec![format!(
            "preserved existing content at {}; the managed phasegent skill now owns {}",
            backup.display(),
            target.display()
        )],
        ..SkillInstallOutcome::default()
    })
}

fn write_fresh(target: &Path) -> Result<(), SkillInstallError> {
    ensure_directory(target, "skill directory")?;
    write_atomic(
        &target.join(MANAGED_MARKER_FILE),
        MANAGED_MARKER_CONTENT.as_bytes(),
    )?;
    ensure_directory(&target.join(REFERENCES_DIR), "skill reference directory")?;
    for file in BUNDLED_FILES {
        let path = target.join(file.relative);
        if let Some(parent) = path.parent() {
            ensure_directory(parent, "skill reference directory")?;
        }
        write_atomic(&path, file.content.as_bytes())?;
    }
    Ok(())
}
fn update_managed(target: &Path) -> Result<SkillInstallOutcome, SkillInstallError> {
    let mut outcome = SkillInstallOutcome::default();
    let mut changed = false;
    for file in BUNDLED_FILES {
        let path = target.join(file.relative);
        if let Some(parent) = path.parent() {
            ensure_directory(parent, "skill reference directory")?;
        }
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                write_atomic(&path, file.content.as_bytes())?;
                changed = true;
            }
            Err(error) => {
                return Err(SkillInstallError::new(
                    "filesystem",
                    format!("cannot inspect skill file {}: {error}", path.display()),
                ));
            }
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(SkillInstallError::new(
                    "conflict",
                    format!(
                        "refusing to manage {}: it is a symlink; remove or repoint it manually, then re-run skill install",
                        path.display()
                    ),
                ));
            }
            Ok(meta) if !meta.is_file() => {
                return Err(SkillInstallError::new(
                    "conflict",
                    format!(
                        "refusing to manage {}: it is not a regular file; inspect it manually",
                        path.display()
                    ),
                ));
            }
            Ok(_) => {
                let current = std::fs::read(&path).map_err(|error| {
                    SkillInstallError::new(
                        "filesystem",
                        format!("cannot read skill file {}: {error}", path.display()),
                    )
                })?;
                if current != file.content.as_bytes() {
                    write_atomic(&path, file.content.as_bytes())?;
                    changed = true;
                }
            }
        }
    }
    if changed {
        outcome.updated.push(target_label(target));
    } else {
        outcome.skipped.push(target_label(target));
    }
    Ok(outcome)
}

fn is_managed(target: &Path) -> bool {
    let marker = target.join(MANAGED_MARKER_FILE);
    match std::fs::symlink_metadata(&marker) {
        Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => std::fs::read(&marker)
            .map(|bytes| bytes == MANAGED_MARKER_CONTENT.as_bytes())
            .unwrap_or(false),
        _ => false,
    }
}

/// The directory the destination path is anchored to for symlink checks.
/// Global and project roots share the `.agents/skills` suffix: the anchor is
/// the directory above `.agents`, so a legitimate symlinked home or checkout
/// stays trusted while the installer-owned descendants are still checked. Any
/// other custom root is checked component by component from its own start.
fn destination_anchor(root: &Path) -> Option<&Path> {
    let skills = root.file_name()?;
    if skills != "skills" {
        return None;
    }
    let agents = root.parent()?.file_name()?;
    if agents != ".agents" {
        return None;
    }
    let candidate = root.parent()?.parent()?;
    // Only the resolved home roots and the current checkout are trusted
    // anchors (a legitimate symlinked home stays usable). A custom root under
    // `.agents/skills` is still checked component by component.
    let mut anchors: Vec<PathBuf> = Vec::new();
    for name in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(name)
            && !value.is_empty()
        {
            anchors.push(PathBuf::from(value));
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        anchors.push(cwd);
    }
    if anchors.iter().any(|anchor| anchor == candidate) {
        Some(candidate)
    } else {
        None
    }
}

/// Create or verify every installer-owned component of the skills root while
/// preserving the mode of a directory that already exists.
fn prepare_root(root: &Path) -> Result<(), SkillInstallError> {
    let anchor = destination_anchor(root);
    let mut current = PathBuf::new();
    let relative = match anchor {
        Some(anchor) => {
            current.push(anchor);
            root.strip_prefix(anchor).map_err(|_| {
                SkillInstallError::new(
                    "conflict",
                    format!(
                        "refusing to manage {}: it is not inside the trusted root {}",
                        root.display(),
                        anchor.display()
                    ),
                )
            })?
        }
        None => root,
    };
    for component in relative.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                current.push(component.as_os_str());
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(SkillInstallError::new(
                    "conflict",
                    format!(
                        "refusing to manage {}: a skills path must not contain '..'",
                        root.display()
                    ),
                ));
            }
            std::path::Component::Normal(name) => {
                current.push(name);
                ensure_directory(&current, "skills root")?;
            }
        }
    }
    Ok(())
}

/// Refuse a symlinked installer-owned descendant before any write. The target
/// directory itself and its `references` child are the only descendants the
/// installer creates or updates.
fn refuse_symlinked_descendants(target: &Path) -> Result<(), SkillInstallError> {
    let meta = match std::fs::symlink_metadata(target) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(SkillInstallError::new(
                "filesystem",
                format!("cannot inspect skill path {}: {error}", target.display()),
            ));
        }
        Ok(meta) => meta,
    };
    if meta.file_type().is_symlink() {
        return Err(symlink_conflict(target, "skill directory"));
    }
    if !meta.is_dir() {
        return Ok(());
    }
    let references = target.join(REFERENCES_DIR);
    match std::fs::symlink_metadata(&references) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(symlink_conflict(&references, "skill reference directory"));
        }
        Ok(meta) if !meta.is_dir() => {
            return Err(SkillInstallError::new(
                "conflict",
                format!(
                    "refusing to manage skill reference directory {}: it is not a directory",
                    references.display()
                ),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(SkillInstallError::new(
                "filesystem",
                format!(
                    "cannot inspect skill reference directory {}: {error}",
                    references.display()
                ),
            ));
        }
        Ok(_) => {}
    }
    Ok(())
}

fn symlink_conflict(path: &Path, label: &str) -> SkillInstallError {
    SkillInstallError::new(
        "conflict",
        format!(
            "refusing to manage {label} {}: it is a symlink; remove or repoint it manually",
            path.display()
        ),
    )
}

/// Create the directory when it is missing, and never change the mode of one
/// that already exists. A symlinked component or a non-directory is refused.
fn ensure_directory(path: &Path, label: &str) -> Result<(), SkillInstallError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path).map_err(|error| {
                SkillInstallError::new(
                    "filesystem",
                    format!("cannot create {label} {}: {error}", path.display()),
                )
            })?;
            set_directory_mode(path, label)?;
        }
        Err(error) => {
            return Err(SkillInstallError::new(
                "filesystem",
                format!("cannot inspect {label} {}: {error}", path.display()),
            ));
        }
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(symlink_conflict(path, label));
        }
        Ok(meta) if !meta.is_dir() => {
            return Err(SkillInstallError::new(
                "conflict",
                format!(
                    "refusing to manage {label} {}: it is not a directory",
                    path.display()
                ),
            ));
        }
        Ok(_) => {}
    }
    Ok(())
}

/// Apply the controlled mode only to a directory this run created.
#[cfg(unix)]
fn set_directory_mode(path: &Path, label: &str) -> Result<(), SkillInstallError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|error| {
        SkillInstallError::new(
            "filesystem",
            format!("cannot set {label} mode {}: {error}", path.display()),
        )
    })
}

#[cfg(not(unix))]
fn set_directory_mode(_path: &Path, _label: &str) -> Result<(), SkillInstallError> {
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), SkillInstallError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    let temp = dir.join(format!(
        ".phasegent-skill-{}-{unique}.tmp",
        std::process::id()
    ));
    std::fs::write(&temp, bytes).map_err(|error| {
        SkillInstallError::new(
            "filesystem",
            format!("cannot write {}: {error}", temp.display()),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o644)).map_err(
            |error| {
                SkillInstallError::new(
                    "filesystem",
                    format!("cannot set permissions on {}: {error}", temp.display()),
                )
            },
        )?;
    }
    std::fs::rename(&temp, path).map_err(|error| {
        SkillInstallError::new(
            "filesystem",
            format!("cannot replace {}: {error}", path.display()),
        )
    })
}

fn target_label(path: &Path) -> String {
    path.to_string_lossy().to_string()
}
