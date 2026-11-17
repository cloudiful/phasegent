use crate::auth::RedmineStoredConfig;
use crate::infra::config_file::runtime;
use crate::policy::Role;

pub(crate) fn update_redmine_config_field(
    role: Role,
    mutate: impl FnOnce(&mut RedmineStoredConfig),
) -> Result<(), String> {
    let mut config = runtime::load_redmine_config(role)?.unwrap_or_default();
    mutate(&mut config);
    runtime::save_redmine_config(role, &config)
}
