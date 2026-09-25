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
    Role::Tester,
];

const ALL_CAPABILITIES: &[Capability] = &[
    Capability::IssueRead,
    Capability::IssueSearch,
    Capability::IssueCreate,
    Capability::IssueUpdateBody,
    Capability::IssueClose,
    Capability::IssueAttachmentUpload,
    Capability::RepoCreate,
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
        if matches!(spec.name, "gui" | "doctor") {
            // `gui`/`doctor` take no trailing help token; their bare form is
            // the accepted invocation.
            let invocation =
                parse_with_role_env(&args(&[spec.name]), None).expect("bare command must parse");
            match spec.name {
                "gui" => assert!(matches!(invocation.command, Command::Gui)),
                "doctor" => assert!(matches!(invocation.command, Command::Doctor)),
                _ => unreachable!(),
            }
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
    for name in ["frobnicate", "issues", "mcp-serve", "Issue", "role"] {
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
    assert_eq!(
        find(&["repo"]).unwrap().provider_scope,
        ProviderScope::NonRedmine
    );
    for name in ["project", "status", "version", "relation", "timer"] {
        assert_eq!(
            find(&[name]).unwrap().provider_scope,
            ProviderScope::Redmine,
            "{name} must stay Redmine-scoped"
        );
    }
    assert!(ProviderScope::Redmine.visible(Some(ProviderKind::Redmine)));
    assert!(!ProviderScope::Redmine.visible(None));
    assert!(!ProviderScope::Redmine.visible(Some(ProviderKind::Forgejo)));
    assert!(ProviderScope::NonRedmine.visible(None));
    assert!(ProviderScope::NonRedmine.visible(Some(ProviderKind::Gitlab)));
    assert!(ProviderScope::NonRedmine.visible(Some(ProviderKind::Local)));
    assert!(!ProviderScope::NonRedmine.visible(Some(ProviderKind::Redmine)));
    assert!(ProviderScope::Any.visible(Some(ProviderKind::Redmine)));

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
fn gui_feature_boundary_is_registered() {
    let gui = find(&["gui"]).unwrap();
    assert_eq!(gui.feature, Some(Feature::Gui));
    assert_eq!(Feature::Gui.name(), "gui");
    // The registry records the boundary; the build decides the answer.
    assert_eq!(Feature::Gui.is_compiled(), cfg!(feature = "gui"));
    assert_eq!(gui.is_compiled(), Feature::Gui.is_compiled());
    every_node(|node| {
        if node.name == "gui" {
            assert_eq!(node.feature, Some(Feature::Gui));
        } else {
            assert_eq!(node.feature, None, "{} carries no feature", node.name);
        }
    });
    // The feature boundary is part of visibility in every role context: an
    // uncompiled node is hidden even from the role-less superset view and
    // reports the stable not-compiled reason instead.
    assert_eq!(gui.visible_for(None), Feature::Gui.is_compiled());
    for role in ALL_ROLES {
        assert_eq!(
            gui.visible_for(Some(*role)),
            Feature::Gui.is_compiled(),
            "gui visibility for {role}"
        );
        assert_eq!(
            allows_role(*role, &["gui"]),
            Feature::Gui.is_compiled(),
            "gui acceptance for {role}"
        );
    }
    for role in [None, Some(Role::Executor), Some(Role::Admin)] {
        assert_eq!(
            unavailability(role, &["gui"]),
            (!Feature::Gui.is_compiled()).then_some(Unavailable::NotCompiled(Feature::Gui)),
            "gui availability for {role:?}"
        );
    }
    assert_eq!(
        Feature::Gui.not_compiled_message(),
        "GUI support was not compiled into this binary; rebuild with --features gui to enable the desktop shell"
    );
}

#[test]
fn no_role_context_keeps_the_superset_view_for_compiled_nodes() {
    every_node(|node| {
        assert_eq!(
            node.visible_for(None),
            node.is_compiled(),
            "{} superset visibility must follow the compiled boundary",
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
                node.is_compiled() && node.allows_role(*role),
                "{} visibility must follow its gate and feature boundary",
                node.name
            );
        }
    });
}

#[test]
fn compiled_nodes_stay_available_for_every_role_and_roleless_view() {
    for (path, spec) in collect_paths() {
        if !spec.is_compiled() {
            continue;
        }
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
