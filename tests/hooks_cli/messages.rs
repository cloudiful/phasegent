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

fn bind_issues(r: &Repo, ids: &[&str]) {
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
fn same_number_across_scopes_fails_closed_in_hooks() {
    let r = Repo::new();
    assert!(
        r.cli(&["--provider", "local", "issue", "bind", "5"])
            .status
            .success()
    );
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
