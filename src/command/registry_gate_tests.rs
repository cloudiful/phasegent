//! Exhaustive gate coverage for the registry: every descriptor in each gate
//! variant is checked, not a sample, and the gate membership is pinned to an
//! explicit path list so a descriptor cannot silently change gate.

use super::super::*;
use super::{ALL_ROLES, collect_paths, sorted_paths_matching};
use crate::policy::{Capability, Role};

const EXPECTED_ORCHESTRATOR_ONLY: &[&str] = &[
    "issue sync",
    "status advance",
    "status set",
    "status transition",
    "timer finish",
    "timer get",
    "timer list",
    "timer recover",
    "timer start",
    "worktree acquire",
    "worktree heartbeat",
    "worktree prune",
    "worktree release",
];

const EXPECTED_WORKTREE_READ: &[&str] = &["worktree list", "worktree probe", "worktree status"];

const EXPECTED_HUMAN_ONLY: &[&str] = &[
    "admin",
    "admin auth",
    "admin config",
    "admin config clear",
    "admin config provider",
    "admin config provider clear",
    "admin config provider set",
    "admin config set",
    "admin workflow",
    "admin workflow bootstrap",
    "auth",
    "workflow",
];

const EXPECTED_ROLELESS_OR_ONLY: &[&str] = &["issue bind", "issue unbind"];

fn owned(expected: &[&str]) -> Vec<String> {
    expected.iter().map(|path| (*path).to_owned()).collect()
}

#[test]
fn capability_gates_reject_roleless_and_mirror_role_allows() {
    for (_, spec) in collect_paths() {
        if let RoleAccess::Capability(capability) = spec.access {
            assert!(
                !spec.access.allows_roleless(),
                "{} must require a role",
                spec.name
            );
            for role in ALL_ROLES {
                assert_eq!(
                    spec.access.allows_role(*role),
                    role.allows(capability),
                    "{}/{} gate drifted from Role::allows",
                    spec.name,
                    capability.operation()
                );
            }
        }
    }
}

#[test]
fn orchestrator_only_gate_covers_every_descriptor() {
    assert_eq!(
        sorted_paths_matching(|spec| spec.access == RoleAccess::Only(ORCHESTRATOR_ONLY)),
        owned(EXPECTED_ORCHESTRATOR_ONLY)
    );
    let mut checked = 0;
    for (path, spec) in collect_paths() {
        if spec.access != RoleAccess::Only(ORCHESTRATOR_ONLY) {
            continue;
        }
        checked += 1;
        let path = path.join(" ");
        assert!(spec.access.allows_role(Role::Orchestrator), "{path}");
        for role in [Role::Admin, Role::Executor, Role::Reviewer, Role::Tester] {
            assert!(!spec.access.allows_role(role), "{path} must deny {role}");
        }
        assert!(!spec.access.allows_roleless(), "{path} must require a role");
    }
    assert_eq!(checked, EXPECTED_ORCHESTRATOR_ONLY.len());
}

#[test]
fn timer_gate_covers_every_subcommand() {
    let timer = find(&["timer"]).expect("the timer group is registered");
    let mut names: Vec<&str> = timer.children.iter().map(|child| child.name).collect();
    names.sort_unstable();
    assert_eq!(names, ["finish", "get", "list", "recover", "start"]);
    for child in timer.children {
        assert_eq!(
            child.access,
            RoleAccess::Only(ORCHESTRATOR_ONLY),
            "timer {} must stay orchestrator-only",
            child.name
        );
    }
}

#[test]
fn worktree_read_gate_covers_every_descriptor() {
    assert_eq!(
        sorted_paths_matching(|spec| spec.access == RoleAccess::Only(WORKTREE_READ_ROLES)),
        owned(EXPECTED_WORKTREE_READ)
    );
    for (path, spec) in collect_paths() {
        if spec.access != RoleAccess::Only(WORKTREE_READ_ROLES) {
            continue;
        }
        let path = path.join(" ");
        for role in [Role::Orchestrator, Role::Executor, Role::Reviewer] {
            assert!(spec.access.allows_role(role), "{path} must allow {role}");
        }
        for role in [Role::Admin, Role::Tester] {
            assert!(!spec.access.allows_role(role), "{path} must deny {role}");
        }
        assert!(!spec.access.allows_roleless(), "{path} must require a role");
    }
}

#[test]
fn notify_gate_covers_every_descriptor() {
    assert_eq!(
        sorted_paths_matching(|spec| matches!(
            spec.access,
            RoleAccess::Capability(Capability::Notify)
        )),
        vec!["notify send".to_owned()]
    );
    let access = find(&["notify", "send"]).unwrap().access;
    assert_eq!(access, RoleAccess::Capability(Capability::Notify));
    for role in ALL_ROLES {
        assert_eq!(
            access.allows_role(*role),
            role.allows(Capability::Notify),
            "notify send gate must mirror Role::allows for {role}"
        );
        assert_eq!(
            *role != Role::Admin,
            role.allows(Capability::Notify),
            "notify send must stay denied for admin only: {role}"
        );
    }
    assert!(!access.allows_roleless());
}

#[test]
fn human_only_gate_covers_every_descriptor() {
    assert_eq!(
        sorted_paths_matching(|spec| spec.access == RoleAccess::AdminOnly),
        owned(EXPECTED_HUMAN_ONLY)
    );
    for (path, spec) in collect_paths() {
        if spec.access != RoleAccess::AdminOnly {
            continue;
        }
        let path = path.join(" ");
        assert!(spec.access.allows_role(Role::Admin), "{path}");
        for ai_role in [
            Role::Orchestrator,
            Role::Executor,
            Role::Reviewer,
            Role::Tester,
        ] {
            assert!(
                !spec.access.allows_role(ai_role),
                "{path} must deny {ai_role}"
            );
        }
        assert!(!spec.access.allows_roleless(), "{path} must require a role");
    }
}

#[test]
fn roleless_gate_covers_every_descriptor() {
    assert_eq!(
        sorted_paths_matching(|spec| matches!(spec.access, RoleAccess::RolelessOrOnly(_))),
        owned(EXPECTED_ROLELESS_OR_ONLY)
    );
    for (path, spec) in collect_paths() {
        let path = path.join(" ");
        match spec.access {
            RoleAccess::Open => {
                assert!(spec.access.allows_roleless(), "{path} must accept no role");
                for role in ALL_ROLES {
                    assert!(spec.access.allows_role(*role), "{path} must allow {role}");
                }
            }
            RoleAccess::RolelessOrOnly(roles) => {
                assert_eq!(roles, ORCHESTRATOR_ONLY, "{path}");
                assert!(
                    spec.access.allows_roleless(),
                    "{path} keeps role-less calls"
                );
                assert!(spec.access.allows_role(Role::Orchestrator), "{path}");
                for role in [Role::Admin, Role::Executor, Role::Reviewer, Role::Tester] {
                    assert!(!spec.access.allows_role(role), "{path} must deny {role}");
                }
            }
            _ => {}
        }
    }
}

#[test]
fn command_level_gates_declare_a_permission_operation() {
    // Capability nodes derive their label from `Capability::operation`;
    // command-level gates must declare one so a parser denial names the same
    // operation the execution layer would.
    for (path, spec) in collect_paths() {
        let path = path.join(" ");
        if !spec.children.is_empty() {
            // Group nodes are denied through their registry gate, not labelled
            // by the permission envelope; only leaves need a label.
            continue;
        }
        match spec.access {
            RoleAccess::Capability(capability) => assert_eq!(
                spec.operation(),
                capability.operation(),
                "{path} must derive its operation from its capability"
            ),
            RoleAccess::Open => {}
            _ => assert!(
                !spec.operation.is_empty(),
                "{path} must declare an explicit permission operation"
            ),
        }
    }
}

#[test]
fn admin_auth_setup_is_role_scoped_for_every_role() {
    let spec = find(&["admin", "auth", "setup"]).expect("admin auth setup is registered");
    assert_eq!(spec.access, RoleAccess::AnyRole);
    assert_eq!(spec.operation(), "admin auth setup");
    for role in ALL_ROLES {
        assert!(
            spec.allows_role(*role),
            "admin auth setup must allow {role}"
        );
    }
    assert!(!spec.access.allows_roleless());
    // The enclosing group stays human-only; only the credential entry is
    // role-scoped.
    assert_eq!(find(&["admin"]).unwrap().access, RoleAccess::AdminOnly);
}

#[test]
fn any_role_gate_requires_a_role() {
    assert_eq!(
        sorted_paths_matching(|spec| spec.access == RoleAccess::AnyRole),
        vec!["admin auth setup".to_owned(), "mcp serve".to_owned()]
    );
    for path in [&["admin", "auth", "setup"][..], &["mcp", "serve"][..]] {
        let access = find(path).unwrap().access;
        for role in ALL_ROLES {
            assert!(access.allows_role(*role), "{path:?} must allow {role}");
        }
        assert!(!access.allows_roleless(), "{path:?} must require a role");
    }
}

/// Phase 3: the compile-time feature boundary never rewrites a role gate.
/// `gui` stays the role-open desktop entry so the parser keeps accepting it
/// for the execution layer's structured not-compiled error, while the shared
/// availability query reports the missing feature for every role context.
#[test]
fn feature_boundary_does_not_rewrite_role_gates() {
    let gui = find(&["gui"]).expect("gui is registered");
    assert_eq!(gui.access, RoleAccess::Open);
    assert!(gui.access.allows_roleless());
    if !gui.is_compiled() {
        assert_eq!(
            unavailability(None, &["gui"]),
            Some(Unavailable::NotCompiled(Feature::Gui))
        );
        for role in ALL_ROLES {
            assert!(
                !allows_role(*role, &["gui"]),
                "an uncompiled gui must not count as runnable for {role}"
            );
        }
    }
}
