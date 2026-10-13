use super::fixtures::Repo;

#[test]
fn native_commit_modes_and_generated_sources() {
    let r = Repo::new();
    r.install();
    r.git(&["commit", "--allow-empty", "-m", "plain"]);
    assert!(
        String::from_utf8_lossy(&r.git(&["log", "-1", "--format=%B"]).stdout).contains("Refs #9")
    );
    r.git(&["commit", "--allow-empty", "--amend", "-m", "amended"]);
    r.git(&["commit", "--allow-empty", "--amend", "--no-edit"]);
    for mode in ["-c", "-C"] {
        r.git(&["commit", "--allow-empty", mode, "HEAD"]);
    }
    r.write("template", b"template subject\n\n# comment\n");
    r.git(&[
        "commit",
        "--allow-empty",
        "--template",
        "template",
        "--cleanup=strip",
    ]);
    for source in ["commit", "merge", "squash"] {
        r.write("message", b"generated\n");
        assert!(r.hook("prepare-commit-msg", Some(source)).status.success());
        assert_eq!(r.read("message"), b"generated\n");
    }
}

#[test]
fn parser_rejects_malformed_arity_and_accepts_opaque_commit_object() {
    let r = Repo::new();
    r.write("message", b"generated\n");
    for args in [
        vec!["hooks", "run", "prepare-commit-msg"],
        vec![
            "hooks",
            "run",
            "prepare-commit-msg",
            "message",
            "message",
            "HEAD",
        ],
        vec![
            "hooks",
            "run",
            "prepare-commit-msg",
            "message",
            "commit",
            "HEAD",
            "extra",
        ],
        vec!["hooks", "run", "commit-msg", "message", "extra"],
        vec!["hooks", "run", "prepare-commit-msg", "-file"],
    ] {
        assert!(!r.cli(&args).status.success(), "{args:?}");
    }
    assert!(
        r.cli(&[
            "hooks",
            "run",
            "prepare-commit-msg",
            "message",
            "commit",
            "not-a-revision"
        ])
        .status
        .success()
    );
}

#[test]
fn merge_and_squash_native_calls() {
    let r = Repo::new();
    r.git(&["checkout", "-b", "topic"]);
    r.write("topic-file", b"topic");
    r.git(&["add", "topic-file"]);
    r.git(&["commit", "-m", "topic"]);
    r.git(&["checkout", "feat/9"]);
    r.install();
    r.git(&["merge", "--no-ff", "topic", "-m", "merge"]);
    r.git(&["checkout", "-b", "second"]);
    r.write("second-file", b"second");
    r.git(&["add", "second-file"]);
    r.git(&["commit", "-m", "second"]);
    r.git(&["checkout", "feat/9"]);
    r.git(&["merge", "--squash", "second"]);
    r.git(&["commit", "--no-edit"]);
}
