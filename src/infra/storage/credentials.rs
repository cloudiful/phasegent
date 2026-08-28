use super::Storage;
use crate::policy::Role;
use rusqlite::{OptionalExtension, params};

impl Storage {
    /// Read the credential stored for `(role, provider)`. Returns
    /// `Ok(None)` when no credential exists so the caller can prompt
    /// the operator instead of failing on a missing row.
    pub fn load_credential(&self, role: Role, provider: &str) -> Result<Option<String>, String> {
        let mut statement = self
            .connection
            .prepare("SELECT credential FROM role_credential WHERE role = ?1 AND provider = ?2")
            .map_err(|error| format!("could not prepare credential load: {error}"))?;
        let value = statement
            .query_row(params![role.as_str(), provider], |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .map_err(|error| format!("could not read credential: {error}"))?;
        Ok(value)
    }

    /// Store the credential for `(role, provider)`, overwriting any
    /// existing value. The credential is stored verbatim and never
    /// surfaced in errors; callers are responsible for trimming and
    /// rejecting empty input before invoking this method.
    pub fn save_credential(
        &self,
        role: Role,
        provider: &str,
        credential: &str,
    ) -> Result<(), String> {
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin credential write: {error}"))?;
        transaction
            .execute(
                "INSERT INTO role_credential (role, provider, credential) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT(role, provider) DO UPDATE SET credential = excluded.credential",
                params![role.as_str(), provider, credential],
            )
            .map_err(|error| format!("could not write credential: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit credential write: {error}"))?;
        Ok(())
    }

    /// Wipe the credential for `(role, provider)`. Used by tests and
    /// not currently called from production code; left in the public
    /// surface so the storage layer stays self-contained.
    #[allow(dead_code)]
    pub fn delete_credential(&self, role: Role, provider: &str) -> Result<(), String> {
        self.connection
            .execute(
                "DELETE FROM role_credential WHERE role = ?1 AND provider = ?2",
                params![role.as_str(), provider],
            )
            .map_err(|error| format!("could not delete credential: {error}"))?;
        Ok(())
    }

    /// Describe the credential stored for `(role, provider)` without
    /// surfacing the value itself. Reports presence and length so
    /// `config show` can render a redacted snapshot of every role.
    pub fn credential_summary(&self, role: Role, provider: &str) -> Result<(bool, usize), String> {
        match self.load_credential(role, provider)? {
            Some(value) => Ok((true, value.chars().count())),
            None => Ok((false, 0)),
        }
    }
}
