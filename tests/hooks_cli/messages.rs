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
