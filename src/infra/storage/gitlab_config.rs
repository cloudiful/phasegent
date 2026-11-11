use rusqlite::{OptionalExtension, params};

use super::Storage;
use crate::policy::Role;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GitlabStoredConfig {
    pub api_base: String,
    pub repository: Option<String>,
    pub source: Option<String>,
    pub updated_at: Option<i64>,
}

impl Storage {
    pub(crate) fn load_gitlab_config(
        &self,
        role: Role,
    ) -> Result<Option<GitlabStoredConfig>, String> {
        self.connection
            .query_row(
                "SELECT api_base, repository, source, updated_at \
                   FROM role_gitlab_config WHERE role = ?1",
                [role.as_str()],
                |row| {
                    Ok(GitlabStoredConfig {
                        api_base: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        repository: row.get(1)?,
                        source: row.get(2)?,
                        updated_at: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|error| format!("could not read GitLab config: {error}"))
    }

    pub(crate) fn save_gitlab_auth(
        &self,
        role: Role,
        api_base: &str,
        repository: &str,
        credential: &str,
    ) -> Result<(), String> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "system time precedes Unix epoch".to_owned())?
            .as_secs() as i64;
        let fingerprint = super::credentials::credential_fingerprint(credential);
        let tx = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin GitLab auth update: {error}"))?;
        tx.execute(
            "INSERT INTO role_gitlab_config (role, api_base, repository, source, updated_at) \
             VALUES (?1, ?2, ?3, 'admin-auth-setup', ?4) \
             ON CONFLICT(role) DO UPDATE SET api_base = excluded.api_base, \
                 repository = excluded.repository, source = excluded.source, \
                 updated_at = excluded.updated_at",
            params![role.as_str(), api_base, repository, now],
        )
        .map_err(|error| format!("could not write GitLab config: {error}"))?;
        tx.execute(
            "INSERT INTO role_credential (role, provider, credential, fingerprint, credential_updated_at) \
             VALUES (?1, 'gitlab', ?2, ?3, ?4) \
             ON CONFLICT(role, provider) DO UPDATE SET credential = excluded.credential, \
                 fingerprint = excluded.fingerprint, \
                 credential_updated_at = excluded.credential_updated_at",
            params![role.as_str(), credential, fingerprint, now],
        )
        .map_err(|error| format!("could not store GitLab credential: {error}"))?;
        tx.commit()
            .map_err(|error| format!("could not commit GitLab auth update: {error}"))
    }
}
