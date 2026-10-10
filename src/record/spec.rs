//! The metadata a `record create` invocation carries, plus the
//! per-kind field rules that reject an impossible combination before any
//! provider access.

use crate::policy::Role;
use crate::record::kind::RecordKind;
use crate::record::token;

/// One validated `record create` request: the kind, the bound actor
/// role, the stable request key, and the kind-specific optional fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecordSpec {
    pub(crate) kind: RecordKind,
    pub(crate) actor: Role,
    pub(crate) key: String,
    pub(crate) phase: Option<String>,
    pub(crate) attempt: Option<u32>,
    pub(crate) review: Option<String>,
    pub(crate) recon: Option<String>,
}

/// The `--review` vocabulary.
pub(crate) const REVIEW_KINDS: &[&str] = &["final", "checkpoint"];

impl RecordSpec {
    /// Validate the whole request. Runs before the body file is read and
    /// before any provider is resolved, so an invalid combination never
    /// consumes input or touches the network.
    pub(crate) fn validate(&self) -> Result<(), String> {
        self.validate_actor()?;
        self.validate_fields()
    }

    /// Whether the session role may publish this kind at all.
    pub(crate) fn validate_actor(&self) -> Result<(), String> {
        if self.kind.allows(self.actor) {
            Ok(())
        } else {
            Err(self.kind.denied(self.actor))
        }
    }

    /// Validate the field combination a stored header must satisfy.
    ///
    /// This deliberately does not check the actor binding: an
    /// orchestrator may publish an executor or reviewer record, so a
    /// stored header is judged on its own fields only.
    pub(crate) fn validate_fields(&self) -> Result<(), String> {
        token::validate("--key", &self.key)?;
        if let Some(phase) = &self.phase {
            token::validate("--phase", phase)?;
        }
        if let Some(recon) = &self.recon {
            token::validate("--recon", recon)?;
        }
        match self.kind {
            RecordKind::Executor | RecordKind::Reviewer => {
                if self.phase.is_none() {
                    return Err(format!(
                        "record create --kind {} requires --phase",
                        self.kind
                    ));
                }
                if !self.attempt.is_some_and(|attempt| attempt > 0) {
                    return Err(format!(
                        "record create --kind {} requires a positive --attempt",
                        self.kind
                    ));
                }
                if self.recon.is_some() {
                    return Err(format!(
                        "record create --kind {} does not accept --recon",
                        self.kind
                    ));
                }
            }
            RecordKind::Recon => {
                if self.recon.is_none() {
                    return Err("record create --kind recon requires --recon".to_owned());
                }
                if self.phase.is_some() {
                    return Err("record create --kind recon does not accept --phase".to_owned());
                }
                if self.attempt.is_some() {
                    return Err("record create --kind recon does not accept --attempt".to_owned());
                }
            }
        }
        // The review scope is mandatory for a reviewer record: a review
        // without a declared scope cannot be told apart from the final
        // audit, so both the CLI gate and the stored-envelope decode
        // reject its absence. Every other kind rejects `--review`.
        match &self.review {
            None if self.kind == RecordKind::Reviewer => {
                return Err(
                    "record create --kind reviewer requires --review (final or checkpoint)"
                        .to_owned(),
                );
            }
            None => {}
            Some(review) if self.kind != RecordKind::Reviewer => {
                return Err(format!(
                    "record create --kind {} does not accept --review",
                    self.kind
                ));
            }
            Some(review) => {
                if !REVIEW_KINDS.contains(&review.as_str()) {
                    return Err(format!(
                        "invalid --review '{review}'; expected final or checkpoint"
                    ));
                }
                if self.recon.is_some() {
                    return Err("record create --kind recon does not accept --review".to_owned());
                }
            }
        }
        Ok(())
    }

    /// The role name stored in the header. There is no actor override:
    /// the value is always the CLI session role.
    pub(crate) fn actor_name(&self) -> &'static str {
        self.actor.as_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(kind: RecordKind) -> RecordSpec {
        match kind {
            RecordKind::Executor | RecordKind::Reviewer => RecordSpec {
                kind,
                actor: kind_role(kind),
                key: "k1".to_owned(),
                phase: Some("P1".to_owned()),
                attempt: Some(1),
                // A reviewer record always carries its scope; only the
                // executor default stays scope-free.
                review: (kind == RecordKind::Reviewer).then(|| "final".to_owned()),
                recon: None,
            },
            RecordKind::Recon => RecordSpec {
                kind,
                actor: Role::Explore,
                key: "k1".to_owned(),
                phase: None,
                attempt: None,
                review: None,
                recon: Some("scan".to_owned()),
            },
        }
    }

    fn kind_role(kind: RecordKind) -> Role {
        match kind {
            RecordKind::Executor => Role::Executor,
            RecordKind::Reviewer => Role::Reviewer,
            RecordKind::Recon => Role::Explore,
        }
    }

    #[test]
    fn each_kind_accepts_its_own_fields_and_reviewer_requires_scope() {
        for kind in RecordKind::ALL {
            let spec = base(*kind);
            assert!(spec.validate().is_ok(), "{kind}");
        }
        // A reviewer record without a declared scope is impossible: the
        // review cannot be told apart from the final audit.
        let mut missing_scope = base(RecordKind::Reviewer);
        missing_scope.review = None;
        assert!(
            missing_scope
                .validate()
                .unwrap_err()
                .contains("requires --review")
        );
        // The explicit scope is the only accepted form.
        let mut scoped = base(RecordKind::Reviewer);
        scoped.review = Some("checkpoint".to_owned());
        assert!(
            scoped.validate().is_ok(),
            "reviewer with --review checkpoint"
        );
    }

    #[test]
    fn executor_and_reviewer_require_phase_and_attempt() {
        for kind in [RecordKind::Executor, RecordKind::Reviewer] {
            let mut spec = base(kind);
            spec.phase = None;
            assert!(spec.validate().unwrap_err().contains("--phase"));
            spec = base(kind);
            spec.attempt = None;
            assert!(spec.validate().unwrap_err().contains("--attempt"));
        }
    }

    #[test]
    fn recon_requires_a_label_and_forbids_phase_attempt_review() {
        let mut spec = base(RecordKind::Recon);
        spec.recon = None;
        assert!(spec.validate().unwrap_err().contains("--recon"));
        let mut spec = base(RecordKind::Recon);
        spec.phase = Some("P1".to_owned());
        assert!(spec.validate().unwrap_err().contains("--phase"));
        let mut spec = base(RecordKind::Recon);
        spec.attempt = Some(1);
        assert!(spec.validate().unwrap_err().contains("--attempt"));
        let mut spec = base(RecordKind::Recon);
        spec.review = Some("final".to_owned());
        assert!(spec.validate().unwrap_err().contains("--review"));
    }

    #[test]
    fn review_is_reviewer_only_and_from_the_fixed_vocabulary() {
        let mut spec = base(RecordKind::Reviewer);
        spec.review = Some("midway".to_owned());
        assert!(spec.validate().unwrap_err().contains("invalid --review"));
        let mut spec = base(RecordKind::Executor);
        spec.review = Some("final".to_owned());
        assert!(spec.validate().unwrap_err().contains("--review"));
    }

    #[test]
    fn wrong_kind_and_admin_are_rejected() {
        let mut spec = base(RecordKind::Executor);
        spec.actor = Role::Reviewer;
        assert!(spec.validate().unwrap_err().contains("executor records"));
        let mut spec = base(RecordKind::Reviewer);
        spec.actor = Role::Admin;
        assert!(spec.validate().unwrap_err().contains("admin"));
        let mut spec = base(RecordKind::Recon);
        spec.actor = Role::Executor;
        assert!(spec.validate().unwrap_err().contains("recon records"));
    }

    #[test]
    fn the_key_must_be_a_token() {
        for bad in ["", "a b", "-->", &"x".repeat(129)] {
            let mut spec = base(RecordKind::Executor);
            spec.key = bad.to_owned();
            assert!(spec.validate().is_err(), "{bad:?} must be rejected");
        }
    }
}
