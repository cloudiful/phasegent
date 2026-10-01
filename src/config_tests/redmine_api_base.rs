//! Regression coverage for the canonical global Redmine REST API base
//! (`redmine_api_base`): global persistence/snapshot plumbing, runtime
//! precedence, and the bounded legacy role-address migration. Credential
//! and identity isolation is asserted alongside so the address
//! consolidation can never silently touch secrets.
//!
//! The scenarios are split across adjacent submodules by surface
//! (`global_setting`, `resolution`, `migration`, `auth_setup`) so no single
//! file carries the whole matrix.

const REDMINE_ENV_VARS: &[&str] = &["PHASEGENT_REDMINE_API_BASE", "PHASEGENT_API_BASE"];

fn redmine_config_with_base(api_base: Option<&str>) -> crate::auth::RedmineStoredConfig {
    crate::auth::RedmineStoredConfig {
        api_base: api_base.map(str::to_owned),
        project_id: None,
        close_status_id: None,
        group_name: None,
        group_role: None,
    }
}

mod auth_setup;
mod global_setting;
mod migration;
mod resolution;
