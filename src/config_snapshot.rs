//! Snapshot types and sanitisation helpers used by `config show`.
//!
//! The render-side code lives in its own module so the `config`
//! facade can stay focused on parsing, dispatching, and the
//! `set`/`clear` write path via `config_write`. The snapshot mirrors the
//! typed configuration tree: every machine-wide setting appears under the
//! section that owns it, never under a flat environment-style name.
//!
//! The snapshot keeps every secret-bearing value redacted: a secret
//! exposes presence and length only, a URL is sanitised before it is
//! rendered, and the remaining values are reported as they are.
//!
//! The Redmine REST address is machine-wide, so it is reported once under
//! the `redmine` section and never per role. The per-role entries keep
//! the role-scoped Redmine close-status id and the Redmine credential
//! presence/length.

use crate::infra::config_file::runtime::{self};
use crate::infra::config_file::setting_path::{self, Redaction};
use crate::policy::Role;
use serde::Serialize;

/// Per-role snapshot consumed by `config show`. The structure is
/// flat so the JSON output stays compact and operator-friendly.
/// The machine-wide Redmine REST address is deliberately absent: it
/// is rendered once under the `redmine` section instead, so no role entry
/// can present the address as role-scoped.
#[derive(Debug, Serialize)]
pub struct RoleSnapshot {
    pub role: &'static str,
    pub provider: Option<String>,
    pub redmine_close_status_id: Option<u64>,
    pub redmine_credential: CredentialSummary,
}

/// One machine-wide setting as rendered by `config show`. A secret is
/// summarised via `present` and `length` only. A non-secret value lands
/// in the optional `value` slot with its JSON type, and a URL lands in
/// the optional `sanitized_value` slot with its credentials stripped.
#[derive(Debug, Serialize)]
pub struct SettingJson {
    pub present: bool,
    #[serde(skip_serializing_if = "is_zero")]
    pub length: usize,
    /// Sanitised URL, only populated for the URL-shaped settings.
    /// Credentials embedded in the userinfo, query, or fragment are
    /// stripped before rendering.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sanitized_value: Option<String>,
    /// The typed value of a non-secret setting. The value is the one the
    /// resolver reads, so the snapshot stays in sync with the runtime
    /// precedence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

/// The machine-wide Redmine section of the snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedmineSnapshot {
    pub api_base: SettingJson,
    pub repository_url: SettingJson,
    pub git_mirror: GitMirrorSnapshot,
}

/// The Redmine git-mirror section of the snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitMirrorSnapshot {
    pub api_key: SettingJson,
}

/// The issue-index section of the snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexSnapshot {
    pub postgres: PostgresSnapshot,
}

/// The PostgreSQL index section of the snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostgresSnapshot {
    pub url: SettingJson,
}

/// The notification section of the snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationsSnapshot {
    pub enabled: SettingJson,
    pub channel: SettingJson,
    pub ntfy: NtfySnapshot,
    pub webhook: WebhookSnapshot,
    pub dingtalk: DingtalkSnapshot,
    pub email: EmailSnapshot,
}

/// The ntfy section of the snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NtfySnapshot {
    pub base_url: SettingJson,
    pub topic: SettingJson,
    pub token: SettingJson,
}

/// The webhook section of the snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookSnapshot {
    pub url: SettingJson,
    pub token: SettingJson,
}

/// The DingTalk section of the snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DingtalkSnapshot {
    pub webhook_url: SettingJson,
    pub secret: SettingJson,
    pub keywords: SettingJson,
    pub message_type: SettingJson,
}

/// The e-mail section of the snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailSnapshot {
    pub smtp: SmtpSnapshot,
    pub username: SettingJson,
    pub password: SettingJson,
    pub from: SettingJson,
    pub to: SettingJson,
    pub reply_to: SettingJson,
}

/// The SMTP section of the snapshot.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SmtpSnapshot {
    pub host: SettingJson,
    pub port: SettingJson,
    pub tls: SettingJson,
}

/// Compact summary for a per-role credential. Only presence,
/// length, the last-4-characters fingerprint, and the store
/// timestamp are reported so the snapshot can never echo secret
/// content even by accident.
#[derive(Debug, Serialize)]
pub struct CredentialSummary {
    pub present: bool,
    #[serde(skip_serializing_if = "is_zero")]
    pub length: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Top-level snapshot rendered by `config show`. Field order is part
/// of the output contract; reorder only after confirming downstream
/// tooling tolerates it.
#[derive(Debug, Serialize)]
pub struct ConfigSnapshot {
    /// Absolute path of the JSONC configuration file this snapshot was
    /// rendered from. The configuration file is distinct from the
    /// business database, which holds task state only.
    pub config_path: String,
    /// Machine-wide default provider. Mirrors `config provider get` so a
    /// single `config show` invocation reports both the global picture
    /// and the resolver-relevant default. The value is one of the
    /// canonical `redmine` / `local` / `forgejo` / `gitlab` literals (or
    /// `null` when unset); it is never echoed from the environment or
    /// from a secret field. Always rendered (the `null` value stays in
    /// the JSON) so downstream tooling can switch on the field name
    /// without branching on presence.
    pub provider: Option<&'static str>,
    pub redmine: RedmineSnapshot,
    pub index: IndexSnapshot,
    pub notifications: NotificationsSnapshot,
    pub roles: Vec<RoleSnapshot>,
}

pub fn render(role: Option<Role>) -> Result<ConfigSnapshot, String> {
    let roles_iter: Box<dyn Iterator<Item = Role>> = match role {
        Some(single) => Box::new(std::iter::once(single)),
        None => Box::new(
            [
                Role::Admin,
                Role::Orchestrator,
                Role::Executor,
                Role::Reviewer,
                Role::Explore,
            ]
            .into_iter(),
        ),
    };
    let mut roles = Vec::new();
    for entry in roles_iter {
        roles.push(snapshot_role(entry)?);
    }
    // The machine-wide default provider is intentionally rendered
    // through `ProviderKind::from_str` so a stale value containing an
    // unknown literal surfaces as a structured config error rather than
    // being echoed verbatim. The slot is omitted from the JSON when no
    // value is persisted so callers can rely on absence to mean "unset"
    // instead of "set to an empty string".
    let provider = match runtime::load_global_setting(setting_path::PROVIDER)? {
        Some(raw) => Some(
            raw.parse::<crate::providers::config::ProviderKind>()
                .map_err(|error| format!("persisted provider is invalid: {error}"))?
                .as_str(),
        ),
        None => None,
    };
    Ok(ConfigSnapshot {
        config_path: runtime::config_path()?.display().to_string(),
        provider,
        redmine: RedmineSnapshot {
            api_base: setting_json(setting_path::REDMINE_API_BASE)?,
            repository_url: setting_json(setting_path::REDMINE_REPOSITORY_URL)?,
            git_mirror: GitMirrorSnapshot {
                api_key: setting_json(setting_path::REDMINE_GIT_MIRROR_API_KEY)?,
            },
        },
        index: IndexSnapshot {
            postgres: PostgresSnapshot {
                url: setting_json(setting_path::INDEX_PG_URL)?,
            },
        },
        notifications: NotificationsSnapshot {
            enabled: setting_json(setting_path::NOTIFY_ENABLED)?,
            channel: setting_json(setting_path::NOTIFY_CHANNEL)?,
            ntfy: NtfySnapshot {
                base_url: setting_json(setting_path::NOTIFY_NTFY_BASE_URL)?,
                topic: setting_json(setting_path::NOTIFY_NTFY_TOPIC)?,
                token: setting_json(setting_path::NOTIFY_NTFY_TOKEN)?,
            },
            webhook: WebhookSnapshot {
                url: setting_json(setting_path::NOTIFY_WEBHOOK_URL)?,
                token: setting_json(setting_path::NOTIFY_WEBHOOK_TOKEN)?,
            },
            dingtalk: DingtalkSnapshot {
                webhook_url: setting_json(setting_path::NOTIFY_DINGTALK_WEBHOOK_URL)?,
                secret: setting_json(setting_path::NOTIFY_DINGTALK_SECRET)?,
                keywords: setting_json(setting_path::NOTIFY_DINGTALK_KEYWORDS)?,
                message_type: setting_json(setting_path::NOTIFY_DINGTALK_MESSAGE_TYPE)?,
            },
            email: EmailSnapshot {
                smtp: SmtpSnapshot {
                    host: setting_json(setting_path::NOTIFY_EMAIL_SMTP_HOST)?,
                    port: setting_json(setting_path::NOTIFY_EMAIL_SMTP_PORT)?,
                    tls: setting_json(setting_path::NOTIFY_EMAIL_SMTP_TLS)?,
                },
                username: setting_json(setting_path::NOTIFY_EMAIL_USERNAME)?,
                password: setting_json(setting_path::NOTIFY_EMAIL_PASSWORD)?,
                from: setting_json(setting_path::NOTIFY_EMAIL_FROM)?,
                to: setting_json(setting_path::NOTIFY_EMAIL_TO)?,
                reply_to: setting_json(setting_path::NOTIFY_EMAIL_REPLY_TO)?,
            },
        },
        roles,
    })
}

pub(crate) fn snapshot_role(role: Role) -> Result<RoleSnapshot, String> {
    let role_config = runtime::load_role_config(role)?;
    let redmine_config = runtime::load_redmine_config(role)?;
    let redmine = runtime::credential_summary(role, crate::infra::storage::PROVIDER_REDMINE)?;
    let summarize = |identity: runtime::CredentialIdentity| CredentialSummary {
        present: identity.present,
        length: identity.length,
        fingerprint: identity.fingerprint,
        updated_at: identity.updated_at,
    };
    Ok(RoleSnapshot {
        role: role.as_str(),
        provider: role_config
            .as_ref()
            .and_then(|config| config.provider.clone()),
        redmine_close_status_id: redmine_config.and_then(|config| config.close_status_id),
        redmine_credential: summarize(redmine),
    })
}

/// Render one machine-wide setting. The rendering follows the redaction
/// class of the setting: a secret exposes presence and length only, a
/// URL is sanitised, and every other non-secret value keeps its JSON
/// type.
fn setting_json(path: &str) -> Result<SettingJson, String> {
    let Some(setting) = setting_path::global_setting(path) else {
        return Err("unknown configuration setting".to_owned());
    };
    let value = runtime::load_setting(path)?;
    let present = value.is_some();
    let length = value
        .as_ref()
        .map(|value| value.to_text().chars().count())
        .unwrap_or(0);
    let mut rendered = SettingJson {
        present,
        length,
        sanitized_value: None,
        value: None,
    };
    let Some(value) = value else {
        return Ok(rendered);
    };
    match setting.redaction {
        Redaction::Secret | Redaction::Opaque => {}
        Redaction::Url => {
            rendered.sanitized_value = Some(sanitize_url(&value.to_text()));
        }
        Redaction::Literal => {
            rendered.value = Some(value.to_json());
        }
    }
    Ok(rendered)
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
