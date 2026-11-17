//! Human-operated migration from the legacy SQLite/TOML configuration
//! sources into the JSONC configuration file.
//!
//! The migration is deliberately conservative:
//!
//!   1. The seven legacy configuration tables are read from an existing
//!      database. The database file is never created, migrated, or
//!      modified at this point, and the business rows are never read.
//!   2. An explicit TOML file is decoded and folded over the SQLite
//!      values with the old TOML-over-SQLite precedence. The ambient
//!      environment is never ingested.
//!   3. The values are mapped onto the typed JSONC model. A value the
//!      model cannot represent is an error, never a silent drop.
//!   4. The destination configuration file is written atomically and
//!      privately, then re-read and compared value by value, including
//!      the credential timestamps.
//!   5. A consistent private backup of the source database is made.
//!   6. Only the seven configuration tables are cleared, in a single
//!      transaction, after the source rows are re-read and confirmed
//!      unchanged.
//!
//! The configuration file lock and the source write transaction are held
//! across the whole export, verification, backup, and purge sequence, so
//! no normal configuration writer and no other database writer can change
//! what is migrated between the export and the purge.
//!
//! A crash between the file write and the purge is recoverable: an
//! identical destination resumes the purge, a different destination is
//! refused. Nothing here overwrites an unrelated destination file, and
//! no secret value is ever printed.

pub mod source;
pub mod toml;

use crate::infra::config_file::{ConfigDocument, ConfigFile, runtime};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// What the operator asked to migrate.
#[derive(Clone, Debug)]
pub struct MigrationOptions {
    /// Absolute path of the legacy phasegent database.
    pub from_sqlite: PathBuf,
    /// Absolute path of an explicit legacy TOML overlay.
    pub from_toml: Option<PathBuf>,
}

/// The outcome of one migration run. Only paths and counts are reported;
/// no configuration value or credential ever appears here.
#[derive(Clone, Debug, Serialize)]
pub struct MigrationReport {
    /// Absolute path of the legacy source database.
    pub source: String,
    /// Absolute path of the JSONC configuration file that now holds the
    /// configuration.
    pub destination: String,
    /// Absolute path of the private backup of the source database. The
    /// backup contains the migrated credentials.
    pub backup: String,
    /// Rows read from every legacy configuration table.
    pub exported: Vec<(&'static str, usize)>,
    /// Rows cleared from every legacy configuration table.
    pub purged: Vec<(&'static str, usize)>,
    /// Total number of configuration values that were migrated.
    pub migrated_rows: usize,
    /// True when the destination already held exactly these values and
    /// the purge was resumed.
    pub resumed: bool,
}

/// Migrate the legacy configuration into the JSONC configuration file.
pub fn migrate(options: &MigrationOptions) -> Result<MigrationReport, String> {
    require_absolute(&options.from_sqlite, "--from-sqlite")?;
    if let Some(path) = &options.from_toml {
        require_absolute(path, "--from-toml")?;
    }

    let store = runtime::store()?;
    let destination = store.path().to_path_buf();
    // The configuration lock is held for the whole run: no normal
    // configuration writer can change the migrated values between the
    // verification and the purge.
    let lock = store.lock()?;

    let overlay = match &options.from_toml {
        Some(path) => Some(toml::load_overlay(path)?),
        None => None,
    };

    source::with_locked_source(&options.from_sqlite, |locked| {
        let mut rows = source::read_rows(locked.connection())?;
        let counts = source::count_rows(locked.connection())?;
        let database_rows = counts.total();
        rows.toml_overlay = overlay;
        let overlay_rows = overlay_value_count(rows.toml_overlay.as_ref());
        if database_rows == 0 && overlay_rows == 0 {
            return Err(
                "the source database holds no configuration rows and no explicit TOML overlay \
                 was given; there is nothing to migrate (the configuration was already migrated, \
                 or the source is not a phasegent configuration database)"
                    .to_owned(),
            );
        }

        let file = ConfigFile::from_legacy(rows.clone())?;
        let (resumed, written) = write_destination(&store, &lock, &file)?;
        verify_destination(&store, &file)?;

        let backup_path = create_backup_file(&destination)?;
        source::backup(locked.reader(), &backup_path)?;

        // The destructive boundary. The advisory lock only keeps
        // cooperating phasegent writers out, so the destination is read
        // again here, immediately before the source is cleared. An edit
        // that a direct editor made in the meantime aborts the migration
        // and leaves the source untouched.
        if let Err(error) = reverify_destination(&store, &written) {
            let _ = locked.rollback();
            return Err(error);
        }

        // The purge checks the destination once more inside the held
        // source transaction, after the deletes and immediately before
        // the commit. A changed, removed or unsafe destination rolls
        // every delete back.
        let purged = locked.purge(&rows, || reverify_destination(&store, &written))?;

        Ok(MigrationReport {
            source: options.from_sqlite.display().to_string(),
            destination: destination.display().to_string(),
            backup: backup_path.display().to_string(),
            exported: counts.counts,
            purged: purged.counts,
            migrated_rows: database_rows + overlay_rows,
            resumed,
        })
    })
}

/// The number of values an explicit TOML overlay contributes.
fn overlay_value_count(overlay: Option<&crate::infra::config_file::LegacyTomlOverlay>) -> usize {
    let Some(overlay) = overlay else {
        return 0;
    };
    let mut count = 0;
    for value in [
        overlay.default_provider.as_ref(),
        overlay.redmine_api_base.as_ref(),
        overlay.redmine_repository_url.as_ref(),
        overlay.index_backend.as_ref(),
    ] {
        if value.is_some() {
            count += 1;
        }
    }
    for role in overlay.roles.values() {
        if role.provider.is_some() {
            count += 1;
        }
        if role.redmine_api_base.is_some() {
            count += 1;
        }
        if role.redmine_close_status_id.is_some() {
            count += 1;
        }
    }
    count
}

/// Write the migrated configuration to the destination file. Returns
/// `true` when the destination already held exactly these values and the
/// write was skipped, which is the resume path after a crash between the
/// file write and the purge.
fn write_destination(
    store: &crate::infra::config_file::ConfigFileStore,
    lock: &crate::infra::config_file::ConfigLock,
    file: &ConfigFile,
) -> Result<(bool, Vec<u8>), String> {
    if store.path().exists() {
        let existing = store.load()?;
        if existing.model() == file {
            let bytes = store
                .read_bytes()?
                .ok_or_else(|| "the configuration file disappeared".to_owned())?;
            return Ok((true, bytes));
        }
        return Err(format!(
            "the configuration file at {} already holds different values; \
             refusing to overwrite it (move it aside or migrate into a clean profile)",
            store.path().display()
        ));
    }
    let text = serde_json::to_string_pretty(file)
        .map_err(|error| format!("could not encode the migrated configuration: {error}"))?;
    let document = ConfigDocument::parse(&text)?;
    store.create_locked(lock, &document)?;
    let bytes = store
        .read_bytes()?
        .ok_or_else(|| "the configuration file was not written".to_owned())?;
    Ok((false, bytes))
}

/// Read the destination again and compare it with the bytes that were
/// verified. A changed or removed destination aborts the migration before
/// the source is cleared.
fn reverify_destination(
    store: &crate::infra::config_file::ConfigFileStore,
    expected: &[u8],
) -> Result<(), String> {
    match store.read_bytes()? {
        Some(current) if current == expected => Ok(()),
        Some(_) => Err(
            "the configuration file changed while the migration was running; \
             refusing to clear the source"
                .to_owned(),
        ),
        None => Err(
            "the configuration file was removed while the migration was running; \
             refusing to clear the source"
                .to_owned(),
        ),
    }
}

/// Re-read the destination and compare every value, including the
/// credential timestamps.
fn verify_destination(
    store: &crate::infra::config_file::ConfigFileStore,
    file: &ConfigFile,
) -> Result<(), String> {
    let written = store.load()?;
    if written.model() != file {
        return Err(
            "the migrated configuration could not be verified against the destination file"
                .to_owned(),
        );
    }
    Ok(())
}

/// Reserve the private backup path next to the configuration file. The
/// file is created exclusively, so an existing database is never
/// overwritten.
fn create_backup_file(destination: &Path) -> Result<PathBuf, String> {
    let parent = destination.parent().ok_or_else(|| {
        "the configuration path has no parent directory for the backup".to_owned()
    })?;
    source::create_backup_file(parent)
}

fn require_absolute(path: &Path, flag: &str) -> Result<(), String> {
    if !path.is_absolute() {
        return Err(format!("{flag} must be an absolute path"));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::config_file::LegacyTomlOverlay;
    use std::collections::BTreeMap;

    #[test]
    fn a_toml_only_profile_counts_its_own_values() {
        let overlay = LegacyTomlOverlay {
            default_provider: Some("forgejo".to_owned()),
            redmine_api_base: None,
            redmine_repository_url: None,
            index_backend: Some("postgres".to_owned()),
            roles: BTreeMap::from([(
                "executor".to_owned(),
                crate::infra::config_file::LegacyTomlRole {
                    provider: Some("gitlab".to_owned()),
                    redmine_api_base: None,
                    redmine_close_status_id: Some(9),
                },
            )]),
        };
        assert_eq!(overlay_value_count(Some(&overlay)), 4);
        assert_eq!(overlay_value_count(None), 0);
    }

    #[test]
    fn a_changed_destination_is_told_apart_from_an_identical_one() {
        let mut file = ConfigFile::new();
        file.set_setting(
            "provider",
            crate::infra::config_file::SettingValue::Text("forgejo".to_owned()),
        )
        .unwrap();
        let mut other = file.clone();
        other
            .set_setting(
                "provider",
                crate::infra::config_file::SettingValue::Text("gitlab".to_owned()),
            )
            .unwrap();
        assert_ne!(file, other, "a changed value must not compare equal");
        assert_eq!(file, file.clone(), "an identical value compares equal");
    }

    #[test]
    fn the_migrated_document_is_valid_jsonc_that_round_trips() {
        let mut file = ConfigFile::new();
        file.set_setting(
            "provider",
            crate::infra::config_file::SettingValue::Text("forgejo".to_owned()),
        )
        .unwrap();
        let text = serde_json::to_string_pretty(&file).unwrap();
        let document = ConfigDocument::parse(&text).unwrap();
        assert_eq!(document.model(), &file);
    }
}
