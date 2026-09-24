use super::*;

#[test]
fn status_next_and_advance_parse_positional_and_status_option() {
    let next = ["--provider", "redmine", "status", "next", "51"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    match command::parse_with_role_env(&next, Some("executor"))
        .unwrap()
        .command
    {
        command::Command::Status(command::StatusCommand::Next { number }) => {
            assert_eq!(number, 51);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // `next` takes exactly one positional and no options.
    for extra in [vec!["51", "52"], vec!["51", "--status", "Blocked"]] {
        let mut args = vec!["status", "next"];
        args.extend(extra);
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(command::parse_with_role_env(&args, Some("orchestrator")).is_err());
    }

    let advance = [
        "--provider",
        "redmine",
        "status",
        "advance",
        "51",
        "--status",
        "In Review",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse_with_role_env(&advance, Some("orchestrator"))
        .unwrap()
        .command
    {
        command::Command::Status(command::StatusCommand::Advance { number, status }) => {
            assert_eq!(number, 51);
            assert_eq!(status, "In Review");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let missing_status = ["--provider", "redmine", "status", "advance", "51"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(command::parse_with_role_env(&missing_status, Some("orchestrator")).is_err());
}

/// The canonical transition graph is the single source of truth for the
/// phase workflow, so every documented edge and every terminal status is
/// asserted directly against the policy helpers.
#[test]
fn canonical_transition_policy_matches_the_documented_phase_graph() {
    let expected: &[(&str, &[&str])] = &[
        ("New", &["In Progress", "Cancelled"]),
        ("In Progress", &["In Review", "Blocked", "Cancelled"]),
        (
            "In Review",
            &["Resolved", "Changes Requested", "Blocked", "Cancelled"],
        ),
        (
            "Changes Requested",
            &["In Progress", "Blocked", "Cancelled"],
        ),
        ("Blocked", &["In Progress", "Cancelled"]),
        ("Resolved", &["Closed", "In Progress"]),
        ("Closed", &[]),
        ("Cancelled", &[]),
    ];
    for (current, allowed) in expected {
        assert_eq!(
            canonical_allowed_next(current).expect("canonical status"),
            *allowed,
            "allowed_next mismatch for {current}"
        );
        for target in *allowed {
            assert_eq!(
                evaluate_transition(current, target),
                TransitionVerdict::Allowed,
                "{current} -> {target} must be allowed"
            );
        }
    }

    // Illegal edges are rejected with the allowed set attached so the
    // caller can surface concrete guidance.
    match evaluate_transition("Resolved", "In Review") {
        TransitionVerdict::Forbidden { allowed_next } => {
            assert_eq!(allowed_next, &["Closed", "In Progress"])
        }
        other => panic!("expected Forbidden, got {other:?}"),
    }
    match evaluate_transition("Closed", "In Progress") {
        TransitionVerdict::Forbidden { allowed_next } => assert!(allowed_next.is_empty()),
        other => panic!("expected Forbidden, got {other:?}"),
    }
}

/// `Resolved` is the non-terminal "AI work finished, awaiting the user's
/// verification" state, so the policy keeps two outgoing edges: the
/// verified terminal `Closed` as the auto-route default and `In Progress`
/// as the explicitly targeted resume after a reviewed phase. Losing the
/// resume edge would strand every multi-phase task after its first
/// reviewed phase.
#[test]
fn resolved_status_allows_final_close_and_phase_continuation() {
    assert_eq!(
        canonical_allowed_next("Resolved").expect("canonical status"),
        &["Closed", "In Progress"],
        "Resolved must offer the final close before the resume edge"
    );
    assert_eq!(
        evaluate_transition("Resolved", "Closed"),
        TransitionVerdict::Allowed,
        "the verified terminal close edge must stay allowed"
    );
    assert_eq!(
        evaluate_transition("Resolved", "In Progress"),
        TransitionVerdict::Allowed,
        "a remaining phase must be able to resume implementation"
    );
    // The continuation edge must not turn Resolved into a general
    // re-entry point: every other phase state stays unreachable.
    for target in ["In Review", "Changes Requested", "Blocked", "Cancelled"] {
        assert!(
            matches!(
                evaluate_transition("Resolved", target),
                TransitionVerdict::Forbidden { .. }
            ),
            "Resolved -> {target} must stay forbidden"
        );
    }
}

/// Same-status transitions are no-ops, installation casing is tolerated,
/// and any unknown status makes the verdict advisory instead of claiming
/// permission the server may not grant.
#[test]
fn transition_policy_handles_no_op_casing_and_custom_statuses() {
    assert_eq!(
        evaluate_transition("In Progress", "In Progress"),
        TransitionVerdict::NoOp
    );
    assert_eq!(
        evaluate_transition("Triaged", "triaged"),
        TransitionVerdict::NoOp,
        "a custom status re-applied to itself is still a no-op"
    );
    assert_eq!(canonical_status_name("in progress"), Some("In Progress"));
    assert_eq!(canonical_status_name("  BLOCKED "), Some("Blocked"));
    assert_eq!(canonical_status_name("Triaged"), None);
    assert!(canonical_allowed_next("Triaged").is_none());

    match evaluate_transition("Triaged", "In Progress") {
        TransitionVerdict::Advisory { reason } => {
            assert!(reason.contains("current status 'Triaged'"), "{reason}");
            assert!(reason.contains("server decides"), "{reason}");
        }
        other => panic!("expected Advisory, got {other:?}"),
    }
    match evaluate_transition("In Progress", "Escalated") {
        TransitionVerdict::Advisory { reason } => {
            assert!(reason.contains("target status 'Escalated'"), "{reason}");
        }
        other => panic!("expected Advisory, got {other:?}"),
    }
}

#[test]
fn status_set_parses_number_and_validated_status_value() {
    let args = [
        "--provider",
        "redmine",
        "status",
        "set",
        "12",
        "--status",
        "In Progress",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse_with_role_env(&args, Some("orchestrator"))
        .unwrap()
        .command
    {
        command::Command::Status(command::StatusCommand::Set { number, status }) => {
            assert_eq!(number, 12);
            assert_eq!(status, "In Progress");
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let missing = ["--provider", "redmine", "status", "set", "12"]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(command::parse_with_role_env(&missing, Some("orchestrator")).is_err());

    // The inline escape hatch keeps leading-dash values usable.
    let inline = [
        "--provider",
        "redmine",
        "status",
        "set",
        "12",
        "--status=-Blocked",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    match command::parse_with_role_env(&inline, Some("orchestrator"))
        .unwrap()
        .command
    {
        command::Command::Status(command::StatusCommand::Set { status, .. }) => {
            assert_eq!(status, "-Blocked");
        }
        other => panic!("unexpected command: {other:?}"),
    }
}
