//! Unit tests for the plugin command surface.

use crate::cli::plugin::{InstallEnvelope, StatusEnvelope, UninstallEnvelope};
use crate::command::{self, Command, PluginCommand};

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

#[test]
fn install_parses_with_no_flags() {
    let invocation = command::parse_with_role_env(&strings(["plugin", "install"]), None).unwrap();
    match invocation.command {
        Command::Plugin(PluginCommand::Install {
            project,
            force,
            path,
        }) => {
            assert!(!project);
            assert!(!force);
            assert_eq!(path, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn install_parses_project_and_force_flags() {
    let invocation = command::parse_with_role_env(
        &strings(["plugin", "install", "--project", "--force"]),
        None,
    )
    .unwrap();
    match invocation.command {
        Command::Plugin(PluginCommand::Install {
            project,
            force,
            path,
        }) => {
            assert!(project);
            assert!(force);
            assert_eq!(path, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn status_parses_without_flags() {
    let invocation = command::parse_with_role_env(&strings(["plugin", "status"]), None).unwrap();
    match invocation.command {
        Command::Plugin(PluginCommand::Status { project, path }) => {
            assert!(!project);
            assert_eq!(path, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn status_parses_explicit_path_without_a_role() {
    let invocation = command::parse_with_role_env(
        &strings(["plugin", "status", "--path", "/tmp/chezmoi/plugins"]),
        None,
    )
    .unwrap();
    assert!(invocation.role.is_none());
    match invocation.command {
        Command::Plugin(PluginCommand::Status { project, path }) => {
            assert!(!project);
            assert_eq!(path.as_deref(), Some("/tmp/chezmoi/plugins"));
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn uninstall_parses_with_project_flag() {
    let invocation =
        command::parse_with_role_env(&strings(["plugin", "uninstall", "--project"]), None).unwrap();
    match invocation.command {
        Command::Plugin(PluginCommand::Uninstall { project, path }) => {
            assert!(project);
            assert_eq!(path, None);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn plugin_install_does_not_require_role() {
    let invocation = command::parse_with_role_env(&strings(["plugin", "install"]), None).unwrap();
    assert!(invocation.role.is_none());
    assert!(matches!(
        invocation.command,
        Command::Plugin(PluginCommand::Install { .. })
    ));
}

#[test]
fn unknown_plugin_subcommand_is_rejected() {
    let err = command::parse_with_role_env(&strings(["plugin", "reinstall"]), None).unwrap_err();
    assert!(err.contains("unknown plugin command"), "got: {err}");
}

#[test]
fn envelope_serialises_with_stable_field_order() {
    let envelope = InstallEnvelope {
        installed: vec!["/tmp/a".to_owned()],
        updated: vec![],
        skipped: vec![],
        warnings: vec![],
        errors: vec![],
        global_path: Some("/tmp/global".to_owned()),
        project_path: Some("/tmp/project".to_owned()),
        target_path: None,
    };
    let rendered = serde_json::to_string(&envelope).expect("serialize");
    let installed_at = rendered.find("\"installed\"").expect("installed key");
    let updated_at = rendered.find("\"updated\"").expect("updated key");
    let project_path_at = rendered.find("\"project_path\"").expect("project_path key");
    assert!(installed_at < updated_at);
    assert!(updated_at < project_path_at);
    assert!(!rendered.contains("target_path"), "got: {rendered}");

    let status = StatusEnvelope {
        global: Some(crate::plugin::PluginTargetStatus {
            path: "/tmp/global".to_owned(),
            exists: true,
            managed: true,
            size: 12,
            mtime: Some(0),
        }),
        project: Some(crate::plugin::PluginTargetStatus {
            path: "/tmp/project".to_owned(),
            exists: false,
            managed: false,
            size: 0,
            mtime: None,
        }),
        target: None,
    };
    let rendered_status = serde_json::to_string(&status).expect("serialize status");
    assert!(rendered_status.contains("\"global\""));
    assert!(rendered_status.contains("\"project\""));
    assert!(rendered_status.contains("\"managed\":true"));
    assert!(
        !rendered_status.contains("target"),
        "got: {rendered_status}"
    );

    let uninstall = UninstallEnvelope {
        removed: vec!["/tmp/a".to_owned()],
        warnings: vec![],
        errors: vec![],
        global_path: Some("/tmp/global".to_owned()),
        project_path: Some("/tmp/project".to_owned()),
        target_path: None,
    };
    let rendered_uninstall = serde_json::to_string(&uninstall).expect("serialize uninstall");
    assert!(rendered_uninstall.contains("\"removed\""));
    assert!(rendered_uninstall.contains("\"global_path\""));
    assert!(
        !rendered_uninstall.contains("target_path"),
        "got: {rendered_uninstall}"
    );
}
