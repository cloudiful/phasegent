use super::Storage;
use crate::policy::Role;
use rusqlite::{OptionalExtension, params};
use std::time::{SystemTime, UNIX_EPOCH};

/// Non-secret identity of a stored credential: presence, length, the
/// last-4-characters fingerprint, and the epoch second it was stored.
/// The fingerprint deliberately reveals at most 4 characters (and
/// nothing for secrets shorter than 4) so `config show` and `doctor`
/// can answer "which key is this" without loading the secret for
/// display. Short secrets yield `fingerprint: None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialIdentity {
    pub present: bool,
    pub length: usize,
    pub fingerprint: Option<String>,
    pub updated_at: Option<i64>,
}

/// Last-4-characters fingerprint of a secret. Returns `None` for
/// secrets shorter than 4 characters rather than echoing most of
/// the secret back.
pub fn credential_fingerprint(secret: &str) -> Option<String> {
    let chars: Vec<char> = secret.chars().collect();
    if chars.len() < 4 {
        return None;
    }
    Some(chars[chars.len() - 4..].iter().collect())
}

fn now_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

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
    /// rejecting empty input before invoking this method. The
    /// non-secret fingerprint and store timestamp are maintained in
    /// the same write so display paths never need the secret.
    pub fn save_credential(
        &self,
        role: Role,
        provider: &str,
        credential: &str,
    ) -> Result<(), String> {
        let fingerprint = credential_fingerprint(credential);
        let updated_at = now_epoch_seconds();
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin credential write: {error}"))?;
        transaction
            .execute(
                "INSERT INTO role_credential (role, provider, credential, fingerprint, credential_updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(role, provider) DO UPDATE SET credential = excluded.credential, \
                     fingerprint = excluded.fingerprint, \
                     credential_updated_at = excluded.credential_updated_at",
                params![
                    role.as_str(),
                    provider,
                    credential,
                    fingerprint,
                    updated_at
                ],
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
    /// surfacing the value itself. Reports presence, length, the
    /// stored fingerprint, and the store timestamp so `config show`
    /// and `doctor` can render a redacted snapshot of every role.
    /// Rows written before the fingerprint columns existed are
    /// backfilled from the loaded value on first read; the value
    /// itself is never returned.
    pub fn credential_summary(
        &self,
        role: Role,
        provider: &str,
    ) -> Result<CredentialIdentity, String> {
        let row: Option<(String, Option<String>, Option<i64>)> = {
            let mut statement = self
                .connection
                .prepare(
                    "SELECT credential, fingerprint, credential_updated_at \
                     FROM role_credential WHERE role = ?1 AND provider = ?2",
                )
                .map_err(|error| format!("could not prepare credential summary: {error}"))?;
            statement
                .query_row(params![role.as_str(), provider], |row| {
                    Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
                })
                .optional()
                .map_err(|error| format!("could not read credential summary: {error}"))?
        };
        match row {
            None => Ok(CredentialIdentity {
                present: false,
                length: 0,
                fingerprint: None,
                updated_at: None,
            }),
            Some((value, fingerprint, updated_at)) => {
                let length = value.chars().count();
                let fingerprint = match fingerprint {
                    Some(existing) => Some(existing),
                    None => {
                        let computed = credential_fingerprint(&value);
                        if computed.is_some() {
                            self.connection
                                .execute(
                                    "UPDATE role_credential SET fingerprint = ?3 \
                                     WHERE role = ?1 AND provider = ?2",
                                    params![role.as_str(), provider, computed],
                                )
                                .map_err(|error| {
                                    format!("could not backfill credential fingerprint: {error}")
                                })?;
                        }
                        computed
                    }
                };
                Ok(CredentialIdentity {
                    present: true,
                    length,
                    fingerprint,
                    updated_at,
                })
            }
        }
    }
}
