//! Building and authorizing one `record create` request.
//!
//! Everything here is local: the kind/actor binding, the per-kind field
//! rules, the token shape, and the `--authorized` requirement all decide
//! before a provider is resolved and before a `--body-file` is read, so a
//! rejected request never touches input or the network.

use crate::policy::Role;
use crate::record::{RecordKind, RecordSpec};

/// Outcome of the pre-provider gates.
#[derive(Debug)]
pub(crate) enum Gate {
    /// A structured argument denial, exit code 2.
    Argument(String),
    /// A permission denial, exit code 3.
    Permission(String),
}

/// The metadata fields a `record create` invocation carries. Grouped so
/// the authorization boundary takes one value, not a wide argument list.
pub(crate) struct GateInput<'a> {
    pub(crate) kind: RecordKind,
    pub(crate) key: &'a str,
    pub(crate) phase: Option<&'a str>,
    pub(crate) attempt: Option<u32>,
    pub(crate) review: Option<&'a str>,
    pub(crate) recon: Option<&'a str>,
}

/// Validate the metadata of a parsed create against the session role.
///
/// `role` is the CLI session role and the only actor source: a record's
/// `actor` is never taken from a flag, so an agent cannot label its work
/// as another role's.
pub(crate) fn authorize(
    role: Role,
    input: GateInput<'_>,
    authorized: bool,
) -> Result<RecordSpec, Gate> {
    // A child's write needs explicit authorization, the same requirement
    // the ordinary comment flow carries. The orchestrator owns every
    // kind; each agent role publishes only its own.
    if role != Role::Orchestrator && !authorized {
        return Err(Gate::Permission(
            "record create requires --authorized for the agent roles".to_owned(),
        ));
    }
    if !input.kind.allows(role) {
        return Err(Gate::Permission(input.kind.denied(role)));
    }
    let spec = RecordSpec {
        kind: input.kind,
        actor: role,
        key: input.key.to_owned(),
        phase: input.phase.map(str::to_owned),
        attempt: input.attempt,
        review: input.review.map(str::to_owned),
        recon: input.recon.map(str::to_owned),
    };
    // Field rules after the role binding, so a wrong-role request reports
    // the denial rather than complaining about its flags.
    spec.validate().map_err(Gate::Argument)?;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recon(actor: Role) -> Result<RecordSpec, Gate> {
        authorize(
            actor,
            GateInput {
                kind: RecordKind::Recon,
                key: "k1",
                phase: None,
                attempt: None,
                review: None,
                recon: Some("scan"),
            },
            true,
        )
    }

    fn executor(actor: Role, authorized: bool) -> Result<RecordSpec, Gate> {
        authorize(
            actor,
            GateInput {
                kind: RecordKind::Executor,
                key: "k1",
                phase: Some("P1"),
                attempt: Some(1),
                review: None,
                recon: None,
            },
            authorized,
        )
    }

    #[test]
    fn a_recon_record_is_bound_to_the_explore_role() {
        let spec = recon(Role::Explore).unwrap();
        assert_eq!(spec.actor, Role::Explore);
        assert_eq!(spec.kind, RecordKind::Recon);
    }

    #[test]
    fn a_child_role_must_pass_authorized() {
        let error = executor(Role::Executor, false).unwrap_err();
        assert!(matches!(error, Gate::Permission(_)), "{error:?}");
    }

    #[test]
    fn admin_publishes_no_record_kind() {
        let error = executor(Role::Admin, true).unwrap_err();
        assert!(matches!(error, Gate::Permission(message) if message.contains("admin")));
        assert!(matches!(
            recon(Role::Admin).unwrap_err(),
            Gate::Permission(_)
        ));
    }

    #[test]
    fn a_role_may_not_publish_another_roles_kind() {
        let error = executor(Role::Reviewer, true).unwrap_err();
        assert!(matches!(error, Gate::Permission(message) if message.contains("executor")));
        let error = recon(Role::Executor).unwrap_err();
        assert!(matches!(error, Gate::Permission(message) if message.contains("recon")));
        let error = authorize(
            Role::Explore,
            GateInput {
                kind: RecordKind::Reviewer,
                key: "k1",
                phase: Some("P1"),
                attempt: Some(1),
                review: None,
                recon: None,
            },
            true,
        )
        .unwrap_err();
        assert!(matches!(error, Gate::Permission(message) if message.contains("reviewer")));
    }

    #[test]
    fn an_impossible_combination_is_an_argument_error_not_a_permission_one() {
        let error = authorize(
            Role::Executor,
            GateInput {
                kind: RecordKind::Executor,
                key: "k1",
                phase: None,
                attempt: Some(1),
                review: None,
                recon: None,
            },
            true,
        )
        .unwrap_err();
        assert!(matches!(error, Gate::Argument(message) if message.contains("--phase")));
    }

    #[test]
    fn a_bad_token_is_an_argument_error() {
        let error = authorize(
            Role::Executor,
            GateInput {
                kind: RecordKind::Executor,
                key: "a b",
                phase: Some("P1"),
                attempt: Some(1),
                review: None,
                recon: None,
            },
            true,
        )
        .unwrap_err();
        assert!(matches!(error, Gate::Argument(_)), "{error:?}");
    }

    #[test]
    fn the_orchestrator_publishes_every_kind_without_authorized() {
        for kind in RecordKind::ALL {
            let recon_label = (*kind == RecordKind::Recon).then_some("scan");
            let review = (*kind == RecordKind::Reviewer).then_some("final");
            let phase = (*kind != RecordKind::Recon).then_some("P1");
            let attempt = (*kind != RecordKind::Recon).then_some(1);
            let spec = authorize(
                Role::Orchestrator,
                GateInput {
                    kind: *kind,
                    key: "k1",
                    phase,
                    attempt,
                    review,
                    recon: recon_label,
                },
                false,
            );
            assert!(spec.is_ok(), "{kind}: {spec:?}");
            assert_eq!(spec.unwrap().actor, Role::Orchestrator);
        }
    }
}
