//! Decoding provider comments into records, and the local filters a
//! `record list` applies over them.
//!
//! Ordinary comments are ignored; a reserved envelope that fails to
//! decode is an error. Nothing here trusts a substring of a raw body.

use crate::providers::api::PhasegentError;
use crate::providers::api::{CommentOutput, RecordOutput};
use crate::record::header::{self, DecodedRecord};
use crate::record::kind::RecordKind;
use crate::record::spec::RecordSpec;

/// Local `record list` filters. Every filter is matched exactly.
#[derive(Debug, Default, Clone)]
pub(crate) struct RecordFilter {
    pub(crate) kind: Option<RecordKind>,
    pub(crate) phase: Option<String>,
    pub(crate) recon: Option<String>,
}

impl RecordFilter {
    pub(crate) fn matches(&self, spec: &RecordSpec) -> bool {
        self.kind.is_none_or(|kind| kind == spec.kind)
            && self
                .phase
                .as_deref()
                .is_none_or(|phase| spec.phase.as_deref() == Some(phase))
            && self
                .recon
                .as_deref()
                .is_none_or(|recon| spec.recon.as_deref() == Some(recon))
    }
}

/// One decoded record: the validated header metadata and the plain body
/// plus the native reference the provider reported.
#[derive(Debug, Clone)]
pub(crate) struct StoredRecord {
    pub(crate) spec: RecordSpec,
    pub(crate) output: RecordOutput,
}

/// Decode every comment on an issue into the records it owns, dropping
/// ordinary comments in provider order.
pub(crate) fn decode_all(
    issue: u64,
    comments: Vec<CommentOutput>,
) -> Result<Vec<StoredRecord>, PhasegentError> {
    comments
        .into_iter()
        .filter_map(|comment| decode_one(issue, comment).transpose())
        .collect()
}

/// Decode one comment, or `Ok(None)` when it is an ordinary comment.
pub(crate) fn decode_one(
    issue: u64,
    comment: CommentOutput,
) -> Result<Option<StoredRecord>, PhasegentError> {
    let Some(stored) = comment.body.as_deref() else {
        return Ok(None);
    };
    let decoded = header::decode_body(stored).map_err(|message| PhasegentError::Decode {
        operation: "record read".to_owned(),
        message,
    })?;
    let Some(decoded) = decoded else {
        return Ok(None);
    };
    let DecodedRecord { spec, body } = decoded;
    let output = RecordOutput {
        id: comment.id,
        html_url: comment.html_url,
        issue,
        kind: spec.kind.as_str().to_owned(),
        actor: spec.actor_name().to_owned(),
        key: spec.key.clone(),
        phase: spec.phase.clone(),
        attempt: spec.attempt,
        review: spec.review.clone(),
        recon: spec.recon.clone(),
        body,
    };
    Ok(Some(StoredRecord { spec, output }))
}

/// Whether a decoded record is byte-identical to the request the caller
/// just made. Only an exact match is a retry of the same logical
/// request; anything else sharing the key is a conflict.
pub(crate) fn same_request(record: &StoredRecord, spec: &RecordSpec, body: &str) -> bool {
    record.spec == *spec && record.output.body == body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Role;

    fn comment(id: u64, body: Option<&str>) -> CommentOutput {
        CommentOutput {
            id,
            html_url: Some(format!("http://example/issues/1#change-{id}")),
            marker: None,
            body: body.map(str::to_owned),
        }
    }

    fn spec() -> RecordSpec {
        RecordSpec {
            kind: RecordKind::Executor,
            actor: Role::Executor,
            key: "k1".to_owned(),
            phase: Some("P1".to_owned()),
            attempt: Some(1),
            review: None,
            recon: None,
        }
    }

    #[test]
    fn ordinary_comments_are_ignored_and_records_are_decoded() {
        let stored = spec().encode_body("note").unwrap();
        let decoded = decode_all(
            1,
            vec![comment(1, Some("plain")), comment(2, Some(&stored))],
        )
        .unwrap();
        assert_eq!(decoded.len(), 1);
        let record = &decoded[0].output;
        assert_eq!(record.id, 2);
        assert_eq!(record.body, "note");
        assert_eq!(record.issue, 1);
        assert_eq!(record.kind, "executor");
        assert!(record.html_url.as_deref().unwrap().ends_with("#change-2"));
    }

    #[test]
    fn a_malformed_reserved_header_errors_instead_of_being_ignored() {
        assert!(decode_all(1, vec![comment(1, Some("<!-- phasegent-record {"))]).is_err());
    }

    #[test]
    fn a_comment_without_a_body_is_never_a_record() {
        assert!(decode_one(1, comment(1, None)).unwrap().is_none());
    }

    #[test]
    fn request_identity_is_exact() {
        let stored = spec().encode_body("note").unwrap();
        let record = decode_all(1, vec![comment(1, Some(&stored))])
            .unwrap()
            .remove(0);
        assert!(same_request(&record, &spec(), "note"));
        assert!(!same_request(&record, &spec(), "other note"));
        let mut other = spec();
        other.attempt = Some(2);
        assert!(!same_request(&record, &other, "note"));
    }

    #[test]
    fn filters_match_exactly() {
        let mut recon = RecordSpec {
            kind: RecordKind::Recon,
            actor: Role::Explore,
            key: "k1".to_owned(),
            phase: None,
            attempt: None,
            review: None,
            recon: Some("scan".to_owned()),
        };
        assert!(RecordFilter::default().matches(&recon));
        assert!(
            RecordFilter {
                kind: Some(RecordKind::Recon),
                phase: None,
                recon: Some("scan".to_owned()),
            }
            .matches(&recon)
        );
        assert!(
            !RecordFilter {
                kind: Some(RecordKind::Executor),
                phase: None,
                recon: None,
            }
            .matches(&recon)
        );
        assert!(
            !RecordFilter {
                kind: None,
                phase: Some("P1".to_owned()),
                recon: None,
            }
            .matches(&recon)
        );
        assert!(
            !RecordFilter {
                kind: None,
                phase: None,
                recon: Some("other".to_owned()),
            }
            .matches(&recon)
        );
        recon.recon = None;
        assert!(
            !RecordFilter {
                kind: None,
                phase: None,
                recon: Some("scan".to_owned()),
            }
            .matches(&recon)
        );
    }
}
