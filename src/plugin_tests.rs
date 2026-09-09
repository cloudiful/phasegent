//! Focused tests for the Phase 3 OpenCode plugin installer.
//!
//! Coverage mirrors the four scope rows the Phase 3 task calls out:
//!
//! * **Parser** — every `plugin` subcommand parses, default values
//!   land where the help advertises them, and unknown options are
//!   rejected.
//! * **Install** — idempotent install against a temp directory, with
//!   marker-based ownership so a re-run reports `updated` (template
//!   changed) or `skipped` (already current) and never duplicates.
//! * **Foreign-file refusal** — an existing file without the marker
//!   is refused by default and renamed to `*.phasegent-orig` when
//!   `--force` is supplied.
//! * **Uninstall** — managed files are removed; foreign files are
//!   refused; missing files report a warning instead of an error.
//! * **Adapter template** — the embedded JS contains the
//!   `// phasegent:managed` marker, the
//!   `experimental_workspace.register` shape, and the
//!   `phasegent --role orchestrator worktree acquire` call.
//!
//! All filesystem tests use a temp directory and override
//! `HOME`/`XDG_CONFIG_HOME` so the operator's real `~/.config` is
//! never touched. The marker detection stays string-based so the
//! tests never depend on Bun being installed.

use crate::cli::plugin::{InstallEnvelope, StatusEnvelope, UninstallEnvelope, execute_plugin};
use crate::command::{self, Command, PluginCommand};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::plugin::{
    FOREIGN_BACKUP_SUFFIX, MANAGED_MARKER, PLUGIN_FILENAME, adapter_source, install_at, status_at,
    uninstall_at,
};
use std::path::{Path, PathBuf};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "phasegent-plugin-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir create");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn child(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn override_home(label: &str) -> (TempDir, EnvGuard, EnvGuard) {
    let temp = TempDir::new(label);
    let home = temp.child("home");
    let xdg = temp.child("xdg");
    std::fs::create_dir_all(&home).expect("home dir");
    std::fs::create_dir_all(&xdg).expect("xdg dir");
    let home_guard = EnvGuard::set("HOME", home.to_string_lossy().as_ref());
    let xdg_guard = EnvGuard::set("XDG_CONFIG_HOME", xdg.to_string_lossy().as_ref());
    (temp, home_guard, xdg_guard)
}

fn read_bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn write_bytes(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, bytes).unwrap();
}

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

// ---------------------------------------------------------------------------
// Parser coverage.
// ---------------------------------------------------------------------------

#[test]
fn install_parses_with_no_flags() {
    let _lock = lock_workflow_tests();
    let invocation = command::parse(&strings(["plugin", "install"])).unwrap();
    match invocation.command {
        Command::Plugin(PluginCommand::Install {
            global,
            project,
            force,
        }) => {
            assert!(!global);
            assert!(!project);
            assert!(!force);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn install_parses_with_all_flags() {
    let _lock = lock_workflow_tests();
    let invocation = command::parse(&strings([
        "plugin",
        "install",
        "--global",
        "--project",
        "--force",
    ]))
    .unwrap();
    match invocation.command {
        Command::Plugin(PluginCommand::Install {
            global,
            project,
            force,
        }) => {
            assert!(global);
            assert!(project);
            assert!(force);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn status_parses_without_flags() {
    let _lock = lock_workflow_tests();
    let invocation = command::parse(&strings(["plugin", "status"])).unwrap();
    assert!(matches!(
        invocation.command,
        Command::Plugin(PluginCommand::Status)
    ));
}

#[test]
fn uninstall_parses_with_project_flag() {
    let _lock = lock_workflow_tests();
    let invocation = command::parse(&strings(["plugin", "uninstall", "--project"])).unwrap();
    match invocation.command {
        Command::Plugin(PluginCommand::Uninstall { global, project }) => {
            assert!(!global);
            assert!(project);
        }
        other => panic!("unexpected command {other:?}"),
    }
}

#[test]
fn plugin_install_does_not_require_role() {
    let _lock = lock_workflow_tests();
    let invocation = command::parse(&strings(["plugin", "install"])).unwrap();
    assert!(invocation.role.is_none());
    assert!(matches!(
        invocation.command,
        Command::Plugin(PluginCommand::Install { .. })
    ));
}

#[test]
fn unknown_plugin_subcommand_is_rejected() {
    let _lock = lock_workflow_tests();
    let err = command::parse(&strings(["plugin", "reinstall"])).unwrap_err();
    assert!(err.contains("unknown plugin command"), "got: {err}");
}

// ---------------------------------------------------------------------------
// Install coverage.
// ---------------------------------------------------------------------------

#[test]
fn install_at_writes_managed_file_with_marker_and_register_call() {
    let _lock = lock_workflow_tests();
    let dir = TempDir::new("install-fresh");
    let outcome = install_at(dir.path(), false).expect("install succeeds");
    assert_eq!(outcome.installed.len(), 1);
    assert_eq!(outcome.updated.len(), 0);
    assert_eq!(outcome.skipped.len(), 0);
    let target = dir.child(PLUGIN_FILENAME);
    let bytes = read_bytes(&target);
    let text = std::str::from_utf8(&bytes).expect("utf-8 adapter");
    assert!(
        text.starts_with(MANAGED_MARKER),
        "missing marker: {:?}",
        &text[..40]
    );
    assert!(text.contains("experimental_workspace.register"));
    assert!(text.contains("\"phasegent\""));
    assert!(text.contains("phasegent issue status"));
    assert!(text.contains("--role"));
    assert!(text.contains("orchestrator"));
    assert!(text.contains("worktree acquire"));
    assert!(text.contains("--format"));
    assert!(text.contains("\"json\""));
}

#[test]
fn install_at_is_idempotent_for_a_managed_file() {
    let _lock = lock_workflow_tests();
    let dir = TempDir::new("install-idem");
    install_at(dir.path(), false).expect("first install");
    let outcome = install_at(dir.path(), false).expect("second install");
    assert!(outcome.installed.is_empty());
    assert!(outcome.updated.is_empty());
    assert_eq!(
        outcome.skipped.len(),
        1,
        "expected skipped, got {:?}",
        outcome
    );
}

#[test]
fn install_at_updates_when_managed_template_drifts() {
    let _lock = lock_workflow_tests();
    let dir = TempDir::new("install-update");
    install_at(dir.path(), false).expect("first install");
    let target = dir.child(PLUGIN_FILENAME);
    // Inject a managed drift (still tagged with the marker so the
    // re-install classifies the file as managed, not foreign).
    let mut drifted = adapter_source().as_bytes().to_vec();
    drifted.extend_from_slice(b"\n// drift\n");
    write_bytes(&target, &drifted);
    let outcome = install_at(dir.path(), false).expect("second install");
    assert!(outcome.installed.is_empty());
    assert_eq!(outcome.updated.len(), 1);
    assert!(outcome.skipped.is_empty());
    // Bytes are restored to the embedded template.
    let bytes = read_bytes(&target);
    assert_eq!(bytes, adapter_source().as_bytes());
}

#[test]
fn install_at_refuses_foreign_file_without_force() {
    let _lock = lock_workflow_tests();
    let dir = TempDir::new("install-foreign");
    let target = dir.child(PLUGIN_FILENAME);
    write_bytes(&target, b"#!/usr/bin/env node\nconsole.log('not ours');\n");
    let error = install_at(dir.path(), false).expect_err("must refuse");
    assert_eq!(error.kind, "conflict");
    assert!(error.message.contains("phasegent:managed"));
    // Original bytes untouched.
    let bytes = read_bytes(&target);
    assert!(bytes.starts_with(b"#!/usr/bin/env node"));
    assert!(
        !dir.path()
            .join(format!("{PLUGIN_FILENAME}{FOREIGN_BACKUP_SUFFIX}"))
            .exists()
    );
}

#[test]
fn install_at_with_force_displaces_foreign_file_to_backup() {
    let _lock = lock_workflow_tests();
    let dir = TempDir::new("install-force");
    let target = dir.child(PLUGIN_FILENAME);
    write_bytes(&target, b"#!/usr/bin/env node\nconsole.log('not ours');\n");
    let outcome = install_at(dir.path(), true).expect("forced install");
    assert_eq!(outcome.installed.len(), 1);
    assert_eq!(outcome.warnings.len(), 1);
    let backup = dir
        .path()
        .join(format!("{PLUGIN_FILENAME}{FOREIGN_BACKUP_SUFFIX}"));
    assert!(backup.exists(), "backup must exist at {}", backup.display());
    let backup_bytes = read_bytes(&backup);
    assert!(backup_bytes.starts_with(b"#!/usr/bin/env node"));
    let bytes = read_bytes(&target);
    assert!(bytes.starts_with(MANAGED_MARKER.as_bytes()));
}

#[test]
fn install_at_refuses_when_backup_already_exists() {
    let _lock = lock_workflow_tests();
    let dir = TempDir::new("install-backup-exists");
    let target = dir.child(PLUGIN_FILENAME);
    let backup = dir
        .path()
        .join(format!("{PLUGIN_FILENAME}{FOREIGN_BACKUP_SUFFIX}"));
    write_bytes(&target, b"#!/usr/bin/env node\nnot ours;\n");
    write_bytes(&backup, b"stale backup\n");
    let error = install_at(dir.path(), true).expect_err("must refuse when backup exists");
    assert_eq!(error.kind, "conflict");
    // Original bytes still untouched.
    let bytes = read_bytes(&target);
    assert!(bytes.starts_with(b"#!/usr/bin/env node"));
}

#[test]
fn install_at_refuses_symlinked_target() {
    let _lock = lock_workflow_tests();
    let dir = TempDir::new("install-symlink");
    let target = dir.child(PLUGIN_FILENAME);
    let external = dir.child("elsewhere.js");
    write_bytes(&external, b"#!/usr/bin/env node\nexternal;\n");
    std::os::unix::fs::symlink(&external, &target).expect("symlink create");
    let error = install_at(dir.path(), false).expect_err("symlink must be refused");
    assert_eq!(error.kind, "conflict");
    assert!(error.message.contains("symlink"));
}

#[test]
fn install_at_handles_dir_at_target_path() {
    let _lock = lock_workflow_tests();
    let dir = TempDir::new("install-dir-target");
    let target = dir.child(PLUGIN_FILENAME);
    std::fs::create_dir_all(&target).expect("dir create");
    let error = install_at(dir.path(), false).expect_err("directory at path must be refused");
    assert_eq!(error.kind, "conflict");
}

// ---------------------------------------------------------------------------
// Status coverage.
// ---------------------------------------------------------------------------

#[test]
fn status_at_reports_both_slots_when_nothing_is_installed() {
    let (_temp, _home, _xdg) = override_home("status-empty");
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let report = status_at(&cwd);
    assert!(!report.global.exists);
    assert!(!report.global.managed);
    assert!(!report.project.exists);
    assert!(!report.project.managed);
    assert!(report.global.path.contains(PLUGIN_FILENAME));
    assert!(report.project.path.contains(PLUGIN_FILENAME));
}

#[test]
fn status_at_reflects_installed_marker_and_size() {
    let (_temp, _home, _xdg) = override_home("status-installed");
    let dir = TempDir::new("status-installed-target");
    // install_at writes <dir>/<PLUGIN_FILENAME>; status_at(cwd) reads
    // the project slot at <cwd>/.opencode/plugins/<PLUGIN_FILENAME>.
    // Pass the project plugins dir directly so the two paths agree.
    let project_plugins = dir.child(".opencode").join("plugins");
    install_at(&project_plugins, false).expect("install");
    let report = status_at(dir.path());
    let project_path = project_plugins
        .join(PLUGIN_FILENAME)
        .to_string_lossy()
        .to_string();
    let slots = [&report.global, &report.project];
    let matching = slots
        .iter()
        .find(|target| target.path == project_path)
        .expect("project path appears in status");
    assert!(matching.exists);
    assert!(matching.managed);
    assert!(matching.size > 0);
    assert!(matching.mtime.is_some());
}

// ---------------------------------------------------------------------------
// Uninstall coverage.
// ---------------------------------------------------------------------------

#[test]
fn uninstall_at_removes_managed_file() {
    let _lock = lock_workflow_tests();
    let dir = TempDir::new("uninstall-managed");
    install_at(dir.path(), false).expect("install");
    let outcome = uninstall_at(dir.path()).expect("uninstall");
    assert_eq!(outcome.removed.len(), 1);
    assert!(outcome.warnings.is_empty());
    assert!(!dir.child(PLUGIN_FILENAME).exists());
}

#[test]
fn uninstall_at_refuses_foreign_file() {
    let _lock = lock_workflow_tests();
    let dir = TempDir::new("uninstall-foreign");
    let target = dir.child(PLUGIN_FILENAME);
    write_bytes(&target, b"#!/usr/bin/env node\nnot ours;\n");
    let error = uninstall_at(dir.path()).expect_err("must refuse foreign file");
    assert_eq!(error.kind, "conflict");
    assert!(
        dir.child(PLUGIN_FILENAME).exists(),
        "file must remain untouched"
    );
}

#[test]
fn uninstall_at_warns_when_target_missing() {
    let _lock = lock_workflow_tests();
    let dir = TempDir::new("uninstall-missing");
    let outcome = uninstall_at(dir.path()).expect("missing is warning, not error");
    assert!(outcome.removed.is_empty());
    assert_eq!(outcome.warnings.len(), 1);
}

// ---------------------------------------------------------------------------
// CLI executor coverage (uses HOME override so the real ~/.config is safe).
// ---------------------------------------------------------------------------

#[test]
fn execute_install_then_status_then_uninstall_round_trip_via_home_override() {
    let _lock = lock_workflow_tests();
    let (temp, _home_guard, xdg_guard) = override_home("exec-roundtrip");
    // Construct a project cwd that lives inside the temp dir so the
    // executor resolves `resolve_project_dir` against a sandbox.
    let project_cwd = temp.child("project");
    std::fs::create_dir_all(&project_cwd).expect("project cwd create");
    // cd into the sandbox so resolve_project_dir lands inside `temp`.
    let previous_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    std::env::set_current_dir(&project_cwd).expect("set cwd");

    let install_rc = execute_plugin(PluginCommand::Install {
        global: false,
        project: true,
        force: false,
    });
    assert_eq!(install_rc, 0, "install rc=0");

    let target = project_cwd
        .join(".opencode")
        .join("plugins")
        .join(PLUGIN_FILENAME);
    assert!(
        target.exists(),
        "project install wrote file at {}",
        target.display()
    );
    let bytes = read_bytes(&target);
    assert!(bytes.starts_with(MANAGED_MARKER.as_bytes()));

    // Second install must be idempotent (skipped, not duplicated).
    let install_rc2 = execute_plugin(PluginCommand::Install {
        global: false,
        project: true,
        force: false,
    });
    assert_eq!(install_rc2, 0);

    let _xdg_for_type = xdg_guard;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let report = status_at(&cwd);
    assert!(report.project.exists);
    assert!(report.project.managed);
    assert!(report.project.size > 0);

    let uninstall_rc = execute_plugin(PluginCommand::Uninstall {
        global: false,
        project: true,
    });
    assert_eq!(uninstall_rc, 0);
    assert!(!target.exists());

    // Restore the original cwd before the EnvGuards drop, so
    // subsequent tests run from the same starting state.
    let _ = std::env::set_current_dir(&previous_cwd);
}

// ---------------------------------------------------------------------------
// Adapter template coverage (compile-time invariants on the JS).
// ---------------------------------------------------------------------------

#[test]
fn adapter_template_is_well_formed_for_opencode_experimental_api() {
    let _lock = lock_workflow_tests();
    let source = adapter_source();
    assert!(source.starts_with(MANAGED_MARKER));
    assert!(source.contains("experimental_workspace.register"));
    assert!(source.contains("\"phasegent\""));
    assert!(source.contains("async configure"));
    assert!(source.contains("async create"));
    assert!(source.contains("async remove"));
    assert!(source.contains("async target"));
    // Branch binding detection (local-only path).
    assert!(source.contains("phasegent issue status"));
    // Worktree acquire uses --role orchestrator and --format json.
    assert!(source.contains("--role"));
    assert!(source.contains("orchestrator"));
    assert!(source.contains("worktree acquire"));
    assert!(source.contains("--format"));
    assert!(source.contains("json"));
    // Target returns the documented shape.
    assert!(source.contains("type: \"local\""));
    assert!(source.contains("directory:"));
    // create performs the best-effort mkdir -p.
    assert!(source.contains("mkdir -p"));
    // Git detection.
    assert!(source.contains("git rev-parse --git-common-dir"));
}

#[test]
fn envelope_serialises_with_stable_field_order() {
    let _lock = lock_workflow_tests();
    let envelope = InstallEnvelope {
        installed: vec!["/tmp/a".to_owned()],
        updated: vec![],
        skipped: vec![],
        warnings: vec![],
        errors: vec![],
        global_path: "/tmp/global".to_owned(),
        project_path: "/tmp/project".to_owned(),
    };
    let rendered = serde_json::to_string(&envelope).expect("serialize");
    // Stable ordering: installed before updated, etc.
    let installed_at = rendered.find("\"installed\"").expect("installed key");
    let updated_at = rendered.find("\"updated\"").expect("updated key");
    let project_path_at = rendered.find("\"project_path\"").expect("project_path key");
    assert!(installed_at < updated_at);
    assert!(updated_at < project_path_at);

    let status = StatusEnvelope {
        global: crate::plugin::PluginTargetStatus {
            path: "/tmp/global".to_owned(),
            exists: true,
            managed: true,
            size: 12,
            mtime: Some(0),
        },
        project: crate::plugin::PluginTargetStatus {
            path: "/tmp/project".to_owned(),
            exists: false,
            managed: false,
            size: 0,
            mtime: None,
        },
    };
    let rendered_status = serde_json::to_string(&status).expect("serialize status");
    assert!(rendered_status.contains("\"global\""));
    assert!(rendered_status.contains("\"project\""));
    assert!(rendered_status.contains("\"managed\":true"));

    let uninstall = UninstallEnvelope {
        removed: vec!["/tmp/a".to_owned()],
        warnings: vec![],
        errors: vec![],
        global_path: "/tmp/global".to_owned(),
        project_path: "/tmp/project".to_owned(),
    };
    let rendered_uninstall = serde_json::to_string(&uninstall).expect("serialize uninstall");
    assert!(rendered_uninstall.contains("\"removed\""));
    assert!(rendered_uninstall.contains("\"global_path\""));
}
