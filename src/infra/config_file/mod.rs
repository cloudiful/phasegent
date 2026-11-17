//! Typed JSONC configuration core.
//!
//! This module is the P1 authority for the JSONC configuration document. It
//! parses and validates the complete persisted configuration, exposes pure
//! read/write helpers over the typed model, keeps comments and unrelated
//! fields when a setting is changed through the CST, and provides the private,
//! lock-protected file adapter that later phases will call.
//!
//! The module is the authority for the JSONC configuration document. It
//! parses and validates the complete persisted configuration, exposes pure
//! read/write helpers over the typed model, keeps comments and unrelated
//! fields when a setting is changed through the CST, and provides the
//! private, lock-protected file adapter the runtime resolves through.
//!
//! ## Schema
//!
//! ```jsonc
//! {
//!   "version": 1,
//!   "settings": {
//!     "PHASEGENT_DEFAULT_PROVIDER": "redmine"
//!   },
//!   "roles": {
//!     "executor": {
//!       "provider": "forgejo",
//!       "legacy": { "apiBase": "https://legacy.example", "repository": "owner/repo" },
//!       "redmine": { "apiBase": "https://redmine.example", "projectId": "tools", "closeStatusId": 5 },
//!       "forgejo": { "apiBase": "https://forgejo.example", "repository": "owner/repo", "source": "admin-auth-setup", "updatedAt": 1700000000 },
//!       "gitlab": { "apiBase": "https://gitlab.example", "projectId": 12, "repository": "owner/repo", "source": "admin-auth-setup", "updatedAt": 1700000000 },
//!       "credentials": {
//!         "redmine": { "value": "plaintext-secret", "updatedAt": 1700000000 }
//!       },
//!       "redmineUser": { "userId": 7, "login": "phasegent-executor" }
//!     }
//!   }
//! }
//! ```
//!
//! `settings` is keyed by the canonical `PHASEGENT_*` global setting names.
//! `roles` is keyed by the exact role names and each entry mirrors the seven
//! legacy configuration tables: `role_config` (`provider` plus inert `legacy`
//! columns), `role_redmine_config`, `role_forgejo_config`,
//! `role_gitlab_config`, `role_credential`, and `role_redmine_user`.
//! `global_setting` is the `settings` map. The credential fingerprint is
//! derived from the stored value instead of being persisted, so it cannot
//! drift from the secret.

mod file;
mod jsonc;
mod legacy;
mod model;
pub mod runtime;

pub use file::{ConfigFileStore, ConfigLock};
pub use jsonc::ConfigDocument;
pub use legacy::{
    LegacyConfigRows, LegacyCredentialRow, LegacyForgejoConfigRow, LegacyGitlabConfigRow,
    LegacyGlobalSettingRow, LegacyRedmineConfigRow, LegacyRedmineUserRow, LegacyRoleConfigRow,
    LegacyTomlOverlay, LegacyTomlRole,
};
pub use model::{ConfigFile, SCHEMA_VERSION, is_known_setting};
