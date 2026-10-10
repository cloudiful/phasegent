//! Parsed `record` subcommands.
//!
//! The AST is intentionally small: agents supply metadata and a plain
//! body, and every validation of a flag combination happens in
//! [`crate::record::RecordSpec`] so the parser stays a pure syntax
//! layer.

use crate::record::RecordKind;

#[derive(Debug)]
pub enum RecordCommand {
    Create {
        issue: u64,
        kind: RecordKind,
        /// Stable request key. Reused only to retry one identical
        /// logical request; a new attempt must use a new key.
        key: String,
        phase: Option<String>,
        attempt: Option<u32>,
        review: Option<String>,
        recon: Option<String>,
        body: String,
        /// Optional one-shot file input (`--body-file`), mutually exclusive
        /// with `--body` and deleted after a confirmed write unless
        /// `--keep-body-file` was supplied.
        body_file: Option<String>,
        keep_body_file: bool,
        authorized: bool,
    },
    Get {
        issue: u64,
        record: u64,
    },
    List {
        issue: u64,
        kind: Option<RecordKind>,
        phase: Option<String>,
        recon: Option<String>,
    },
}
