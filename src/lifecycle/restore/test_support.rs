//! Isolated Git/runner fixtures for the restore module.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::git_runner::{GitError, GitOutput, GitRunner};
use crate::infra::storage::Storage;
use crate::worktree::{
    GitOutput as WorktreeOutput, ProcessWorktreeRunner, WorktreeError, WorktreeRunner,
};

use super::store::{BaselineScope, CheckoutState, read_baseline};

pub(crate) struct TempDir(pub(crate) PathBuf);

impl TempDir {
    pub(crate) fn new(label: &str) -> Self {
        let dir = crate::test_scratch::root().join(format!(
            "phasegent-restore-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir create");
        Self(dir)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) struct TempDb {
    pub(crate) dir: TempDir,
    pub(crate) path: PathBuf,
}

impl TempDb {
    pub(crate) fn new(label: &str) -> Self {
        let dir = TempDir::new(label);
        let path = dir.path().join("phasegent.sqlite3");
        Self { dir, path }
    }

    pub(crate) fn open(&self) -> Storage {
        Storage::open_at(&self.path).expect("storage open")
    }

    pub(crate) fn path_text(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

pub(crate) struct SwitchState {
    pub repo_path: PathBuf,
    pub branch: String,
    pub head: String,
    pub git_dir: String,
    pub common_dir: String,
    pub clean: bool,
    pub checkout_status: i32,
    pub checkout_error: bool,
    pub after_checkout_branch: Option<String>,
    pub calls: Vec<String>,
    pub checkout_observed_pending: Option<bool>,
    pub db_path: Option<PathBuf>,
    pub scope: Option<BaselineScope>,
}

impl SwitchState {
    pub(crate) fn new(repo_path: &Path, branch: &str, head: &str) -> Self {
        let text = repo_path.to_string_lossy().into_owned();
        Self {
            repo_path: repo_path.to_path_buf(),
            branch: branch.to_owned(),
            head: head.to_owned(),
            git_dir: text.clone(),
            common_dir: text,
            clean: true,
            checkout_status: 0,
            checkout_error: false,
            after_checkout_branch: None,
            calls: Vec::new(),
            checkout_observed_pending: None,
            db_path: None,
            scope: None,
        }
    }
}

pub(crate) struct FakeGit {
    pub(crate) state: Rc<RefCell<SwitchState>>,
}

pub(crate) struct FakeWorktree {
    pub(crate) state: Rc<RefCell<SwitchState>>,
}

pub(crate) fn fake_switch(state: SwitchState) -> (FakeGit, FakeWorktree, Rc<RefCell<SwitchState>>) {
    let state = Rc::new(RefCell::new(state));
    (
        FakeGit {
            state: Rc::clone(&state),
        },
        FakeWorktree {
            state: Rc::clone(&state),
        },
        state,
    )
}

fn ok(stdout: impl Into<String>) -> GitOutput {
    GitOutput {
        status: 0,
        stdout: stdout.into(),
    }
}

impl GitRunner for FakeGit {
    fn run(&self, args: &[&str]) -> Result<GitOutput, GitError> {
        let mut state = self.state.borrow_mut();
        state.calls.push(format!("git {}", args.join(" ")));
        match args {
            ["remote", "get-url", "origin"] => Ok(ok("git@github.com:owner/repo.git")),
            ["rev-parse", "--show-toplevel"] => {
                Ok(ok(state.repo_path.to_string_lossy().into_owned()))
            }
            ["symbolic-ref", "--quiet", "--short", "HEAD"] => {
                if state.branch.is_empty() {
                    Ok(GitOutput {
                        status: 1,
                        stdout: String::new(),
                    })
                } else {
                    Ok(ok(state.branch.clone()))
                }
            }
            ["symbolic-ref", "refs/remotes/origin/HEAD"] => Ok(ok("refs/remotes/origin/main")),
            ["show-ref", "--verify", "--quiet", _] => Ok(GitOutput {
                status: 0,
                stdout: String::new(),
            }),
            ["checkout", target] => {
                if let (Some(db), Some(scope)) = (state.db_path.clone(), state.scope.clone()) {
                    let observed = Storage::open_at(&db).ok().and_then(|storage| {
                        read_baseline(&storage.connection, &scope).ok().flatten()
                    });
                    state.checkout_observed_pending =
                        Some(observed.is_some_and(|row| row.state == CheckoutState::Pending));
                }
                if state.checkout_error {
                    return Err(GitError::new("git", "checkout could not run"));
                }
                if state.checkout_status == 0 {
                    state.branch = (*target).to_owned();
                }
                if let Some(branch) = state.after_checkout_branch.clone() {
                    state.branch = branch;
                }
                Ok(GitOutput {
                    status: state.checkout_status,
                    stdout: String::new(),
                })
            }
            _ => Err(GitError::new(
                "git",
                format!("unexpected git invocation {args:?}"),
            )),
        }
    }
}

pub(crate) struct RealRepo {
    root: TempDir,
    pub(crate) bare: PathBuf,
    pub(crate) work: PathBuf,
    pub(crate) writer: PathBuf,
}

impl RealRepo {
    pub(crate) fn new(label: &str, with_upstream: bool) -> Self {
        let root = TempDir::new(label);
        let bare = root.path().join("remote.git");
        let work = root.path().join("work");
        std::fs::create_dir_all(&work).expect("work dir");
        let runner = ProcessWorktreeRunner::new();
        run_ok(
            &runner,
            root.path(),
            &["init", "--bare", "-q", &path_text(&bare)],
        );
        run_ok(&runner, &work, &["init", "-q", "-b", "main", "."]);
        run_ok(
            &runner,
            &work,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/owner/repo.git",
            ],
        );
        run_ok(
            &runner,
            &work,
            &["remote", "add", "fixture", &path_text(&bare)],
        );
        commit_empty(&runner, &work, "init");
        if with_upstream {
            run_ok(&runner, &work, &["push", "-q", "-u", "fixture", "main"]);
        } else {
            run_ok(&runner, &work, &["push", "-q", "fixture", "main"]);
        }
        let writer = root.path().join("writer");
        Self {
            root,
            bare,
            work,
            writer,
        }
    }

    pub(crate) fn advance_remote(&self) -> String {
        let runner = ProcessWorktreeRunner::new();
        if !self.writer.exists() {
            run_ok(
                &runner,
                self.root.path(),
                &[
                    "clone",
                    "-q",
                    &path_text(&self.bare),
                    &path_text(&self.writer),
                ],
            );
        }
        commit_empty(&runner, &self.writer, "remote advance");
        run_ok(&runner, &self.writer, &["push", "-q", "origin", "main"]);
        self.head(&self.writer)
    }

    pub(crate) fn head(&self, checkout: &Path) -> String {
        output_ok(
            &ProcessWorktreeRunner::new(),
            checkout,
            &["rev-parse", "HEAD"],
        )
    }

    pub(crate) fn branch(&self, checkout: &Path) -> String {
        output_ok(
            &ProcessWorktreeRunner::new(),
            checkout,
            &["symbolic-ref", "--quiet", "--short", "HEAD"],
        )
    }

    pub(crate) fn commit_empty(&self, checkout: &Path, message: &str) -> String {
        commit_empty(&ProcessWorktreeRunner::new(), checkout, message);
        self.head(checkout)
    }

    pub(crate) fn checkout_new(&self, branch: &str) {
        run_ok(
            &ProcessWorktreeRunner::new(),
            &self.work,
            &["checkout", "-q", "-b", branch],
        );
    }

    pub(crate) fn local_branch_exists(&self, branch: &str) -> bool {
        ProcessWorktreeRunner::new()
            .run(
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
                &self.work,
            )
            .is_ok_and(|output| output.status == 0)
    }

    pub(crate) fn delete_branch(&self, branch: &str) {
        run_ok(
            &ProcessWorktreeRunner::new(),
            &self.work,
            &["branch", "-D", branch],
        );
    }

    pub(crate) fn update_ref(&self, reference: &str, sha: &str) {
        run_ok(
            &ProcessWorktreeRunner::new(),
            &self.work,
            &["update-ref", reference, sha],
        );
    }

    pub(crate) fn add_worktree(&self, path: &Path, branch: &str) {
        run_ok(
            &ProcessWorktreeRunner::new(),
            &self.work,
            &["worktree", "add", "-q", "-b", branch, &path_text(path)],
        );
    }

    pub(crate) fn remove_worktree(&self, path: &Path) {
        run_ok(
            &ProcessWorktreeRunner::new(),
            &self.work,
            &["worktree", "remove", &path_text(path)],
        );
    }

    pub(crate) fn set_remote_url(&self, remote: &str, url: &str) {
        run_ok(
            &ProcessWorktreeRunner::new(),
            &self.work,
            &["remote", "set-url", remote, url],
        );
    }

    pub(crate) fn origin_url(&self) -> String {
        "https://github.com/owner/repo.git".to_owned()
    }

    pub(crate) fn scope(&self, issue: u64) -> BaselineScope {
        let repo_key = crate::branch_links::resolve_repo_key(Some(&self.origin_url()), &self.work)
            .expect("repo key")
            .key;
        let checkout_path = std::fs::canonicalize(&self.work)
            .expect("canonical work")
            .to_string_lossy()
            .into_owned();
        BaselineScope::new(&repo_key, "redmine", "tools", issue, &checkout_path).expect("scope")
    }

    pub(crate) fn identity(&self) -> String {
        crate::worktree::repo_identity(&ProcessWorktreeRunner::new(), &self.work)
            .expect("repo identity")
    }
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn run_ok(runner: &dyn WorktreeRunner, cwd: &Path, args: &[&str]) -> String {
    let output = runner.run(args, cwd).expect("git invocation");
    assert_eq!(
        output.status,
        0,
        "git {args:?} in {} failed: {}",
        cwd.display(),
        output.stdout
    );
    output.stdout
}

fn output_ok(runner: &dyn WorktreeRunner, cwd: &Path, args: &[&str]) -> String {
    run_ok(runner, cwd, args).trim().to_owned()
}

fn commit_empty(runner: &dyn WorktreeRunner, cwd: &Path, message: &str) {
    run_ok(
        runner,
        cwd,
        &[
            "-c",
            "user.name=phasegent-test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            message,
        ],
    );
}

impl WorktreeRunner for FakeWorktree {
    fn run(&self, args: &[&str], workdir: &Path) -> Result<WorktreeOutput, WorktreeError> {
        let mut state = self.state.borrow_mut();
        state.calls.push(format!(
            "wt {} {}",
            args.join(" "),
            workdir.to_string_lossy()
        ));
        match args {
            ["rev-parse", "--show-toplevel"] => Ok(WorktreeOutput {
                status: 0,
                stdout: state.repo_path.to_string_lossy().into_owned(),
            }),
            ["rev-parse", "--git-common-dir"] => Ok(WorktreeOutput {
                status: 0,
                stdout: state.common_dir.clone(),
            }),
            ["rev-parse", "--git-dir"] => Ok(WorktreeOutput {
                status: 0,
                stdout: state.git_dir.clone(),
            }),
            ["rev-parse", "--verify", "--quiet", "HEAD"] => Ok(WorktreeOutput {
                status: if state.head.is_empty() { 1 } else { 0 },
                stdout: state.head.clone(),
            }),
            ["symbolic-ref", "--quiet", "--short", "HEAD"] => Ok(WorktreeOutput {
                status: if state.branch.is_empty() { 1 } else { 0 },
                stdout: state.branch.clone(),
            }),
            ["status", "--porcelain"] => Ok(WorktreeOutput {
                status: 0,
                stdout: if state.clean {
                    String::new()
                } else {
                    " M src/main.rs".to_owned()
                },
            }),
            _ => Err(WorktreeError::new(
                "git",
                format!("unexpected worktree invocation {args:?}"),
            )),
        }
    }
}
