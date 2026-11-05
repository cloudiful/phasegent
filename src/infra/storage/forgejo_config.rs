use rusqlite::{OptionalExtension, params};

use super::Storage;
use crate::policy::Role;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ForgejoStoredConfig {
    pub api_base: String,
    pub repository: String,
    pub source: String,
    pub updated_at: i64,
}

impl Storage {
    pub(crate) fn load_forgejo_config(
        &self,
        role: Role,
    ) -> Result<Option<ForgejoStoredConfig>, String> {
        self.connection.query_row("SELECT api_base, repository, source, updated_at FROM role_forgejo_config WHERE role = ?1", [role.as_str()], |row| Ok(ForgejoStoredConfig { api_base: row.get(0)?, repository: row.get(1)?, source: row.get(2)?, updated_at: row.get(3)? }))
            .optional().map_err(|error| format!("could not read Forgejo config: {error}"))
    }

    pub(crate) fn save_forgejo_auth(
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
            .map_err(|error| format!("could not begin Forgejo auth update: {error}"))?;
        tx.execute("INSERT INTO role_forgejo_config (role, api_base, repository, source, updated_at) VALUES (?1, ?2, ?3, 'admin-auth-setup', ?4) ON CONFLICT(role) DO UPDATE SET api_base = excluded.api_base, repository = excluded.repository, source = excluded.source, updated_at = excluded.updated_at", params![role.as_str(), api_base, repository, now]).map_err(|error| format!("could not write Forgejo config: {error}"))?;
        tx.execute("INSERT INTO role_credential (role, provider, credential, fingerprint, credential_updated_at) VALUES (?1, 'forgejo', ?2, ?3, ?4) ON CONFLICT(role, provider) DO UPDATE SET credential = excluded.credential, fingerprint = excluded.fingerprint, credential_updated_at = excluded.credential_updated_at", params![role.as_str(), credential, fingerprint, now]).map_err(|error| format!("could not store Forgejo credential: {error}"))?;
        tx.commit()
            .map_err(|error| format!("could not commit Forgejo auth update: {error}"))
    }
}
