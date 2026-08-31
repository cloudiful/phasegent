use crate::auth::{GitlabStoredConfig, RedmineStoredConfig, StoredConfig};
use crate::infra::storage::Storage;
use crate::policy::Role;

pub(crate) fn update_role_config_field(
    storage: &Storage,
    role: Role,
    mutate: impl FnOnce(&mut StoredConfig),
) -> Result<(), String> {
    let mut config = storage.load_role_config(role)?.unwrap_or_default();
    mutate(&mut config);
    storage.save_role_config(role, &config)
}

pub(crate) fn update_redmine_config_field(
    storage: &Storage,
    role: Role,
    mutate: impl FnOnce(&mut RedmineStoredConfig),
) -> Result<(), String> {
    let mut config = storage.load_redmine_config(role)?.unwrap_or_default();
    mutate(&mut config);
    storage.save_redmine_config(role, &config)
}

pub(crate) fn update_gitlab_config_field(
    storage: &Storage,
    role: Role,
    mutate: impl FnOnce(&mut GitlabStoredConfig),
) -> Result<(), String> {
    let mut config = storage.load_gitlab_config(role)?.unwrap_or_default();
    mutate(&mut config);
    storage.save_gitlab_config(role, &config)
}
