//! Structured agent records.
//!
//! A record is an ordinary provider comment whose body is one CLI-owned,
//! versioned JSON envelope followed by the agent's plain note. Agents
//! supply metadata and prose; the CLI owns the header, so a record can
//! be read back as structured fields and referenced by its native id
//! without parsing prose.
//!
//! The pieces are deliberately separate: [`kind`] and [`spec`] own the
//! request vocabulary and its authorization rules, [`header`] owns the
//! wire envelope, [`store`] owns decoding and listing filters, and
//! [`service`] owns key ownership, conflict detection, and the bounded
//! recovery that follows an uncertain write.

mod header;
mod kind;
mod service;
mod spec;
mod store;
mod token;

pub(crate) use kind::RecordKind;
pub(crate) use service::{create, get, list};
pub(crate) use spec::RecordSpec;
pub(crate) use store::RecordFilter;

/// Decode a stored body into its record metadata, or `None` for an
/// ordinary comment. Exposed so a provider that stores structured
/// columns can bind them at write time from the same CLI-owned envelope
/// the read path decodes.
pub(crate) fn decode_stored_body(stored: &str) -> Result<Option<RecordSpec>, String> {
    Ok(header::decode_body(stored)?.map(|decoded| decoded.spec))
}
