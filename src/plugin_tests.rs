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
//!   `// phasegent:managed` marker, the OpenCode v2
//!   `export default { id, setup }` shape, the
//!   `phasegent worktree acquire` call with its per-call
//!   `PHASEGENT_ROLE` scope, the v2
//!   `worktree.transform` strategy, the issue #440
//!   `tool.execute.before` redirect helpers, and the issue #533
//!   `skill.transform` embedded skill registration. Issue #572 adds
//!   the `agent.transform` binding that prepends each protocol
//!   agent's own slim skill to its system prompt. It must not
//!   register a slash command: the live v2.0.11 command draft only
//!   accepts an Effect-returning `execute`, which a promise plugin
//!   cannot build. The embedded skill body must also match
//!   `skills/phasegent/SKILL.md` byte-for-byte (issue #544).
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
    resolve_global_dir, status_at, uninstall_at,
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
fn status_at_reports_both_slots_when_nothing_is_installed() {
    let _lock = lock_workflow_tests();
    let (_temp, _home, _xdg) = override_home("status-empty");
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let report = status_at(&cwd).expect("status resolves");
    assert!(!report.global.exists);
    assert!(!report.global.managed);
    assert!(!report.project.exists);
    assert!(!report.project.managed);
    assert!(report.global.path.contains(PLUGIN_FILENAME));
    assert!(report.project.path.contains(PLUGIN_FILENAME));
}

#[test]
fn status_at_reflects_installed_marker_and_size() {
    let _lock = lock_workflow_tests();
    let (_temp, _home, _xdg) = override_home("status-installed");
    let dir = TempDir::new("status-installed-target");
    // install_at writes <dir>/<PLUGIN_FILENAME>; status_at(cwd) reads
    // the project slot at <cwd>/.opencode/plugins/<PLUGIN_FILENAME>.
    // Pass the project plugins dir directly so the two paths agree.
    let project_plugins = dir.child(".opencode").join("plugins");
    install_at(&project_plugins, false).expect("install");
    let report = status_at(dir.path()).expect("status resolves");
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
fn status_at_errors_instead_of_resolving_a_relative_target() {
    let _lock = lock_workflow_tests();
    let _env = ScopedEnv::apply(&[
        ("XDG_CONFIG_HOME", None),
        ("HOME", None),
        ("USERPROFILE", None),
    ]);
    let error = status_at(Path::new("/tmp")).expect_err("unresolved status must error");
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
        global: true,
        project: false,
        force: false,
    });
    assert_eq!(install_rc, 0, "global install rc=0");
    assert!(
        global_target.exists(),
        "install must write {}",
        global_target.display()
    );

    let report = status_at(&project_cwd).expect("status resolves through the same resolver");
    assert_eq!(
        report.global.path,
        global_target.to_string_lossy(),
        "status must inspect the USERPROFILE-derived path"
    );
    assert!(report.global.exists);
    assert!(report.global.managed);

    let uninstall_rc = execute_plugin(PluginCommand::Uninstall {
        global: true,
        project: false,
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
    let report = status_at(&cwd).expect("status resolves");
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
    // The acquired worktree becomes the session directory (v2 session.move).
    assert!(source.contains("session.move"));
    // Graceful degradation keeps the original directory on any failure.
    assert!(source.contains("reusing original directory"));
    // Removal is never performed by the adapter.
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
    // Absolute paths pass through and no-worktree sessions short-circuit.
    assert!(source.contains("if (isAbsolutePath(value)) return value"));
    assert!(source.contains("if (typeof workdir !== \"string\" || workdir.length === 0) return;"));
    // The worktree is remembered when acquire succeeds.
    assert!(source.contains("rememberWorktree(sessionId, acquired.path)"));
    // issue #541: shell command rewriting (agent-role injection, sub-agent
    // refusal, segment-scoped `--session`) replaced the append-only session
    // injection helper; the pure path redirect no longer touches commands.
    assert!(source.contains("rewritePhasegentCommand"));
    assert!(source.contains("agentRole"));
    assert!(source.contains("sessionPlaced"));
}

#[test]
fn adapter_template_registers_embedded_skill_without_a_command() {
    let _lock = lock_workflow_tests();
    let source = adapter_source();
    // issue #533: the live v2.0.11 command draft only accepts an
    // Effect-returning `execute`, which a promise plugin cannot build, so the
    // adapter must not touch the command domain at all. The previous
    // `update(name, mutate)` template raised a TypeError in the host and the
    // host then disabled the whole plugin, redirect hook included.
    assert!(!source.contains("context.command"));
    assert!(!source.contains("ACQUIRE_COMMAND_NAME"));
    assert!(!source.contains("worktreeAcquireCommandTemplate"));
    // issue #533: the phasegent skill travels with the plugin as an embedded
    // `Skill.Info` added through the runtime skill draft.
    assert!(source.contains("skill.transform"));
    // issue #572: each protocol agent gets its own slim skill prepended to its
    // `system` through the agent draft, so the role skill is the agent's stable
    // system prefix. The command domain stays untouched.
    assert!(source.contains("agent.transform"));
    assert!(source.contains("draft.add(definition)"));
    assert!(source.contains("const SKILL_ID = \"phasegent\""));
    assert!(source.contains("/builtin/phasegent.md"));
    assert!(source.contains("PHASEGENT_SESSION_ID"));
    assert!(source.contains("PHASEGENT_WORKTREE_NO_DISCOVER"));
    // The old SDK source shape is gone; the definition is the flat info with
    // the runtime field names.
    assert!(!source.contains("draft.source("));
    assert!(source.contains("path: SKILL_PATH"));
    // A missing registration surface degrades to a warning instead of failing
    // setup.
    assert!(source.contains("the phasegent skill stays unregistered"));
    assert!(source.contains("host skill draft exposes no add"));
}

/// Extract the JS template literal assigned to `SKILL_CONTENT` and resolve its
/// escapes back to the bytes the adapter hands the host, so the embedded skill
/// can be compared with the repository copy (issue #544).
///
/// Only the escapes the skill text actually needs are resolved: `` \` ``,
/// `\\` and `\$`. Any other backslash pair keeps both characters, so an
/// unsupported escape shows up as a difference instead of being dropped.
fn embedded_skill() -> String {
    const DECLARATION: &str = "const SKILL_CONTENT = `";
    let source = adapter_source();
    let start = source
        .find(DECLARATION)
        .expect("the adapter must declare SKILL_CONTENT")
        + DECLARATION.len();
    let body = &source[start..];
    let mut resolved = String::new();
    let mut chars = body.char_indices();
    let mut end = None;
    while let Some((index, ch)) = chars.next() {
        match ch {
            '`' => {
                end = Some(index);
                break;
            }
            '\\' => {
                let escaped = chars
                    .next()
                    .expect("SKILL_CONTENT must not end inside an escape")
                    .1;
                match escaped {
                    '`' => resolved.push('`'),
                    '\\' => resolved.push('\\'),
                    '$' => resolved.push('$'),
                    other => {
                        resolved.push('\\');
                        resolved.push(other);
                    }
                }
            }
            other => resolved.push(other),
        }
    }
    let end = end.expect("SKILL_CONTENT must close its template literal");
    assert!(
        body[end..].starts_with("`;"),
        "SKILL_CONTENT must be terminated by a lone backtick + semicolon"
    );
    assert!(
        resolved.contains("# Phasegent") && resolved.contains("## Marker protocol"),
        "the extracted SKILL_CONTENT must carry the skill body"
    );
    resolved
}

#[test]
fn embedded_skill_matches_the_repository_copy() {
    let _lock = lock_workflow_tests();
    // src/plugin.rs declares the embedded skill ships "same bytes" as
    // `skills/phasegent/SKILL.md`; this is the assertion behind that contract.
    // It reads the JS source rather than importing Bun, so the mirror stays
    // enforced even where Bun is unavailable.
    let embedded = embedded_skill();
    let on_disk = include_str!("../skills/phasegent/SKILL.md");
    assert_eq!(
        embedded, on_disk,
        "the adapter's embedded SKILL_CONTENT and \
         skills/phasegent/SKILL.md must stay byte-for-byte identical; \
         editing one means re-escaping the other in the same change \
         (改一份必须同步另一份)"
    );
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
