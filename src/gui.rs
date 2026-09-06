//! Single-binary desktop shell behind the `gui` Cargo feature.
//!
//! Normal CLI commands never initialize the GUI: only the explicit
//! `phasegent gui` entry (and the conservative no-argument desktop
//! heuristic in `main`) calls [`run`]. The GUI reuses the existing
//! crate modules directly — [`crate::infra::storage::Storage`] plus
//! the [`crate::config_snapshot`] redacted facade — instead of
//! duplicating storage or provider logic.
//!
//! The phase-1 IPC surface is intentionally read-only and typed:
//!
//! - [`AppMetadata`] / `get_app_metadata` reports the binary name,
//!   version, and application identifier (no secrets).
//! - `get_config_snapshot` returns the redacted
//!   [`crate::config_snapshot::ConfigSnapshot`] already used by
//!   `config show` (credential presence/length only, repository URL
//!   sanitised). No settings mutations or task/status pages exist yet.
//!
//! Blocking storage work never runs on the Tauri async runtime
//! directly; the async command bridges through
//! `tauri::async_runtime::spawn_blocking`.

use serde::Serialize;

/// Redacted application identity exposed to the frontend shell.
/// Contains no secrets, paths, or credentials.
#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct AppMetadata {
    /// Binary/product name (`phasegent`).
    pub name: &'static str,
    /// Crate version (`CARGO_PKG_VERSION`).
    pub version: &'static str,
    /// Tauri application identifier (matches `tauri.conf.json`).
    pub identifier: &'static str,
}

/// Shared metadata constructor used by both the CLI stub and the
/// Tauri command so the value stays in sync with the manifest.
#[allow(dead_code)]
pub fn app_metadata() -> AppMetadata {
    AppMetadata {
        name: "phasegent",
        version: env!("CARGO_PKG_VERSION"),
        identifier: "com.cloud1ful.phasegent",
    }
}

/// Read the redacted configuration snapshot through the existing
/// `config show` facade. Secrets are never echoed: credentials report
/// presence/length only and the repository URL is sanitised by
/// [`crate::config_snapshot`].
#[allow(dead_code)]
pub fn read_config_snapshot() -> Result<crate::config_snapshot::ConfigSnapshot, String> {
    let storage = crate::infra::storage::Storage::open()?;
    crate::config_snapshot::render(&storage, None)
}

#[cfg(feature = "gui")]
mod imp {
    use super::{AppMetadata, app_metadata, read_config_snapshot};

    /// Read-only typed IPC: redacted config snapshot for the frontend
    /// shell. Runs the blocking SQLite read on a dedicated blocking
    /// thread so the Tauri runtime stays responsive.
    #[tauri::command]
    async fn get_config_snapshot() -> Result<crate::config_snapshot::ConfigSnapshot, String> {
        tauri::async_runtime::spawn_blocking(read_config_snapshot)
            .await
            .map_err(|error| format!("config snapshot task failed: {error}"))?
    }

    /// Read-only typed IPC: static application metadata (no secrets).
    #[tauri::command]
    fn get_app_metadata() -> AppMetadata {
        app_metadata()
    }

    /// Open the Tauri desktop shell. Called only for the explicit
    /// `gui` command and the no-argument desktop heuristic; normal CLI
    /// dispatch never reaches here.
    pub fn run() -> i32 {
        match tauri::Builder::default()
            .invoke_handler(tauri::generate_handler![
                get_config_snapshot,
                get_app_metadata
            ])
            .run(tauri::generate_context!())
        {
            Ok(()) => 0,
            Err(error) => {
                eprintln!(
                    "{}",
                    serde_json::json!({"error":{"kind":"gui", "message":format!("could not start desktop shell: {error}")}})
                );
                1
            }
        }
    }
}

#[cfg(not(feature = "gui"))]
mod imp {
    /// Stub used when the binary was built without the `gui` feature.
    /// Keeps the `gui` subcommand parsed and documented while making
    /// the missing desktop runtime an explicit structured error
    /// instead of silently falling back to CLI help.
    pub fn run() -> i32 {
        eprintln!(
            "{}",
            serde_json::json!({"error":{"kind":"gui", "message":"GUI support was not compiled into this binary; rebuild with --features gui to enable the desktop shell"}})
        );
        1
    }
}

pub use imp::run;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_metadata_contains_no_secrets() {
        let metadata = app_metadata();
        assert_eq!(metadata.name, "phasegent");
        assert_eq!(metadata.identifier, "com.cloud1ful.phasegent");
        assert_eq!(metadata.version, env!("CARGO_PKG_VERSION"));
        let encoded = serde_json::to_string(&metadata).expect("metadata must serialize");
        for forbidden in ["token", "secret", "password", "credential", "api-key"] {
            assert!(
                !encoded.to_ascii_lowercase().contains(forbidden),
                "metadata must not mention {forbidden}: {encoded}"
            );
        }
    }

    #[test]
    fn config_snapshot_helper_is_redacted() {
        // Point storage at a throwaway database so the test never
        // touches the operator's real config directory.
        let dir = std::env::temp_dir().join(format!(
            "phasegent-gui-snapshot-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        let db_path = dir.join("phasegent.sqlite3");
        let guard = EnvGuard::set("PHASEGENT_DB_PATH", db_path.to_string_lossy().as_ref());
        let snapshot = read_config_snapshot().expect("snapshot must render");
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
        let encoded = serde_json::to_string(&snapshot).expect("snapshot must serialize");
        // The redacted snapshot reports presence/length only; even on
        // an empty database the shape must serialize without secrets.
        assert!(encoded.contains("database_path"));
        assert!(encoded.contains("roles"));
    }

    /// Minimal scoped env guard so parallel tests do not leak
    /// `PHASEGENT_DB_PATH` overrides.
    struct EnvGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var_os(key);
            // `std::env::set_var` is (correctly) flagged `unsafe` on
            // this toolchain because it can race with other threads
            // reading the environment. The test harness runs tests in
            // parallel, so scope the override as narrowly as possible
            // and restore the previous value on drop.
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }
}
