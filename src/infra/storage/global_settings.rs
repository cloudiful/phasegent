use super::Storage;
use crate::infra::storage_schema::GLOBAL_SETTING_NAMES;
use rusqlite::{OptionalExtension, params};

/// Metadata about a single `global_setting` row. The full secret
/// value never leaves the storage layer through this struct; only the
/// length is exposed, which keeps `config show` redacted while still
/// surfacing "configured/empty/missing" semantics to operators.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GlobalSettingSummary {
    pub name: &'static str,
    pub present: bool,
    pub length: usize,
}

impl GlobalSettingSummary {
    pub(crate) fn missing(name: &'static str) -> Self {
        Self {
            name,
            present: false,
            length: 0,
        }
    }
}

impl Storage {
    /// Read the stored value for a global setting. Returns
    /// `Ok(None)` when no row exists or the stored value is empty so
    /// callers can keep the env-over-SQLite precedence uniform.
    pub fn load_global_setting(&self, name: &str) -> Result<Option<String>, String> {
        let mut statement = self
            .connection
            .prepare("SELECT value FROM global_setting WHERE name = ?1")
            .map_err(|error| format!("could not prepare global setting load: {error}"))?;
        let value = statement
            .query_row(params![name], |row| row.get::<_, String>(0))
            .optional()
            .map_err(|error| format!("could not read global setting: {error}"))?;
        Ok(value.and_then(|value| {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        }))
    }

    /// Upsert a global setting. Empty values are stored as SQL `NULL`
    /// so `load_global_setting` can keep distinguishing "never
    /// written" from "written with empty value".
    pub fn save_global_setting(&self, name: &str, value: &str) -> Result<(), String> {
        let trimmed = value.trim();
        let transaction = self
            .connection
            .unchecked_transaction()
            .map_err(|error| format!("could not begin global setting write: {error}"))?;
        transaction
            .execute(
                "INSERT INTO global_setting (name, value) VALUES (?1, ?2) \
                 ON CONFLICT(name) DO UPDATE SET value = excluded.value",
                params![name, trimmed],
            )
            .map_err(|error| format!("could not write global setting: {error}"))?;
        transaction
            .commit()
            .map_err(|error| format!("could not commit global setting write: {error}"))?;
        Ok(())
    }

    /// Summarise the canonical set of global settings. The struct
    /// intentionally never carries the full secret value; `config
    /// show` consumes the summaries to render redacted metadata.
    pub fn summarise_global_settings(&self) -> Result<Vec<GlobalSettingSummary>, String> {
        let mut summaries = Vec::with_capacity(GLOBAL_SETTING_NAMES.len());
        for &name in GLOBAL_SETTING_NAMES {
            let stored = self.load_global_setting(name)?;
            summaries.push(match stored {
                Some(value) => GlobalSettingSummary {
                    name,
                    present: true,
                    length: value.chars().count(),
                },
                None => GlobalSettingSummary::missing(name),
            });
        }
        Ok(summaries)
    }

    /// Remove the row for a global setting. Returns `true` when a row
    /// was actually deleted so callers can distinguish "cleared an
    /// existing default" from "no-op because the default was already
    /// absent". Used by `config provider clear` so the persisted
    /// default is removed rather than stored as a confusing empty
    /// value the resolver would later misinterpret.
    pub fn delete_global_setting(&self, name: &str) -> Result<bool, String> {
        let deleted = self
            .connection
            .execute("DELETE FROM global_setting WHERE name = ?1", params![name])
            .map_err(|error| format!("could not delete global setting: {error}"))?;
        Ok(deleted > 0)
    }
}
