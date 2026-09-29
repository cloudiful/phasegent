use super::*;
use crate::branch_context::{BranchContextError, GitOutput, GitRunner};
use crate::branch_links::{self, IssueKey};

struct FakeGit {
    list_stdout: String,
    list_status: i32,
}

impl GitRunner for FakeGit {
    fn run(&self, args: &[&str]) -> Result<GitOutput, BranchContextError> {
        if args.starts_with(&["config", "--local", "--get-regexp"]) {
            return Ok(GitOutput {
                status: self.list_status,
                stdout: self.list_stdout.clone(),
            });
        }
        Err(BranchContextError::new("git", "unexpected invocation"))
    }
}

#[test]
fn legacy_list_parses_branches_with_dots_and_slashes() {
    let runner = FakeGit {
        list_stdout: "branch.main.redmine-issue-id 616\nbranch.feat/628.redmine-issue-id 628\n"
            .to_owned(),
        list_status: 0,
    };
    let bindings = branch_links::list_legacy_bindings(&runner).expect("list must work");
    assert_eq!(bindings.len(), 2);
    assert!(bindings.contains(&branch_links::LegacyBinding {
        branch: "main".to_owned(),
        issue_id: 616,
    }));
}

#[test]
fn legacy_list_skips_malformed_values_without_failing() {
    let runner = FakeGit {
        list_stdout: "branch.main.redmine-issue-id nope\nbranch.feat/1.redmine-issue-id 0\nbranch.ok.redmine-issue-id 7\n".to_owned(),
        list_status: 0,
    };
    let bindings = branch_links::list_legacy_bindings(&runner).expect("list must work");
    assert_eq!(
        bindings,
        vec![branch_links::LegacyBinding {
            branch: "ok".to_owned(),
            issue_id: 7,
        }]
    );
}

#[test]
fn per_clone_import_is_idempotent_and_keeps_git_source() {
    let repo = TempRepo::init("legacy-import");
    repo.set_binding("main", 616);
    repo.set_binding("feat/628", 628);

    struct DirRunner {
        dir: PathBuf,
    }
    impl GitRunner for DirRunner {
        fn run(&self, args: &[&str]) -> Result<GitOutput, BranchContextError> {
            let output = std::process::Command::new("git")
                .args(args)
                .current_dir(&self.dir)
                .output()
                .map_err(|error| BranchContextError::new("git", format!("spawn: {error}")))?;
            Ok(GitOutput {
                status: output.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            })
        }
    }

    let runner = DirRunner {
        dir: repo.path().to_path_buf(),
    };
    let bindings = branch_links::list_legacy_bindings(&runner).expect("list must work");
    assert_eq!(bindings.len(), 2);

    let connection = open_memory_db();
    let repo_key = "forge.example.com/owner/repo";
    let first = branch_links::import_legacy_bindings(
        &connection,
        repo_key,
        &bindings,
        "redmine",
        "tools/phasegent",
        1_700_000_001,
    )
    .expect("first import");
    assert_eq!(first.imported, 2);

    let second = branch_links::import_legacy_bindings(
        &connection,
        repo_key,
        &bindings,
        "redmine",
        "tools/phasegent",
        1_700_000_002,
    )
    .expect("second import");
    assert_eq!(second.imported, 0);
    assert_eq!(second.already_present, 2);

    assert_eq!(repo.get_binding("main").as_deref(), Some("616"));
    assert_eq!(repo.get_binding("feat/628").as_deref(), Some("628"));

    let rows = branch_links::issues_for_branch(
        &connection,
        repo_key,
        "main",
        false,
        &branch_links::UnknownState,
    )
    .expect("read");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].source, branch_links::LEGACY_SOURCE);
}

#[test]
fn import_does_not_resurrect_explicit_detach() {
    let connection = open_memory_db();
    let repo_key = "forge.example.com/owner/repo";
    let bindings = vec![branch_links::LegacyBinding {
        branch: "main".to_owned(),
        issue_id: 616,
    }];
    branch_links::import_legacy_bindings(
        &connection,
        repo_key,
        &bindings,
        "redmine",
        "tools/phasegent",
        10,
    )
    .expect("import");
    let issue = IssueKey::from_number("redmine", "tools/phasegent", 616).expect("key");
    branch_links::store::detach(&connection, repo_key, "main", &issue, "manual", 11)
        .expect("detach");
    let summary = branch_links::import_legacy_bindings(
        &connection,
        repo_key,
        &bindings,
        "redmine",
        "tools/phasegent",
        12,
    )
    .expect("reimport");
    assert_eq!(summary.imported, 0);
    let active = branch_links::issues_for_branch(
        &connection,
        repo_key,
        "main",
        false,
        &branch_links::UnknownState,
    )
    .expect("read");
    assert!(active.is_empty());
}

#[test]
fn stored_stale_provider_fails_closed_and_preserves_rows() {
    use crate::infra::storage::test_support::lock_workflow_tests;
    use crate::policy::Role;
    let _lock = lock_workflow_tests();
    let _env = clear_provider_env();
    let (dir, _db) = pin_db("scope-stale");
    let storage = crate::infra::storage::Storage::open_at(&dir.join("phasegent.sqlite3"))
        .expect("temp storage must open");
    storage
        .save_role_config(
            Role::Orchestrator,
            &crate::auth::StoredConfig {
                provider: Some("forgejo".to_owned()),
                api_base: None,
                repository: Some("owner/repo".to_owned()),
            },
        )
        .expect("role config must save");
    drop(storage);
    // A stale stored provider fails with the same actionable config
    // guidance as the other resolver paths instead of scoping the
    // link to a removed provider; nothing is guessed. Role-less
    // resolves stored config as orchestrator for scope only.
    let error = branch_links::resolve_link_scope(None, None, None, None)
        .expect_err("stale stored provider must fail closed");
    assert!(
        error.contains("forgejo"),
        "guidance must name the stale value: {error}"
    );
    assert!(
        error.contains("admin config provider clear"),
        "guidance must name the explicit remedy: {error}"
    );
    // The legacy rows are preserved verbatim for explicit
    // clear/replace; resolution never rewrites them.
    let storage = crate::infra::storage::Storage::open().expect("pinned storage must open");
    let stored = storage
        .load_role_config(Role::Orchestrator)
        .expect("read must work")
        .expect("legacy row must survive");
    assert_eq!(stored.provider.as_deref(), Some("forgejo"));
    assert_eq!(stored.repository.as_deref(), Some("owner/repo"));
}
