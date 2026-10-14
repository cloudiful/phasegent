// Bridge-to-CLI subprocess scenarios (issue #859 P1; heredoc-opaque P2).
//
// Each test drives the generated plugin through Bun (rewrite + nonce header
// + exact-match consume on a fresh env), then executes the resulting env
// against an isolated local-provider scratch store. Known explore record
// operations stay authorized with the explore author; a no-role managed
// marker fails closed; an ordinary Operator without either marker stays
// functional. The P2 heredoc test executes a quoted heredoc through POSIX
// `sh` and proves the body stays byte-exact while the trailing real call
// authorizes as explore. No test touches a production provider or API.

use super::fixtures::{
    Scratch, drive_bridge_via_bun, error_envelope, init_local, json_stdout, run_local_with_markers,
    seed_issue,
};
use super::support::{phasegent_bin, stderr_text, stdout_text};

/// Known explore bridge env authorizes `record create`/`list` with
/// `--authorized`; the stored record carries the explore author.
#[test]
fn bridge_explore_env_authorizes_record_create_and_list() {
    let scratch = Scratch::new("explore-authorized");
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "managed shell explore");
    let id = issue.to_string();

    // Drive the bridge for a canonical explore shell with a rewriteable
    // command. The driver restores the exact rewritten bytes and adopts
    // explore + the real session on the fresh env only.
    let list_command = format!("phasegent --provider local record list {id}");
    let bridged = drive_bridge_via_bun(
        "explore",
        "ses-explore-1",
        &list_command,
        None,
        "ses-host-1",
    );
    assert!(bridged.decorated_had_header);
    assert_eq!(bridged.env_role.as_deref(), Some("explore"));
    assert_eq!(bridged.env_session.as_deref(), Some("ses-explore-1"));
    assert_eq!(bridged.host_session.as_deref(), Some("ses-host-1"));
    // The restored command keeps the rewrite (role prefix may be present);
    // the CLI authorizes via the bridge env, not via header residue.
    assert!(!bridged.command.starts_with("# phasegent-context:"));

    // `record list` with the bridged env succeeds.
    let out = run_local_with_markers(
        &db,
        bridged.env_role.as_deref(),
        bridged.env_session.as_deref(),
        bridged.host_session.as_deref(),
        &["--provider", "local", "record", "list", &id],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));

    // `record create` with the bridged explore env and `--authorized`
    // succeeds and is authored by explore.
    let create_command = format!(
        "phasegent --provider local record create {id} --kind recon --key k-explore-1 --recon scan --body hello --authorized"
    );
    let bridged_create = drive_bridge_via_bun(
        "explore",
        "ses-explore-1",
        &create_command,
        None,
        "ses-host-1",
    );
    assert_eq!(bridged_create.env_role.as_deref(), Some("explore"));
    let out = run_local_with_markers(
        &db,
        bridged_create.env_role.as_deref(),
        bridged_create.env_session.as_deref(),
        bridged_create.host_session.as_deref(),
        &[
            "--provider",
            "local",
            "record",
            "create",
            &id,
            "--kind",
            "recon",
            "--key",
            "k-explore-1",
            "--recon",
            "scan",
            "--body",
            "hello",
            "--authorized",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    let created = json_stdout(&out);
    assert_eq!(created["actor"], "explore");
}

/// Unknown agent bridge env clears the inherited role and still marks the
/// shell managed, so business calls fail closed instead of becoming
/// Operator.
#[test]
fn bridge_unknown_role_clears_inherited_role_and_fails_closed() {
    let scratch = Scratch::new("unknown-fails-closed");
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "managed shell unknown");
    let id = issue.to_string();

    let list_command = format!("phasegent --provider local record list {id}");
    // Unknown agent with an inherited privileged role in the fresh env: the
    // shell hook must delete it and keep only the session marker.
    let bridged = drive_bridge_via_bun(
        "general",
        "ses-unknown-1",
        &list_command,
        Some("executor"),
        "ses-host-9",
    );
    assert!(bridged.decorated_had_header);
    assert_eq!(bridged.env_role, None);
    assert_eq!(bridged.env_session.as_deref(), Some("ses-unknown-1"));

    let out = run_local_with_markers(
        &db,
        bridged.env_role.as_deref(),
        bridged.env_session.as_deref(),
        bridged.host_session.as_deref(),
        &["--provider", "local", "record", "list", &id],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr_text(&out));
    assert_eq!(stdout_text(&out), "");
    assert!(
        error_envelope(&out)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("managed session requires a role"),
        "{}",
        stderr_text(&out)
    );
    assert!(
        !stderr_text(&out).contains("hello-secret"),
        "denial must not echo body secrets"
    );
}

/// Host-only marker without a role fails closed for business calls, while an
/// ordinary Operator without either marker still runs.
#[test]
fn host_only_marker_rejected_and_operator_still_allowed() {
    let scratch = Scratch::new("host-vs-operator");
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "host vs operator");
    let id = issue.to_string();

    // Host-only marker (no PHASEGENT_ROLE, no PHASEGENT_SESSION_ID) fails
    // closed before any provider access.
    let out = run_local_with_markers(
        &db,
        None,
        None,
        Some("ses-host-only"),
        &["--provider", "local", "record", "list", &id],
    );
    assert_eq!(out.status.code(), Some(2), "{}", stderr_text(&out));
    assert!(
        error_envelope(&out)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("managed session requires a role")
    );

    // Ordinary Operator without either marker still lists.
    let out = run_local_with_markers(
        &db,
        None,
        None,
        None,
        &["--provider", "local", "record", "list", &id],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
}

/// Heredoc-missed rewrite still carries identity through the bridge env:
/// the tool command contains a quoted heredoc with an apostrophe plus a
/// trailing real invocation, the rewrite leaves the body opaque (P2 owns the
/// scanner fix), but the bridge header still provides the explore env so the
/// trailing call authorizes instead of falling back to Operator.
#[test]
fn bridge_carries_identity_when_rewrite_misses_heredoc_shape() {
    let scratch = Scratch::new("heredoc-bridge");
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "heredoc bridge");
    let id = issue.to_string();

    // Incident-shaped command: heredoc body with an apostrophe swallows the
    // scanner today, so the rewrite returns the command unchanged. The bridge
    // still decorates the whole shell string and the shell hook still adopts
    // the explore env.
    let heredoc_command =
        format!("cat <<'EOF'\nit's a body\nEOF\nphasegent --provider local record list {id}");
    let bridged = drive_bridge_via_bun(
        "explore",
        "ses-heredoc-1",
        &heredoc_command,
        None,
        "ses-host-h",
    );
    assert!(bridged.decorated_had_header);
    assert_eq!(bridged.env_role.as_deref(), Some("explore"));
    assert_eq!(bridged.env_session.as_deref(), Some("ses-heredoc-1"));
    // The restored command is byte-exact (header stripped only).
    assert!(!bridged.command.starts_with("# phasegent-context:"));
    assert!(bridged.command.contains("it's a body"));

    // Executing the trailing real call with the bridged env authorizes.
    let out = run_local_with_markers(
        &db,
        bridged.env_role.as_deref(),
        bridged.env_session.as_deref(),
        bridged.host_session.as_deref(),
        &["--provider", "local", "record", "list", &id],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
}

/// Heredoc-opaque end-to-end (issue #859 P2): a quoted heredoc containing
/// the incident apostrophe plus fake elevated `phasegent` lines stays data
/// through POSIX `sh`, while the trailing real recon authorizes as explore.
///
/// The shell script writes the body via `cat` heredoc, then runs the
/// rewritten trailing `record create --body-file` with `--keep-body-file`.
/// Assertions prove the body file is byte-exact, the stored record is
/// authored by explore with the same body, only one record exists (no issue
/// created and no role switch on behalf of the fake lines), and the rewrite
/// never prefixed a role inside the body.
#[test]
fn heredoc_opaque_body_exact_and_trailing_recon_authored_explore() {
    use std::process::{Command, Stdio};

    let scratch = Scratch::new("heredoc-p2");
    let db = scratch.local_db();
    init_local(&db);
    let issue = seed_issue(&db, "heredoc opaque p2");
    let id = issue.to_string();
    let body_path = scratch.path().join("heredoc-body.txt");
    let body_display = body_path.to_string_lossy().into_owned();
    let bin = phasegent_bin().to_string();

    // Quoted heredoc with the incident apostrophe plus fake elevated lines.
    let tool_command = format!(
        "cat > {body_display} <<'BODY'\nit's the body with apostrophe\nphasegent issue create --title t --body b\nPHASEGENT_ROLE=orchestrator phasegent issue get 999\nBODY\n{bin} --provider local record create {id} --kind recon --key k-heredoc-p2 --recon scan --body-file {body_display} --keep-body-file --authorized"
    );
    let bridged = drive_bridge_via_bun(
        "explore",
        "ses-heredoc-p2",
        &tool_command,
        None,
        "ses-host-p2",
    );
    assert!(bridged.decorated_had_header);
    assert_eq!(bridged.env_role.as_deref(), Some("explore"));
    assert_eq!(bridged.env_session.as_deref(), Some("ses-heredoc-p2"));
    assert!(!bridged.command.starts_with("# phasegent-context:"));
    // Body bytes survive; no role prefix was injected inside the body.
    assert!(bridged.command.contains("it's the body with apostrophe"));
    assert!(
        bridged
            .command
            .contains("phasegent issue create --title t --body b"),
        "fake body line must survive as data: {}",
        bridged.command
    );
    assert!(
        bridged
            .command
            .contains("PHASEGENT_ROLE=orchestrator phasegent issue get 999"),
        "fake role claim must survive untouched: {}",
        bridged.command
    );
    assert!(
        !bridged
            .command
            .contains("PHASEGENT_ROLE=explore phasegent issue create"),
        "no spoofed body invocation may gain a role: {}",
        bridged.command
    );
    // The trailing real call is scoped (absolute-path invocation still scopes).
    assert!(
        bridged.command.contains("PHASEGENT_ROLE=explore"),
        "trailing recon must be scoped: {}",
        bridged.command
    );

    // Execute the restored shell script through POSIX `sh` with the bridged
    // env plus the isolated local store. Only `sh` execution is claimed;
    // other platforms rely on the same opaque ranges without an e2e run.
    let missing_toml = db
        .parent()
        .map(|dir| dir.join("phasegent-missing.toml"))
        .unwrap_or_else(|| std::env::temp_dir().join("phasegent-missing.toml"));
    let mut shell = Command::new("sh");
    shell
        .arg("-c")
        .arg(&bridged.command)
        .env("PHASEGENT_LOCAL_DB_PATH", &db)
        .env("PHASEGENT_CONFIG_PATH", &missing_toml)
        .env("RUST_BACKTRACE", "0")
        .env_remove("PHASEGENT_ROLE")
        .env_remove("PHASEGENT_SESSION_ID")
        .env_remove("OPENCODE_SESSION_ID")
        .env_remove("PHASEGENT_PROVIDER")
        .env_remove("PHASEGENT_DEFAULT_PROVIDER")
        .env_remove("PHASEGENT_DB_PATH")
        .env_remove("PHASEGENT_API_BASE")
        .env_remove("PHASEGENT_REDMINE_API_BASE")
        .env_remove("PHASEGENT_REPOSITORY")
        .env_remove("PHASEGENT_PROJECT_ID")
        .env_remove("PHASEGENT_REDMINE_PROJECT_ID")
        .env_remove("PHASEGENT_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_REDMINE_CLOSE_STATUS_ID")
        .env_remove("PHASEGENT_REDMINE_GIT_MIRROR_API_KEY")
        .env_remove("PHASEGENT_REDMINE_REPOSITORY_URL")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(role) = bridged.env_role.as_deref() {
        shell.env("PHASEGENT_ROLE", role);
    }
    if let Some(session) = bridged.env_session.as_deref() {
        shell.env("PHASEGENT_SESSION_ID", session);
    }
    if let Some(host) = bridged.host_session.as_deref() {
        shell.env("OPENCODE_SESSION_ID", host);
    }
    let shell_out = shell.output().expect("spawn POSIX sh");
    assert_eq!(
        shell_out.status.code(),
        Some(0),
        "sh heredoc script failed stdout={} stderr={}",
        String::from_utf8_lossy(&shell_out.stdout),
        String::from_utf8_lossy(&shell_out.stderr)
    );

    // Body file is byte-exact: apostrophe, fake commands, and role claim are
    // data, never executed or rewritten.
    let body = std::fs::read_to_string(&body_path).expect("read heredoc body");
    assert_eq!(
        body,
        "it's the body with apostrophe\nphasegent issue create --title t --body b\nPHASEGENT_ROLE=orchestrator phasegent issue get 999\n"
    );

    // Exactly one record exists, authored by explore, carrying the same body.
    let out = run_local_with_markers(
        &db,
        bridged.env_role.as_deref(),
        bridged.env_session.as_deref(),
        bridged.host_session.as_deref(),
        &["--provider", "local", "record", "list", &id],
    );
    assert_eq!(out.status.code(), Some(0), "{}", stderr_text(&out));
    let records: Vec<serde_json::Value> =
        serde_json::from_str(stdout_text(&out).trim()).expect("record list JSON");
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0]["actor"], "explore");
    assert_eq!(records[0]["kind"], "recon");
    assert_eq!(records[0]["key"], "k-heredoc-p2");
    assert!(
        records[0]["body"]
            .as_str()
            .unwrap()
            .contains("it's the body with apostrophe"),
        "{:?}",
        records[0]
    );
    assert!(
        records[0]["body"]
            .as_str()
            .unwrap()
            .contains("phasegent issue create"),
        "fake lines stay record body data, never a second issue: {:?}",
        records[0]
    );
}
