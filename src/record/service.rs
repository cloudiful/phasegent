//! The record service: the whole `record create/get/list` behavior
//! expressed over the existing `IssueProvider` comment primitives.
//!
//! Key ownership, not atomic deduplication, is the contract. A `--key`
//! is a stable request token for a transport retry of one logical
//! request; reusing it for different content is a conflict, and two
//! journals claiming one key is an error rather than a guess.
//!
//! A read never fails because one stored comment carries a malformed
//! reserved header: that comment is shown as an invalid entry and the
//! other records stay readable. A write stays strict: a malformed
//! header whose request key cannot be established refuses the write
//! rather than claiming or overwriting a key.
//!
//! The provider's own write response is never confirmation: a legacy
//! comment primitive may match a journal by substring, so every write —
//! including one the transport reported as successful — is only accepted
//! after a bounded exact read returns exactly one decoded record whose
//! actor, kind, metadata, and body equal the request. The service never
//! issues a second write.

use crate::providers::IssueProvider;
use crate::providers::api::PhasegentError;
use crate::providers::api::RecordOutput;
use crate::record::spec::RecordSpec;
use crate::record::store::{
    self, InvalidSummary, RecordEntry, RecordFilter, RecordListEntry, StoredEntry,
};

const OPERATION: &str = "record create";

/// How many bounded confirmation listings run after a write (or an
/// uncertain write result) before the request is reported as unconfirmed.
/// Each listing is a normal provider read, so it already carries the
/// shared transport timeout and retry policy.
const RECOVERY_ATTEMPTS: usize = 3;

/// A published record plus the invalid entries that did not block it.
///
/// It derefs to the published record, so a caller reads the record fields
/// directly and reaches the invalid entries through `invalid`.
#[derive(Debug)]
pub(crate) struct CreatedRecord {
    pub(crate) output: RecordOutput,
    pub(crate) invalid: InvalidSummary,
}

impl std::ops::Deref for CreatedRecord {
    type Target = RecordOutput;

    fn deref(&self) -> &RecordOutput {
        &self.output
    }
}

/// One page of a record collection plus the invalid entries of the whole
/// collection.
pub(crate) struct RecordPage {
    pub(crate) page: crate::pagination::Paged<RecordListEntry>,
    pub(crate) invalid: InvalidSummary,
}

/// Create one record, or return the existing record for an identical
/// retry of the same logical request.
pub(crate) fn create<P: IssueProvider<Error = PhasegentError>>(
    provider: &P,
    issue: u64,
    spec: &RecordSpec,
    body: &str,
) -> Result<CreatedRecord, PhasegentError> {
    let stored = spec.encode_body(body).map_err(PhasegentError::config)?;
    let entries = store::decode_all(issue, provider.list_comments(issue)?);
    match decide(issue, spec, body, &entries) {
        Decision::Match { output, tolerated } => Ok(CreatedRecord {
            output,
            invalid: tolerated,
        }),
        Decision::Refuse(error) => Err(error),
        Decision::Write => write(provider, issue, spec, body, &stored),
    }
}

/// Read one record by its native reference id. A comment whose reserved
/// header is not a valid record is returned as an explicit invalid
/// entry; an ordinary comment is not a record at all.
pub(crate) fn get<P: IssueProvider<Error = PhasegentError>>(
    provider: &P,
    issue: u64,
    record: u64,
) -> Result<RecordEntry, PhasegentError> {
    let comment = provider.get_comment(issue, record)?;
    match store::decode_one(issue, comment) {
        Some(StoredEntry::Record(stored)) => Ok(RecordEntry::Record(stored.output)),
        Some(StoredEntry::Invalid(invalid)) => Ok(RecordEntry::Invalid(invalid.output())),
        None => Err(PhasegentError::not_found(
            "record get",
            &format!("record {record} was not found"),
        )),
    }
}

/// Every record on an issue that matches the local filters, in provider
/// order. Ordinary comments are ignored; every invalid entry is listed.
pub(crate) fn list<P: IssueProvider<Error = PhasegentError>>(
    provider: &P,
    issue: u64,
    filter: RecordFilter,
) -> Result<Vec<RecordListEntry>, PhasegentError> {
    let entries = store::decode_all(issue, provider.list_comments(issue)?);
    Ok(store::collect(entries, &filter).items)
}

pub(crate) fn list_page<P: IssueProvider<Error = PhasegentError>>(
    provider: &P,
    issue: u64,
    filter: RecordFilter,
    page: crate::pagination::PageRequest,
) -> Result<RecordPage, PhasegentError> {
    let entries = store::decode_all(issue, provider.list_comments(issue)?);
    let collection = store::collect(entries, &filter);
    Ok(RecordPage {
        page: crate::pagination::slice(collection.items, page),
        invalid: InvalidSummary::new(&collection.invalid),
    })
}

/// The decision the existing-comment scan reaches.
enum Decision {
    /// Nothing owns the key yet.
    Write,
    /// Exactly one record owns the key and is byte-identical to the request.
    Match {
        output: RecordOutput,
        tolerated: InvalidSummary,
    },
    /// The key cannot be resolved, so no write is attempted.
    Refuse(PhasegentError),
}

/// The key ownership of one issue, decided from the stored comments.
///
/// A valid record that owns the key keeps the existing replay and
/// conflict rules. An invalid entry is tolerated only when its single
/// recovered key is a different one; an invalid entry that claims the
/// requested key, or whose key cannot be established at all, refuses the
/// request before any write.
fn decide(issue: u64, spec: &RecordSpec, body: &str, entries: &[StoredEntry]) -> Decision {
    let mut owned = Vec::new();
    let mut tolerated = Vec::new();
    for entry in entries {
        match entry {
            StoredEntry::Record(record) => {
                if record.spec.key == spec.key {
                    owned.push(record);
                }
            }
            StoredEntry::Invalid(invalid) => match &invalid.recovered_key {
                Some(key) if key == &spec.key => {
                    return Decision::Refuse(PhasegentError::request(
                        OPERATION,
                        format!(
                            "request key '{}' is claimed by an invalid record on issue {issue}; its stored header is not a valid record, so the key ownership is not resolvable",
                            spec.key
                        ),
                    ));
                }
                Some(_) => tolerated.push(invalid.clone()),
                None => {
                    return Decision::Refuse(PhasegentError::request(
                        OPERATION,
                        format!(
                            "issue {issue} holds an invalid record whose request key cannot be determined; the ownership of key '{}' is unknown, so no write is attempted",
                            spec.key
                        ),
                    ));
                }
            },
        }
    }
    let tolerated = InvalidSummary::new(&tolerated);
    match owned.as_slice() {
        [] => Decision::Write,
        [record] if store::same_request(record, spec, body) => Decision::Match {
            output: record.output.clone(),
            tolerated,
        },
        [_] => Decision::Refuse(PhasegentError::request(
            OPERATION,
            format!(
                "request key '{}' already identifies a different record on issue {issue}; reuse the key only to retry the identical request",
                spec.key
            ),
        )),
        many => Decision::Refuse(PhasegentError::request(
            OPERATION,
            format!(
                "request key '{}' matches {} records on issue {issue}; resolve the duplicate before retrying",
                spec.key,
                many.len()
            ),
        )),
    }
}

/// Confirm a write by an exact decoded owned-key read. The primitive's
/// own matched journal is never authority and no second write is ever
/// issued, so this only ever reads.
fn confirm<P: IssueProvider<Error = PhasegentError>>(
    provider: &P,
    issue: u64,
    spec: &RecordSpec,
    body: &str,
) -> Result<(RecordOutput, InvalidSummary), ConfirmFailure> {
    for _ in 0..RECOVERY_ATTEMPTS {
        let entries = match provider.list_comments(issue) {
            Ok(comments) => store::decode_all(issue, comments),
            // A read that itself failed stays uncertainty: it is never a
            // definitive negative and never worth a second write.
            Err(_) => return Err(ConfirmFailure::Unreadable),
        };
        match decide(issue, spec, body, &entries) {
            Decision::Match { output, tolerated } => return Ok((output, tolerated)),
            Decision::Refuse(error) => return Err(ConfirmFailure::Refused(error)),
            Decision::Write => {}
        }
    }
    Err(ConfirmFailure::Unconfirmed)
}

/// Why a write could not be confirmed by an exact read.
enum ConfirmFailure {
    /// Bounded reads never yielded exactly one equal record.
    Unconfirmed,
    /// A confirmation read itself failed, so the result stays unknown.
    Unreadable,
    /// The stored comments leave the key ownership unresolvable.
    Refused(PhasegentError),
}

fn write<P: IssueProvider<Error = PhasegentError>>(
    provider: &P,
    issue: u64,
    spec: &RecordSpec,
    body: &str,
    stored: &str,
) -> Result<CreatedRecord, PhasegentError> {
    // The generated envelope doubles as the write marker: it is unique
    // per request and lets the provider match the journal it just
    // created without any substring guessing by the caller.
    let marker = stored.lines().next().unwrap_or_default().to_owned();
    match provider.create_comment(issue, stored, &marker) {
        // The transport succeeded, but the journal the primitive matched
        // is not authority. Only an exact decoded read confirms the
        // record, so the created id is never adopted from this response.
        Ok(_) => match confirm(provider, issue, spec, body) {
            Ok((output, invalid)) => Ok(CreatedRecord { output, invalid }),
            Err(ConfirmFailure::Refused(error)) => Err(error),
            Err(ConfirmFailure::Unconfirmed | ConfirmFailure::Unreadable) => {
                Err(unconfirmed(spec, issue))
            }
        },
        Err(error) if is_uncertain(&error) => {
            // Recover by reading only. A definitive conflict found by the
            // read is surfaced; an unconfirmed or unreadable result keeps
            // the original uncertain write error, and a second write is
            // never attempted.
            match confirm(provider, issue, spec, body) {
                Ok((output, invalid)) => Ok(CreatedRecord { output, invalid }),
                Err(ConfirmFailure::Refused(conflict)) => Err(conflict),
                Err(ConfirmFailure::Unconfirmed | ConfirmFailure::Unreadable) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

fn unconfirmed(spec: &RecordSpec, issue: u64) -> PhasegentError {
    PhasegentError::request(
        OPERATION,
        format!(
            "the write for request key '{}' on issue {issue} was not confirmed by an exact record read; no equal record was found",
            spec.key
        ),
    )
}

/// Whether a failed write may have reached the provider.
///
/// A transport failure, a response that could not be decoded, or a
/// lookup that could not find the journal the provider just wrote all
/// leave the write result unknown, so those are the ones worth
/// re-reading. A rejected request (4xx), an auth/config error, or an
/// unsupported operation never wrote anything.
fn is_uncertain(error: &PhasegentError) -> bool {
    match error {
        PhasegentError::Request { .. }
        | PhasegentError::Decode { .. }
        | PhasegentError::NotFound { .. }
        | PhasegentError::Pagination { .. } => true,
        PhasegentError::Http { status, .. } => *status >= 500,
        PhasegentError::Config(_)
        | PhasegentError::Auth(_)
        | PhasegentError::NotSupported { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Capability, Role};
    use crate::providers::api::{
        CommentOutput, IssueSearchOptions, IssueSearchResult, IssueSummary, IssueSummaryPage,
    };
    use crate::providers::{IssueProvider, ProviderCapabilities};
    use crate::record::kind::RecordKind;
    use std::cell::{Cell, RefCell};

    #[test]
    fn a_rejected_request_never_triggers_a_recovery_read() {
        assert!(!is_uncertain(&PhasegentError::config("x")));
        assert!(!is_uncertain(&PhasegentError::auth("x")));
        assert!(!is_uncertain(&PhasegentError::not_supported(
            "redmine", "x"
        )));
        assert!(!is_uncertain(&PhasegentError::Http {
            operation: "record create".to_owned(),
            status: 422,
            message: "invalid".to_owned()
        }));
    }

    #[test]
    fn an_unknown_write_result_is_recovered_by_reading() {
        assert!(is_uncertain(&PhasegentError::Http {
            operation: "record create".to_owned(),
            status: 503,
            message: "unavailable".to_owned()
        }));
        assert!(is_uncertain(&PhasegentError::not_found(
            "record create",
            "Redmine did not return the created journal"
        )));
        assert!(is_uncertain(&PhasegentError::request(
            "record create",
            "connection reset".to_owned()
        )));
    }

    #[test]
    fn the_recovery_bound_is_finite() {
        let attempts = RECOVERY_ATTEMPTS;
        assert!(attempts > 0);
        assert!(attempts <= 5, "recovery must stay bounded");
    }

    #[test]
    fn the_kind_binding_is_decided_before_the_service_runs() {
        let mut spec = RecordSpec {
            kind: RecordKind::Recon,
            actor: Role::Explore,
            key: "k1".to_owned(),
            phase: None,
            attempt: None,
            review: None,
            recon: Some("scan".to_owned()),
        };
        assert!(spec.validate().is_ok());
        spec.actor = Role::Executor;
        assert!(spec.validate().is_err());
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

    /// A reserved header that is not a valid record but carries exactly
    /// one usable top-level key.
    fn malformed_with_key(key: &str) -> String {
        format!(
            "<!-- phasegent-record {{\"v\":1,\"kind\":\"recon\",\"actor\":\"explore\",\"key\":\"{key}\"}} -->\nnote"
        )
    }

    /// A reserved header whose key cannot be established at all.
    fn malformed_without_key() -> String {
        "<!-- phasegent-record {\"v\":1,\"kind\":\"recon\",\"actor\":\"explore\"} -->\nnote"
            .to_owned()
    }

    /// The smallest in-memory provider the record service needs: the
    /// stored comments of one issue, a write counter, and the ability to
    /// store a comment that only appears once a write happened.
    ///
    /// It lives in the test module, so the production service keeps no
    /// seam for it.
    struct FakeProvider {
        comments: RefCell<Vec<CommentOutput>>,
        writes: Cell<usize>,
        /// The id the next stored comment gets.
        next_id: Cell<u64>,
        /// A comment stored by the first write, so a confirmation read
        /// can meet a stored comment that was not there before it.
        after_write: RefCell<Option<String>>,
    }

    impl Default for FakeProvider {
        fn default() -> Self {
            Self {
                comments: RefCell::new(Vec::new()),
                writes: Cell::new(0),
                next_id: Cell::new(700),
                after_write: RefCell::new(None),
            }
        }
    }

    impl FakeProvider {
        /// A provider that already holds the given stored comments.
        fn with_comments(bodies: Vec<(u64, String)>) -> Self {
            let provider = Self::default();
            {
                let mut comments = provider.comments.borrow_mut();
                for (id, body) in bodies {
                    comments.push(comment(id, &body));
                }
            }
            provider
        }

        /// A provider that stores `body` once a write happened.
        fn with_after_write(body: &str) -> Self {
            let provider = Self::default();
            *provider.after_write.borrow_mut() = Some(body.to_owned());
            provider
        }

        fn writes(&self) -> usize {
            self.writes.get()
        }
    }

    fn comment(id: u64, body: &str) -> CommentOutput {
        CommentOutput {
            id,
            html_url: Some(format!("http://example/issues/21#change-{id}")),
            marker: None,
            body: Some(body.to_owned()),
        }
    }

    /// A stored comment is handed out by value, so the fake copies it.
    fn copy(stored: &CommentOutput) -> CommentOutput {
        CommentOutput {
            id: stored.id,
            html_url: stored.html_url.clone(),
            marker: stored.marker.clone(),
            body: stored.body.clone(),
        }
    }

    impl IssueProvider for FakeProvider {
        type Error = PhasegentError;

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                issue_lifecycle: false,
                comments: true,
                project_discovery: false,
                legacy_numeric_leases: false,
            }
        }

        fn supports(&self, capability: Capability) -> bool {
            matches!(
                capability,
                Capability::CommentCreate | Capability::CommentRead | Capability::CommentFindMarker
            )
        }

        fn get_issue(&self, _number: u64) -> Result<IssueSummary, Self::Error> {
            unimplemented!("the record protocol never reads an issue")
        }

        fn search_issues(
            &self,
            _options: &IssueSearchOptions,
        ) -> Result<IssueSearchResult, Self::Error> {
            unimplemented!("the record protocol never searches issues")
        }

        fn search_issue_page(
            &self,
            _options: &IssueSearchOptions,
        ) -> Result<IssueSummaryPage, Self::Error> {
            unimplemented!("the record protocol never searches issues")
        }

        fn create_issue(&self, _title: &str, _body: &str) -> Result<IssueSummary, Self::Error> {
            unimplemented!("the record protocol never creates an issue")
        }

        fn update_body(&self, _number: u64, _body: &str) -> Result<IssueSummary, Self::Error> {
            unimplemented!("the record protocol never updates an issue")
        }

        fn close_issue(&self, _number: u64) -> Result<IssueSummary, Self::Error> {
            unimplemented!("the record protocol never closes an issue")
        }

        fn create_comment(
            &self,
            _issue: u64,
            body: &str,
            _marker: &str,
        ) -> Result<CommentOutput, Self::Error> {
            self.writes.set(self.writes.get() + 1);
            let mut comments = self.comments.borrow_mut();
            let id = self.next_id.get();
            self.next_id.set(id + 1);
            comments.push(comment(id, body));
            if let Some(after) = self.after_write.borrow_mut().take() {
                comments.push(comment(id + 1, &after));
            }
            Ok(comment(999, "unrelated primitive response"))
        }

        fn get_comment(&self, _issue: u64, comment: u64) -> Result<CommentOutput, Self::Error> {
            self.comments
                .borrow()
                .iter()
                .find(|stored| stored.id == comment)
                .map(copy)
                .ok_or_else(|| PhasegentError::not_found("record get", "no such comment"))
        }

        fn find_marker(&self, _issue: u64, _marker: &str) -> Result<CommentOutput, Self::Error> {
            unimplemented!("the record protocol never looks a marker up")
        }

        fn list_comments(&self, _issue: u64) -> Result<Vec<CommentOutput>, Self::Error> {
            Ok(self.comments.borrow().iter().map(copy).collect())
        }

        fn list_comments_page(
            &self,
            _issue: u64,
            _page: crate::pagination::PageRequest,
        ) -> Result<crate::pagination::Paged<CommentOutput>, Self::Error> {
            unimplemented!("the record service never pages the provider")
        }
    }

    #[test]
    fn an_unrelated_invalid_record_permits_one_write_and_confirms_it() {
        let provider = FakeProvider::with_comments(vec![(501, malformed_with_key("unrelated"))]);
        let created = create(&provider, 21, &spec(), "note").expect("the write is permitted");
        assert_eq!(provider.writes(), 1, "exactly one write");
        // The record is adopted from the confirming read, not from the
        // primitive's own response.
        assert_eq!(created.id, 700);
        assert_eq!(created.body, "note");
        assert_eq!(created.key, "k1");
        // The tolerated invalid entry is reported next to the write.
        assert_eq!(created.invalid.count(), 1);
    }

    #[test]
    fn a_replay_with_an_unrelated_invalid_record_writes_nothing() {
        let stored = spec().encode_body("note").unwrap();
        let provider =
            FakeProvider::with_comments(vec![(501, stored), (502, malformed_with_key("k9"))]);
        let replayed =
            create(&provider, 21, &spec(), "note").expect("the replay returns the record");
        assert_eq!(provider.writes(), 0, "a replay never writes");
        assert_eq!(replayed.id, 501);
        assert_eq!(replayed.invalid.count(), 1);
    }

    #[test]
    fn a_same_key_invalid_record_refuses_the_write() {
        let provider = FakeProvider::with_comments(vec![(501, malformed_with_key("k1"))]);
        let error = create(&provider, 21, &spec(), "note")
            .expect_err("an invalid record that claims the key must refuse");
        assert_eq!(provider.writes(), 0, "a refused request never writes");
        let message = error.to_string();
        assert!(message.contains("k1"), "{message}");
        assert!(message.contains("invalid record"), "{message}");
    }

    #[test]
    fn a_same_key_invalid_record_refuses_a_replay() {
        let stored = spec().encode_body("note").unwrap();
        // The identical record would be a replay, but the invalid entry
        // claims the same key, so the ownership stays unresolvable.
        let provider =
            FakeProvider::with_comments(vec![(501, stored), (502, malformed_with_key("k1"))]);
        let error = create(&provider, 21, &spec(), "note")
            .expect_err("the unresolvable key must refuse the replay too");
        assert_eq!(provider.writes(), 0, "a refused replay never writes");
        assert!(error.to_string().contains("invalid record"), "{error}");
    }

    #[test]
    fn an_unknown_key_invalid_record_refuses_the_write() {
        for body in [
            malformed_without_key(),
            "<!-- phasegent-record {".to_owned(),
            "<!-- phasegent-record {\"key\":\"other\",\"key\":\"k1\"} -->".to_owned(),
        ] {
            let provider = FakeProvider::with_comments(vec![(501, body.clone())]);
            let error = create(&provider, 21, &spec(), "note")
                .expect_err("an unknown key ownership must refuse");
            assert_eq!(provider.writes(), 0, "a refused request never writes");
            assert!(
                error.to_string().contains("cannot be determined"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_duplicate_key_refuses_the_write() {
        let stored = spec().encode_body("note").unwrap();
        let provider = FakeProvider::with_comments(vec![(501, stored.clone()), (502, stored)]);
        let error =
            create(&provider, 21, &spec(), "note").expect_err("a duplicated key must refuse");
        assert_eq!(provider.writes(), 0, "a refused request never writes");
        assert!(error.to_string().contains("matches 2 records"), "{error}");
    }

    #[test]
    fn a_blocker_found_after_the_write_is_not_reported_as_unconfirmed() {
        // The blocker only appears once the write happened, so the
        // confirmation read is the one that meets it.
        let provider = FakeProvider::with_after_write(&malformed_with_key("k1"));
        let error = create(&provider, 21, &spec(), "note")
            .expect_err("the discovered ownership blocker must surface");
        assert_eq!(provider.writes(), 1, "the write itself happened");
        let message = error.to_string();
        assert!(message.contains("invalid record"), "{message}");
        assert!(
            !message.contains("was not confirmed by an exact record read"),
            "a discovered blocker must not be masked as an unconfirmed write: {message}"
        );
    }
}
