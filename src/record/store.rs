//! Decoding provider comments into records, and the local filters a
//! `record list` applies over them.
//!
//! Ordinary comments are ignored; a reserved envelope that fails to
//! decode is kept as an invalid entry instead of failing the whole read,
//! so one malformed header never hides the other records on an issue.
//! Nothing here trusts a substring of a raw body.

use crate::providers::api::{CommentOutput, InvalidRecordOutput, RecordErrorOutput, RecordOutput};
use crate::record::header::{self, DecodedRecord};
use crate::record::kind::RecordKind;
use crate::record::spec::RecordSpec;
use serde::Serialize;

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

/// One stored comment on an issue: a decoded record, or a reserved header
/// that is not a valid record.
#[derive(Debug, Clone)]
pub(crate) enum StoredEntry {
    /// A comment whose reserved header decoded into a valid record. The
    /// record is boxed to keep the enum small.
    Record(Box<StoredRecord>),
    /// A comment whose reserved header did not decode.
    Invalid(InvalidRecord),
}

/// A reserved-header comment that is not a valid record.
///
/// The raw stored body is kept so the entry can be shown as it is, and
/// the request key is recovered only when exactly one usable top-level
/// key exists. An invalid entry is never record metadata.
#[derive(Debug, Clone)]
pub(crate) struct InvalidRecord {
    pub(crate) id: u64,
    pub(crate) html_url: Option<String>,
    pub(crate) issue: u64,
    pub(crate) raw: String,
    pub(crate) message: String,
    /// The request key recovered from the malformed header, when exactly
    /// one usable top-level key exists.
    pub(crate) recovered_key: Option<String>,
}

impl InvalidRecord {
    /// The explicit invalid view of this entry.
    pub(crate) fn output(&self) -> InvalidRecordOutput {
        InvalidRecordOutput {
            status: "invalid",
            id: self.id,
            html_url: self.html_url.clone(),
            issue: self.issue,
            raw: self.raw.clone(),
            error: RecordErrorOutput {
                kind: "decode",
                operation: "record read",
                message: self.message.clone(),
            },
        }
    }
}

/// One read result: a decoded record, or an invalid reserved header.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum RecordEntry {
    /// A valid record, shown exactly as `record get` has always shown it.
    Record(RecordOutput),
    /// A reserved-header comment that is not a valid record.
    Invalid(InvalidRecordOutput),
}

impl RecordEntry {
    /// The decoded record, when the entry is a valid record.
    #[cfg(test)]
    pub(crate) fn record(&self) -> Option<&RecordOutput> {
        match self {
            Self::Record(record) => Some(record),
            Self::Invalid(_) => None,
        }
    }
}

/// One item of a `record list` collection.
///
/// A valid record keeps every record field and adds the `status`
/// discriminator; an invalid entry carries only its identity, the stored
/// body, and the decode failure.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(crate) enum RecordListEntry {
    /// A valid record.
    Record {
        status: &'static str,
        #[serde(flatten)]
        record: RecordOutput,
    },
    /// A reserved-header comment that is not a valid record.
    Invalid(InvalidRecordOutput),
}

impl RecordListEntry {
    /// The decoded record, when the item is a valid record.
    #[cfg(test)]
    pub(crate) fn record(&self) -> Option<&RecordOutput> {
        match self {
            Self::Record { record, .. } => Some(record),
            Self::Invalid(_) => None,
        }
    }
}

/// The invalid entries of one collection, summarized for a bounded
/// warning. Only the identity of an entry is kept: a warning never
/// repeats raw comment content.
#[derive(Debug, Default, Clone)]
pub(crate) struct InvalidSummary {
    ids: Vec<u64>,
    links: Vec<Option<String>>,
}

impl InvalidSummary {
    /// The bounded summary of the invalid entries.
    pub(crate) fn new(entries: &[InvalidRecord]) -> Self {
        Self {
            ids: entries.iter().map(|entry| entry.id).collect(),
            links: entries.iter().map(|entry| entry.html_url.clone()).collect(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub(crate) fn count(&self) -> usize {
        self.ids.len()
    }

    /// One bounded line naming the invalid entries. The raw stored bodies
    /// stay in the entries, never in the warning.
    pub(crate) fn message(&self) -> String {
        const SHOWN: usize = 5;
        let count = self.ids.len();
        let shown = count.min(SHOWN);
        let ids = self.ids[..shown]
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        let mut message = format!(
            "{count} invalid record {} on this issue {} not {}: {ids}",
            if count == 1 { "entry" } else { "entries" },
            if count == 1 { "is" } else { "are" },
            if count == 1 {
                "a valid record"
            } else {
                "valid records"
            },
        );
        if shown < count {
            message.push_str(&format!(" and {} more", count - shown));
        }
        let links = self.links[..shown]
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        if !links.is_empty() {
            message.push_str(&format!(" ({})", links.join(", ")));
        }
        message
    }
}

/// The items and the invalid entries of one filtered collection.
pub(crate) struct Collection {
    /// The valid records that match the filter and every invalid entry,
    /// in provider order.
    pub(crate) items: Vec<RecordListEntry>,
    /// The invalid entries of the whole collection.
    pub(crate) invalid: Vec<InvalidRecord>,
}

/// Decode every comment on an issue into the entries it owns, dropping
/// ordinary comments in provider order. A reserved header that does not
/// decode is kept as an invalid entry instead of failing the read.
pub(crate) fn decode_all(issue: u64, comments: Vec<CommentOutput>) -> Vec<StoredEntry> {
    comments
        .into_iter()
        .filter_map(|comment| decode_one(issue, comment))
        .collect()
}

/// Decode one comment, or `None` when it is an ordinary comment.
pub(crate) fn decode_one(issue: u64, comment: CommentOutput) -> Option<StoredEntry> {
    let stored = comment.body.as_deref()?;
    match header::decode_body(stored) {
        Ok(Some(decoded)) => {
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
            Some(StoredEntry::Record(Box::new(StoredRecord { spec, output })))
        }
        Ok(None) => None,
        Err(message) => Some(StoredEntry::Invalid(InvalidRecord {
            id: comment.id,
            html_url: comment.html_url,
            issue,
            raw: stored.to_owned(),
            message,
            recovered_key: header::recover_key(stored),
        })),
    }
}

/// The filtered items of one collection. A valid record obeys the
/// filters; an invalid entry is always listed, because its metadata is
/// not filterable.
pub(crate) fn collect(entries: Vec<StoredEntry>, filter: &RecordFilter) -> Collection {
    let mut items = Vec::new();
    let mut invalid = Vec::new();
    for entry in entries {
        match entry {
            StoredEntry::Record(stored) => {
                if filter.matches(&stored.spec) {
                    items.push(RecordListEntry::Record {
                        status: "record",
                        record: stored.output,
                    });
                }
            }
            StoredEntry::Invalid(entry) => {
                items.push(RecordListEntry::Invalid(entry.output()));
                invalid.push(entry);
            }
        }
    }
    Collection { items, invalid }
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
        );
        assert_eq!(decoded.len(), 1);
        let StoredEntry::Record(record) = &decoded[0] else {
            panic!("the valid record must decode");
        };
        let record = &record.output;
        assert_eq!(record.id, 2);
        assert_eq!(record.body, "note");
        assert_eq!(record.issue, 1);
        assert_eq!(record.kind, "executor");
        assert!(record.html_url.as_deref().unwrap().ends_with("#change-2"));
    }

    #[test]
    fn a_malformed_reserved_header_is_kept_as_an_invalid_entry() {
        let decoded = decode_all(1, vec![comment(1, Some("<!-- phasegent-record {"))]);
        assert_eq!(decoded.len(), 1);
        let StoredEntry::Invalid(invalid) = &decoded[0] else {
            panic!("a malformed header must be an invalid entry");
        };
        assert_eq!(invalid.id, 1);
        assert_eq!(invalid.raw, "<!-- phasegent-record {");
        assert!(!invalid.message.is_empty());
        // An unterminated header owns no key.
        assert_eq!(invalid.recovered_key, None);
    }

    #[test]
    fn a_comment_without_a_body_is_never_a_record() {
        assert!(decode_one(1, comment(1, None)).is_none());
    }

    #[test]
    fn request_identity_is_exact() {
        let stored = spec().encode_body("note").unwrap();
        let decoded = decode_all(1, vec![comment(1, Some(&stored))]);
        let StoredEntry::Record(record) = &decoded[0] else {
            panic!("the valid record must decode");
        };
        assert!(same_request(record, &spec(), "note"));
        assert!(!same_request(record, &spec(), "other note"));
        let mut other = spec();
        other.attempt = Some(2);
        assert!(!same_request(record, &other, "note"));
    }

    #[test]
    fn an_invalid_entry_carries_no_trusted_record_metadata() {
        let stored = "<!-- phasegent-record {\"v\":1,\"kind\":\"recon\",\"actor\":\"explore\",\"key\":\"k1\"} -->\nnote";
        let decoded = decode_all(1, vec![comment(7, Some(stored))]);
        let StoredEntry::Invalid(invalid) = &decoded[0] else {
            panic!("the malformed header must be an invalid entry");
        };
        // The single usable key is recovered for ownership decisions only.
        assert_eq!(invalid.recovered_key.as_deref(), Some("k1"));
        let output = serde_json::to_value(invalid.output()).unwrap();
        assert_eq!(output["status"], "invalid");
        assert_eq!(output["id"], 7);
        assert_eq!(output["issue"], 1);
        assert_eq!(output["raw"], stored);
        assert_eq!(output["error"]["kind"], "decode");
        assert_eq!(output["error"]["operation"], "record read");
        assert!(!output["error"]["message"].as_str().unwrap().is_empty());
        for field in [
            "kind", "actor", "key", "phase", "attempt", "review", "recon", "body",
        ] {
            assert!(
                output.get(field).is_none(),
                "an invalid entry must not carry {field}: {output}"
            );
        }
    }

    #[test]
    fn a_collection_keeps_the_valid_records_and_every_invalid_entry() {
        let stored = spec().encode_body("note").unwrap();
        let mut other = spec();
        other.kind = RecordKind::Recon;
        other.actor = Role::Explore;
        other.key = "k2".to_owned();
        other.phase = None;
        other.attempt = None;
        other.recon = Some("scan".to_owned());
        let recon = other.encode_body("scan").unwrap();
        let entries = decode_all(
            1,
            vec![
                comment(1, Some(&stored)),
                comment(2, Some("<!-- phasegent-record {")),
                comment(3, Some(&recon)),
            ],
        );
        let collection = collect(entries, &RecordFilter::default());
        assert_eq!(collection.items.len(), 3);
        assert_eq!(collection.invalid.len(), 1);
        assert_eq!(collection.invalid[0].id, 2);
        // The provider order is kept.
        let ids = collection
            .items
            .iter()
            .map(|item| match item {
                RecordListEntry::Record { record, .. } => record.id,
                RecordListEntry::Invalid(invalid) => invalid.id,
            })
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn a_filter_keeps_the_valid_matches_and_every_invalid_entry() {
        let stored = spec().encode_body("note").unwrap();
        let entries = decode_all(
            1,
            vec![
                comment(1, Some(&stored)),
                comment(2, Some("<!-- phasegent-record {")),
            ],
        );
        let collection = collect(
            entries,
            &RecordFilter {
                kind: Some(RecordKind::Reviewer),
                phase: None,
                recon: None,
            },
        );
        // The valid executor record is filtered out; the invalid entry is
        // not filterable, so it stays.
        assert_eq!(collection.items.len(), 1);
        assert_eq!(collection.invalid.len(), 1);
    }

    #[test]
    fn the_invalid_summary_names_the_entries_without_their_content() {
        let entries = decode_all(
            1,
            vec![
                comment(1, Some("<!-- phasegent-record {")),
                comment(2, Some("<!-- phasegent-record {")),
            ],
        );
        let collection = collect(entries, &RecordFilter::default());
        let summary = InvalidSummary::new(&collection.invalid);
        assert_eq!(summary.count(), 2);
        assert!(!summary.is_empty());
        let message = summary.message();
        assert!(message.contains('1') && message.contains('2'), "{message}");
        assert!(
            !message.contains("phasegent-record"),
            "a warning must not repeat the raw content: {message}"
        );
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
