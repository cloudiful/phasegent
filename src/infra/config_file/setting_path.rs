//! The narrow mapping between the flat setting names and the schema-v2
//! configuration tree.
//!
//! The configuration file stores every machine-wide setting inside the
//! typed hierarchy. The flat `PHASEGENT_*` names stay the external
//! interface: they are the environment override names and the historical
//! CLI aliases. This module is the single place that knows both, so no
//! consumer needs the file layout and the file never stores an environment
//! name.
//!
//! The inert legacy index-backend selector has no destination: backend
//! selection is driven by the presence of the PostgreSQL URL, so the old
//! value was ignored and is not part of the supported configuration.

use super::model::SettingKind;

/// How a value is rendered in the redacted snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Redaction {
    /// The value is a secret: only presence and length are reported.
    Secret,
    /// The value is a URL: it is rendered with its credentials stripped.
    Url,
    /// The value is a non-secret literal: it is rendered as it is.
    Literal,
    /// The value is not rendered verbatim: only presence and length are
    /// reported.
    Opaque,
}

/// One machine-wide setting of the configuration tree.
#[derive(Clone, Copy, Debug)]
pub struct GlobalSetting {
    /// The dotted path of the setting inside the configuration file.
    pub dotted: &'static str,
    /// The path segments inside the configuration file.
    pub segments: &'static [&'static str],
    /// The historical flat name: the environment override name and the
    /// historical CLI alias.
    pub legacy_name: &'static str,
    /// The kind of value the setting carries.
    pub kind: SettingKind,
    /// How the snapshot renders the value.
    pub redaction: Redaction,
}

impl GlobalSetting {
    /// Whether the value is write-only.
    pub fn is_secret(&self) -> bool {
        self.redaction == Redaction::Secret
    }
}

/// The machine-wide default provider.
pub const PROVIDER: &str = "provider";
/// The machine-wide Redmine REST address.
pub const REDMINE_API_BASE: &str = "redmine.apiBase";
/// The Redmine repository URL override.
pub const REDMINE_REPOSITORY_URL: &str = "redmine.repositoryUrl";
/// The Redmine git-mirror bearer key.
pub const REDMINE_GIT_MIRROR_API_KEY: &str = "redmine.gitMirror.apiKey";
/// The PostgreSQL connection URL of the shared issue index.
pub const INDEX_PG_URL: &str = "index.postgres.url";
/// Whether agent notifications are enabled.
pub const NOTIFY_ENABLED: &str = "notifications.enabled";
/// The selected notification channel.
pub const NOTIFY_CHANNEL: &str = "notifications.channel";
/// The ntfy server base URL.
pub const NOTIFY_NTFY_BASE_URL: &str = "notifications.ntfy.baseUrl";
/// The ntfy topic.
pub const NOTIFY_NTFY_TOPIC: &str = "notifications.ntfy.topic";
/// The ntfy token.
pub const NOTIFY_NTFY_TOKEN: &str = "notifications.ntfy.token";
/// The webhook URL.
pub const NOTIFY_WEBHOOK_URL: &str = "notifications.webhook.url";
/// The webhook token.
pub const NOTIFY_WEBHOOK_TOKEN: &str = "notifications.webhook.token";
/// The DingTalk webhook URL.
pub const NOTIFY_DINGTALK_WEBHOOK_URL: &str = "notifications.dingtalk.webhookUrl";
/// The DingTalk signing secret.
pub const NOTIFY_DINGTALK_SECRET: &str = "notifications.dingtalk.secret";
/// The DingTalk keywords.
pub const NOTIFY_DINGTALK_KEYWORDS: &str = "notifications.dingtalk.keywords";
/// The DingTalk message type.
pub const NOTIFY_DINGTALK_MESSAGE_TYPE: &str = "notifications.dingtalk.messageType";
/// The SMTP host.
pub const NOTIFY_EMAIL_SMTP_HOST: &str = "notifications.email.smtp.host";
/// The SMTP port.
pub const NOTIFY_EMAIL_SMTP_PORT: &str = "notifications.email.smtp.port";
/// The SMTP transport security mode.
pub const NOTIFY_EMAIL_SMTP_TLS: &str = "notifications.email.smtp.tls";
/// The SMTP user name.
pub const NOTIFY_EMAIL_USERNAME: &str = "notifications.email.username";
/// The SMTP password.
pub const NOTIFY_EMAIL_PASSWORD: &str = "notifications.email.password";
/// The sender address.
pub const NOTIFY_EMAIL_FROM: &str = "notifications.email.from";
/// The recipient addresses.
pub const NOTIFY_EMAIL_TO: &str = "notifications.email.to";
/// The reply-to address.
pub const NOTIFY_EMAIL_REPLY_TO: &str = "notifications.email.replyTo";

/// Every machine-wide setting of the configuration tree.
pub const GLOBAL_SETTINGS: &[GlobalSetting] = &[
    GlobalSetting {
        dotted: PROVIDER,
        segments: &["provider"],
        legacy_name: crate::infra::storage_schema::GLOBAL_DEFAULT_PROVIDER,
        kind: SettingKind::Choice(&["redmine", "local", "forgejo", "gitlab"]),
        redaction: Redaction::Literal,
    },
    GlobalSetting {
        dotted: REDMINE_API_BASE,
        segments: &["redmine", "apiBase"],
        legacy_name: crate::infra::storage_schema::GLOBAL_REDMINE_API_BASE,
        kind: SettingKind::Text,
        redaction: Redaction::Url,
    },
    GlobalSetting {
        dotted: REDMINE_REPOSITORY_URL,
        segments: &["redmine", "repositoryUrl"],
        legacy_name: crate::infra::storage_schema::GLOBAL_REDMINE_REPOSITORY_URL,
        kind: SettingKind::Text,
        redaction: Redaction::Url,
    },
    GlobalSetting {
        dotted: REDMINE_GIT_MIRROR_API_KEY,
        segments: &["redmine", "gitMirror", "apiKey"],
        legacy_name: crate::infra::storage_schema::GLOBAL_REDMINE_GIT_MIRROR_API_KEY,
        kind: SettingKind::Text,
        redaction: Redaction::Secret,
    },
    GlobalSetting {
        dotted: INDEX_PG_URL,
        segments: &["index", "postgres", "url"],
        legacy_name: crate::infra::storage_schema::GLOBAL_INDEX_PG_URL,
        kind: SettingKind::Text,
        redaction: Redaction::Secret,
    },
    GlobalSetting {
        dotted: NOTIFY_ENABLED,
        segments: &["notifications", "enabled"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_ENABLED,
        kind: SettingKind::Bool,
        redaction: Redaction::Literal,
    },
    GlobalSetting {
        dotted: NOTIFY_CHANNEL,
        segments: &["notifications", "channel"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_CHANNEL,
        kind: SettingKind::Choice(&["ntfy", "webhook", "dingtalk", "email"]),
        redaction: Redaction::Literal,
    },
    GlobalSetting {
        dotted: NOTIFY_NTFY_BASE_URL,
        segments: &["notifications", "ntfy", "baseUrl"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_NTFY_BASE_URL,
        kind: SettingKind::Text,
        redaction: Redaction::Url,
    },
    GlobalSetting {
        dotted: NOTIFY_NTFY_TOPIC,
        segments: &["notifications", "ntfy", "topic"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_NTFY_TOPIC,
        kind: SettingKind::Text,
        redaction: Redaction::Opaque,
    },
    GlobalSetting {
        dotted: NOTIFY_NTFY_TOKEN,
        segments: &["notifications", "ntfy", "token"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_NTFY_TOKEN,
        kind: SettingKind::Text,
        redaction: Redaction::Secret,
    },
    GlobalSetting {
        dotted: NOTIFY_WEBHOOK_URL,
        segments: &["notifications", "webhook", "url"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_WEBHOOK_URL,
        kind: SettingKind::Text,
        redaction: Redaction::Url,
    },
    GlobalSetting {
        dotted: NOTIFY_WEBHOOK_TOKEN,
        segments: &["notifications", "webhook", "token"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_WEBHOOK_TOKEN,
        kind: SettingKind::Text,
        redaction: Redaction::Secret,
    },
    GlobalSetting {
        dotted: NOTIFY_DINGTALK_WEBHOOK_URL,
        segments: &["notifications", "dingtalk", "webhookUrl"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_DINGTALK_WEBHOOK_URL,
        kind: SettingKind::Text,
        redaction: Redaction::Url,
    },
    GlobalSetting {
        dotted: NOTIFY_DINGTALK_SECRET,
        segments: &["notifications", "dingtalk", "secret"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_DINGTALK_SECRET,
        kind: SettingKind::Text,
        redaction: Redaction::Secret,
    },
    GlobalSetting {
        dotted: NOTIFY_DINGTALK_KEYWORDS,
        segments: &["notifications", "dingtalk", "keywords"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_DINGTALK_KEYWORDS,
        kind: SettingKind::List,
        redaction: Redaction::Opaque,
    },
    GlobalSetting {
        dotted: NOTIFY_DINGTALK_MESSAGE_TYPE,
        segments: &["notifications", "dingtalk", "messageType"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_DINGTALK_MSG_TYPE,
        kind: SettingKind::Choice(&["text", "markdown"]),
        redaction: Redaction::Literal,
    },
    GlobalSetting {
        dotted: NOTIFY_EMAIL_SMTP_HOST,
        segments: &["notifications", "email", "smtp", "host"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_EMAIL_SMTP_HOST,
        kind: SettingKind::Text,
        redaction: Redaction::Opaque,
    },
    GlobalSetting {
        dotted: NOTIFY_EMAIL_SMTP_PORT,
        segments: &["notifications", "email", "smtp", "port"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_EMAIL_SMTP_PORT,
        kind: SettingKind::Port,
        redaction: Redaction::Literal,
    },
    GlobalSetting {
        dotted: NOTIFY_EMAIL_SMTP_TLS,
        segments: &["notifications", "email", "smtp", "tls"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_EMAIL_TLS,
        kind: SettingKind::Choice(&["auto", "implicit", "starttls", "plain"]),
        redaction: Redaction::Literal,
    },
    GlobalSetting {
        dotted: NOTIFY_EMAIL_USERNAME,
        segments: &["notifications", "email", "username"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_EMAIL_USERNAME,
        kind: SettingKind::Text,
        redaction: Redaction::Opaque,
    },
    GlobalSetting {
        dotted: NOTIFY_EMAIL_PASSWORD,
        segments: &["notifications", "email", "password"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_EMAIL_PASSWORD,
        kind: SettingKind::Text,
        redaction: Redaction::Secret,
    },
    GlobalSetting {
        dotted: NOTIFY_EMAIL_FROM,
        segments: &["notifications", "email", "from"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_EMAIL_FROM,
        kind: SettingKind::Text,
        redaction: Redaction::Opaque,
    },
    GlobalSetting {
        dotted: NOTIFY_EMAIL_TO,
        segments: &["notifications", "email", "to"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_EMAIL_TO,
        kind: SettingKind::List,
        redaction: Redaction::Opaque,
    },
    GlobalSetting {
        dotted: NOTIFY_EMAIL_REPLY_TO,
        segments: &["notifications", "email", "replyTo"],
        legacy_name: crate::infra::storage_schema::GLOBAL_NOTIFY_EMAIL_REPLY_TO,
        kind: SettingKind::Text,
        redaction: Redaction::Opaque,
    },
];

/// The role-scoped settings. They live in the per-role entries of the
/// configuration file, so they have no machine-wide path.
pub const ROLE_SCOPED_SETTINGS: &[&str] = &[
    "PHASEGENT_PROVIDER",
    "PHASEGENT_REDMINE_CLOSE_STATUS_ID",
    "PHASEGENT_CLOSE_STATUS_ID",
];

/// The machine-wide setting at `path`.
pub fn global_setting(path: &str) -> Option<&'static GlobalSetting> {
    GLOBAL_SETTINGS
        .iter()
        .find(|setting| setting.dotted == path)
}

/// The machine-wide setting with the flat legacy name.
pub fn global_setting_by_legacy_name(name: &str) -> Option<&'static GlobalSetting> {
    GLOBAL_SETTINGS
        .iter()
        .find(|setting| setting.legacy_name == name)
}

/// The path segments of a machine-wide setting.
pub fn setting_segments(path: &str) -> Option<&'static [&'static str]> {
    global_setting(path).map(|setting| setting.segments)
}
