//! Executor for the `plugin` command group (issue #239 Phase 3).
//!
//! Subcommand routing is mirror-shaped with the hooks install
//! surface: no role is required, no provider is touched, no network
//! is involved. The executor funnels the three subcommands through
//! the same `install_at` / `status_at` / `uninstall_at` helpers in
//! [`crate::plugin`] so the CLI layer stays purely presentational.

use std::path::PathBuf;

use serde::Serialize;

use crate::command::PluginCommand;
use crate::plugin::{
    InstallOutcome, PluginError, PluginTargetStatus, StatusReport, UninstallOutcome, install_at,
    resolve_global_dir, resolve_project_dir, status_at, uninstall_at,
};

/// JSON envelope returned by `plugin install`. Stable field order so
/// downstream tooling can replay the same shape across releases.
#[derive(Debug, Serialize)]
pub struct InstallEnvelope {
    pub installed: Vec<String>,
    pub updated: Vec<String>,
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub global_path: String,
    pub project_path: String,
}

impl From<(InstallOutcome, InstallOutcome, String, String)> for InstallEnvelope {
    fn from(
        (global, project, global_path, project_path): (
            InstallOutcome,
            InstallOutcome,
            String,
            String,
        ),
    ) -> Self {
        let mut installed = global.installed.clone();
        installed.extend(project.installed.clone());
        let mut updated = global.updated.clone();
        updated.extend(project.updated.clone());
        let mut skipped = global.skipped.clone();
        skipped.extend(project.skipped.clone());
        let mut warnings = global.warnings.clone();
        warnings.extend(project.warnings.clone());
        let mut errors = global.errors.clone();
        errors.extend(project.errors.clone());
        Self {
            installed,
            updated,
            skipped,
            warnings,
            errors,
            global_path,
            project_path,
        }
    }
}

/// JSON envelope returned by `plugin uninstall`.
#[derive(Debug, Serialize)]
pub struct UninstallEnvelope {
    pub removed: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub global_path: String,
    pub project_path: String,
}

impl From<(UninstallOutcome, UninstallOutcome, String, String)> for UninstallEnvelope {
    fn from(
        (global, project, global_path, project_path): (
            UninstallOutcome,
            UninstallOutcome,
            String,
            String,
        ),
    ) -> Self {
        let mut removed = global.removed.clone();
        removed.extend(project.removed.clone());
        let mut warnings = global.warnings.clone();
        warnings.extend(project.warnings.clone());
        let mut errors = global.errors.clone();
        errors.extend(project.errors.clone());
        Self {
            removed,
            warnings,
            errors,
            global_path,
            project_path,
        }
    }
}

/// JSON envelope returned by `plugin status`. The inner
/// `PluginTargetStatus` shape is stable: `{path, exists, managed,
/// size, mtime}`.
#[derive(Debug, Serialize)]
pub struct StatusEnvelope {
    pub global: PluginTargetStatus,
    pub project: PluginTargetStatus,
}

impl From<StatusReport> for StatusEnvelope {
    fn from(report: StatusReport) -> Self {
        Self {
            global: report.global,
            project: report.project,
        }
    }
}

pub(crate) fn execute_plugin(command: PluginCommand) -> i32 {
    match command {
        PluginCommand::Install {
            global,
            project,
            force,
        } => execute_install(global, project, force),
        PluginCommand::Status => execute_status(),
        PluginCommand::Uninstall { global, project } => execute_uninstall(global, project),
    }
}

fn execute_install(global_flag: bool, project_flag: bool, force: bool) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (global_dir, project_dir) = match resolve_targets(&cwd) {
        Ok(targets) => targets,
        Err(error) => return super::structured_error(error.json(), 1),
    };
    let global_path = global_dir
        .join(crate::plugin::PLUGIN_FILENAME)
        .to_string_lossy()
        .to_string();
    let project_path = project_dir
        .join(crate::plugin::PLUGIN_FILENAME)
        .to_string_lossy()
        .to_string();

    let run_global = global_flag || !project_flag;
    let run_project = project_flag || !global_flag;

    let mut errors: Vec<String> = Vec::new();
    let global_outcome = if run_global {
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
    let project_outcome = if run_project {
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

    let envelope = InstallEnvelope::from((
        global_outcome,
        project_outcome,
        global_path.clone(),
        project_path.clone(),
    ));
    // Errors are surfaced inline so the operator can see them
    // without losing the per-scope outcome. Best-effort: install
    // mirrors the `hooks install` philosophy of never silently
    // dropping an error.
    let mut merged = envelope;
    merged.errors.extend(errors);
    super::print_json(&merged)
}

fn execute_status() -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let envelope: StatusEnvelope = status_at(&cwd).into();
    super::print_json(&envelope)
}

fn execute_uninstall(global_flag: bool, project_flag: bool) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (global_dir, project_dir) = match resolve_targets(&cwd) {
        Ok(targets) => targets,
        Err(error) => return super::structured_error(error.json(), 1),
    };
    let global_path = global_dir
        .join(crate::plugin::PLUGIN_FILENAME)
        .to_string_lossy()
        .to_string();
    let project_path = project_dir
        .join(crate::plugin::PLUGIN_FILENAME)
        .to_string_lossy()
        .to_string();

    let run_global = global_flag || !project_flag;
    let run_project = project_flag || !global_flag;

    let mut errors: Vec<String> = Vec::new();
    let global_outcome = if run_global {
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
    let project_outcome = if run_project {
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

    let envelope = UninstallEnvelope::from((
        global_outcome,
        project_outcome,
        global_path.clone(),
        project_path.clone(),
    ));
    let mut merged = envelope;
    merged.errors.extend(errors);
    super::print_json(&merged)
}

fn resolve_targets(cwd: &std::path::Path) -> Result<(PathBuf, PathBuf), PluginError> {
    let global_dir = resolve_global_dir()?;
    let project_dir = resolve_project_dir(cwd);
    Ok((global_dir, project_dir))
}
