//! Executor for `phasegent skill install` (issue #22 P2).
//!
//! Installation is local and role-free, like `plugin install`. The bundled
//! `phasegent` directory is written into the selected skills root; no status or
//! uninstall surface exists, so a user's pre-existing skills are never removed.

use std::path::PathBuf;

use serde::Serialize;

use crate::command::SkillCommand;
use crate::skill_install::{
    SKILL_DIR_NAME, SkillInstallOutcome, install_at, resolve_global_skills_root,
    resolve_project_skills_root,
};

#[derive(Debug, Serialize)]
pub struct SkillInstallEnvelope {
    pub installed: Vec<String>,
    pub updated: Vec<String>,
    pub skipped: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub target_path: String,
}

pub(crate) fn execute_skill(command: SkillCommand) -> i32 {
    match command {
        SkillCommand::Install {
            project,
            force,
            path,
        } => execute_install(project, force, path),
    }
}

fn execute_install(project: bool, force: bool, path: Option<String>) -> i32 {
    let root = match path {
        Some(path) => PathBuf::from(path),
        None if project => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            resolve_project_skills_root(&cwd)
        }
        None => match resolve_global_skills_root() {
            Ok(root) => root,
            Err(error) => return super::structured_error(error.json(), 1),
        },
    };
    let target = root.join(SKILL_DIR_NAME);
    let mut envelope = SkillInstallEnvelope {
        installed: Vec::new(),
        updated: Vec::new(),
        skipped: Vec::new(),
        warnings: Vec::new(),
        errors: Vec::new(),
        target_path: target.to_string_lossy().to_string(),
    };
    match install_at(&root, force) {
        Ok(SkillInstallOutcome {
            installed,
            updated,
            skipped,
            warnings,
        }) => {
            envelope.installed = installed;
            envelope.updated = updated;
            envelope.skipped = skipped;
            envelope.warnings = warnings;
        }
        Err(error) => envelope.errors.push(error.to_string()),
    }
    super::print_json(&envelope)
}
