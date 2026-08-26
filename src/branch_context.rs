//! Branch-scoped Redmine issue context stored in native local Git config.
//!
//! Binding state lives in `[branch "<name>"] redmine-issue-id` inside the
//! checkout's own `.git/config`. There is deliberately no SQLite or global
//! fallback: a binding belongs to one branch in one checkout and switches
//! with the branch. Nothing in this module reads credentials or prints
//! secrets; all Git arguments are passed as discrete argv entries and never
//! through a shell.

use std::path::PathBuf;
use std::process::Command;

/// Config key suffix under each `branch.<name>` section.
pub const CONFIG_KEY_SUFFIX: &str = "redmine-issue-id";

/// Upper bound for echoing Git output back into structured errors so
/// arbitrary repository stderr cannot flood logs or leak unrelated content.
const MAX_ECHO_CHARS: usize = 200;

#[derive(Debug)]
pub struct BranchContextError {
    /// Stable machine-readable kind: `argument`, `branch`, `conflict`,
    /// `git`, or `not_implemented`.
    pub kind: &'static str,
    pub message: String,
    pub branch: Option<String>,
}

impl BranchContextError {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            branch: None,
        }
    }

    fn for_branch(kind: &'static str, message: impl Into<String>, branch: &str) -> Self {
        Self {
            kind,
            message: message.into(),
            branch: Some(branch.to_owned()),
        }
    }

    pub fn json(&self) -> serde_json::Value {
        let mut value = serde_json::json!({ "kind": self.kind, "message": self.message });
        if let Some(branch) = &self.branch {
            value["branch"] = serde_json::json!(branch);
        }
        value
    }
}

#[derive(Debug)]
pub struct GitOutput {
    pub status: i32,
    pub stdout: String,
}

/// Abstraction over Git invocation so detached-HEAD and overwrite paths can
/// be tested without spawning processes.
pub trait GitRunner {
    /// Runs `git` with discrete argv entries; never through a shell.
    fn run(&self, args: &[&str]) -> Result<GitOutput, BranchContextError>;
}

pub struct ProcessGitRunner {
    workdir: Option<PathBuf>,
}

impl ProcessGitRunner {
    pub fn new() -> Self {
        Self { workdir: None }
    }

    /// Test/phase-2 helper for running Git inside a specific checkout;
    /// unused by the CLI itself today.
    #[allow(dead_code)]
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
    fn run(&self, args: &[&str]) -> Result<GitOutput, BranchContextError> {
        let mut command = Command::new("git");
        command.args(args);
        if let Some(workdir) = &self.workdir {
            command.current_dir(workdir);
        }
        let output = command.output().map_err(|error| {
            BranchContextError::new("git", format!("failed to run git: {error}"))
        })?;
        let status = output.status.code().unwrap_or(-1);
        Ok(GitOutput {
            status,
            stdout: sanitize_output(&output.stdout),
        })
    }
}

/// Strips control characters, trims whitespace, and bounds the echoed
/// length so Git output embedded in structured messages stays safe.
pub fn sanitize_output(raw: &[u8]) -> String {
    let lossy = String::from_utf8_lossy(raw);
    let cleaned: String = lossy.chars().filter(|c| !c.is_control()).collect();
    cleaned.trim().chars().take(MAX_ECHO_CHARS).collect()
}

/// Canonical config key for a branch binding.
pub fn config_key(branch: &str) -> String {
    format!("branch.{branch}.{CONFIG_KEY_SUFFIX}")
}

/// Validates an issue ID as a strictly positive integer.
pub fn parse_issue_id(raw: &str) -> Result<u64, BranchContextError> {
    raw.parse::<u64>().ok().filter(|id| *id > 0).ok_or_else(|| {
        BranchContextError::new(
            "argument",
            format!("issue id '{raw}' must be a positive integer"),
        )
    })
}

/// Resolves the current named branch. Detached HEAD is a structured,
/// actionable error rather than a silent failure.
pub fn current_branch(runner: &dyn GitRunner) -> Result<String, BranchContextError> {
    let output = runner.run(&["symbolic-ref", "--quiet", "--short", "HEAD"])?;
    let name = output.stdout.trim();
    if output.status == 0 && !name.is_empty() {
        return Ok(name.to_owned());
    }
    if output.status == 1 {
        return Err(BranchContextError::new(
            "branch",
            "HEAD is detached; switch to a named branch before working with its Redmine issue binding",
        ));
    }
    Err(BranchContextError::new(
        "git",
        format!("git symbolic-ref failed with exit status {}", output.status),
    ))
}

pub fn read_issue_id(
    runner: &dyn GitRunner,
    branch: &str,
) -> Result<Option<u64>, BranchContextError> {
    let output = runner.run(&["config", "--local", "--get", &config_key(branch)])?;
    match output.status {
        0 => {
            let raw = output.stdout.trim();
            parse_issue_id(raw).map(Some).map_err(|_| {
                BranchContextError::for_branch(
                    "git",
                    format!(
                        "stored value '{raw}' for {} is not a valid issue id",
                        config_key(branch)
                    ),
                    branch,
                )
            })
        }
        1 => Ok(None),
        _ => Err(BranchContextError::for_branch(
            "git",
            format!("git config --get failed with exit status {}", output.status),
            branch,
        )),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct BindOutcome {
    pub branch: String,
    pub issue_id: u64,
    pub replaced_existing: bool,
    pub already_bound: bool,
}

/// Binds the current branch to an issue. An existing different binding is
/// rejected unless `replace` is explicit; re-binding the same issue is an
/// idempotent no-op.
pub fn bind(
    runner: &dyn GitRunner,
    issue_id: u64,
    replace: bool,
) -> Result<BindOutcome, BranchContextError> {
    let branch = current_branch(runner)?;
    let existing = read_issue_id(runner, &branch)?;
    if existing == Some(issue_id) {
        return Ok(BindOutcome {
            branch,
            issue_id,
            replaced_existing: false,
            already_bound: true,
        });
    }
    if let Some(current) = existing.filter(|_| !replace) {
        return Err(BranchContextError::for_branch(
            "conflict",
            format!(
                "branch '{branch}' is already bound to issue {current}; \
                 re-run with --replace to overwrite"
            ),
            &branch,
        ));
    }
    let output = runner.run(&[
        "config",
        "--local",
        &config_key(&branch),
        &issue_id.to_string(),
    ])?;
    if output.status != 0 {
        return Err(BranchContextError::for_branch(
            "git",
            format!("git config failed with exit status {}", output.status),
            &branch,
        ));
    }
    Ok(BindOutcome {
        branch,
        issue_id,
        replaced_existing: existing.is_some(),
        already_bound: false,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum UnbindOutcome {
    Unbound { branch: String },
    NotBound { branch: String },
}

/// Removes the current branch's binding. Absence is a no-op, not an error.
pub fn unbind(runner: &dyn GitRunner) -> Result<UnbindOutcome, BranchContextError> {
    let branch = current_branch(runner)?;
    if read_issue_id(runner, &branch)?.is_none() {
        return Ok(UnbindOutcome::NotBound { branch });
    }
    let output = runner.run(&["config", "--local", "--unset", &config_key(&branch)])?;
    // Exit 5 means the key or section vanished concurrently; the desired
    // end state (unbound) is already reached.
    if output.status != 0 && output.status != 5 {
        return Err(BranchContextError::for_branch(
            "git",
            format!(
                "git config --unset failed with exit status {}",
                output.status
            ),
            &branch,
        ));
    }
    Ok(UnbindOutcome::Unbound { branch })
}

#[derive(Debug, PartialEq, Eq)]
pub struct BranchStatus {
    pub branch: String,
    pub issue_id: Option<u64>,
}

pub fn status(runner: &dyn GitRunner) -> Result<BranchStatus, BranchContextError> {
    let branch = current_branch(runner)?;
    let issue_id = read_issue_id(runner, &branch)?;
    Ok(BranchStatus { branch, issue_id })
}

pub fn execute_bind(
    runner: &dyn GitRunner,
    issue_id: u64,
    replace: bool,
) -> Result<serde_json::Value, BranchContextError> {
    let outcome = bind(runner, issue_id, replace)?;
    Ok(serde_json::json!({
        "bound": true,
        "branch": outcome.branch,
        "issue_id": outcome.issue_id,
        "replaced": outcome.replaced_existing,
        "already_bound": outcome.already_bound,
    }))
}

pub fn execute_unbind(runner: &dyn GitRunner) -> Result<serde_json::Value, BranchContextError> {
    let outcome = unbind(runner)?;
    let (unbound, branch) = match outcome {
        UnbindOutcome::Unbound { branch } => (true, branch),
        UnbindOutcome::NotBound { branch } => (false, branch),
    };
    Ok(serde_json::json!({ "unbound": unbound, "branch": branch }))
}

pub fn execute_status(runner: &dyn GitRunner) -> Result<serde_json::Value, BranchContextError> {
    let status = status(runner)?;
    Ok(serde_json::json!({
        "branch": status.branch,
        "issue_id": status.issue_id,
    }))
}
