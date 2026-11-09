//! Executor for the `plugin` command group (issue #239 Phase 3; issue #22 P2
//! scope cleanup).
//!
//! Subcommand routing is mirror-shaped with the hooks install surface: no
//! role is required, no provider is touched, no network is involved. The
//! executor funnels the subcommands through the same `install_at` /
//! `uninstall_at` helpers in [`crate::plugin`] so the CLI layer stays purely
//! presentational.
//!
//! The default target is the global plugin directory. `--project` selects the
//! project directory, and `--path DIR` replaces the project selector with one
//! explicit plugin directory. `--global` was removed.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::command::PluginCommand;
use crate::plugin::{
    InstallOutcome, PLUGIN_FILENAME, PluginError, PluginTargetStatus, UninstallOutcome,
    inspect_target, install_at, resolve_global_dir, resolve_project_dir, status_global,
    status_project, uninstall_at,
};

/// JSON envelope returned by `plugin install`. `target_path` names the selected
/// plugin file; the scope-specific path fields are retained for compatibility.
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
    fn filled(outcome: &InstallOutcome) -> Self {
        Self {
            installed: outcome.installed.clone(),
            updated: outcome.updated.clone(),
            skipped: outcome.skipped.clone(),
            warnings: outcome.warnings.clone(),
            errors: outcome.errors.clone(),
            global_path: None,
            project_path: None,
            target_path: None,
        }
    }

    fn with_global(mut self, path: &Path) -> Self {
        self.global_path = Some(path.to_string_lossy().to_string());
        self
    }

    fn with_project(mut self, path: &Path) -> Self {
        self.project_path = Some(path.to_string_lossy().to_string());
        self
    }

    fn with_target(mut self, path: &Path) -> Self {
        self.target_path = Some(path.to_string_lossy().to_string());
        self
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
    fn filled(outcome: &UninstallOutcome) -> Self {
        Self {
            removed: outcome.removed.clone(),
            warnings: outcome.warnings.clone(),
            errors: outcome.errors.clone(),
            global_path: None,
            project_path: None,
            target_path: None,
        }
    }

    fn with_global(mut self, path: &Path) -> Self {
        self.global_path = Some(path.to_string_lossy().to_string());
        self
    }

    fn with_project(mut self, path: &Path) -> Self {
        self.project_path = Some(path.to_string_lossy().to_string());
        self
    }

    fn with_target(mut self, path: &Path) -> Self {
        self.target_path = Some(path.to_string_lossy().to_string());
        self
    }
}

/// JSON envelope returned by `plugin status`. Exactly one slot is set: the
/// global target by default, the project target with `--project`, or the
/// explicit `target` with `--path`. The inner `PluginTargetStatus` shape is
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

pub(crate) fn execute_plugin(command: PluginCommand) -> i32 {
    match command {
        PluginCommand::Install {
            project,
            force,
            path,
        } => execute_install(project, force, path),
        PluginCommand::Status { project, path } => execute_status(project, path),
        PluginCommand::Uninstall { project, path } => execute_uninstall(project, path),
    }
}

fn execute_install(project: bool, force: bool, path: Option<String>) -> i32 {
    if let Some(dir) = path {
        return install_explicit(&PathBuf::from(dir), force);
    }
    let dir = match selected_dir(project) {
        Ok(dir) => dir,
        Err(error) => return super::structured_error(error.json(), 1),
    };
    let envelope = match install_at(&dir, force) {
        Ok(outcome) => InstallEnvelope::filled(&outcome),
        Err(error) => {
            let mut envelope = InstallEnvelope::filled(&InstallOutcome::default());
            envelope.errors.push(error.to_string());
            envelope
        }
    };
    let envelope = if project {
        envelope.with_project(&dir.join(PLUGIN_FILENAME))
    } else {
        envelope.with_global(&dir.join(PLUGIN_FILENAME))
    };
    super::print_json(&envelope)
}

/// `plugin install --path DIR`: one explicit directory, no project selector.
fn install_explicit(dir: &Path, force: bool) -> i32 {
    let target = dir.join(PLUGIN_FILENAME);
    let envelope = match install_at(dir, force) {
        Ok(outcome) => InstallEnvelope::filled(&outcome),
        Err(error) => {
            let mut envelope = InstallEnvelope::filled(&InstallOutcome::default());
            envelope.errors.push(error.to_string());
            envelope
        }
    };
    super::print_json(&envelope.with_target(&target))
}

fn execute_status(project: bool, path: Option<String>) -> i32 {
    if let Some(dir) = path {
        return super::print_json(&StatusEnvelope {
            global: None,
            project: None,
            target: Some(inspect_target(Path::new(&dir))),
        });
    }
    if project {
        let cwd = current_dir();
        return super::print_json(&StatusEnvelope {
            global: None,
            project: Some(status_project(&cwd)),
            target: None,
        });
    }
    match status_global() {
        Ok(status) => super::print_json(&StatusEnvelope {
            global: Some(status),
            project: None,
            target: None,
        }),
        Err(error) => super::structured_error(error.json(), 1),
    }
}

fn execute_uninstall(project: bool, path: Option<String>) -> i32 {
    if let Some(dir) = path {
        return uninstall_explicit(&PathBuf::from(dir));
    }
    let dir = match selected_dir(project) {
        Ok(dir) => dir,
        Err(error) => return super::structured_error(error.json(), 1),
    };
    let envelope = match uninstall_at(&dir) {
        Ok(outcome) => UninstallEnvelope::filled(&outcome),
        Err(error) => {
            let mut envelope = UninstallEnvelope::filled(&UninstallOutcome::default());
            envelope.errors.push(error.to_string());
            envelope
        }
    };
    let envelope = if project {
        envelope.with_project(&dir.join(PLUGIN_FILENAME))
    } else {
        envelope.with_global(&dir.join(PLUGIN_FILENAME))
    };
    super::print_json(&envelope)
}

/// `plugin uninstall --path DIR`: one explicit directory, no project selector.
fn uninstall_explicit(dir: &Path) -> i32 {
    let target = dir.join(PLUGIN_FILENAME);
    let envelope = match uninstall_at(dir) {
        Ok(outcome) => UninstallEnvelope::filled(&outcome),
        Err(error) => {
            let mut envelope = UninstallEnvelope::filled(&UninstallOutcome::default());
            envelope.errors.push(error.to_string());
            envelope
        }
    };
    super::print_json(&envelope.with_target(&target))
}

fn selected_dir(project: bool) -> Result<PathBuf, PluginError> {
    if project {
        Ok(resolve_project_dir(&current_dir()))
    } else {
        resolve_global_dir()
    }
}

fn current_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}
