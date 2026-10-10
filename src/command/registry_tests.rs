//! Registry completeness tests: the descriptor tree must cover every
//! parser-accepted command (the parser shares [`super::top_level`]), keep
//! sibling names unique, reference every capability, and preserve provider
//! and no-role behavior. Gate-specific coverage lives in `tests::gate`.

use super::super::{Command, parse_with_role_env};
use super::*;
use crate::policy::{Capability, Role};
use crate::providers::ProviderKind;

const ALL_ROLES: &[Role] = &[
    Role::Admin,
    Role::Orchestrator,
    Role::Executor,
    Role::Reviewer,
];

const ALL_CAPABILITIES: &[Capability] = &[
    Capability::IssueRead,
    Capability::IssueSearch,
    Capability::IssueCreate,
    Capability::IssueUpdateBody,
    Capability::IssueClose,
    Capability::IssueAttachmentUpload,
    Capability::CommentCreate,
    Capability::CommentRead,
    Capability::CommentFindMarker,
    Capability::Notify,
    Capability::ProjectRead,
    Capability::ProjectCreate,
    Capability::IssueStatusRead,
    Capability::VersionRead,
    Capability::RelationRead,
    Capability::RelationCreate,
    Capability::RelationDelete,
];

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn walk(spec: &CommandSpec, visit: &mut impl FnMut(&CommandSpec)) {
    visit(spec);
    for child in spec.children {
        walk(child, visit);
    }
}

fn every_node(mut visit: impl FnMut(&CommandSpec)) {
    for spec in COMMANDS {
        walk(spec, &mut visit);
    }
}

/// Every descriptor path paired with its node, used by the gate tests to make
/// their coverage exhaustive instead of sampled.
fn collect_paths() -> Vec<(Vec<String>, &'static CommandSpec)> {
    fn record(
        spec: &'static CommandSpec,
        path: &mut Vec<String>,
        out: &mut Vec<(Vec<String>, &'static CommandSpec)>,
    ) {
        path.push(spec.name.to_owned());
        out.push((path.clone(), spec));
        for child in spec.children {
            record(child, path, out);
        }
        path.pop();
    }
    let mut out = Vec::new();
    for spec in COMMANDS {
        record(spec, &mut Vec::new(), &mut out);
    }
    out
}

/// Space-joined, sorted descriptor paths whose node matches `predicate`.
fn sorted_paths_matching(predicate: impl Fn(&CommandSpec) -> bool) -> Vec<String> {
    let mut paths: Vec<String> = collect_paths()
        .into_iter()
        .filter(|(_, spec)| predicate(spec))
        .map(|(path, _)| path.join(" "))
        .collect();
    paths.sort();
    paths
}

fn assert_help_parses(spec: &'static CommandSpec, path: &mut Vec<String>) {
    let mut argv = path.clone();
    argv.push("--help".to_owned());
    match parse_with_role_env(&argv, Some("orchestrator")) {
        Ok(invocation) => assert!(
            matches!(invocation.command, Command::Help(_)),
            "{argv:?} must resolve to help, got {:?}",
            invocation.command
        ),
        Err(error) => panic!("{argv:?} must parse, got: {error}"),
    }
    for child in spec.children {
        path.push(child.name.to_owned());
        assert_help_parses(child, path);
        path.pop();
    }
}

#[test]
fn every_registered_command_is_accepted_by_the_parser() {
    for spec in COMMANDS {
        if spec.name == "doctor" {
            // `doctor` takes no trailing help token; its bare form is the
            // accepted invocation.
            let invocation =
                parse_with_role_env(&args(&[spec.name]), None).expect("bare command must parse");
            assert!(matches!(invocation.command, Command::Doctor));
            continue;
        }
        let mut path = vec![spec.name.to_owned()];
        assert_help_parses(spec, &mut path);
    }
}

#[test]
fn parser_rejects_names_outside_the_registry() {
    // Top-level routing consults `top_level`, so this is the reverse half of
    // the completeness guard: a name the registry does not describe can never
    // be accepted, and the historical error stays byte-identical.
    for name in ["frobnicate", "issues", "widgets", "Issue", "role"] {
        assert!(
            top_level(name).is_none(),
            "{name} must not be a registered command"
        );
        let error = parse_with_role_env(&args(&[name]), Some("orchestrator")).unwrap_err();
        assert_eq!(error, format!("unknown command '{name}'"));
    }
}

#[test]
fn sibling_names_are_unique_at_every_level() {
    fn check(specs: &[CommandSpec]) {
        for (index, spec) in specs.iter().enumerate() {
            for other in &specs[index + 1..] {
                assert_ne!(
                    spec.name, other.name,
                    "duplicate sibling '{}' in the registry",
                    spec.name
                );
            }
            check(spec.children);
        }
    }
    check(COMMANDS);
}

#[test]
fn every_capability_is_referenced_by_the_registry() {
    let mut registered = Vec::new();
    every_node(|node| {
        if let RoleAccess::Capability(capability) = node.access {
            registered.push(capability);
        }
    });
    for capability in ALL_CAPABILITIES {
        assert!(
            registered.contains(capability),
            "capability {capability:?} is not referenced by the registry"
        );
    }
}

#[test]
fn provider_scopes_match_the_root_help_conditions() {
    for name in ["project", "status", "version", "relation", "timer"] {
        assert_eq!(
            find(&[name]).unwrap().provider_scope,
            ProviderScope::Redmine,
            "{name} must stay Redmine-scoped"
        );
    }
    assert!(ProviderScope::Redmine.visible(Some(ProviderKind::Redmine)));
    assert!(!ProviderScope::Redmine.visible(None));
    assert!(!ProviderScope::Redmine.visible(Some(ProviderKind::Local)));
    assert!(ProviderScope::Any.visible(Some(ProviderKind::Redmine)));
    assert!(ProviderScope::Any.visible(Some(ProviderKind::Local)));
    assert!(ProviderScope::Any.visible(None));

    // Provider scope lives on the top-level group only, so a consumer always
    // resolves the enclosing group first.
    fn assert_descendants_any(spec: &CommandSpec) {
        for child in spec.children {
            assert_eq!(
                child.provider_scope,
                ProviderScope::Any,
                "{} must not override its group's provider scope",
                child.name
            );
            assert_descendants_any(child);
        }
    }
    for spec in COMMANDS {
        assert_descendants_any(spec);
    }
}

#[test]
fn no_registry_node_has_an_optional_feature() {
    // No registered command is feature-gated: every command is compiled into
    // the binary and the boundary type has no variants.
    every_node(|node| {
        assert_eq!(
            node.feature, None,
            "{} must not carry a compile-time feature",
            node.name
        );
        assert!(node.is_compiled(), "{} must stay compiled", node.name);
    });
}

#[test]
fn no_role_context_keeps_the_superset_view_for_every_command() {
    every_node(|node| {
        assert!(
            node.visible_for(None),
            "{} must stay visible in the role-less superset view",
            node.name
        );
    });
    assert!(RoleAccess::AdminOnly.visible(None));
}

#[test]
fn role_visibility_covers_at_least_the_gated_roles() {
    every_node(|node| {
        for role in ALL_ROLES {
            assert_eq!(
                node.visible_for(Some(*role)),
                node.allows_role(*role),
                "{} visibility must follow its gate",
                node.name
            );
        }
    });
}

#[test]
fn registered_nodes_stay_available_for_every_role_and_roleless_view() {
    for (path, spec) in collect_paths() {
        let path: Vec<&str> = path.iter().map(String::as_str).collect();
        assert_eq!(unavailability(None, &path), None, "{path:?}");
        for role in ALL_ROLES {
            assert_eq!(
                allows_role(*role, &path),
                spec.allows_role(*role),
                "{path:?} acceptance for {role}"
            );
            assert_eq!(
                denied_operation(*role, &path),
                (!spec.allows_role(*role)).then(|| spec.operation()),
                "{path:?} denial operation for {role}"
            );
        }
    }
}

#[test]
fn unknown_registry_paths_stay_denied_without_a_reason() {
    assert!(!allows_role(Role::Orchestrator, &["frobnicate"]));
    assert_eq!(
        unavailability(Some(Role::Orchestrator), &["frobnicate"]),
        None
    );
    assert_eq!(
        denied_operation(Role::Orchestrator, &["frobnicate"]),
        None,
        "an unknown path keeps the unknown-command contract; the parser handles it"
    );
}

#[path = "registry_gate_tests.rs"]
mod gate;
