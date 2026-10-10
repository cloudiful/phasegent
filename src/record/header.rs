//! The CLI-owned record envelope.
//!
//! Exactly one versioned JSON envelope starts at byte 0 of a record's
//! stored body; the agent supplies only the plain note body after it.
//! Parsing is exact — a reserved prefix at byte 0 is either a complete,
//! valid envelope or an error — so no substring match can ever promote
//! untrusted prose to header authority.

use crate::policy::Role;
use crate::record::kind::RecordKind;
use crate::record::spec::RecordSpec;
use serde::{Deserialize, Serialize};

/// The literal every generated envelope starts with. A user body that
/// begins with it is rejected rather than concatenated, so the header is
/// always CLI-owned.
pub(crate) const HEADER_PREFIX: &str = "<!-- phasegent-record ";

/// The envelope version this build writes and the only version it reads.
pub(crate) const HEADER_VERSION: u32 = 1;

const HEADER_SUFFIX: &str = " -->";

/// On-the-wire envelope. Field order is the declaration order, so
/// `serde_json` emits a stable serializer for identical metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RecordHeader {
    pub(crate) v: u32,
    pub(crate) kind: RecordKindWire,
    pub(crate) actor: String,
    pub(crate) key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) phase: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) review: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recon: Option<String>,
}

/// Wire form of [`RecordKind`]. Serializes as the same lowercase token
/// the CLI accepts on `--kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RecordKindWire {
    Executor,
    Reviewer,
    Recon,
}

impl From<RecordKind> for RecordKindWire {
    fn from(kind: RecordKind) -> Self {
        match kind {
            RecordKind::Executor => Self::Executor,
            RecordKind::Reviewer => Self::Reviewer,
            RecordKind::Recon => Self::Recon,
        }
    }
}

impl From<RecordKindWire> for RecordKind {
    fn from(kind: RecordKindWire) -> Self {
        match kind {
            RecordKindWire::Executor => Self::Executor,
            RecordKindWire::Reviewer => Self::Reviewer,
            RecordKindWire::Recon => Self::Recon,
        }
    }
}

/// A decoded header plus the plain body that followed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedRecord {
    pub(crate) spec: RecordSpec,
    pub(crate) body: String,
}

/// Whether `body` opens with the reserved envelope prefix. Only a body
/// that does is ever decoded; everything else is an ordinary comment.
pub(crate) fn has_reserved_prefix(body: &str) -> bool {
    body.starts_with(HEADER_PREFIX)
}

impl RecordSpec {
    /// Render the complete stored body: the generated envelope followed
    /// by the agent's plain note body.
    ///
    /// The field rules are enforced here too, so this build can never
    /// persist an envelope its own decoder would reject — for example a
    /// reviewer record without its mandatory scope.
    pub(crate) fn encode_body(&self, body: &str) -> Result<String, String> {
        self.validate_fields()?;
        if has_reserved_prefix(body) {
            return Err("record body must not begin with the reserved record header".to_owned());
        }
        let header = RecordHeader {
            v: HEADER_VERSION,
            kind: self.kind.into(),
            actor: self.actor_name().to_owned(),
            key: self.key.clone(),
            phase: self.phase.clone(),
            attempt: self.attempt,
            review: self.review.clone(),
            recon: self.recon.clone(),
        };
        let json = serde_json::to_string(&header)
            .map_err(|error| format!("record header could not be serialized: {error}"))?;
        Ok(format!("{HEADER_PREFIX}{json}{HEADER_SUFFIX}\n{body}"))
    }
}

/// Split a stored body into its envelope and plain body.
///
/// Returns `Ok(None)` for an ordinary comment. A body that starts with
/// the reserved prefix but is not a complete, valid envelope is an
/// error: silently ignoring it would let a corrupted or hand-written
/// record read back as ordinary prose.
pub(crate) fn decode_body(stored: &str) -> Result<Option<DecodedRecord>, String> {
    if !has_reserved_prefix(stored) {
        return Ok(None);
    }
    let payload = &stored[HEADER_PREFIX.len()..];
    let end = payload
        .find("-->")
        .ok_or_else(|| "record header is not terminated".to_owned())?;
    let json = payload[..end].trim_end_matches(' ');
    let header: RecordHeader = serde_json::from_str(json)
        .map_err(|error| format!("record header is malformed: {error}"))?;
    let rest = &payload[end + "-->".len()..];
    let body = rest.strip_prefix('\n').unwrap_or(rest);
    if header.v != HEADER_VERSION {
        return Err(format!(
            "unsupported record header version {}; expected {HEADER_VERSION}",
            header.v
        ));
    }
    let spec = RecordSpec {
        kind: header.kind.into(),
        actor: header
            .actor
            .parse::<Role>()
            .map_err(|error| format!("record header actor is invalid: {error}"))?,
        key: header.key,
        phase: header.phase,
        attempt: header.attempt,
        review: header.review,
        recon: header.recon,
    };
    // Field rules only: the actor binding is a create-time authorization
    // decision, and an orchestrator-authored executor record is valid.
    spec.validate_fields()
        .map_err(|error| format!("record header is inconsistent: {error}"))?;
    Ok(Some(DecodedRecord {
        spec,
        body: body.to_owned(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> RecordSpec {
        RecordSpec {
            kind: RecordKind::Executor,
            actor: Role::Executor,
            key: "issue754-P1-a1".to_owned(),
            phase: Some("P1".to_owned()),
            attempt: Some(2),
            review: None,
            recon: None,
        }
    }

    #[test]
    fn round_trips_through_the_envelope() {
        let stored = spec().encode_body("plain note body").unwrap();
        assert!(stored.starts_with(HEADER_PREFIX));
        let decoded = decode_body(&stored).unwrap().unwrap();
        assert_eq!(decoded.spec, spec());
        assert_eq!(decoded.body, "plain note body");
    }

    // A stored header records who wrote it, not who may write that kind:
    // the orchestrator publishes executor and reviewer records too.
    #[test]
    fn an_orchestrator_authored_header_decodes() {
        let mut authored = spec();
        authored.actor = Role::Orchestrator;
        let stored = authored.encode_body("note").unwrap();
        let decoded = decode_body(&stored).unwrap().unwrap();
        assert_eq!(decoded.spec.actor, Role::Orchestrator);
        assert_eq!(decoded.spec.kind, RecordKind::Executor);
    }

    #[test]
    fn the_serializer_is_stable_for_identical_metadata() {
        assert_eq!(
            spec().encode_body("b").unwrap(),
            spec().encode_body("b").unwrap()
        );
    }

    #[test]
    fn omits_absent_optional_fields() {
        let stored = spec().encode_body("b").unwrap();
        assert!(!stored.contains("\"review\""));
        assert!(!stored.contains("\"recon\""));
        assert!(stored.contains("\"attempt\":2"));
    }

    #[test]
    fn an_ordinary_comment_is_never_decoded() {
        assert_eq!(decode_body("just a note").unwrap(), None);
        assert_eq!(decode_body("").unwrap(), None);
        assert_eq!(decode_body("text <!-- ai-executor --> more").unwrap(), None);
        assert_eq!(
            decode_body("<!-- ai-executor issue=1 phase=P1 attempt=1 marker=m -->").unwrap(),
            None
        );
    }

    #[test]
    fn a_reserved_prefix_that_is_not_an_envelope_is_an_error() {
        for stored in [
            "<!-- phasegent-record ",
            "<!-- phasegent-record {}",
            "<!-- phasegent-record not-json -->",
            "<!-- phasegent-record {\"v\":1} -->",
        ] {
            assert!(decode_body(stored).is_err(), "{stored:?} must error");
        }
    }

    #[test]
    fn rejects_an_unsupported_version_and_an_unknown_actor() {
        let future = HEADER_PREFIX.to_owned()
            + r#"{"v":99,"kind":"executor","actor":"executor","key":"k"}"#
            + HEADER_SUFFIX;
        assert!(decode_body(&future).unwrap_err().contains("version"));
        let bad_actor = HEADER_PREFIX.to_owned()
            + r#"{"v":1,"kind":"executor","actor":"wizard","key":"k"}"#
            + HEADER_SUFFIX;
        assert!(decode_body(&bad_actor).unwrap_err().contains("actor"));
    }

    #[test]
    fn rejects_a_user_body_that_starts_with_the_reserved_prefix() {
        let error = spec()
            .encode_body("<!-- phasegent-record sneaky -->")
            .unwrap_err();
        assert!(error.contains("reserved record header"));
    }

    #[test]
    fn preserves_an_empty_and_a_multiline_body() {
        let empty = spec().encode_body("").unwrap();
        assert_eq!(decode_body(&empty).unwrap().unwrap().body, "");
        let multi = spec().encode_body("line one\nline two\n").unwrap();
        assert_eq!(
            decode_body(&multi).unwrap().unwrap().body,
            "line one\nline two\n"
        );
    }

    #[test]
    fn a_reviewer_envelope_without_scope_cannot_be_built_or_decoded() {
        let spec = RecordSpec {
            kind: RecordKind::Reviewer,
            actor: Role::Reviewer,
            key: "issue754-P5-a1".to_owned(),
            phase: Some("final".to_owned()),
            attempt: Some(1),
            review: None,
            recon: None,
        };
        // The encoder refuses to persist an envelope the decoder rejects.
        assert!(
            spec.encode_body("verdict")
                .unwrap_err()
                .contains("--review"),
        );

        // A hand-written stored envelope missing `review` fails decode.
        let stored = format!(
            "{HEADER_PREFIX}{}{HEADER_SUFFIX}\nbody",
            r#"{"v":1,"kind":"reviewer","actor":"reviewer","key":"k","phase":"final","attempt":1}"#
        );
        let error = decode_body(&stored).unwrap_err();
        assert!(error.contains("requires --review"), "{error}");
    }
}
