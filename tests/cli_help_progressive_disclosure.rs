//! Black-box regression coverage for the root and nested help
//! surface. The contract is intentionally small: root help must be
//! short and task-oriented, must not duplicate the deep resolver or
//! role/security essays, and must point operators at the canonical
//! nested help pages. Deep help pages must continue to carry the
//! details they own (`config provider` for the resolver chain,
//! `auth` for the role/security guidance, `workflow bootstrap` for
//! `--close-status-name`). Routing must remain intact so that every
//! documented pointer resolves to a non-error page.

// This shared fixture module serves several integration tests; this test
// intentionally uses only its binary and stdout helpers.
#[path = "support/mod.rs"]
mod support;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use support::{phasegent_bin, stdout_text};

/// Per-test scratch directory holding the throwaway SQLite the help
/// commands would otherwise touch. Help-only invocations never open
/// the database, but the runner pins `PHASEGENT_DB_PATH` so the test
/// environment matches production isolation rules. `PHASEGENT_CONFIG_PATH`
/// points at a guaranteed-missing file so the ProjectDirs default TOML can
/// never shadow help assertions.
struct ScratchDb {
    dir: PathBuf,
}

impl Drop for ScratchDb {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl ScratchDb {
    fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = support::scratch_root().join(format!(
            "phasegent-it-help-{}-{}-{}",
            std::process::id(),
            nanos,
            (nanos as u64) ^ (std::process::id() as u64),
        ));
        fs::create_dir_all(&dir).expect("create scratch dir");
        Self { dir }
    }

    fn path(&self) -> &std::path::Path {
        &self.dir
    }
}

fn run_help(args: &[&str]) -> Output {
    run_help_with_role(args, None)
}

/// Run help/CLI with an explicit `PHASEGENT_ROLE` so the role-aware surface can
/// be asserted black-box.
fn run_help_with_role(args: &[&str], role: Option<&str>) -> Output {
    let db = ScratchDb::new();
    let mut command = Command::new(phasegent_bin());
    command
        .args(args)
        .env("PHASEGENT_DB_PATH", db.path().as_os_str())
        .env_remove("PHASEGENT_PROVIDER")
        .env_remove("PHASEGENT_DEFAULT_PROVIDER")
        .env_remove("PHASEGENT_ROLE")
        .env_remove("PHASEGENT_API_BASE")
        .env_remove("PHASEGENT_REDMINE_API_BASE")
        .env_remove("PHASEGENT_REPOSITORY")
        .env_remove("PHASEGENT_PROJECT_ID")
        .env_remove("PHASEGENT_REDMINE_PROJECT_ID")
        .env_remove("PHASEGENT_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_REDMINE_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
        .env_remove("PHASEGENT_REDMINE_REPOSITORY_URL")
        .env(
            "PHASEGENT_CONFIG_PATH",
            db.dir.join("phasegent-missing.toml").as_os_str(),
        )
        .env("RUST_BACKTRACE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(role) = role {
        command.env("PHASEGENT_ROLE", role);
    }
    command.output().expect("spawn phasegent binary")
}

/// Root help must be short, must not leak the resolver chain or the
/// role disclaimer, and must not advertise bootstrap-only flags.
#[test]
fn root_help_is_short_and_points_at_deep_pages() {
    let output = run_help(&["--help"]);
    assert!(output.status.success(), "--help exited non-zero");
    let stdout = stdout_text(&output);

    assert!(
        !stdout.contains("Provider resolution precedence"),
        "root help must not embed the resolver chain; got:\n{stdout}",
    );
    assert!(
        !stdout.contains("Role selects a capability policy"),
        "root help must not repeat the role disclaimer; got:\n{stdout}",
    );
    assert!(
        !stdout.contains("--close-status-name"),
        "root help must not advertise bootstrap-only --close-status-name; got:\n{stdout}",
    );

    // Visible command index entries that operators rely on for one-glance routing.
    // Provisioning lives under the human-operator `admin` group; the legacy
    // top-level `auth`/`workflow` entries are gone by design.
    for command in [
        "issue",
        "comment",
        "admin",
        "config",
        "hooks",
        "--help <command>",
    ] {
        assert!(
            stdout.contains(command),
            "root help missing command/pointer {command:?}; got:\n{stdout}",
        );
    }

    assert!(
        stdout.contains("phasegent --help config provider"),
        "root help must point operators at the resolver chain page; got:\n{stdout}",
    );
    assert!(
        stdout.contains("phasegent --help admin"),
        "root help must point operators at the role/credential page; got:\n{stdout}",
    );

    // Universal global options stay advertised at root.
    for option in [
        "--provider",
        "--api-base",
        "--repository",
        "--project-id",
        "--close-status-id",
        "-h, --help",
        "-V, --version",
    ] {
        assert!(
            stdout.contains(option),
            "root help missing global option {option:?}; got:\n{stdout}",
        );
    }
}

/// Root help rendered with a Redmine provider filter must still
/// satisfy the contract and additionally expose the Redmine-scoped
/// commands without re-introducing the resolver or role disclaimer.
#[test]
fn root_help_remains_short_with_provider_filter() {
    let output = run_help(&["--provider", "redmine", "--help"]);
    assert!(
        output.status.success(),
        "--provider redmine --help exited non-zero"
    );
    let stdout = stdout_text(&output);

    assert!(
        !stdout.contains("Provider resolution precedence"),
        "filtered root help must not embed the resolver chain; got:\n{stdout}",
    );
    assert!(
        !stdout.contains("Role selects a capability policy"),
        "filtered root help must not repeat the role disclaimer; got:\n{stdout}",
    );
    assert!(
        !stdout.contains("--close-status-name"),
        "filtered root help must not advertise bootstrap-only --close-status-name; got:\n{stdout}",
    );

    for command in ["workflow", "timer", "status", "project"] {
        assert!(
            stdout.contains(command),
            "redmine-filtered root help missing {command:?}; got:\n{stdout}",
        );
    }
}

/// Issue 443 added `status transition` as the preferred status write (a
/// `--to`/`--status` target, or a bare call that auto-routes to the policy's
/// first allowed next status). The parser and the skill both treat it as the
/// primary entry point, so the help surface must advertise it and must not
/// claim a provider set the dispatcher does not serve: Redmine and local
/// implement `next`/`advance`/`transition`, while GitLab only serves
/// `list`/`set` and Forgejo rejects the whole status surface.
#[test]
fn status_help_advertises_transition_with_the_real_provider_set() {
    let output = run_help(&["--help", "status"]);
    assert!(output.status.success(), "--help status exited non-zero");
    let stdout = stdout_text(&output);
    for command in ["list", "next", "set", "advance", "transition"] {
        assert!(
            stdout.contains(command),
            "--help status must list {command:?}; got:\n{stdout}",
        );
    }

    let output = run_help(&["--help", "status", "advance"]);
    assert!(
        output.status.success(),
        "--help status advance exited non-zero"
    );
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("Redmine and local"),
        "--help status advance must name the real provider set; got:\n{stdout}",
    );
    assert!(
        !stdout.contains("Redmine-only"),
        "--help status advance must not claim Redmine-only; got:\n{stdout}",
    );

    let output = run_help(&["--help", "status", "set"]);
    assert!(output.status.success(), "--help status set exited non-zero");
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("GitLab"),
        "--help status set must name GitLab (managed workflow label); got:\n{stdout}",
    );

    // Every command the listing advertises must also resolve as a deep page,
    // and an unknown topic must still be rejected.
    let output = run_help(&["--help", "status", "transition"]);
    assert!(
        output.status.success(),
        "--help status transition must resolve; stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("status transition <NUMBER>") && stdout.contains("Redmine and local"),
        "--help status transition must document the command and its provider set; got:\n{stdout}",
    );

    let output = run_help(&["--help", "status", "bogus"]);
    assert!(
        !output.status.success(),
        "an unknown status help topic must still be rejected",
    );
}

/// The resolver chain must still be reachable, exactly once, through
/// the canonical nested page. This is the deep help page that the
/// root pointer now points at. The chain must name the TOML overlay,
/// its path override, and the SQLite-only write contract.
#[test]
fn config_provider_help_carries_the_resolver_chain() {
    let output = run_help(&["--help", "config", "provider"]);
    assert!(
        output.status.success(),
        "--help config provider exited non-zero"
    );
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("PHASEGENT_PROVIDER")
            && stdout.contains("PHASEGENT_DEFAULT_PROVIDER")
            && stdout.contains("role_config.provider")
            && stdout.contains("forgejo fallback"),
        "config provider help must carry the resolver chain; got:\n{stdout}",
    );
    assert!(
        stdout.contains("TOML")
            && stdout.contains("PHASEGENT_CONFIG_PATH")
            && stdout.contains("SQLite")
            && stdout.contains("shadows"),
        "config provider help must name the TOML overlay, path override, and SQLite-shadow contract; got:\n{stdout}",
    );
}

/// The role/security guidance remains owned by `auth` and is no
/// longer duplicated at root.
#[test]
fn auth_help_carries_role_and_credential_guidance() {
    let output = run_help(&["--help", "auth"]);
    assert!(output.status.success(), "--help auth exited non-zero");
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("capability policy")
            && stdout.contains("least-privilege")
            && stdout.contains("never accepted as command-line arguments"),
        "auth help must carry the role/credential guidance; got:\n{stdout}",
    );
}

/// `--close-status-name` remains documented under the command that
/// actually accepts it.
#[test]
fn workflow_bootstrap_help_documents_close_status_name() {
    let output = run_help(&["--help", "workflow", "bootstrap"]);
    assert!(
        output.status.success(),
        "--help workflow bootstrap exited non-zero"
    );
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("--close-status-name"),
        "workflow bootstrap help must still document --close-status-name; got:\n{stdout}",
    );
    assert!(
        stdout.contains("--close-status-id"),
        "workflow bootstrap help must still document --close-status-id; got:\n{stdout}",
    );
}

/// Config help must state the read-only TOML overlay, the CLI >
/// environment > TOML > SQLite > defaults precedence, and the
/// `PHASEGENT_CONFIG_PATH` override without echoing secrets.
#[test]
fn config_help_documents_toml_overlay_and_precedence() {
    let output = run_help(&["--help", "config"]);
    assert!(output.status.success(), "--help config exited non-zero");
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("CLI flags")
            && stdout.contains("TOML")
            && stdout.contains("PHASEGENT_CONFIG_PATH")
            && stdout.contains("SQLite only"),
        "config help must document TOML overlay, precedence, and path override; got:\n{stdout}",
    );
    let show = stdout_text(&run_help(&["--help", "config", "show"]));
    assert!(
        show.contains("SQLite only") || show.contains("persisted SQLite only"),
        "config show help must state the persisted-SQLite view; got:\n{show}",
    );
    assert!(
        show.contains("TOML") && show.contains("shadows"),
        "config show help must warn that TOML shadows SQLite; got:\n{show}",
    );
    let set = stdout_text(&run_help(&["--help", "config", "set"]));
    assert!(
        set.contains("SQLite only") && set.contains("TOML") && set.contains("shadows"),
        "config set help must state SQLite-only writes with TOML shadow; got:\n{set}",
    );
}

/// Workflow bootstrap help must state the admin-only flow, local SQLite
/// credential storage, and the shared precedence contract.
#[test]
fn workflow_bootstrap_help_documents_admin_only_and_toml() {
    let output = run_help(&["--help", "workflow", "bootstrap"]);
    assert!(
        output.status.success(),
        "--help workflow bootstrap exited non-zero"
    );
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("Only the admin")
            && stdout.contains("SQLite")
            && stdout.contains("never TOML"),
        "workflow bootstrap help must state admin-only provisioning into SQLite; got:\n{stdout}",
    );
    assert!(
        stdout.contains("CLI flags") && stdout.contains("TOML") && stdout.contains("SQLite"),
        "workflow bootstrap help must state CLI > env > TOML > SQLite precedence; got:\n{stdout}",
    );
}

/// Auth help must carry the shared precedence contract and the
/// admin-only bootstrap note without duplicating the resolver chain.
#[test]
fn auth_help_documents_toml_and_admin_bootstrap() {
    let output = run_help(&["--help", "auth"]);
    assert!(output.status.success(), "--help auth exited non-zero");
    let stdout = stdout_text(&output);
    assert!(
        stdout.contains("TOML")
            && stdout.contains("PHASEGENT_CONFIG_PATH")
            && stdout.contains("never stored in TOML"),
        "auth help must document the TOML overlay and credential boundary; got:\n{stdout}",
    );
    assert!(
        stdout.contains("needs only the admin"),
        "auth help must note bootstrap needs only the admin key; got:\n{stdout}",
    );
}

/// Command routing must remain intact: every documented next-help
/// pointer resolves to a non-error page. This protects against
/// accidental renames in `command::HelpTopic` or `print_help`.
#[test]
fn documented_next_help_pointers_all_resolve() {
    let pointers: &[&[&str]] = &[
        &["--help", "issue"],
        &["--help", "issue", "upload-attachment"],
        &["--help", "comment"],
        &["--help", "auth"],
        &["--help", "config"],
        &["--help", "config", "provider"],
        &["--help", "config", "provider", "set"],
        &["--help", "hooks"],
        &["--help", "timer"],
        &["--help", "timer", "start"],
        &["--help", "workflow", "bootstrap"],
    ];
    for pointer in pointers {
        let output = run_help(pointer);
        assert!(
            output.status.success(),
            "pointer {:?} exited non-zero: stderr={}",
            pointer,
            String::from_utf8_lossy(&output.stderr),
        );
        let stdout = stdout_text(&output);
        assert!(
            !stdout.trim().is_empty(),
            "pointer {:?} produced empty help",
            pointer,
        );
    }
}

/// Whether the root overview carries a command row for `name`. The page footer
/// mentions `--help admin`, so a bare substring check would false-positive.
fn has_root_row(stdout: &str, name: &str) -> bool {
    let prefix = format!("{name} ");
    stdout
        .lines()
        .any(|line| line.trim_start().starts_with(&prefix))
}

/// Phase 2: a role's root help lists only commands that role can run, and a
/// group whose subcommands are all denied is dropped from the overview.
#[test]
fn role_specific_root_help_only_lists_available_commands() {
    let output = run_help_with_role(&["--provider", "redmine", "--help"], Some("executor"));
    assert!(output.status.success(), "--help exited non-zero");
    let stdout = stdout_text(&output);
    for command in [
        "issue", "comment", "config", "doctor", "hooks", "notify", "mcp", "plugin", "project",
        "status", "version", "relation", "worktree",
    ] {
        assert!(
            has_root_row(&stdout, command),
            "executor root help must list {command:?};\n{stdout}",
        );
    }
    for command in ["admin", "timer", "repo"] {
        assert!(
            !has_root_row(&stdout, command),
            "executor root help must hide {command:?};\n{stdout}",
        );
    }
    // The no-role superset keeps the human-only and Redmine-only rows.
    let superset = stdout_text(&run_help(&["--provider", "redmine", "--help"]));
    for command in ["admin", "timer", "project", "status", "version", "relation"] {
        assert!(
            has_root_row(&superset, command),
            "roleless root help must keep {command:?};\n{superset}",
        );
    }
}

/// Phase 2: requesting a role-denied detail or group page never leaks its
/// parameters; it prints the existing stable denial line instead.
#[test]
fn role_denied_help_pages_print_the_stable_denial() {
    for (role, args) in [
        ("executor", &["--help", "issue", "sync"][..]),
        ("executor", &["--help", "issue", "bind"][..]),
        ("executor", &["--help", "admin"][..]),
        ("orchestrator", &["--help", "admin"][..]),
        ("executor", &["--help", "timer", "start"][..]),
        ("tester", &["--help", "worktree"][..]),
    ] {
        let output = run_help_with_role(args, Some(role));
        assert!(output.status.success(), "{args:?} exited non-zero");
        assert_eq!(
            stdout_text(&output).trim_end(),
            format!("No command available for {role}."),
            "denied page {args:?} must not leak parameters",
        );
    }
    // Allowed pages still render, including the role-open local branch status.
    let get = stdout_text(&run_help_with_role(
        &["--help", "issue", "get"],
        Some("executor"),
    ));
    assert!(get.contains("Usage: issue get"), "got: {get}");
    let status = stdout_text(&run_help_with_role(
        &["--help", "issue", "status"],
        Some("executor"),
    ));
    assert!(status.contains("Usage: issue status"), "got: {status}");
    let admin = stdout_text(&run_help_with_role(&["--help", "admin"], Some("admin")));
    assert!(
        admin.contains("Human-operator provisioning"),
        "the admin human role keeps the page; got: {admin}",
    );
}

/// Phase 2: a role-denied command is rejected at parse time with the stable
/// structured permission envelope, before any provider or credential lookup
/// (the scratch environment has no credentials configured).
#[test]
fn role_denied_command_fails_before_any_provider_access() {
    let output = run_help_with_role(
        &[
            "--provider",
            "redmine",
            "issue",
            "create",
            "--title",
            "T",
            "--body",
            "B",
        ],
        Some("executor"),
    );
    assert_eq!(output.status.code(), Some(3), "denied create must exit 3");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let envelope: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("structured error envelope on stderr");
    assert_eq!(envelope["error"]["kind"], "permission", "stderr: {stderr}");
    assert_eq!(envelope["error"]["role"], "executor");
    assert_eq!(envelope["error"]["operation"], "issue create");

    // The same gate covers the human-only admin surface for an AI role.
    let admin = run_help_with_role(
        &["admin", "config", "set", "notify-enabled", "true"],
        Some("orchestrator"),
    );
    assert_eq!(admin.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&admin.stderr);
    let envelope: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("structured error envelope on stderr");
    assert_eq!(envelope["error"]["kind"], "permission");
    assert_eq!(envelope["error"]["operation"], "admin config set");

    // An unknown command stays a distinct argument error.
    let unknown = run_help_with_role(&["frobnicate"], Some("orchestrator"));
    assert_eq!(unknown.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&unknown.stderr);
    let envelope: serde_json::Value =
        serde_json::from_str(stderr.trim()).expect("structured error envelope on stderr");
    assert_eq!(envelope["error"]["kind"], "argument");
    assert_eq!(envelope["error"]["message"], "unknown command 'frobnicate'");
}

/// Phase 2 round 2: the human-operator `admin config` write surface is a
/// separate help topic, so an AI role is denied it instead of falling back to
/// the role-open top-level `config` page. The read-only `config` and
/// `config provider` pages stay reachable but never expose admin write flags,
/// secret-setting usage, or the provider-write page.
#[test]
fn admin_config_write_help_is_denied_for_ai_roles_and_absent_from_read_only_pages() {
    // Both the global `--help admin config[...]` form and the equivalent
    // command-form `admin config ... --help` / detail forms land on the
    // dedicated admin topics and are denied with the stable line.
    let denied_forms: &[&[&str]] = &[
        &["--help", "admin", "config"],
        &["--help", "admin", "config", "set"],
        &["--help", "admin", "config", "provider"],
        &["admin", "config", "--help"],
        &["admin", "config", "provider", "--help"],
        &["admin", "config", "set", "--help"],
        &["admin", "config", "clear", "--help"],
        &["admin", "config", "provider", "set", "--help"],
        &["admin", "config", "provider", "clear", "--help"],
    ];
    for role in ["executor", "orchestrator"] {
        for args in denied_forms {
            let output = run_help_with_role(args, Some(role));
            assert!(output.status.success(), "{args:?} exited non-zero");
            assert_eq!(
                stdout_text(&output).trim_end(),
                format!("No command available for {role}."),
                "{args:?} must be denied for {role}",
            );
        }
    }

    // The read-only pages an AI role can still reach never mention the admin
    // config write surface at all. The top-level write details are denied too.
    for role in ["executor", "orchestrator"] {
        for args in [
            &["--help", "config"][..],
            &["--help", "config", "show"][..],
            &["--help", "config", "provider"][..],
            &["--help", "config", "provider", "get"][..],
        ] {
            let output = run_help_with_role(args, Some(role));
            assert!(output.status.success(), "{args:?} exited non-zero");
            let stdout = stdout_text(&output);
            for leak in ["admin config", "--stdin", "Secret settings"] {
                assert!(
                    !stdout.contains(leak),
                    "{args:?} for {role} leaked {leak:?}:\n{stdout}",
                );
            }
            assert!(
                stdout.contains("show") || stdout.contains(" get"),
                "{args:?} for {role} must still document the read-only surface:\n{stdout}",
            );
        }
        for args in [
            &["--help", "config", "set"][..],
            &["--help", "config", "clear"][..],
            &["--help", "config", "provider", "set"][..],
            &["--help", "config", "provider", "clear"][..],
        ] {
            let output = run_help_with_role(args, Some(role));
            assert!(output.status.success(), "{args:?} exited non-zero");
            assert_eq!(
                stdout_text(&output).trim_end(),
                format!("No command available for {role}."),
                "top-level write detail {args:?} must be admin-scoped for {role}",
            );
        }
    }

    // The resolver chain is not admin-sensitive and stays documented for both
    // the role-less and role-resolved provider page.
    for role in [None, Some("executor")] {
        let provider = stdout_text(&run_help_with_role(&["--help", "config", "provider"], role));
        for needle in [
            "PHASEGENT_PROVIDER",
            "PHASEGENT_DEFAULT_PROVIDER",
            "role_config.provider",
            "forgejo fallback",
        ] {
            assert!(
                provider.contains(needle),
                "provider help for {role:?} missing {needle:?}:\n{provider}",
            );
        }
    }

    // The human role and the role-less superset keep the full write page for
    // the global and command-form group/detail routes.
    for args in [
        &["--help", "admin", "config"][..],
        &["admin", "config", "--help"][..],
    ] {
        let admin = stdout_text(&run_help_with_role(args, Some("admin")));
        assert!(
            admin.contains("admin config set") && admin.contains("--stdin"),
            "admin role must keep the write page for {args:?}:\n{admin}",
        );
    }
    for args in [
        &["admin", "config", "provider", "--help"][..],
        &["--help", "admin", "config", "provider"][..],
    ] {
        let admin = stdout_text(&run_help_with_role(args, Some("admin")));
        assert!(
            admin.contains("admin config provider set") && admin.contains("clear"),
            "admin role must keep the provider group page for {args:?}:\n{admin}",
        );
    }
    let provider_set_detail = stdout_text(&run_help_with_role(
        &["admin", "config", "provider", "set", "--help"],
        Some("admin"),
    ));
    assert!(
        provider_set_detail.contains("admin config provider set"),
        "admin role must keep the provider set detail page:\n{provider_set_detail}",
    );
    let set_detail = stdout_text(&run_help_with_role(
        &["admin", "config", "set", "--help"],
        Some("admin"),
    ));
    assert!(
        set_detail.contains("Secret settings") && set_detail.contains("admin config set"),
        "admin role must keep the set detail page:\n{set_detail}",
    );
    let superset = stdout_text(&run_help(&["--help", "admin", "config"]));
    assert!(
        superset.contains("admin config set"),
        "roleless superset must keep the admin write page:\n{superset}",
    );
}

/// Phase 3: the registry's compile-time feature boundary drives the role-less
/// superset help too. `gui` appears in the overview and the usage block only
/// when the desktop shell was compiled into the binary under test.
#[test]
fn root_help_follows_the_gui_feature_boundary() {
    let output = run_help(&["--help"]);
    assert!(output.status.success(), "--help exited non-zero");
    let stdout = stdout_text(&output);
    let compiled = cfg!(feature = "gui");
    assert_eq!(
        has_root_row(&stdout, "gui"),
        compiled,
        "root overview gui row must follow the feature boundary;\n{stdout}",
    );
    assert_eq!(
        stdout.contains("phasegent gui"),
        compiled,
        "root usage must advertise the desktop entry only when compiled;\n{stdout}",
    );
    if compiled {
        assert!(
            stdout.contains("Open the desktop GUI (single-binary shell)"),
            "a compiled gui row must keep its summary;\n{stdout}",
        );
    } else {
        assert!(
            !stdout.contains("Open the desktop GUI"),
            "an uncompiled gui must not render its ordinary row;\n{stdout}",
        );
    }
}

/// Phase 3: a direct detail request for an uncompiled command never renders
/// the ordinary page; it prints the same stable not-compiled message the
/// execution layer returns. A compiled build keeps the ordinary page for every
/// role context.
#[test]
fn gui_detail_help_reports_the_feature_boundary() {
    for role in [None, Some("executor")] {
        let output = run_help_with_role(&["--help", "gui"], role);
        assert!(output.status.success(), "--help gui exited non-zero");
        let stdout = stdout_text(&output);
        if cfg!(feature = "gui") {
            assert!(
                stdout.contains("Usage: phasegent gui") && stdout.contains("Tauri shell"),
                "compiled gui help must render the ordinary page for {role:?};\n{stdout}",
            );
        } else {
            assert_eq!(
                stdout.trim_end(),
                "GUI support was not compiled into this binary; rebuild with --features gui to enable the desktop shell",
                "uncompiled gui help must render the stable not-compiled message for {role:?}",
            );
            assert!(
                !stdout.contains("Tauri shell"),
                "uncompiled gui help must not render the ordinary page for {role:?};\n{stdout}",
            );
        }
    }
}

/// Phase 3: the no-GUI runtime path stays reachable and explicit. `gui` parses
/// without a role and the execution layer returns the structured not-compiled
/// error, byte-identical to the help message and distinct from the
/// unknown-command argument error.
#[cfg(not(feature = "gui"))]
#[test]
fn uncompiled_gui_runtime_reports_the_structured_not_compiled_error() {
    const MESSAGE: &str = "GUI support was not compiled into this binary; rebuild with --features gui to enable the desktop shell";
    for role in [None, Some("executor")] {
        let output = run_help_with_role(&["gui"], role);
        assert_eq!(
            output.status.code(),
            Some(1),
            "uncompiled gui must exit 1 for {role:?}; stderr={}",
            String::from_utf8_lossy(&output.stderr),
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        let envelope: serde_json::Value =
            serde_json::from_str(stderr.trim()).expect("structured error envelope on stderr");
        assert_eq!(envelope["error"]["kind"], "gui", "stderr: {stderr}");
        assert_eq!(envelope["error"]["message"], MESSAGE);
    }

    let help = stdout_text(&run_help(&["--help", "gui"]));
    assert_eq!(
        help.trim_end(),
        MESSAGE,
        "help and runtime must share one not-compiled contract",
    );

    let unknown = run_help_with_role(&["gui-unknown"], None);
    assert_eq!(unknown.status.code(), Some(2));
    let unknown_stderr = String::from_utf8_lossy(&unknown.stderr);
    let unknown_envelope: serde_json::Value =
        serde_json::from_str(unknown_stderr.trim()).expect("structured error envelope on stderr");
    assert_eq!(unknown_envelope["error"]["kind"], "argument");
    assert_eq!(
        unknown_envelope["error"]["message"],
        "unknown command 'gui-unknown'"
    );
}
