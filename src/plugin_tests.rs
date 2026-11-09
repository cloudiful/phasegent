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
//! * **Adapter template** — the generated JS contains the
//!   `// phasegent:managed` marker, the OpenCode v2
//!   `export default { id, setup }` shape, the `worktree.transform`
//!   strategy, the issue #440 `tool.execute.before` redirect helpers,
//!   and the issue #22 `agent.transform` binding that prepends only a
//!   short routing/safety hint. It registers no embedded skill and no
//!   slash command: the protocol and role references are one filesystem
//!   skill installed by `phasegent skill install`. Issue #616 makes
//!   worktree creation opt-in: the lazy path never acquires one, and a
//!   host create request passes `isolate` so the dedicated directory is
//!   really created.
//!
//! All filesystem tests use a temp directory and override
//! `HOME`/`XDG_CONFIG_HOME` so the operator's real `~/.config` is
//! never touched. The marker detection stays string-based so the
//! tests never depend on Bun being installed.

use crate::cli::plugin::{InstallEnvelope, StatusEnvelope, UninstallEnvelope, execute_plugin};
use crate::command::{self, Command, PluginCommand};
use crate::infra::storage::test_support::{EnvGuard, lock_workflow_tests};
use crate::plugin::{
    FOREIGN_BACKUP_SUFFIX, MANAGED_MARKER, PLUGIN_FILENAME, adapter_source, install_at,
    resolve_global_dir, status_global, status_project, uninstall_at,
};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let dir = crate::test_scratch::root().join(format!(
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

/// Set, empty, or unset the given environment variables for the duration of
/// the guard, restoring the previous values on Drop. The `EnvGuard` helper in
/// `test_support` only sets a value, while the Windows-profile resolution
/// tests need to model an unset `HOME`/`XDG_CONFIG_HOME`, so this local guard
/// covers both. Callers hold `lock_workflow_tests()` because the mutation is
/// process-wide.
struct ScopedEnv {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl ScopedEnv {
    fn apply(changes: &[(&'static str, Option<&str>)]) -> Self {
        let mut saved = Vec::with_capacity(changes.len());
        for &(name, value) in changes {
            saved.push((name, std::env::var_os(name)));
            // SAFETY::`set_var`/`remove_var` are unsafe in this toolchain;
            // tests serialise on `lock_workflow_tests()`.
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
        Self { saved }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        for (name, previous) in self.saved.drain(..).rev() {
            // SAFETY::symmetric to `apply` above; still under the lock.
            unsafe {
                match previous {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
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
    let _lock = lock_workflow_tests();
    let invocation =
        command::parse(&strings(["plugin", "install", "--project", "--force"])).unwrap();
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
    let _lock = lock_workflow_tests();
    let invocation = command::parse(&strings(["plugin", "status"])).unwrap();
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
    let _lock = lock_workflow_tests();
    let invocation = command::parse(&strings([
        "plugin",
        "status",
        "--path",
        "/tmp/chezmoi/plugins",
    ]))
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
    let _lock = lock_workflow_tests();
    let invocation = command::parse(&strings(["plugin", "uninstall", "--project"])).unwrap();
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
fn install_at_writes_managed_file_with_marker_and_v2_plugin_definition() {
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
    assert!(text.contains("id: \"phasegent-worktree\""));
    assert!(text.contains("async setup(context)"));
    assert!(text.contains("export default PhasegentWorktreePlugin"));
    assert!(text.contains("PhasegentWorktreePlugin.redirect"));
    assert!(text.contains("\"execute.before\""));
    assert!(text.contains("worktree.transform"));
    assert!(text.contains("session.move"));
    assert!(text.contains("[\"issue\", \"status\"]"));
    assert!(text.contains("PHASEGENT_ROLE"));
    assert!(text.contains("orchestrator"));
    assert!(!text.contains("--role"));
    assert!(text.contains("worktree acquire"));
    assert!(text.contains("--format"));
    assert!(text.contains("\"json\""));
    assert!(text.contains("redirectPaths"));
    assert!(
        !text.contains("experimental_workspace.register"),
        "the v1 workspace adapter contract must be gone from the template"
    );
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
fn status_global_reports_the_default_target_when_nothing_is_installed() {
    let _lock = lock_workflow_tests();
    let (_temp, _home, _xdg) = override_home("status-empty");
    let report = status_global().expect("status resolves");
    assert!(!report.exists);
    assert!(!report.managed);
    assert!(report.path.contains(PLUGIN_FILENAME));
}

#[test]
fn install_at_preserves_an_existing_private_directory_mode() {
    let _lock = lock_workflow_tests();
    let dir = TempDir::new("install-private-mode");
    std::fs::create_dir_all(dir.path()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        install_at(dir.path(), false).expect("install");
        assert_eq!(
            std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    #[cfg(not(unix))]
    {
        install_at(dir.path(), false).expect("install");
    }
}

#[test]
fn status_project_reflects_installed_marker_and_size() {
    let _lock = lock_workflow_tests();
    let (_temp, _home, _xdg) = override_home("status-installed");
    let dir = TempDir::new("status-installed-target");
    let project_plugins = dir.child(".opencode").join("plugins");
    install_at(&project_plugins, false).expect("install");
    let report = status_project(dir.path());
    let project_path = project_plugins
        .join(PLUGIN_FILENAME)
        .to_string_lossy()
        .to_string();
    assert_eq!(report.path, project_path);
    assert!(report.exists);
    assert!(report.managed);
    assert!(report.size > 0);
}

// ---------------------------------------------------------------------------
// Global path resolution coverage (issue #591).
// ---------------------------------------------------------------------------

#[test]
fn global_dir_prefers_xdg_over_home_and_profile() {
    let _lock = lock_workflow_tests();
    let temp = TempDir::new("global-xdg");
    let xdg = temp.child("xdg");
    let home = temp.child("home");
    let profile = temp.child("profile");
    let xdg_str = xdg.to_string_lossy().to_string();
    let home_str = home.to_string_lossy().to_string();
    let profile_str = profile.to_string_lossy().to_string();
    let _env = ScopedEnv::apply(&[
        ("XDG_CONFIG_HOME", Some(&xdg_str)),
        ("HOME", Some(&home_str)),
        ("USERPROFILE", Some(&profile_str)),
    ]);
    assert_eq!(
        resolve_global_dir().expect("xdg root resolves"),
        xdg.join("opencode").join("plugins")
    );
}

#[test]
fn global_dir_falls_back_to_home_config() {
    let _lock = lock_workflow_tests();
    let temp = TempDir::new("global-home");
    let home = temp.child("home");
    let profile = temp.child("profile");
    let home_str = home.to_string_lossy().to_string();
    let profile_str = profile.to_string_lossy().to_string();
    let _env = ScopedEnv::apply(&[
        ("XDG_CONFIG_HOME", None),
        ("HOME", Some(&home_str)),
        ("USERPROFILE", Some(&profile_str)),
    ]);
    assert_eq!(
        resolve_global_dir().expect("home root resolves"),
        home.join(".config").join("opencode").join("plugins")
    );
}

#[test]
fn global_dir_falls_back_to_userprofile_without_home() {
    let _lock = lock_workflow_tests();
    let temp = TempDir::new("global-profile");
    let profile = temp.child("profile");
    let profile_str = profile.to_string_lossy().to_string();
    let _env = ScopedEnv::apply(&[
        ("XDG_CONFIG_HOME", None),
        ("HOME", None),
        ("USERPROFILE", Some(&profile_str)),
    ]);
    assert_eq!(
        resolve_global_dir().expect("profile root resolves"),
        profile.join(".config").join("opencode").join("plugins"),
        "Windows without HOME must resolve under %USERPROFILE%/.config"
    );
}

#[test]
fn global_dir_skips_empty_roots() {
    let _lock = lock_workflow_tests();
    let temp = TempDir::new("global-empty");
    let home = temp.child("home");
    let profile = temp.child("profile");
    let home_str = home.to_string_lossy().to_string();
    let profile_str = profile.to_string_lossy().to_string();

    let env = ScopedEnv::apply(&[
        ("XDG_CONFIG_HOME", Some("")),
        ("HOME", Some(&home_str)),
        ("USERPROFILE", Some(&profile_str)),
    ]);
    assert_eq!(
        resolve_global_dir().expect("empty xdg is skipped"),
        home.join(".config").join("opencode").join("plugins")
    );
    drop(env);

    let _env = ScopedEnv::apply(&[
        ("XDG_CONFIG_HOME", Some("")),
        ("HOME", Some("")),
        ("USERPROFILE", Some(&profile_str)),
    ]);
    assert_eq!(
        resolve_global_dir().expect("empty home is skipped"),
        profile.join(".config").join("opencode").join("plugins")
    );
}

#[test]
fn global_dir_errors_when_no_root_is_usable() {
    let _lock = lock_workflow_tests();
    let _env = ScopedEnv::apply(&[
        ("XDG_CONFIG_HOME", None),
        ("HOME", None),
        ("USERPROFILE", None),
    ]);
    let error = resolve_global_dir().expect_err("no usable root must error");
    assert_eq!(error.kind, "filesystem");
    for name in ["XDG_CONFIG_HOME", "HOME", "USERPROFILE"] {
        assert!(
            error.message.contains(name),
            "error must name {name}; got: {}",
            error.message
        );
    }
}

#[test]
fn status_global_errors_instead_of_resolving_a_relative_target() {
    let _lock = lock_workflow_tests();
    let _env = ScopedEnv::apply(&[
        ("XDG_CONFIG_HOME", None),
        ("HOME", None),
        ("USERPROFILE", None),
    ]);
    let error = status_global().expect_err("unresolved status must error");
    assert_eq!(error.kind, "filesystem");
    assert!(
        error.message.contains("USERPROFILE"),
        "error must stay clear about the missing roots; got: {}",
        error.message
    );
}

#[test]
fn plugin_scopes_share_the_global_resolver_via_userprofile() {
    let _lock = lock_workflow_tests();
    let temp = TempDir::new("exec-profile");
    let profile = temp.child("profile");
    let project_cwd = temp.child("project");
    std::fs::create_dir_all(&profile).expect("profile dir");
    std::fs::create_dir_all(&project_cwd).expect("project cwd");
    let profile_str = profile.to_string_lossy().to_string();
    let env = ScopedEnv::apply(&[
        ("XDG_CONFIG_HOME", None),
        ("HOME", None),
        ("USERPROFILE", Some(&profile_str)),
    ]);
    let previous_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    std::env::set_current_dir(&project_cwd).expect("set cwd");

    let global_target = profile
        .join(".config")
        .join("opencode")
        .join("plugins")
        .join(PLUGIN_FILENAME);

    let install_rc = execute_plugin(PluginCommand::Install {
        project: false,
        force: false,
        path: None,
    });
    assert_eq!(install_rc, 0, "global install rc=0");
    assert!(
        global_target.exists(),
        "install must write {}",
        global_target.display()
    );

    let report = status_global().expect("status resolves through the same resolver");
    assert_eq!(
        report.path,
        global_target.to_string_lossy(),
        "status must inspect the USERPROFILE-derived path"
    );
    assert!(report.exists);
    assert!(report.managed);

    let uninstall_rc = execute_plugin(PluginCommand::Uninstall {
        project: false,
        path: None,
    });
    assert_eq!(uninstall_rc, 0, "global uninstall rc=0");
    assert!(!global_target.exists());

    let _ = std::env::set_current_dir(&previous_cwd);
    drop(env);
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
        project: true,
        force: false,
        path: None,
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
        project: true,
        force: false,
        path: None,
    });
    assert_eq!(install_rc2, 0);

    let _xdg_for_type = xdg_guard;
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let report = status_project(&cwd);
    assert!(report.exists);
    assert!(report.managed);
    assert!(report.size > 0);

    let uninstall_rc = execute_plugin(PluginCommand::Uninstall {
        project: true,
        path: None,
    });
    assert_eq!(uninstall_rc, 0);
    assert!(!target.exists());

    // Restore the original cwd before the EnvGuards drop, so
    // subsequent tests run from the same starting state.
    let _ = std::env::set_current_dir(&previous_cwd);
}

// ---------------------------------------------------------------------------
// Explicit `--path DIR` executor coverage (issue 666).
// ---------------------------------------------------------------------------

#[test]
fn explicit_path_install_status_uninstall_targets_only_that_directory() {
    let _lock = lock_workflow_tests();
    let (temp, _home_guard, _xdg_guard) = override_home("exec-explicit-path");
    // A chezmoi-style source directory: the operator names the directory that
    // holds phasegent-worktree.js and the installer appends the filename.
    let source_dir = temp.child("chezmoi/home/dot_config/opencode/plugins");
    let project_cwd = temp.child("project");
    std::fs::create_dir_all(&project_cwd).expect("project cwd create");
    let previous_cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    std::env::set_current_dir(&project_cwd).expect("set cwd");

    let path = source_dir.to_string_lossy().to_string();
    let install_rc = execute_plugin(PluginCommand::Install {
        project: false,
        force: false,
        path: Some(path.clone()),
    });
    assert_eq!(install_rc, 0, "explicit install rc=0");

    let target = source_dir.join(PLUGIN_FILENAME);
    assert!(
        target.exists(),
        "explicit install wrote {}",
        target.display()
    );
    assert!(read_bytes(&target).starts_with(MANAGED_MARKER.as_bytes()));

    // The default scopes stay untouched by an explicit-path invocation.
    let global_target = resolve_global_dir()
        .expect("global dir resolves")
        .join(PLUGIN_FILENAME);
    let project_target = project_cwd
        .join(".opencode")
        .join("plugins")
        .join(PLUGIN_FILENAME);
    assert!(!global_target.exists(), "global slot must stay untouched");
    assert!(!project_target.exists(), "project slot must stay untouched");

    // Idempotent re-run leaves the managed bytes in place.
    let install_rc2 = execute_plugin(PluginCommand::Install {
        project: false,
        force: false,
        path: Some(path.clone()),
    });
    assert_eq!(install_rc2, 0, "explicit re-install rc=0");
    assert_eq!(read_bytes(&target), adapter_source().as_bytes());

    // Explicit status inspects exactly this directory through the same
    // primitive the scoped slots use, and returns it under `target`.
    let status_rc = execute_plugin(PluginCommand::Status {
        project: false,
        path: Some(path.clone()),
    });
    assert_eq!(status_rc, 0, "explicit status rc=0");
    let status = crate::plugin::inspect_target(&source_dir);
    assert!(status.exists && status.managed && status.size > 0);
    assert_eq!(status.path, target.to_string_lossy().to_string());

    let uninstall_rc = execute_plugin(PluginCommand::Uninstall {
        project: false,
        path: Some(path),
    });
    assert_eq!(uninstall_rc, 0, "explicit uninstall rc=0");
    assert!(!target.exists(), "explicit uninstall removed the target");
    assert!(!global_target.exists());
    assert!(!project_target.exists());

    let _ = std::env::set_current_dir(&previous_cwd);
}

#[test]
fn explicit_path_install_keeps_the_foreign_file_backup_behavior() {
    let _lock = lock_workflow_tests();
    let (temp, _home_guard, _xdg_guard) = override_home("exec-explicit-force");
    let dir = temp.child("chezmoi/plugins");
    let target = dir.join(PLUGIN_FILENAME);
    let backup = dir.join(format!("{PLUGIN_FILENAME}{FOREIGN_BACKUP_SUFFIX}"));
    write_bytes(&target, b"#!/usr/bin/env node\nnot ours;\n");
    let path = dir.to_string_lossy().to_string();

    let refused = execute_plugin(PluginCommand::Install {
        project: false,
        force: false,
        path: Some(path.clone()),
    });
    assert_eq!(refused, 0);
    assert!(read_bytes(&target).starts_with(b"#!/usr/bin/env node"));
    assert!(!backup.exists(), "a refused install writes no backup");

    let forced = execute_plugin(PluginCommand::Install {
        project: false,
        force: true,
        path: Some(path),
    });
    assert_eq!(forced, 0);
    assert!(backup.exists(), "forced install backs up the foreign file");
    assert!(read_bytes(&target).starts_with(MANAGED_MARKER.as_bytes()));
}

#[test]
fn explicit_path_envelopes_use_the_additive_target_field() {
    let _lock = lock_workflow_tests();
    let target = "/tmp/chezmoi/plugins/phasegent-worktree.js".to_owned();

    let install = InstallEnvelope {
        installed: vec![target.clone()],
        updated: vec![],
        skipped: vec![],
        warnings: vec![],
        errors: vec![],
        global_path: None,
        project_path: None,
        target_path: Some(target.clone()),
    };
    let rendered = serde_json::to_string(&install).expect("serialize explicit install");
    assert!(rendered.contains("\"target_path\""));
    assert!(!rendered.contains("global_path"), "got: {rendered}");
    assert!(!rendered.contains("project_path"), "got: {rendered}");

    let status = StatusEnvelope {
        global: None,
        project: None,
        target: Some(crate::plugin::PluginTargetStatus {
            path: target.clone(),
            exists: true,
            managed: true,
            size: 12,
            mtime: Some(0),
        }),
    };
    let rendered_status = serde_json::to_string(&status).expect("serialize explicit status");
    assert!(rendered_status.contains("\"target\""));
    assert!(
        !rendered_status.contains("\"global\""),
        "got: {rendered_status}"
    );
    assert!(
        !rendered_status.contains("\"project\""),
        "got: {rendered_status}"
    );

    let uninstall = UninstallEnvelope {
        removed: vec![target.clone()],
        warnings: vec![],
        errors: vec![],
        global_path: None,
        project_path: None,
        target_path: Some(target),
    };
    let rendered_uninstall =
        serde_json::to_string(&uninstall).expect("serialize explicit uninstall");
    assert!(rendered_uninstall.contains("\"target_path\""));
    assert!(!rendered_uninstall.contains("\"global_path\""));
}

// ---------------------------------------------------------------------------
// Adapter template coverage (compile-time invariants on the JS).
// ---------------------------------------------------------------------------

#[test]
fn adapter_template_is_well_formed_for_opencode_v2_api() {
    let _lock = lock_workflow_tests();
    let source = adapter_source();
    assert!(source.starts_with(MANAGED_MARKER));
    assert!(source.contains("export default PhasegentWorktreePlugin"));
    assert!(source.contains("id: \"phasegent-worktree\""));
    assert!(source.contains("async setup(context)"));
    // v2 hook + strategy registrations replace the v1 workspace adapter.
    assert!(source.contains("\"execute.before\""));
    assert!(source.contains("worktree.transform"));
    assert!(source.contains("editor.add("));
    assert!(!source.contains("experimental_workspace.register"));
    // Branch binding detection (local-only path): `phasegent issue status`, run
    // with the project directory as cwd.
    assert!(source.contains("[\"issue\", \"status\"]"));
    // Worktree acquire scopes the orchestrator role to the call and uses
    // --format json.
    assert!(source.contains("PHASEGENT_ROLE"));
    assert!(source.contains("orchestrator"));
    assert!(!source.contains("--role"));
    assert!(source.contains("worktree acquire"));
    assert!(source.contains("--format"));
    assert!(source.contains("json"));
    // Issue 651 P4: the adapter guarantees the session identity on every
    // managed acquire — `--session` travels with the call — and refuses
    // an anonymous acquire before any CLI round-trip instead of booking
    // under a fabricated owner.
    assert!(source.contains("args.push(\"--session\", String(sessionId))"));
    assert!(source.contains("without a session identity"));
    // The acquired worktree becomes the session directory (v2 session.move).
    assert!(source.contains("session.move"));
    // Graceful degradation keeps the original directory on any failure.
    assert!(source.contains("reusing original directory"));
    // Removal is never performed by the adapter.
    assert!(source.contains("phasegent worktree prune"));
}

/// `issue close` returns a closing session to the main checkout through the
/// OpenCode API before removing its worktree, so the adapter's registry can
/// name a directory that is already gone. The host
/// must therefore outrank the registry, and the bundle must carry that
/// contract: a fresh (never cached) host read, an invalidation that clears
/// every reference to the dead directory, and an exact per-session registry
/// with no cross-session fallback.
#[test]
fn adapter_template_reconciles_the_registry_against_the_host() {
    let _lock = lock_workflow_tests();
    let source = adapter_source();
    // `adapter_source()` is the linked dist, where the build resolves `export`
    // away, so these assert the declaration the artifact actually carries.
    // The authoritative read is a separate entry point from the cached one,
    // so a reconciliation can never be answered from the session cache.
    assert!(source.contains("async function readAuthoritativeDirectory"));
    assert!(source.contains("async function readSessionInfo"));
    assert!(source.contains("async function reconcileSessionLocation"));
    // A settled placement is reconciled before its cached target is trusted,
    // and the invalidation clears the session entry, the settled and confirmed
    // markers, an in-flight move, and the cached host record. Registry lookups
    // are session-scoped, with no shared fallback.
    assert!(source.contains("function forgetSessionWorktree"));
    for cleared in [
        "sessionWorktrees.delete(key)",
        "sessionInfo.delete(key)",
        "placedSessions.delete(key)",
        "confirmedSessions.delete(key)",
        "moveAttempts.delete(key)",
    ] {
        assert!(
            source.contains(cleared),
            "invalidation must clear {cleared}"
        );
    }
    assert!(!source.contains("activeWorktree"));
    assert!(source.contains("reconcileSessionLocation(context, sessionId, readDirectory)"));
    assert!(source.contains("refusing to acquire a worktree"));
    // The plugin still owns placement and only placement: it never spawns the
    // `opencode` client, and the CLI keeps the close-time move.
    assert!(!source.contains("\"opencode\""));
    assert!(source.contains("phasegent worktree prune"));
}

#[test]
fn adapter_template_documents_redirect_contract() {
    let _lock = lock_workflow_tests();
    let source = adapter_source();
    // v2 entry point registers the redirect hook and returns a cleanup.
    assert!(source.contains("export default PhasegentWorktreePlugin"));
    assert!(source.contains("async setup(context)"));
    assert!(source.contains("createRedirectHook"));
    assert!(source.contains("\"execute.before\""));
    // Pure helpers live on the exported plugin so the module has a single
    // `default` export for the v2 module schema.
    assert!(source.contains("PhasegentWorktreePlugin.redirect"));
    assert!(source.contains("isAbsolutePath"));
    assert!(source.contains("redirectPathValue"));
    assert!(source.contains("redirectPaths"));
    // v2 argument names: file tools use `path`, the shell tool is `shell`.
    assert!(source.contains("read: [\"path\"]"));
    assert!(source.contains("glob: [\"path\"]"));
    assert!(source.contains("SHELL_TOOLS = [\"shell\", \"bash\"]"));
    // The shell gets a bare/relative workdir; command rewriting (issue #541)
    // lives in the hook, not in the pure path redirect.
    assert!(source.contains("redirected.workdir = workdir"));
    // Absolute paths pass through (pure helper contract). Placement is a
    // `session.move` that fails closed when the host cannot perform it, so a
    // turn is never run unplaced instead of rewriting tool arguments.
    assert!(source.contains("if (isAbsolutePath(value)) return value"));
    assert!(source.contains("PLACEMENT_ERROR_PREFIX"));
    // Issue 37: the session prompt hook owns the whole placement decision and
    // the tool hook carries command rewriting only, so no tool invocation is
    // cancelled to move a session and no pending-placement safety fallback
    // remains.
    assert!(source.contains("context.session.hook"));
    assert!(source.contains("createPromptHook"));
    assert_eq!(source.matches("await ensureSessionWorktree(").count(), 1);
    assert!(source.contains("await ensureSessionWorktree(context, event.sessionID, deps)"));
    assert!(!source.contains("PLACEMENT_AT_PROMPT"));
    assert!(!source.contains("session placement pending"));
    // Obsolete placement state: the landing re-check only existed to decide
    // whether to cancel the current tool call.
    assert!(!source.contains("movedSessions"));
    assert!(!source.contains("hostSessionDirectory"));
    // Issue 616: creating a worktree is opt-in. The lazy path never acquires
    // one — it stays in the current checkout and points at the explicit
    // isolation command — while a host create request passes `isolate` so the
    // requested dedicated directory is really created.
    assert!(!source.contains("rememberWorktree(sessionId, acquired.path)"));
    assert!(source.contains("no worktree was created for issue"));
    assert!(source.contains("staying in the current checkout"));
    assert!(source.contains("args.push(\"--isolate\")"));
    assert!(source.contains("{ isolate: true }"));
    // issue #541: shell command rewriting (agent-role injection, sub-agent
    // refusal, segment-scoped `--session`) replaced the append-only session
    // injection helper; the pure path redirect no longer touches commands.
    assert!(source.contains("rewritePhasegentCommand"));
    assert!(source.contains("agentRole"));
    assert!(source.contains("sessionPlaced"));
    // issue #754 P3: explore maps to its own least-privilege role, not the
    // reviewer surface, so it can publish an authorized recon record.
    assert!(source.contains("[\"explore\", \"explore\"]"));
    assert!(source.contains("[\"reviewer\", \"reviewer\"]"));
    assert!(!source.contains("[\"explore\", \"reviewer\"]"));
    // issue #859 P1: canonical ids are exact (no substring elevation), the
    // per-shell nonce bridge carries the managed env, and the host marker is
    // classification-only.
    assert!(source.contains("agent === hint"));
    assert!(source.contains("createManagedShellBridge"));
    assert!(source.contains("createShellEnvHook"));
    assert!(source.contains("createToolAfterHook"));
    assert!(source.contains("isShellHookSupported"));
    assert!(source.contains("# phasegent-context:"));
    assert!(source.contains("PHASEGENT_SESSION_ID"));
    assert!(source.contains("\"create.before\""));
    assert!(source.contains("\"execute.after\""));
    assert!(source.contains("host exposes no shell hook"));
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
        global_path: Some("/tmp/global".to_owned()),
        project_path: Some("/tmp/project".to_owned()),
        target_path: None,
    };
    let rendered = serde_json::to_string(&envelope).expect("serialize");
    // Stable ordering: installed before updated, etc.
    let installed_at = rendered.find("\"installed\"").expect("installed key");
    let updated_at = rendered.find("\"updated\"").expect("updated key");
    let project_path_at = rendered.find("\"project_path\"").expect("project_path key");
    assert!(installed_at < updated_at);
    assert!(updated_at < project_path_at);
    // The explicit-path field stays absent in the default scope mode.
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
