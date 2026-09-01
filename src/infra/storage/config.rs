use super::Storage;
use crate::auth::{GitlabStoredConfig, RedmineStoredConfig, StoredConfig};
#[cfg(test)]
use crate::infra::storage_schema::PROVIDER_GITLAB;
use crate::infra::storage_schema::PROVIDER_REDMINE;
use crate::policy::Role;
use rusqlite::{OptionalExtension, params};

impl Storage {
    /// Load the role-level configuration (provider preference plus the
    /// Forgejo api_base/repository). Returns `None` when no row exists
    /// for `role` so callers can distinguish "never written" from
    /// "written with all fields null".
    pub fn load_role_config(&self, role: Role) -> Result<Option<StoredConfig>, String> {
        let mut statement = self
            .connection
            .prepare("SELECT provider, api_base, repository FROM role_config WHERE role = ?1")
            .map_err(|error| format!("could not prepare role config load: {error}"))?;
        let value = statement
            .query_row(params![role.as_str()], |row| {
                Ok(StoredConfig {
                    provider: row.get(0)?,
                    api_base: row.get(1)?,
                    repository: row.get(2)?,
                })
            })
            .optional()
            .map_err(|error| format!("could not read role config: {error}"))?;
        Ok(value)
    }

    /// Upsert the role-level configuration. `None` fields are stored
    /// as SQL `NULL`; existing non-null values are overwritten.
    pub fn save_role_config(&self, role: Role, config: &StoredConfig) -> Result<(), String> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin role config write: {error}"))?;
        transaction
            .execute(
                "INSERT INTO role_config (role, provider, api_base, repository) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(role) DO UPDATE SET \
                    provider = excluded.provider, \
                    api_base = excluded.api_base, \
                    repository = excluded.repository",
                params![
                    role.as_str(),
                    config.provider,
                    config.api_base,
                    config.repository,
                ],
            )
            .map_err(|error| format!("could not write role config: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit role config write: {error}"))?;
        Ok(())
    }

    /// Set only the `provider` column on `role_config` without
    /// touching `api_base` or `repository`. Used by `auth setup` when
    /// the caller switches providers without re-supplying the URL.
    pub fn update_provider(&self, role: Role, provider: &str) -> Result<(), String> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin provider update: {error}"))?;
        transaction
            .execute(
                "INSERT INTO role_config (role, provider, api_base, repository) \
                 VALUES (?1, ?2, NULL, NULL) \
                 ON CONFLICT(role) DO UPDATE SET provider = excluded.provider",
                params![role.as_str(), provider],
            )
            .map_err(|error| format!("could not update provider: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit provider update: {error}"))?;
        Ok(())
    }

    /// Load the Redmine-specific configuration for `role`. Mirrors the
    /// semantics of [`load_role_config`].
    /// Project-id was removed in Phase 1; stored values are ignored
    /// (always returned as `None`) and the column is lazily cleared
    /// via the connection migration. `group_name`/`group_role` remain
    /// legacy-decodable but are never read.
    pub fn load_redmine_config(&self, role: Role) -> Result<Option<RedmineStoredConfig>, String> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT api_base, close_status_id \
                 FROM role_redmine_config WHERE role = ?1",
            )
            .map_err(|error| format!("could not prepare redmine config load: {error}"))?;
        let value = statement
            .query_row(params![role.as_str()], |row| {
                Ok(RedmineStoredConfig {
                    api_base: row.get(0)?,
                    project_id: None,
                    close_status_id: row.get(1)?,
                    group_name: None,
                    group_role: None,
                })
            })
            .optional()
            .map_err(|error| format!("could not read redmine config: {error}"))?;
        // Backwards-compat: old databases may still contain a
        // `project_id` column with legacy values. The migration in
        // `connection.rs` clears them, but we also tolerate the column
        // being present by ignoring its content above. If the table
        // somehow still has non-NULL project_id, the resolver never
        // sees it because we return `None`.
        Ok(value)
    }

    /// Upsert the Redmine-specific configuration.
    /// Project-id is no longer persisted; the column is left untouched
    /// for non-destructive migration and `load` always returns `None`.
    pub fn save_redmine_config(
        &self,
        role: Role,
        config: &RedmineStoredConfig,
    ) -> Result<(), String> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin redmine config write: {error}"))?;
        transaction
            .execute(
                "INSERT INTO role_redmine_config (role, api_base, close_status_id) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(role) DO UPDATE SET \
                    api_base = excluded.api_base, \
                    close_status_id = excluded.close_status_id",
                params![role.as_str(), config.api_base, config.close_status_id,],
            )
            .map_err(|error| format!("could not write redmine config: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit redmine config write: {error}"))?;
        Ok(())
    }

    /// Set only the bootstrap identity (api_base, close_status_id)
    /// without disturbing an existing provider preference on
    /// `role_config`. The `project_id` argument is retained for
    /// backward-compatible call sites but is ignored: Phase 1 no longer
    /// persists the project id. The provider preference is still
    /// updated to `redmine` so later phases can rely on it.
    pub fn persist_redmine_bootstrap(
        &self,
        role: Role,
        api_base: Option<String>,
        project_id: u64,
        close_status_id: u64,
    ) -> Result<(), String> {
        if project_id == 0 || close_status_id == 0 {
            return Err(
                "Redmine project and close status IDs must be greater than zero".to_owned(),
            );
        }
        let mut config = self.load_redmine_config(role)?.unwrap_or_default();
        if api_base.is_some() {
            config.api_base = api_base;
        }
        // project_id deliberately ignored — stored value is always None
        // and legacy rows are cleared by the migration.
        let _ = project_id;
        config.close_status_id = Some(close_status_id);
        self.save_redmine_config(role, &config)?;
        self.update_provider(role, PROVIDER_REDMINE)
    }

    /// Load the GitLab-specific configuration for `role`. Mirrors the
    /// Redmine helper except the persisted `project_id` is a numeric
    /// GitLab identifier, not a free-text slug. Phase 1 makes the
    /// project id inert: stored values are ignored and always returned
    /// as `None`.
    pub fn load_gitlab_config(&self, role: Role) -> Result<Option<GitlabStoredConfig>, String> {
        let mut statement = self
            .connection
            .prepare("SELECT api_base FROM role_gitlab_config WHERE role = ?1")
            .map_err(|error| format!("could not prepare gitlab config load: {error}"))?;
        let value = statement
            .query_row(params![role.as_str()], |row| {
                Ok(GitlabStoredConfig {
                    api_base: row.get(0)?,
                    project_id: None,
                })
            })
            .optional()
            .map_err(|error| format!("could not read gitlab config: {error}"))?;
        Ok(value)
    }

    /// Upsert the GitLab-specific configuration. The numeric project id
    /// is stored as `INTEGER` so the column never holds a placeholder
    /// string that callers might confuse with a Redmine slug.
    /// Phase 1 no longer persists `project_id`; the column is left
    /// untouched and `load` always returns `None`.
    pub fn save_gitlab_config(
        &self,
        role: Role,
        config: &GitlabStoredConfig,
    ) -> Result<(), String> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin gitlab config write: {error}"))?;
        transaction
            .execute(
                "INSERT INTO role_gitlab_config (role, api_base) \
                 VALUES (?1, ?2) \
                 ON CONFLICT(role) DO UPDATE SET \
                    api_base = excluded.api_base",
                params![role.as_str(), config.api_base,],
            )
            .map_err(|error| format!("could not write gitlab config: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit gitlab config write: {error}"))?;
        Ok(())
    }

    /// Persist the bootstrap identity (`api_base`) without disturbing an
    /// existing provider preference on `role_config`. The `project_id`
    /// argument is retained for backward-compatible call sites but is
    /// ignored: Phase 1 no longer persists the numeric project id.
    /// The provider preference is still flipped to "gitlab" so the
    /// resolver doesn't drift back to the default Forgejo path.
    #[cfg(test)]
    pub fn persist_gitlab_bootstrap(
        &self,
        role: Role,
        api_base: Option<String>,
        project_id: u64,
    ) -> Result<(), String> {
        if project_id == 0 {
            return Err("GitLab project id must be greater than zero".to_owned());
        }
        let mut config = self.load_gitlab_config(role)?.unwrap_or_default();
        if api_base.is_some() {
            config.api_base = api_base;
        }
        let _ = project_id;
        self.save_gitlab_config(role, &config)?;
        self.update_provider(role, PROVIDER_GITLAB)
    }
}
