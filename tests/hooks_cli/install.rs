use super::fixtures::Repo;
use std::os::unix::fs::PermissionsExt;

#[test]
fn foreign_marker_argv_exit_and_fail_closed_reinstall() {
    let r = Repo::new();
    let foreign = b"#!/bin/sh\n# mention # phasegent:managed is not ownership\nprintf '%s\\n' \"$@\" > argv\nexit 23\n";
    r.executable(".git/hooks/prepare-commit-msg", foreign);
    r.install();
    assert_eq!(
        r.read(".git/hooks/phasegent-original/prepare-commit-msg"),
        foreign
    );
    assert_eq!(
        std::fs::metadata(
            r.root
                .join(".git/hooks/phasegent-original/prepare-commit-msg")
        )
        .unwrap()
        .permissions()
        .mode()
            & 0o777,
        0o755
    );
    let wrapper = r.read(".git/hooks/prepare-commit-msg");
    r.install();
    assert_eq!(r.read(".git/hooks/prepare-commit-msg"), wrapper);
    let out = r
        .command(r.root.join(".git/hooks/prepare-commit-msg"))
        .args(["a b", "commit", "opaque"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(23));
    assert_eq!(r.read("argv"), b"a b\ncommit\nopaque\n");
    let original = r
        .root
        .join(".git/hooks/phasegent-original/prepare-commit-msg");
    std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o644)).unwrap();
    for missing in [false, true] {
        if missing {
            std::fs::remove_file(&original).unwrap();
        }
        let out = r
            .command(r.root.join(".git/hooks/prepare-commit-msg"))
            .args(["message"])
            .output()
            .unwrap();
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("restore"));
        assert!(!r.cli(&["hooks", "install"]).status.success());
        assert_eq!(r.read(".git/hooks/prepare-commit-msg"), wrapper);
    }
}

#[test]
fn legacy_recognition_custom_path_and_collision() {
    let r = Repo::new();
    r.git(&["config", "core.hooksPath", "hooks space's"]);
    std::fs::create_dir_all(r.root.join("hooks space's")).unwrap();
    r.executable("hooks space's/prepare-commit-msg", b"#!/bin/sh\nexit 0\n");
    r.install();
    let wrapper = String::from_utf8(r.read("hooks space's/prepare-commit-msg")).unwrap();
    let start = wrapper.find("if [ ! -f").unwrap();
    let end = wrapper[start..].find("fi\n").unwrap() + start + 3;
    let legacy = format!("{}{}", &wrapper[..start], &wrapper[end..]);
    r.executable("hooks space's/prepare-commit-msg", legacy.as_bytes());
    r.install();
    assert_eq!(
        r.read("hooks space's/prepare-commit-msg"),
        wrapper.as_bytes()
    );
    r.executable(
        "hooks space's/prepare-commit-msg",
        b"#!/bin/sh\n# foreign replacement\nexit 0\n",
    );
    let before = r.read("hooks space's/prepare-commit-msg");
    assert!(!r.cli(&["hooks", "install"]).status.success());
    assert_eq!(r.read("hooks space's/prepare-commit-msg"), before);
}

#[test]
fn successful_original_runs_before_phasegent_and_unsafe_second_hook_blocks_all_updates() {
    let r = Repo::new();
    r.executable(
        ".git/hooks/prepare-commit-msg",
        b"#!/bin/sh\nprintf 'original subject\\n' > \"$1\"\n",
    );
    r.executable(".git/hooks/commit-msg", b"#!/bin/sh\nexit 0\n");
    r.install();
    r.write("message", b"input\n");
    let out = r
        .command(r.root.join(".git/hooks/prepare-commit-msg"))
        .args(["message", "message"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(r.read("message"), b"original subject\n\nRefs #9\n");
    let current = String::from_utf8(r.read(".git/hooks/prepare-commit-msg")).unwrap();
    let start = current.find("if [ ! -f").unwrap();
    let end = current[start..].find("fi\n").unwrap() + start + 3;
    let legacy = format!("{}{}", &current[..start], &current[end..]);
    r.executable(".git/hooks/prepare-commit-msg", legacy.as_bytes());
    let original = r.root.join(".git/hooks/phasegent-original/commit-msg");
    std::fs::remove_file(&original).unwrap();
    std::fs::create_dir(&original).unwrap();
    assert!(!r.cli(&["hooks", "install"]).status.success());
    assert_eq!(r.read(".git/hooks/prepare-commit-msg"), legacy.as_bytes());
}

#[test]
fn symlinks_refused_and_worktree_directory_shared() {
    let r = Repo::new();
    std::os::unix::fs::symlink("target", r.root.join(".git/hooks/prepare-commit-msg")).unwrap();
    assert!(!r.cli(&["hooks", "install"]).status.success());
    std::fs::remove_file(r.root.join(".git/hooks/prepare-commit-msg")).unwrap();
    r.install();
    r.git(&["worktree", "add", "-b", "feat/10", "linked"]);
    let out = r
        .command(env!("CARGO_BIN_EXE_phasegent"))
        .current_dir(r.root.join("linked"))
        .args(["hooks", "install"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!r.root.join("linked/.git/hooks").exists());
}
