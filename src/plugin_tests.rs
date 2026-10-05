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
//!   agent's own slim skill to its system prompt; issue 665 adds the
//!   `explore` read-only research skill so every protocol agent is bound. Issue #616
//!   makes
//!   worktree creation opt-in: the lazy path never acquires one, and a
//!   host create request passes `isolate` so the dedicated directory is
//!   really created. It must not
//!   register a slash command: the live v2.0.11 command draft only
//!   accepts an Effect-returning `execute`, which a promise plugin
//!   cannot build. The embedded skill body must also match
//!   `skills/phasegent/SKILL.md` byte-for-byte (issue #544), and each
//!   embedded role prompt must match `skills/phasegent/SKILL.<role>.md`
//!   (issue #602; issue 665 adds `SKILL.explore.md`).
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
            path,
        }) => {
            assert!(!global);
            assert!(!project);
            assert!(!force);
            assert_eq!(path, None);
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
            path,
        }) => {
            assert!(global);
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
        Command::Plugin(PluginCommand::Status { path }) => assert_eq!(path, None),
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
        Command::Plugin(PluginCommand::Status { path }) => {
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
        Command::Plugin(PluginCommand::Uninstall {
            global,
            project,
            path,
        }) => {
            assert!(!global);
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
fn status_at_reports_both_slots_when_nothing_is_installed() {
    let _lock = lock_workflow_tests();
    let (_temp, _home, _xdg) = override_home("status-empty");
    let project = TempDir::new("status-empty-project");
    let report = status_at(project.path()).expect("status resolves");
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
        path: None,
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
        global: false,
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
        global: false,
        project: true,
        force: false,
        path: None,
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
        global: false,
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
        global: false,
        project: false,
        force: false,
        path: Some(path.clone()),
    });
    assert_eq!(install_rc2, 0, "explicit re-install rc=0");
    assert_eq!(read_bytes(&target), adapter_source().as_bytes());

    // Explicit status inspects exactly this directory through the same
    // primitive the scoped slots use, and returns it under `target`.
    let status_rc = execute_plugin(PluginCommand::Status {
        path: Some(path.clone()),
    });
    assert_eq!(status_rc, 0, "explicit status rc=0");
    let envelope = crate::cli::plugin::explicit_status_envelope(&source_dir);
    assert!(envelope.global.is_none() && envelope.project.is_none());
    let status = envelope.target.expect("explicit target status");
    assert!(status.exists && status.managed && status.size > 0);
    assert_eq!(status.path, target.to_string_lossy().to_string());

    let uninstall_rc = execute_plugin(PluginCommand::Uninstall {
        global: false,
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
        global: false,
        project: false,
        force: false,
        path: Some(path.clone()),
    });
    assert_eq!(refused, 0);
    assert!(read_bytes(&target).starts_with(b"#!/usr/bin/env node"));
    assert!(!backup.exists(), "a refused install writes no backup");

    let forced = execute_plugin(PluginCommand::Install {
        global: false,
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
    // Issue 665: every protocol agent is bound, explore included, and its
    // read-only research prompt ships embedded like the other role skills.
    assert!(source.contains("[\"orchestrator\", \"phasegent-orchestrator\"]"));
    assert!(source.contains("[\"executor\", \"phasegent-executor\"]"));
    assert!(source.contains("[\"reviewer\", \"phasegent-reviewer\"]"));
    assert!(source.contains("[\"explore\", \"phasegent-explore\"]"));
    assert!(source.contains("const SKILL_EXPLORE_CONTENT = `"));
    assert!(source.contains("id: \"phasegent-explore\""));
    assert!(source.contains("/builtin/phasegent-explore.md"));
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

/// Extract the JS template literal assigned to `declaration` and resolve its
/// escapes back to the bytes the adapter hands the host, so an embedded prompt
/// can be compared with the repository copy (issue #544; issue #602 extends the
/// same contract to the three role skills).
///
/// Only the escapes the prompt text actually needs are resolved: `` \` ``,
/// `\\` and `\$`. Any other backslash pair keeps both characters, so an
/// unsupported escape shows up as a difference instead of being dropped.
fn embedded_prompt(declaration: &str) -> String {
    let source = adapter_source();
    let start = source
        .find(declaration)
        .unwrap_or_else(|| panic!("the adapter must declare {declaration}"))
        + declaration.len();
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
                    .expect("an embedded prompt must not end inside an escape")
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
    let end = end.expect("the embedded prompt must close its template literal");
    assert!(
        body[end..].starts_with("`;"),
        "an embedded prompt must be terminated by a lone backtick + semicolon"
    );
    resolved
}

fn embedded_skill() -> String {
    let resolved = embedded_prompt("const SKILL_CONTENT = `");
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

/// Issue #602 keeps the role prompts slim, so the embedded mirror matters just
/// as much for them: every `SKILL.<role>.md` the adapter inlines must equal the
/// repository copy byte-for-byte, and still open with its frontmatter. Issue 665
/// adds the `explore` read-only research skill to the same contract.
#[test]
fn embedded_role_skills_match_the_repository_copies() {
    let _lock = lock_workflow_tests();
    for (declaration, on_disk) in [
        (
            "const SKILL_ORCHESTRATOR_CONTENT = `",
            include_str!("../skills/phasegent/SKILL.orchestrator.md"),
        ),
        (
            "const SKILL_EXECUTOR_CONTENT = `",
            include_str!("../skills/phasegent/SKILL.executor.md"),
        ),
        (
            "const SKILL_REVIEWER_CONTENT = `",
            include_str!("../skills/phasegent/SKILL.reviewer.md"),
        ),
        (
            "const SKILL_EXPLORE_CONTENT = `",
            include_str!("../skills/phasegent/SKILL.explore.md"),
        ),
    ] {
        let embedded = embedded_prompt(declaration);
        assert!(
            embedded.starts_with("---\n"),
            "the embedded prompt for {declaration} must keep its frontmatter"
        );
        assert_eq!(
            embedded, on_disk,
            "the adapter's {declaration} and the repository role skill must stay \
             byte-for-byte identical; editing one means re-escaping the other in the \
             same change (改一份必须同步另一份)"
        );
    }
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
