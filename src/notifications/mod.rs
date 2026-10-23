//! Manual-only agent notification envelopes.
//!
//! Manual `notify send` builds a [`NotificationIntent`] with a short
//! title and a bounded body, persists it via
//! [`crate::notifications::fire`] before delivery, and delivers
//! through `cloudiful-notifier`. Bounds keep local storage small and
//! provider payloads predictable.

pub mod config;
pub mod envelope;
pub mod fire;
pub mod question;

#[allow(unused_imports)]
pub use config::{NotifyChannel, NotifyConfig, is_notify_secret, is_notify_setting};
#[allow(unused_imports)]
pub use envelope::{
    NOTIFICATION_BODY_LIMIT, NOTIFICATION_TITLE_LIMIT, NotificationEvent, NotificationIntent,
    QUESTION_BODY_LIMIT, QUESTION_HTML_LIMIT_BYTES,
};
#[allow(unused_imports)]
pub use fire::fire_notification_result;
#[allow(unused_imports)]
pub use question::{
    MAX_CHOICES, MAX_QUESTIONS, QUESTION_INPUT_LIMIT_BYTES, QuestionError, QuestionNotification,
    TOTAL_TEXT_BUDGET, parse_question_input, read_question_input,
};
