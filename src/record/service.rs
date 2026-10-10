//! The record service: the whole `record create/get/list` behavior
//! expressed over the existing `IssueProvider` comment primitives.
//!
//! Key ownership, not atomic deduplication, is the contract. A `--key`
//! is a stable request token for a transport retry of one logical
//! request; reusing it for different content is a conflict, and two
//! journals claiming one key is an error rather than a guess.
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
use crate::record::store::{self, RecordFilter, StoredRecord};

const OPERATION: &str = "record create";

/// How many bounded confirmation listings run after a write (or an
/// uncertain write result) before the request is reported as unconfirmed.
/// Each listing is a normal provider read, so it already carries the
/// shared transport timeout and retry policy.
const RECOVERY_ATTEMPTS: usize = 3;

/// Create one record, or return the existing record for an identical
/// retry of the same logical request.
pub(crate) fn create<P: IssueProvider<Error = PhasegentError>>(
    provider: &P,
    issue: u64,
    spec: &RecordSpec,
    body: &str,
) -> Result<RecordOutput, PhasegentError> {
    let stored = spec.encode_body(body).map_err(PhasegentError::config)?;
    match classify(issue, spec, body, owned_by_key(provider, issue, &spec.key)?) {
        Lookup::Match(output) => Ok(output),
        Lookup::Conflict(error) => Err(error),
        Lookup::Absent => write(provider, issue, spec, body, &stored),
    }
}

/// Read one record by its native reference id.
pub(crate) fn get<P: IssueProvider<Error = PhasegentError>>(
    provider: &P,
    issue: u64,
    record: u64,
) -> Result<RecordOutput, PhasegentError> {
    let comment = provider.get_comment(issue, record)?;
    store::decode_one(issue, comment)?
        .map(|stored| stored.output)
        .ok_or_else(|| {
            PhasegentError::not_found("record get", &format!("record {record} was not found"))
        })
}

/// Every record on an issue that matches the local filters, in provider
/// order. Ordinary comments are ignored.
pub(crate) fn list<P: IssueProvider<Error = PhasegentError>>(
    provider: &P,
    issue: u64,
    filter: RecordFilter,
) -> Result<Vec<RecordOutput>, PhasegentError> {
    Ok(store::decode_all(issue, provider.list_comments(issue)?)?
        .into_iter()
        .filter(|stored| filter.matches(&stored.spec))
        .map(|stored| stored.output)
        .collect())
}

/// The outcome of reading the records that already own `key`.
enum Lookup {
    /// Nothing owns the key yet.
    Absent,
    /// Exactly one record owns the key and is byte-identical to the request.
    Match(RecordOutput),
    /// The key is already owned by something that is not this request.
    Conflict(PhasegentError),
}

fn classify(issue: u64, spec: &RecordSpec, body: &str, records: Vec<StoredRecord>) -> Lookup {
    match records.as_slice() {
        [] => Lookup::Absent,
        [record] if store::same_request(record, spec, body) => Lookup::Match(record.output.clone()),
        [_] => Lookup::Conflict(PhasegentError::request(
            OPERATION,
            format!(
                "request key '{}' already identifies a different record on issue {issue}; reuse the key only to retry the identical request",
                spec.key
            ),
        )),
        many => Lookup::Conflict(PhasegentError::request(
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
) -> Result<RecordOutput, ConfirmFailure> {
    for _ in 0..RECOVERY_ATTEMPTS {
        let records = match owned_by_key(provider, issue, &spec.key) {
            Ok(records) => records,
            // A read that itself failed stays uncertainty: it is never a
            // definitive negative and never worth a second write.
            Err(_) => return Err(ConfirmFailure::Unreadable),
        };
        match classify(issue, spec, body, records) {
            Lookup::Match(output) => return Ok(output),
            Lookup::Conflict(error) => return Err(ConfirmFailure::Conflict(error)),
            Lookup::Absent => {}
        }
    }
    Err(ConfirmFailure::Unconfirmed)
}

/// Why a write could not be confirmed by an exact read.
enum ConfirmFailure {
    /// Bounded reads never yielded exactly one equal record.
    Unconfirmed,
    /// The key is definitively owned by something that is not this request.
    Conflict(PhasegentError),
    /// A confirmation read itself failed, so the result stays unknown.
    Unreadable,
}

fn write<P: IssueProvider<Error = PhasegentError>>(
    provider: &P,
    issue: u64,
    spec: &RecordSpec,
    body: &str,
    stored: &str,
) -> Result<RecordOutput, PhasegentError> {
    // The generated envelope doubles as the write marker: it is unique
    // per request and lets the provider match the journal it just
    // created without any substring guessing by the caller.
    let marker = stored.lines().next().unwrap_or_default().to_owned();
    match provider.create_comment(issue, stored, &marker) {
        // The transport succeeded, but the journal the primitive matched
        // is not authority. Only an exact decoded read confirms the
        // record, so the created id is never adopted from this response.
        Ok(_) => match confirm(provider, issue, spec, body) {
            Ok(record) => Ok(record),
            Err(ConfirmFailure::Conflict(error)) => Err(error),
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
                Ok(record) => Ok(record),
                Err(ConfirmFailure::Conflict(conflict)) => Err(conflict),
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

/// Every record on `issue` whose request key is `key`.
fn owned_by_key<P: IssueProvider<Error = PhasegentError>>(
    provider: &P,
    issue: u64,
    key: &str,
) -> Result<Vec<StoredRecord>, PhasegentError> {
    Ok(store::decode_all(issue, provider.list_comments(issue)?)?
        .into_iter()
        .filter(|stored| stored.spec.key == key)
        .collect())
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
    use crate::policy::Role;
    use crate::record::kind::RecordKind;

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
}
