use crate::auth::RedmineStoredConfig;
use crate::infra::storage::Storage;
use crate::policy::Role;

pub(crate) fn update_redmine_config_field(
    storage: &Storage,
    role: Role,
    mutate: impl FnOnce(&mut RedmineStoredConfig),
) -> Result<(), String> {
    let mut config = storage.load_redmine_config(role)?.unwrap_or_default();
    mutate(&mut config);
    storage.save_redmine_config(role, &config)
}
