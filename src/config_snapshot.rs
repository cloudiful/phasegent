//! Snapshot types and sanitisation helpers used by `config show`.
//!
//! The render-side code lives in its own module so the `config`
//! facade can stay focused on parsing, dispatching, and the
//! `set`/`clear` write path via `config_write`. The snapshot keeps
//! every secret-bearing
//! value redacted: credential rows expose presence and length only,
//! the git mirror bearer key reports presence/length, and the
//! repository URL override is sanitised before being returned to the
//! caller.

use crate::infra::storage::{GlobalSettingSummary, Storage};
use crate::policy::Role;
use serde::Serialize;

/// Per-role snapshot consumed by `config show`. The structure is
/// flat so the JSON output stays compact and operator-friendly.
/// Project-id fields were removed in Phase 1 (remove-project-id);
/// snapshots no longer expose `redmine_project_id` or
/// `gitlab_project_id` and legacy stored values are ignored.
#[derive(Debug, Serialize)]
pub struct RoleSnapshot {
    pub role: &'static str,
    pub provider: Option<String>,
    pub forgejo_api_base: Option<String>,
    pub forgejo_repository: Option<String>,
    pub redmine_api_base: Option<String>,
    pub redmine_close_status_id: Option<u64>,
    pub gitlab_api_base: Option<String>,
    pub forgejo_credential: CredentialSummary,
    pub redmine_credential: CredentialSummary,
    /// Phase-1 GitLab credential summary. Reports presence/length only,
    /// matching the redmine/forgejo field-pair convention so the
    /// snapshot never echoes a GitLab PRIVATE-TOKEN value.
    pub gitlab_credential: CredentialSummary,
}

/// Single global-setting entry as rendered by `config show`. Secrets
/// are summarised via `present` and `length` only. Non-secret
/// values land in the optional `value` slot so operators can read
/// the machine-wide default provider literal directly without
/// parsing the snapshot twice.
#[derive(Debug, Serialize)]
pub struct GlobalSettingJson {
    pub name: &'static str,
    pub present: bool,
    #[serde(skip_serializing_if = "is_zero")]
    pub length: usize,
    /// Sanitised repository URL, only populated for
    /// `PHASEGENT_REDMINE_REPOSITORY_URL`. Credentials embedded in
    /// the userinfo, query, or fragment are stripped before
    /// rendering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sanitized_value: Option<String>,
    /// Non-secret literal value, only populated for entries that
    /// never carry a credential (currently the machine-wide default
    /// provider). The value is the validated `ProviderKind` string
    /// (`forgejo`, `redmine`, or `gitlab`) — the resolver reads the
    /// same string, so the snapshot stays in sync with the runtime
    /// precedence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<&'static str>,
}

/// Compact summary for a per-role credential. Only presence and
/// length are reported so the snapshot can never echo secret
/// content even by accident.
#[derive(Debug, Serialize)]
pub struct CredentialSummary {
    pub present: bool,
    #[serde(skip_serializing_if = "is_zero")]
    pub length: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Top-level snapshot rendered by `config show`. Field order is part
/// of the output contract; reorder only after confirming downstream
/// tooling tolerates it.
#[derive(Debug, Serialize)]
pub struct ConfigSnapshot {
    pub database_path: String,
    pub roles: Vec<RoleSnapshot>,
    pub global_settings: Vec<GlobalSettingJson>,
    /// Machine-wide default provider (`PHASEGENT_DEFAULT_PROVIDER`).
    /// Mirrors `config provider get` so a single `config show`
    /// invocation reports both the global picture and the
    /// resolver-relevant default. The value is one of the canonical
    /// `forgejo` / `redmine` / `gitlab` literals (or `null` when
    /// unset); it is never echoed from the environment or from a
    /// secret field. Always rendered (the `null` value stays in the
    /// JSON) so downstream tooling can switch on the field name
    /// without branching on presence.
    pub global_default_provider: Option<&'static str>,
}

/// Render a redacted snapshot of `storage`. `role` restricts the
/// `roles` array when supplied; passing `None` returns every known
/// role.
pub fn render(storage: &Storage, role: Option<Role>) -> Result<ConfigSnapshot, String> {
    let roles_iter: Box<dyn Iterator<Item = Role>> = match role {
        Some(single) => Box::new(std::iter::once(single)),
        None => Box::new(
            [
                Role::Admin,
                Role::Orchestrator,
                Role::Executor,
                Role::Reviewer,
                Role::Tester,
            ]
            .into_iter(),
        ),
    };
    let mut roles = Vec::new();
    for entry in roles_iter {
        roles.push(snapshot_role(storage, entry)?);
    }
    let global_settings = snapshot_global_settings(storage)?;
    // The machine-wide default provider is intentionally rendered
    // through `ProviderKind::from_str` so a stale row containing an
    // unknown literal surfaces as a structured config error rather
    // than being echoed verbatim. The slot is omitted from the JSON
    // when no value is persisted so callers can rely on absence to
    // mean "unset" instead of "set to an empty string".
    let global_default_provider = match storage.load_global_setting("PHASEGENT_DEFAULT_PROVIDER")? {
        Some(raw) => Some(
            raw.parse::<crate::providers::config::ProviderKind>()
                .map_err(|error| {
                    format!("persisted PHASEGENT_DEFAULT_PROVIDER is invalid: {error}")
                })?
                .as_str(),
        ),
        None => None,
    };
    Ok(ConfigSnapshot {
        database_path: storage.db_path().display().to_string(),
        roles,
        global_settings,
        global_default_provider,
    })
}

fn snapshot_role(storage: &Storage, role: Role) -> Result<RoleSnapshot, String> {
    let role_config = storage.load_role_config(role)?;
    let redmine_config = storage.load_redmine_config(role)?;
    let gitlab_config = storage.load_gitlab_config(role)?;
    let (forgejo_present, forgejo_length) =
        storage.credential_summary(role, crate::infra::storage::PROVIDER_FORGEJO)?;
    let (redmine_present, redmine_length) =
        storage.credential_summary(role, crate::infra::storage::PROVIDER_REDMINE)?;
    let (gitlab_present, gitlab_length) =
        storage.credential_summary(role, crate::infra::storage::PROVIDER_GITLAB)?;
    Ok(RoleSnapshot {
        role: role.as_str(),
        provider: role_config
            .as_ref()
            .and_then(|config| config.provider.clone()),
        forgejo_api_base: role_config
            .as_ref()
            .and_then(|config| config.api_base.clone()),
        forgejo_repository: role_config
            .as_ref()
            .and_then(|config| config.repository.clone()),
        redmine_api_base: redmine_config
            .as_ref()
            .and_then(|config| config.api_base.clone()),
        redmine_close_status_id: redmine_config.and_then(|config| config.close_status_id),
        gitlab_api_base: gitlab_config
            .as_ref()
            .and_then(|config| config.api_base.clone()),
        forgejo_credential: CredentialSummary {
            present: forgejo_present,
            length: forgejo_length,
        },
        redmine_credential: CredentialSummary {
            present: redmine_present,
            length: redmine_length,
        },
        gitlab_credential: CredentialSummary {
            present: gitlab_present,
            length: gitlab_length,
        },
    })
}

fn snapshot_global_settings(storage: &Storage) -> Result<Vec<GlobalSettingJson>, String> {
    let summaries = storage.summarise_global_settings()?;
    let mut entries = Vec::with_capacity(summaries.len());
    for summary in summaries {
        entries.push(global_setting_to_json(storage, summary)?);
    }
    Ok(entries)
}

fn global_setting_to_json(
    storage: &Storage,
    summary: GlobalSettingSummary,
) -> Result<GlobalSettingJson, String> {
    // The sanitised URL is only rendered for the repository URL override
    // because it is the only non-secret global setting whose value
    // contains URL-shaped data. The bearer key summary stays at
    // presence/length.
    let sanitized_value = if summary.name == "PHASEGENT_REDMINE_REPOSITORY_URL" {
        storage
            .load_global_setting(summary.name)?
            .map(|value| sanitize_url(&value))
    } else {
        None
    };
    // The machine-wide default provider is rendered as a non-secret
    // literal so operators can read the resolved value at a glance
    // without needing to run a separate `config provider get`. The
    // ProviderKind parser is the same one the resolver uses, so a
    // stale row containing an unknown literal surfaces as a
    // structured config error rather than being echoed verbatim.
    let value = if summary.name == "PHASEGENT_DEFAULT_PROVIDER" {
        match storage.load_global_setting(summary.name)? {
            Some(raw) => Some(
                raw.parse::<crate::providers::config::ProviderKind>()
                    .map_err(|error| {
                        format!("persisted PHASEGENT_DEFAULT_PROVIDER is invalid: {error}")
                    })?
                    .as_str(),
            ),
            None => None,
        }
    } else {
        None
    };
    Ok(GlobalSettingJson {
        name: summary.name,
        present: summary.present,
        length: summary.length,
        sanitized_value,
        value,
    })
}

/// Safe placeholder returned by [`sanitize_url`] when the input
/// does not parse as a URL. The placeholder is intentionally
/// distinct from any substring an operator might have typed so the
/// redacted snapshot can never echo raw input that may contain
/// embedded credentials (for example a malformed value with a
/// `user:password@host` segment the URL parser refuses to accept).
pub const INVALID_URL_PLACEHOLDER: &str = "<invalid-url-redacted>";

/// Strip credentials embedded in a URL's userinfo, query, and
/// fragment. Returns [`INVALID_URL_PLACEHOLDER`] when parsing fails
/// so the redacted snapshot never echoes raw input that might
/// contain credential-like substrings the URL parser rejected.
///
/// SSH-like schemes (`ssh://`, `git+ssh://`, `ssh+git://`) keep their
/// username: the `git@host` segment is required by the SSH transport
/// and is not a credential. Only the password portion, the query
/// string, and the fragment are dropped.
pub fn sanitize_url(value: &str) -> String {
    match url::Url::parse(value) {
        Ok(parsed) => {
            let mut sanitized = parsed.clone();
            let scheme = sanitized.scheme();
            let ssh_like = matches!(scheme, "ssh" | "git+ssh" | "ssh+git");
            if !ssh_like && !sanitized.username().is_empty() {
                let _ = sanitized.set_username("");
            }
            if sanitized.password().is_some() {
                let _ = sanitized.set_password(None);
            }
            sanitized.set_query(None);
            sanitized.set_fragment(None);
            sanitized.to_string()
        }
        Err(_) => INVALID_URL_PLACEHOLDER.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_url_strips_userinfo_password_query_and_fragment() {
        let sanitized =
            sanitize_url("https://user:secret@git.example.com/owner/repo.git?token=hush#fragment");
        assert_eq!(sanitized, "https://git.example.com/owner/repo.git");
    }

    #[test]
    fn sanitize_url_keeps_clean_https_url_unchanged() {
        let input = "https://git.example.com/owner/repo.git";
        assert_eq!(sanitize_url(input), input);
    }

    #[test]
    fn sanitize_url_keeps_ssh_user_unchanged() {
        let input = "ssh://git@git.example.com/owner/repo.git";
        assert_eq!(sanitize_url(input), input);
    }

    #[test]
    fn sanitize_url_returns_placeholder_when_url_parse_fails() {
        // Scp-style URLs cannot be parsed by the URL parser, so the
        // redaction must fall back to the placeholder rather than
        // echoing the original input verbatim.
        let input = "git@git.example.com:owner/repo.git";
        assert_eq!(sanitize_url(input), INVALID_URL_PLACEHOLDER);
    }

    #[test]
    fn sanitize_url_does_not_leak_credentials_when_url_parse_fails() {
        // The URL parser rejects malformed input that still embeds
        // credential-looking substrings. The function must return the
        // safe placeholder instead of returning the raw input, so the
        // snapshot renderer never echoes the credential fragment.
        let inputs = [
            "git@user:password@host.example.com:owner/repo.git",
            "https://user:pa$$word@example.com:owner/repo.git",
            "https://user:password@example.com:notaport/path",
        ];
        for input in inputs {
            let sanitized = sanitize_url(input);
            assert_eq!(sanitized, INVALID_URL_PLACEHOLDER);
            for forbidden in ["user", "password", "pa$$word", "hush", "secret", "@"] {
                assert!(
                    !sanitized.contains(forbidden),
                    "sanitize_url leaked '{forbidden}' from input '{input}': '{sanitized}'"
                );
            }
        }
    }
}
