//! Channel configuration for agent notifications.
//!
//! One `global_setting` row per field, resolved env-over-SQLite like
//! every other global. Secrets are write-only: they are read for
//! delivery but never rendered in snapshots or errors. Default Cargo
//! features cover ntfy plus webhook; dingtalk/email settings are
//! accepted always but delivery requires the matching
//! `notify-dingtalk` / `notify-email` feature.

use crate::infra::storage::Storage;

/// Supported delivery channels. The literal doubles as the stored
/// `PHASEGENT_NOTIFY_CHANNEL` value and the `channel` column in
/// `notification_deliveries`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyChannel {
    Ntfy,
    Webhook,
    Dingtalk,
    Email,
}

impl NotifyChannel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ntfy => "ntfy",
            Self::Webhook => "webhook",
            Self::Dingtalk => "dingtalk",
            Self::Email => "email",
        }
    }

    pub fn parse(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "ntfy" => Ok(Self::Ntfy),
            "webhook" => Ok(Self::Webhook),
            "dingtalk" => Ok(Self::Dingtalk),
            "email" => Ok(Self::Email),
            _ => Err(format!(
                "invalid notify channel '{value}'; expected ntfy, webhook, dingtalk, or email"
            )),
        }
    }
}

/// Resolved channel configuration. Only the selected channel's fields
/// are validated; unselected channels may be absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyConfig {
    pub enabled: bool,
    pub channel: NotifyChannel,
    pub ntfy_base_url: Option<String>,
    pub ntfy_topic: Option<String>,
    pub ntfy_token: Option<String>,
    pub webhook_url: Option<String>,
    pub webhook_token: Option<String>,
    pub dingtalk_webhook_url: Option<String>,
    pub dingtalk_secret: Option<String>,
    pub dingtalk_keywords: Vec<String>,
    pub dingtalk_msg_type: String,
    pub email_smtp_host: Option<String>,
    pub email_smtp_port: Option<u16>,
    pub email_tls: Option<String>,
    pub email_username: Option<String>,
    pub email_password: Option<String>,
    pub email_from: Option<String>,
    pub email_to: Vec<String>,
    pub email_reply_to: Option<String>,
}

/// True for every `PHASEGENT_NOTIFY_*` canonical name.
pub fn is_notify_setting(canonical: &str) -> bool {
    canonical.starts_with("PHASEGENT_NOTIFY_")
}

/// True for write-only notify secrets. Mirrors
/// `crate::config_write::is_secret_setting` for the notify subset so
/// GUI and snapshot code can share one predicate.
pub fn is_notify_secret(canonical: &str) -> bool {
    matches!(
        canonical,
        "PHASEGENT_NOTIFY_NTFY_TOKEN"
            | "PHASEGENT_NOTIFY_WEBHOOK_TOKEN"
            | "PHASEGENT_NOTIFY_DINGTALK_SECRET"
            | "PHASEGENT_NOTIFY_EMAIL_PASSWORD"
    )
}

/// Load the effective notify config: env wins, SQLite fills gaps.
/// Returns `Ok(None)` when notifications are disabled or unconfigured
/// so callers stay silent and preserve existing CLI behaviour.
pub fn load(storage: &Storage) -> Result<Option<NotifyConfig>, String> {
    let enabled_raw = resolve_field(storage, "PHASEGENT_NOTIFY_ENABLED")?;
    let enabled = match enabled_raw.as_deref() {
        None => false,
        Some(raw) => parse_enabled(raw)?,
    };
    if !enabled {
        return Ok(None);
    }
    let channel_raw = resolve_field(storage, "PHASEGENT_NOTIFY_CHANNEL")?
        .ok_or_else(|| {
            "notifications are enabled but PHASEGENT_NOTIFY_CHANNEL is not set; use admin config set notify-channel <ntfy|webhook|dingtalk|email>".to_owned()
        })?;
    let channel = NotifyChannel::parse(&channel_raw)?;
    let mut config = NotifyConfig {
        enabled: true,
        channel,
        ntfy_base_url: resolve_field(storage, "PHASEGENT_NOTIFY_NTFY_BASE_URL")?,
        ntfy_topic: resolve_field(storage, "PHASEGENT_NOTIFY_NTFY_TOPIC")?,
        ntfy_token: resolve_field(storage, "PHASEGENT_NOTIFY_NTFY_TOKEN")?,
        webhook_url: resolve_field(storage, "PHASEGENT_NOTIFY_WEBHOOK_URL")?,
        webhook_token: resolve_field(storage, "PHASEGENT_NOTIFY_WEBHOOK_TOKEN")?,
        dingtalk_webhook_url: resolve_field(storage, "PHASEGENT_NOTIFY_DINGTALK_WEBHOOK_URL")?,
        dingtalk_secret: resolve_field(storage, "PHASEGENT_NOTIFY_DINGTALK_SECRET")?,
        dingtalk_keywords: resolve_field(storage, "PHASEGENT_NOTIFY_DINGTALK_KEYWORDS")?
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        dingtalk_msg_type: resolve_field(storage, "PHASEGENT_NOTIFY_DINGTALK_MSG_TYPE")?
            .unwrap_or_else(|| "text".to_owned()),
        email_smtp_host: resolve_field(storage, "PHASEGENT_NOTIFY_EMAIL_SMTP_HOST")?,
        email_smtp_port: match resolve_field(storage, "PHASEGENT_NOTIFY_EMAIL_SMTP_PORT")? {
            Some(raw) => Some(raw.parse::<u16>().map_err(|_| {
                format!("PHASEGENT_NOTIFY_EMAIL_SMTP_PORT '{raw}' must be 1-65535")
            })?),
            None => None,
        },
        email_tls: resolve_field(storage, "PHASEGENT_NOTIFY_EMAIL_TLS")?,
        email_username: resolve_field(storage, "PHASEGENT_NOTIFY_EMAIL_USERNAME")?,
        email_password: resolve_field(storage, "PHASEGENT_NOTIFY_EMAIL_PASSWORD")?,
        email_from: resolve_field(storage, "PHASEGENT_NOTIFY_EMAIL_FROM")?,
        email_to: resolve_field(storage, "PHASEGENT_NOTIFY_EMAIL_TO")?
            .map(|raw| {
                raw.split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        email_reply_to: resolve_field(storage, "PHASEGENT_NOTIFY_EMAIL_REPLY_TO")?,
    };
    // Normalise optional dingtalk/email literals without echoing values.
    config.dingtalk_msg_type = config.dingtalk_msg_type.trim().to_ascii_lowercase();
    if config.dingtalk_msg_type.is_empty() {
        config.dingtalk_msg_type = "text".to_owned();
    }
    if let Some(tls) = config.email_tls.take() {
        let lower = tls.trim().to_ascii_lowercase();
        config.email_tls = Some(lower);
    }
    validate_for_channel(&config)?;
    Ok(Some(config))
}

fn parse_enabled(raw: &str) -> Result<bool, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" | "enabled" => Ok(true),
        "false" | "0" | "no" | "off" | "disabled" => Ok(false),
        _ => Err(format!(
            "invalid PHASEGENT_NOTIFY_ENABLED '{raw}'; expected true or false"
        )),
    }
}

fn validate_for_channel(config: &NotifyConfig) -> Result<(), String> {
    match config.channel {
        NotifyChannel::Ntfy => {
            if config
                .ntfy_base_url
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                return Err(
                    "PHASEGENT_NOTIFY_NTFY_BASE_URL is required for channel ntfy".to_owned(),
                );
            }
            if config.ntfy_topic.as_deref().unwrap_or("").trim().is_empty() {
                return Err("PHASEGENT_NOTIFY_NTFY_TOPIC is required for channel ntfy".to_owned());
            }
        }
        NotifyChannel::Webhook => {
            if config
                .webhook_url
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                return Err(
                    "PHASEGENT_NOTIFY_WEBHOOK_URL is required for channel webhook".to_owned(),
                );
            }
        }
        NotifyChannel::Dingtalk => {
            if config
                .dingtalk_webhook_url
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
            {
                return Err(
                    "PHASEGENT_NOTIFY_DINGTALK_WEBHOOK_URL is required for channel dingtalk"
                        .to_owned(),
                );
            }
            if !matches!(config.dingtalk_msg_type.as_str(), "text" | "markdown") {
                return Err(format!(
                    "invalid PHASEGENT_NOTIFY_DINGTALK_MSG_TYPE '{}'; expected text or markdown",
                    config.dingtalk_msg_type
                ));
            }
        }
        NotifyChannel::Email => {
            for (name, present) in [
                (
                    "PHASEGENT_NOTIFY_EMAIL_SMTP_HOST",
                    config
                        .email_smtp_host
                        .as_deref()
                        .unwrap_or("")
                        .trim()
                        .is_empty(),
                ),
                (
                    "PHASEGENT_NOTIFY_EMAIL_FROM",
                    config.email_from.as_deref().unwrap_or("").trim().is_empty(),
                ),
            ] {
                if present {
                    return Err(format!("{name} is required for channel email"));
                }
            }
            if config.email_to.is_empty() {
                return Err("PHASEGENT_NOTIFY_EMAIL_TO is required for channel email".to_owned());
            }
            if let Some(tls) = config.email_tls.as_deref()
                && !matches!(tls, "implicit" | "starttls" | "plain")
            {
                return Err(format!(
                    "invalid PHASEGENT_NOTIFY_EMAIL_TLS '{tls}'; expected implicit, starttls, or plain"
                ));
            }
        }
    }
    Ok(())
}

fn resolve_field(storage: &Storage, name: &str) -> Result<Option<String>, String> {
    if let Some(env) = read_env_trimmed(name)? {
        return Ok(Some(env));
    }
    storage.load_global_setting(name)
}

fn read_env_trimmed(name: &str) -> Result<Option<String>, String> {
    match std::env::var(name) {
        Ok(value) => {
            let trimmed = value.trim().to_owned();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed))
            }
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(error) => Err(format!("could not read {name}: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_parses_all_literals() {
        assert_eq!(NotifyChannel::parse("ntfy").unwrap(), NotifyChannel::Ntfy);
        assert_eq!(
            NotifyChannel::parse("webhook").unwrap(),
            NotifyChannel::Webhook
        );
        assert!(NotifyChannel::parse("pager").is_err());
    }

    #[test]
    fn secret_predicate_covers_tokens_only() {
        assert!(is_notify_secret("PHASEGENT_NOTIFY_NTFY_TOKEN"));
        assert!(is_notify_secret("PHASEGENT_NOTIFY_WEBHOOK_TOKEN"));
        assert!(!is_notify_secret("PHASEGENT_NOTIFY_CHANNEL"));
        assert!(!is_notify_secret("PHASEGENT_NOTIFY_NTFY_TOPIC"));
    }
}
