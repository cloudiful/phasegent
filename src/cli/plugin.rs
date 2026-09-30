//! Executor for the `plugin` command group (issue #239 Phase 3).
//!
//! Subcommand routing is mirror-shaped with the hooks install surface: no
//! role is required, no provider is touched, no network is involved. The
//! executor funnels the subcommands through the same `install_at` /
//! `uninstall_at` helpers in [`crate::plugin`] so the CLI layer stays purely
//! presentational.
//!
//! `--path DIR` (issue 666) replaces the scope selectors with one explicit
//! plugin directory. The default envelopes stay byte-stable: the
//! explicit-path fields are additive and skipped when absent, so a default
//! invocation keeps exactly the previous `global` / `project` shape.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::command::PluginCommand;
use crate::plugin::{
    InstallOutcome, PLUGIN_FILENAME, PluginError, PluginTargetStatus, StatusReport,
    UninstallOutcome, inspect_target, install_at, resolve_global_dir, resolve_project_dir,
    status_at, uninstall_at,
};

/// JSON envelope returned by `plugin install`. Stable field order so
/// downstream tooling can replay the same shape across releases.
/// `global_path` / `project_path` are the default scope targets; the explicit
/// `--path` mode fills the additive `target_path` instead.
#[derive(Debug, Serialize)]
pub struct InstallEnvelope {
    pub installed: Vec<String>,
    pub updated: Vec<String>,
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
}

impl InstallEnvelope {
    fn scoped(global: InstallOutcome, project: InstallOutcome, paths: (String, String)) -> Self {
        let mut installed = global.installed.clone();
        let mut updated = global.updated.clone();
        let mut skipped = global.skipped.clone();
        let mut warnings = global.warnings.clone();
        let mut errors = global.errors.clone();
        installed.extend(project.installed.clone());
        updated.extend(project.updated.clone());
        skipped.extend(project.skipped.clone());
        warnings.extend(project.warnings.clone());
        errors.extend(project.errors.clone());
        Self {
            installed,
            updated,
            skipped,
            warnings,
            errors,
            global_path: Some(paths.0),
            project_path: Some(paths.1),
            target_path: None,
        }
    }

    /// One explicit `--path DIR` target; the scope fields stay absent.
    fn explicit(target: &Path, outcome: InstallOutcome) -> Self {
        Self {
            installed: outcome.installed,
            updated: outcome.updated,
            skipped: outcome.skipped,
            warnings: outcome.warnings,
            errors: outcome.errors,
            global_path: None,
            project_path: None,
            target_path: Some(target.to_string_lossy().to_string()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UninstallEnvelope {
    pub removed: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_path: Option<String>,
}

impl UninstallEnvelope {
    fn scoped(
        global: UninstallOutcome,
        project: UninstallOutcome,
        paths: (String, String),
    ) -> Self {
        let mut removed = global.removed.clone();
        let mut warnings = global.warnings.clone();
        let mut errors = global.errors.clone();
        removed.extend(project.removed.clone());
        warnings.extend(project.warnings.clone());
        errors.extend(project.errors.clone());
        Self {
            removed,
            warnings,
            errors,
            global_path: Some(paths.0),
            project_path: Some(paths.1),
            target_path: None,
        }
    }

    fn explicit(target: &Path, outcome: UninstallOutcome) -> Self {
        Self {
            removed: outcome.removed,
            warnings: outcome.warnings,
            errors: outcome.errors,
            global_path: None,
            project_path: None,
            target_path: Some(target.to_string_lossy().to_string()),
        }
    }
}

/// JSON envelope returned by `plugin status`. The default scope modes keep the
/// `global` / `project` slots; the explicit `--path` mode reports the chosen
/// directory through `target` only. The inner `PluginTargetStatus` shape is
/// stable: `{path, exists, managed, size, mtime}`.
#[derive(Debug, Serialize)]
pub struct StatusEnvelope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global: Option<PluginTargetStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<PluginTargetStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<PluginTargetStatus>,
}

impl From<StatusReport> for StatusEnvelope {
    fn from(report: StatusReport) -> Self {
        Self {
            global: Some(report.global),
            project: Some(report.project),
            target: None,
        }
    }
}

/// Envelope for `plugin status --path DIR`: the explicit directory only,
/// inspected through the same primitive as the scoped slots.
pub(crate) fn explicit_status_envelope(dir: &Path) -> StatusEnvelope {
    StatusEnvelope {
        global: None,
        project: None,
        target: Some(inspect_target(dir)),
    }
}

pub(crate) fn execute_plugin(command: PluginCommand) -> i32 {
    match command {
        PluginCommand::Install {
            global,
            project,
            force,
            path,
        } => execute_install(global, project, force, path),
        PluginCommand::Status { path } => execute_status(path),
        PluginCommand::Uninstall {
            global,
            project,
            path,
        } => execute_uninstall(global, project, path),
    }
}

fn execute_install(
    global_flag: bool,
    project_flag: bool,
    force: bool,
    path: Option<String>,
) -> i32 {
    if let Some(dir) = path {
        return install_explicit(&PathBuf::from(dir), force);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (global_dir, project_dir) = match resolve_targets(&cwd) {
        Ok(targets) => targets,
        Err(error) => return super::structured_error(error.json(), 1),
    };
    let paths = (target_path(&global_dir), target_path(&project_dir));

    let mut errors: Vec<String> = Vec::new();
    let global_outcome = if global_flag || !project_flag {
        match install_at(&global_dir, force) {
            Ok(outcome) => outcome,
            Err(error) => {
                errors.push(format!("global: {error}"));
                InstallOutcome::default()
            }
        }
    } else {
        InstallOutcome::default()
    };
    let project_outcome = if project_flag || !global_flag {
        match install_at(&project_dir, force) {
            Ok(outcome) => outcome,
            Err(error) => {
                errors.push(format!("project: {error}"));
                InstallOutcome::default()
            }
        }
    } else {
        InstallOutcome::default()
    };

    // Errors are surfaced inline so the operator can see them without losing
    // the per-scope outcome; install never silently drops one.
    let mut envelope = InstallEnvelope::scoped(global_outcome, project_outcome, paths);
    envelope.errors.extend(errors);
    super::print_json(&envelope)
}

/// `plugin install --path DIR`: one explicit directory, no scope merge.
fn install_explicit(dir: &Path, force: bool) -> i32 {
    let target = dir.join(PLUGIN_FILENAME);
    super::print_json(&match install_at(dir, force) {
        Ok(outcome) => InstallEnvelope::explicit(&target, outcome),
        Err(error) => {
            let mut envelope = InstallEnvelope::explicit(&target, InstallOutcome::default());
            envelope.errors.push(error.to_string());
            envelope
        }
    })
}

fn execute_status(path: Option<String>) -> i32 {
    if let Some(dir) = path {
        return super::print_json(&explicit_status_envelope(Path::new(&dir)));
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match status_at(&cwd) {
        Ok(report) => super::print_json(&StatusEnvelope::from(report)),
        Err(error) => super::structured_error(error.json(), 1),
    }
}

fn execute_uninstall(global_flag: bool, project_flag: bool, path: Option<String>) -> i32 {
    if let Some(dir) = path {
        return uninstall_explicit(&PathBuf::from(dir));
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (global_dir, project_dir) = match resolve_targets(&cwd) {
        Ok(targets) => targets,
        Err(error) => return super::structured_error(error.json(), 1),
    };
    let paths = (target_path(&global_dir), target_path(&project_dir));

    let mut errors: Vec<String> = Vec::new();
    let global_outcome = if global_flag || !project_flag {
        match uninstall_at(&global_dir) {
            Ok(outcome) => outcome,
            Err(error) => {
                errors.push(format!("global: {error}"));
                UninstallOutcome::default()
            }
        }
    } else {
        UninstallOutcome::default()
    };
    let project_outcome = if project_flag || !global_flag {
        match uninstall_at(&project_dir) {
            Ok(outcome) => outcome,
            Err(error) => {
                errors.push(format!("project: {error}"));
                UninstallOutcome::default()
            }
        }
    } else {
        UninstallOutcome::default()
    };

    let mut envelope = UninstallEnvelope::scoped(global_outcome, project_outcome, paths);
    envelope.errors.extend(errors);
    super::print_json(&envelope)
}

/// `plugin uninstall --path DIR`: one explicit directory, no scope merge.
fn uninstall_explicit(dir: &Path) -> i32 {
    let target = dir.join(PLUGIN_FILENAME);
    super::print_json(&match uninstall_at(dir) {
        Ok(outcome) => UninstallEnvelope::explicit(&target, outcome),
        Err(error) => {
            let mut envelope = UninstallEnvelope::explicit(&target, UninstallOutcome::default());
            envelope.errors.push(error.to_string());
            envelope
        }
    })
}

fn target_path(dir: &Path) -> String {
    dir.join(PLUGIN_FILENAME).to_string_lossy().to_string()
}

fn resolve_targets(cwd: &std::path::Path) -> Result<(PathBuf, PathBuf), PluginError> {
    let global_dir = resolve_global_dir()?;
    let project_dir = resolve_project_dir(cwd);
    Ok((global_dir, project_dir))
}
