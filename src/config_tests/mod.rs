//! Focused unit tests for config command parsing and validation.

use crate::command::{self, Command, ProjectCommand};

mod index_backend;
mod migrate_parse;
mod notify_email;
mod parse_show;
mod project_list;
mod provider_default;
mod redaction;
mod set_clear_parse;
