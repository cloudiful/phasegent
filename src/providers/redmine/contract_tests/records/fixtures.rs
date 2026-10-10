//! Shared fixtures for the Redmine record wire contract.
//!
//! `spec` builds the request metadata and `stored` renders the exact body
//! Redmine persists: the CLI-generated envelope followed by the agent's
//! plain note. Both are used across the split scenario modules.

use crate::policy::Role;
use crate::record::{RecordKind, RecordSpec};

pub(super) fn spec(kind: RecordKind, actor: Role, key: &str) -> RecordSpec {
    match kind {
        RecordKind::Executor | RecordKind::Reviewer => RecordSpec {
            kind,
            actor,
            key: key.to_owned(),
            phase: Some("P1".to_owned()),
            attempt: Some(2),
            // A reviewer record always carries its scope.
            review: (kind == RecordKind::Reviewer).then(|| "final".to_owned()),
            recon: None,
        },
        RecordKind::Recon => RecordSpec {
            kind,
            actor,
            key: key.to_owned(),
            phase: None,
            attempt: None,
            review: None,
            recon: Some("scan".to_owned()),
        },
    }
}

/// The journal body Redmine stores: the CLI-generated envelope followed by
/// the agent's plain note.
pub(super) fn stored(spec: &RecordSpec, body: &str) -> String {
    spec.encode_body(body).unwrap()
}
