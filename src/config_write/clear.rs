use crate::infra::storage::Storage;
use crate::policy::Role;
use serde_json::Value;

use super::{ConfigClearOutcome, is_role_scoped_setting};

/// Clear a setting. For global settings the row is deleted;
/// for role-scoped the field is nulled. Returns whether a
/// value was actually removed.
pub fn clear_setting(
    role: Option<Role>,
    canonical: &str,
    storage: &Storage,
) -> Result<Value, String> {
    if is_role_scoped_setting(canonical) && role.is_none() {
        return Err(format!("--role is required for setting '{canonical}'"));
    }
    let cleared = persist_clear_value(role, canonical, storage)?;
    let outcome = ConfigClearOutcome {
        setting: canonical.to_owned(),
        role: role.map(|r| r.as_str().to_owned()),
        cleared,
    };
    serde_json::to_value(outcome).map_err(|e| format!("could not encode clear outcome: {e}"))
}

fn persist_clear_value(
    role: Option<Role>,
    canonical: &str,
    storage: &Storage,
) -> Result<bool, String> {
    match canonical {
        "PHASEGENT_REDMINE_GIT_MIRROR_API_KEY"
        | "PHASEGENT_REDMINE_REPOSITORY_URL"
        | "PHASEGENT_DEFAULT_PROVIDER" => {
            let cleared = storage.delete_global_setting(canonical)?;
            Ok(cleared)
        }
        "PHASEGENT_PROVIDER" => {
            let role = role.expect("role required");
            let current = storage.load_role_config(role)?;
            if current
                .as_ref()
                .and_then(|c| c.provider.as_deref())
                .is_none()
            {
                Ok(false)
            } else {
                let mut cfg = current.unwrap_or_default();
                cfg.provider = None;
                storage.save_role_config(role, &cfg)?;
                Ok(true)
            }
        }
        "PHASEGENT_API_BASE" => {
            let role = role.expect("role required");
            let mut cleared = false;
            if let Some(mut cfg) = storage.load_role_config(role)? {
                if cfg.api_base.is_some() {
                    cfg.api_base = None;
                    storage.save_role_config(role, &cfg)?;
                    cleared = true;
                }
            }
            if let Some(mut cfg) = storage.load_redmine_config(role)? {
                if cfg.api_base.is_some() {
                    cfg.api_base = None;
                    storage.save_redmine_config(role, &cfg)?;
                    cleared = true;
                }
            }
            if let Some(mut cfg) = storage.load_gitlab_config(role)? {
                if cfg.api_base.is_some() {
                    cfg.api_base = None;
                    storage.save_gitlab_config(role, &cfg)?;
                    cleared = true;
                }
            }
            Ok(cleared)
        }
        "PHASEGENT_REPOSITORY" => {
            let role = role.expect("role required");
            let current = storage.load_role_config(role)?;
            if current
                .as_ref()
                .and_then(|c| c.repository.as_deref())
                .is_none()
            {
                Ok(false)
            } else {
                let mut cfg = current.unwrap_or_default();
                cfg.repository = None;
                storage.save_role_config(role, &cfg)?;
                Ok(true)
            }
        }
        "PHASEGENT_REDMINE_API_BASE" => {
            let role = role.expect("role required");
            let current = storage.load_redmine_config(role)?;
            if current
                .as_ref()
                .and_then(|c| c.api_base.as_deref())
                .is_none()
            {
                Ok(false)
            } else {
                let mut cfg = current.unwrap_or_default();
                cfg.api_base = None;
                storage.save_redmine_config(role, &cfg)?;
                Ok(true)
            }
        }
        "PHASEGENT_REDMINE_PROJECT_ID" => {
            let role = role.expect("role required");
            let current = storage.load_redmine_config(role)?;
            if current
                .as_ref()
                .and_then(|c| c.project_id.as_deref())
                .is_none()
            {
                Ok(false)
            } else {
                let mut cfg = current.unwrap_or_default();
                cfg.project_id = None;
                storage.save_redmine_config(role, &cfg)?;
                Ok(true)
            }
        }
        "PHASEGENT_REDMINE_CLOSE_STATUS_ID" => {
            let role = role.expect("role required");
            let current = storage.load_redmine_config(role)?;
            if current.as_ref().and_then(|c| c.close_status_id).is_none() {
                Ok(false)
            } else {
                let mut cfg = current.unwrap_or_default();
                cfg.close_status_id = None;
                storage.save_redmine_config(role, &cfg)?;
                Ok(true)
            }
        }
        "PHASEGENT_GITLAB_API_BASE" => {
            let role = role.expect("role required");
            let current = storage.load_gitlab_config(role)?;
            if current
                .as_ref()
                .and_then(|c| c.api_base.as_deref())
                .is_none()
            {
                Ok(false)
            } else {
                let mut cfg = current.unwrap_or_default();
                cfg.api_base = None;
                storage.save_gitlab_config(role, &cfg)?;
                Ok(true)
            }
        }
        "PHASEGENT_GITLAB_PROJECT_ID" => {
            let role = role.expect("role required");
            let current = storage.load_gitlab_config(role)?;
            if current.as_ref().and_then(|c| c.project_id).is_none() {
                Ok(false)
            } else {
                let mut cfg = current.unwrap_or_default();
                cfg.project_id = None;
                storage.save_gitlab_config(role, &cfg)?;
                Ok(true)
            }
        }
        "PHASEGENT_PROJECT_ID" => {
            let role = role.expect("role required");
            let mut cleared = false;
            if let Some(mut cfg) = storage.load_redmine_config(role)? {
                if cfg.project_id.is_some() {
                    cfg.project_id = None;
                    storage.save_redmine_config(role, &cfg)?;
                    cleared = true;
                }
            }
            if let Some(mut cfg) = storage.load_gitlab_config(role)? {
                if cfg.project_id.is_some() {
                    cfg.project_id = None;
                    storage.save_gitlab_config(role, &cfg)?;
                    cleared = true;
                }
            }
            Ok(cleared)
        }
        "PHASEGENT_CLOSE_STATUS_ID" => {
            let role = role.expect("role required");
            let current = storage.load_redmine_config(role)?;
            if current.as_ref().and_then(|c| c.close_status_id).is_none() {
                Ok(false)
            } else {
                let mut cfg = current.unwrap_or_default();
                cfg.close_status_id = None;
                storage.save_redmine_config(role, &cfg)?;
                Ok(true)
            }
        }
        _ => Err(format!("unknown setting '{canonical}'")),
    }
}
