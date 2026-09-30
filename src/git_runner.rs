//! Generic Git process wrapper.
//!
//! The abstraction runs `git` with discrete argv entries (never through a
//! shell) and returns a structured [`GitError`] on spawn failure or a
//! non-zero exit. Nothing here reads credentials or prints secrets, and
//! output echoed back into an error is sanitised and bounded so arbitrary
//! repository stderr cannot flood logs.

use std::path::PathBuf;
use std::process::Command;

/// Upper bound for echoing Git output back into structured errors so
/// arbitrary repository stderr cannot flood logs or leak unrelated content.
const MAX_ECHO_CHARS: usize = 200;

#[derive(Debug)]
pub struct GitError {
    /// Stable machine-readable kind: `argument`, `branch`, `conflict`,
    /// `git`, or `not_implemented`.
    pub kind: &'static str,
    pub message: String,
}

impl GitError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({ "kind": self.kind, "message": self.message })
    }
}

#[derive(Debug)]
pub struct GitOutput {
    pub status: i32,
    pub stdout: String,
}

/// Abstraction over Git invocation so detached-HEAD and other paths can be
/// tested without spawning processes.
pub trait GitRunner {
    fn run(&self, args: &[&str]) -> Result<GitOutput, GitError>;
}

pub struct ProcessGitRunner {
    workdir: Option<PathBuf>,
}

impl ProcessGitRunner {
    pub fn new() -> Self {
        Self { workdir: None }
    }

    pub fn in_directory(workdir: impl Into<PathBuf>) -> Self {
        Self {
            workdir: Some(workdir.into()),
        }
    }
}

impl Default for ProcessGitRunner {
    fn default() -> Self {
        Self::new()
    }
}

impl GitRunner for ProcessGitRunner {
    fn run(&self, args: &[&str]) -> Result<GitOutput, GitError> {
        let mut command = Command::new("git");
        command.args(args);
        if let Some(workdir) = &self.workdir {
            command.current_dir(workdir);
        }
        let output = command
            .output()
            .map_err(|error| GitError::new("git", format!("failed to run git: {error}")))?;
        let status = output.status.code().unwrap_or(-1);
        Ok(GitOutput {
            status,
            stdout: sanitize_output(&output.stdout),
        })
    }
}

pub fn sanitize_output(raw: &[u8]) -> String {
    let lossy = String::from_utf8_lossy(raw);
    let cleaned: String = lossy.chars().filter(|c| !c.is_control()).collect();
    cleaned.trim().chars().take(MAX_ECHO_CHARS).collect()
}

/// The current named branch, or a structured error. Detached HEAD is the
/// `branch` kind so callers can treat it as "no branch-scoped context".
pub fn current_branch(runner: &dyn GitRunner) -> Result<String, GitError> {
    let output = runner.run(&["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let name = output.stdout.trim();
    if output.status == 0 && !name.is_empty() {
        return Ok(name.to_owned());
    }
    if output.status == 1 {
        return Err(GitError::new(
            "branch",
            "HEAD is detached; switch to a named branch before working with its Redmine issue binding",
        ));
    }
    Err(GitError::new(
        "git",
        format!("git symbolic-ref failed with exit status {}", output.status),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeGitRunner {
        calls: RefCell<Vec<Vec<String>>>,
        responses: Vec<(&'static [&'static str], i32, String)>,
    }

    impl GitRunner for FakeGitRunner {
        fn run(&self, args: &[&str]) -> Result<GitOutput, GitError> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|value| value.to_string()).collect());
            for (prefix, status, stdout) in &self.responses {
                if args.starts_with(prefix) {
                    return Ok(GitOutput {
                        status: *status,
                        stdout: stdout.clone(),
                    });
                }
            }
            Err(GitError::new(
                "git",
                format!("unexpected git invocation {args:?}"),
            ))
        }
    }

    #[test]
    fn sanitize_output_strips_control_characters_and_bounds_length() {
        assert_eq!(sanitize_output(b"ok\n"), "ok");
        assert_eq!(sanitize_output(b"a\x07b\x1b[31m"), "ab[31m");
        let long = "x".repeat(500);
        let sanitized = sanitize_output(long.as_bytes());
        assert_eq!(sanitized.len(), 200);
    }

    #[test]
    fn current_branch_reads_the_symbolic_ref_and_rejects_detached_head() {
        let named = FakeGitRunner {
            calls: RefCell::new(Vec::new()),
            responses: vec![(&["symbolic-ref"], 0, "feat/9\n".to_owned())],
        };
        assert_eq!(current_branch(&named).unwrap(), "feat/9");
        assert_eq!(
            named.calls.borrow()[0],
            vec![
                "symbolic-ref".to_owned(),
                "--quiet".to_owned(),
                "--short".to_owned(),
                "HEAD".to_owned()
            ]
        );

        let detached = FakeGitRunner {
            calls: RefCell::new(Vec::new()),
            responses: vec![(&["symbolic-ref"], 1, String::new())],
        };
        let error = current_branch(&detached).unwrap_err();
        assert_eq!(error.kind, "branch");
        assert!(error.message.contains("detached"), "{}", error.message);
    }

    #[test]
    fn git_error_json_is_kind_and_message_only() {
        let error = GitError::new("git", "boom");
        assert_eq!(
            error.json(),
            serde_json::json!({"kind": "git", "message": "boom"})
        );
    }
}
