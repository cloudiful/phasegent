use super::*;
use crate::git_runner::{GitOutput, RawGitOutput};

struct Config {
    status: i32,
    bytes: Vec<u8>,
}
impl GitRunner for Config {
    fn run(&self, _: &[&str]) -> Result<GitOutput, GitError> {
        panic!("sanitized interface must not read config")
    }
    fn run_raw(&self, args: &[&str]) -> Result<RawGitOutput, GitError> {
        assert_eq!(
            args,
            [
                "config",
                "--null",
                "--get-regexp",
                "^core\\.comment(char|string)$"
            ]
        );
        Ok(RawGitOutput {
            status: self.status,
            stdout: self.bytes.clone(),
        })
    }
}
#[test]
fn effective_config_preserves_bytes_and_alias_order() {
    for (bytes, expected) in [
        (
            b"core.commentchar\n;\0core.commentstring\n;; \0".as_slice(),
            b";; ".as_slice(),
        ),
        ("core.commentstring\n※\0".as_bytes(), "※".as_bytes()),
        (b"core.commentstring\nauto\0core.commentchar\n#\0", b"#"),
    ] {
        assert_eq!(
            comment_marker(&Config {
                status: 0,
                bytes: bytes.to_vec()
            })
            .unwrap(),
            expected
        );
    }
    assert_eq!(
        comment_marker(&Config {
            status: 1,
            bytes: vec![]
        })
        .unwrap(),
        b"#"
    );
    for (status, bytes) in [
        (2, b"".as_slice()),
        (0, b"bad"),
        (0, b"core.commentchar\nauto\0"),
        (0, b"core.commentstring\n\0"),
    ] {
        assert!(
            comment_marker(&Config {
                status,
                bytes: bytes.to_vec()
            })
            .is_err()
        );
    }
}

#[test]
fn ownership_requires_complete_hook_specific_wrapper() {
    let path = Path::new("/hooks/original");
    for hook in [HookKind::PrepareCommitMsg, HookKind::CommitMsg] {
        assert!(declares_original(
            render_script(hook, Some(path)).as_bytes(),
            hook,
            path
        ));
        assert!(declares_original(
            render_legacy_script(hook, Some(path)).as_bytes(),
            hook,
            path
        ));
        assert!(!declares_original(
            b"#!/bin/sh\n# phasegent:managed\nexit 0\n",
            hook,
            path
        ));
        assert!(!declares_original(
            format!("{}# extra\n", render_script(hook, Some(path))).as_bytes(),
            hook,
            path
        ));
    }
}
