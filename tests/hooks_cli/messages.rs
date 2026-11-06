use super::fixtures::Repo;

#[test]
fn configured_markers_regions_aliases_and_overrides() {
    let r = Repo::new();
    for marker in ["#", ";", "※", ";; "] {
        r.git(&["config", "core.commentString", marker]);
        let bytes = format!(
            "subject\n{marker} refs #88\n{marker} ------------------------ >8 ------------------------\nrefs #77\n"
        );
        r.write("message", bytes.as_bytes());
        assert!(r.hook("prepare-commit-msg", None).status.success());
        let updated = r.read("message");
        assert!(updated.windows(7).any(|w| w == b"Refs #9"));
        assert!(r.hook("commit-msg", None).status.success());
        assert!(r.hook("prepare-commit-msg", None).status.success());
        assert_eq!(r.read("message"), updated);
    }
    r.git(&["config", "core.commentChar", ";"]);
    r.write("message", b"subject\n; refs #88\n");
    assert!(r.hook("prepare-commit-msg", None).status.success());
    r.git(&["config", "--unset", "core.commentChar"]);
    r.git(&["config", "core.commentString", "auto"]);
    r.write("message", b"subject\n");
    assert!(!r.hook("prepare-commit-msg", None).status.success());
    assert_eq!(r.read("message"), b"subject\n");
    let out = r
        .command(env!("CARGO_BIN_EXE_phasegent"))
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.commentChar")
        .env("GIT_CONFIG_VALUE_0", "!")
        .args(["hooks", "run", "prepare-commit-msg", "message"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    r.install();
    r.git(&[
        "-c",
        "core.commentChar=!",
        "commit",
        "--allow-empty",
        "-m",
        "subject\n! refs #88",
    ]);
    assert!(
        String::from_utf8_lossy(&r.git(&["log", "-1", "--format=%B"]).stdout).contains("Refs #9")
    );
}

fn seed_redmine_endpoint(r: &Repo, base: &str) {
    let connection =
        rusqlite::Connection::open(r.root.join("runtime.sqlite3")).expect("fixture config store");
    connection
        .execute(
            "INSERT OR REPLACE INTO global_setting (name, value) VALUES ('PHASEGENT_REDMINE_API_BASE', ?1)",
            [base],
        )
        .expect("fixture Redmine endpoint");
    connection
        .execute(
            "INSERT OR REPLACE INTO role_credential (role, provider, credential, fingerprint, credential_updated_at)              VALUES ('orchestrator', 'redmine', 'fixture-key', '-key', 1)",
            [],
        )
        .expect("fixture Redmine credential");
}

fn spawn_redmine_issue_server(expected_key: &str) -> (String, std::thread::JoinHandle<()>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("loopback listener");
    let base = format!("http://{}", listener.local_addr().unwrap());
    let expected_key = expected_key.to_owned();
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("one fixture request");
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(10)))
            .unwrap();
        let mut data = Vec::new();
        let mut buffer = [0u8; 4096];
        while !data.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).expect("fixture request read");
            if read == 0 {
                break;
            }
            data.extend_from_slice(&buffer[..read]);
        }
        let request = String::from_utf8_lossy(&data);
        let first = request.lines().next().unwrap_or_default();
        assert!(
            first.contains("issues/5.json"),
            "fixture responder saw an unexpected request line: {first}"
        );
        let auth = request
            .lines()
            .find(|line| line.to_ascii_lowercase().starts_with("x-redmine-api-key:"))
            .expect("fixture auth header");
        assert!(
            auth.contains(&expected_key),
            "fixture auth header did not carry the role credential: {auth}"
        );
        let body = r#"{"issue":{"id":5,"subject":"fixture","description":"","project":{"id":1,"name":"p1","identifier":"p1"},"status":{"name":"New","is_closed":false},"journals":[]}}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("fixture response");
    });
    (base, handle)
}

fn seed_local_issues(r: &Repo, count: u64) {
    for _ in 0..count {
        let out = r.cli(&[
            "--provider",
            "local",
            "issue",
            "create",
            "--title",
            "fixture",
            "--body",
            "fixture",
            "--branch",
            "seed-ignore",
        ]);
        assert!(
            out.status.success(),
            "seeding a Local issue failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

fn bind_issues(r: &Repo, ids: &[&str]) {
    let highest = ids
        .iter()
        .filter_map(|id| id.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    seed_local_issues(r, highest);
    let mut args = vec!["--provider", "local", "issue", "bind"];
    args.extend_from_slice(ids);
    let out = r.cli(&args);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn multi_bound_branch_accepts_the_whole_set_and_generates_every_ref() {
    let r = Repo::new();
    bind_issues(&r, &["9", "10"]);
    r.write("message", b"subject\n");
    assert!(r.hook("prepare-commit-msg", None).status.success());
    assert_eq!(r.read("message"), b"subject\n\nRefs #9\nRefs #10\n");
    assert!(r.hook("commit-msg", None).status.success());

    // A free-form reference to another member of the set is allowed.
    r.write("message", b"subject\n\nrefs #10\n");
    assert!(r.hook("commit-msg", None).status.success());

    // A reference outside the set and a duplicated generated trailer for a
    // single member both fail closed.
    r.write("message", b"subject\n\nrefs #11\n");
    assert!(!r.hook("commit-msg", None).status.success());
    r.write("message", b"subject\n\nRefs #9\nRefs #10\nRefs #10\n");
    assert!(!r.hook("commit-msg", None).status.success());
}

#[test]
fn unbind_named_issue_narrows_the_hook_set() {
    let r = Repo::new();
    bind_issues(&r, &["9", "10"]);
    assert!(
        r.cli(&["--provider", "local", "issue", "unbind", "10"])
            .status
            .success()
    );
    r.write("message", b"subject\n");
    assert!(r.hook("prepare-commit-msg", None).status.success());
    assert_eq!(r.read("message"), b"subject\n\nRefs #9\n");
    r.write("message", b"subject\n\nrefs #9\n");
    assert!(r.hook("commit-msg", None).status.success());
    r.write("message", b"subject\n\nrefs #10\n");
    assert!(!r.hook("commit-msg", None).status.success());
}

#[test]
fn unbind_preserves_the_durable_task_reference_and_narrows_the_hook_set() {
    let r = Repo::new();
    bind_issues(&r, &["9", "10"]);
    // The explicit Local bind adopts a durable task and attaches its
    // reference to each link; replace issue 10's reference so the test can
    // tell a preserved value from an incidental one.
    let connection =
        rusqlite::Connection::open(r.root.join("runtime.sqlite3")).expect("fixture store");
    connection
        .execute(
            "UPDATE branch_issue_links SET task_id = 42 WHERE provider = 'local' AND external_id = '10'",
            [],
        )
        .expect("attach task reference");

    assert!(
        r.cli(&["--provider", "local", "issue", "unbind", "10"])
            .status
            .success()
    );

    // Status and hooks expose the same compatible active-link set: only the
    // still-linked issue resolves.
    r.write("message", b"subject\n");
    assert!(r.hook("prepare-commit-msg", None).status.success());
    assert_eq!(r.read("message"), b"subject\n\nRefs #9\n");
    r.write("message", b"subject\n\nrefs #10\n");
    assert!(!r.hook("commit-msg", None).status.success());

    // The detached row keeps its history and its durable task reference.
    let row: (String, Option<i64>) = connection
        .query_row(
            "SELECT status, task_id FROM branch_issue_links \
             WHERE provider = 'local' AND external_id = '10'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("detached row is retained");
    assert_eq!(row, ("detached".to_owned(), Some(42)));
}

#[test]
fn same_number_across_scopes_fails_closed_in_hooks() {
    let r = Repo::new();
    seed_local_issues(&r, 5);
    assert!(
        r.cli(&["--provider", "local", "issue", "bind", "5"])
            .status
            .success()
    );
    let (base, responder) = spawn_redmine_issue_server("fixture-key");
    seed_redmine_endpoint(&r, &base);
    assert!(
        r.cli(&[
            "--provider",
            "redmine",
            "--project-id",
            "p1",
            "issue",
            "bind",
            "5"
        ])
        .status
        .success()
    );
    responder.join().expect("fixture responder joins");
    r.write("message", b"subject\n");
    assert!(
        !r.hook("prepare-commit-msg", None).status.success(),
        "an ambiguous cross-scope binding must not silently append a ref"
    );
    assert_eq!(r.read("message"), b"subject\n");
    assert!(!r.hook("commit-msg", None).status.success());
}

#[test]
fn references_duplicates_conflicts_and_raw_crlf() {
    let r = Repo::new();
    for text in [b"fixes:#9\n".as_slice(), b"refs #9\n"] {
        r.write("message", text);
        assert!(r.hook("prepare-commit-msg", None).status.success());
        assert_eq!(r.read("message"), text);
        assert!(r.hook("commit-msg", None).status.success());
    }
    for text in [
        b"fixes:#8\n".as_slice(),
        b"Refs #9\nRefs #9\n",
        b" refs #8\n",
    ] {
        r.write("message", text);
        assert!(!r.hook("commit-msg", None).status.success());
        assert_eq!(r.read("message"), text);
    }
    let bytes = b"subject\xff\r\n\r\nSigned-off-by: Test <a@b>\r\n\r\n# ------------------------ >8 ------------------------\r\nrefs #88\r\n";
    r.write("message", bytes);
    assert!(r.hook("prepare-commit-msg", None).status.success());
    assert_eq!(r.read("message"), b"subject\xff\r\n\r\nSigned-off-by: Test <a@b>\r\n\r\nRefs #9\r\n# ------------------------ >8 ------------------------\r\nrefs #88\r\n");
}

#[test]
fn scissors_survives_final_git_cleanup_and_noop_branches_ignore_auto() {
    let r = Repo::new();
    r.install();
    r.write(
        "input",
        b"subject\n# ------------------------ >8 ------------------------\nignored\n",
    );
    let out = r
        .command("git")
        .args([
            "commit",
            "--allow-empty",
            "-e",
            "-F",
            "input",
            "--cleanup=scissors",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body = r.git(&["log", "-1", "--format=%B"]).stdout;
    assert!(String::from_utf8_lossy(&body).contains("Refs #9"));
    assert!(!String::from_utf8_lossy(&body).contains("ignored"));
    r.git(&["config", "core.commentChar", "auto"]);
    for branch in ["main", "unbound"] {
        if branch == "unbound" {
            r.git(&["checkout", "-b", branch]);
        } else {
            r.git(&["checkout", branch]);
        }
        r.write("message", b"unchanged");
        assert!(r.hook("prepare-commit-msg", None).status.success());
        assert_eq!(r.read("message"), b"unchanged");
    }
    r.git(&["checkout", "--detach"]);
    assert!(r.hook("commit-msg", None).status.success());
}
