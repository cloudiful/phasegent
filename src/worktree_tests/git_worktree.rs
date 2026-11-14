use super::support::*;
use super::*;

#[test]
fn worktree_add_and_remove_create_then_drop_a_real_worktree() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("wt-add-remove") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let target = repo.dir.path().join("wt-test");
    worktree_add(&runner, repo.dir.path(), &target, "phasegent/239-aaaaaa").expect("worktree add");
    assert!(target.exists());
    let porcelain = Command::new("git")
        .arg("-C")
        .arg(target.to_string_lossy().to_string())
        .args(["status", "--porcelain"])
        .output();
    if let Ok(out) = porcelain {
        assert_eq!(out.status.code().unwrap_or(-1), 0);
    }
    worktree_remove(&runner, repo.dir.path(), &target).expect("worktree remove");
    assert!(!target.exists());
}

#[test]
fn is_clean_returns_true_for_pristine_and_false_for_dirty() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("is-clean") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    assert!(is_clean(&runner, repo.dir.path()).expect("clean check"));
    let dirty = repo.dir.path().join("scratch.txt");
    std::fs::write(&dirty, "scratch\n").expect("write scratch");
    assert!(!is_clean(&runner, repo.dir.path()).expect("dirty check"));
    let _ = std::fs::remove_file(&dirty);
}

#[test]
fn is_clean_reports_structured_error_for_git_failure() {
    let _lock = lock_workflow_tests();
    let runner = FakeWorktreeRunner::new(vec![FakeResponse {
        args: vec!["status".to_string(), "--porcelain".to_string()],
        status: 128,
        stdout: "fatal: not a git repo".to_string(),
    }]);
    let error = is_clean(&runner, Path::new("/tmp/nope")).unwrap_err();
    assert_eq!(error.kind, "git");
}

#[test]
fn checkout_root_fails_closed_on_invalid_git_output() {
    let _lock = lock_workflow_tests();
    for (label, status, stdout) in [
        ("nonzero", 128, "fatal: not a git repository"),
        ("empty", 0, "   "),
        ("relative", 0, "relative/checkout"),
        ("unresolvable", 0, "/phasegent-missing-checkout-root"),
        ("multiline", 0, "/tmp/one\n/tmp/two\n"),
    ] {
        let runner = FakeWorktreeRunner::new(vec![FakeResponse {
            args: vec!["rev-parse".to_string(), "--show-toplevel".to_string()],
            status,
            stdout: stdout.to_string(),
        }]);
        let error = crate::worktree::git::checkout_root(&runner, Path::new("/tmp")).unwrap_err();
        assert_eq!(error.kind, "git", "{label}: {}", error.message);
    }
}

#[test]
fn checkout_git_dirs_rejects_unresolvable_gitdir_or_common_dir() {
    let _lock = lock_workflow_tests();
    let base = TempDir::new("git-dirs-invalid");
    let repo = base.path().join("repo");
    let common = repo.join(".git");
    std::fs::create_dir_all(&common).expect("common dir");
    let canonical_common = common.canonicalize().expect("canonical common");
    let missing = repo.join(".git/missing-worktree");
    for (label, git_dir, common_dir) in [
        (
            "git-dir",
            missing.to_string_lossy().into_owned(),
            canonical_common.to_string_lossy().into_owned(),
        ),
        (
            "common-dir",
            canonical_common.to_string_lossy().into_owned(),
            missing.to_string_lossy().into_owned(),
        ),
        (
            "relative-git-dir",
            "missing/.git".to_owned(),
            canonical_common.to_string_lossy().into_owned(),
        ),
    ] {
        let runner = FakeWorktreeRunner::new(vec![
            FakeResponse {
                args: vec!["rev-parse".to_string(), "--git-dir".to_string()],
                status: 0,
                stdout: format!("{git_dir}\n"),
            },
            FakeResponse {
                args: vec!["rev-parse".to_string(), "--git-common-dir".to_string()],
                status: 0,
                stdout: format!("{common_dir}\n"),
            },
        ]);
        let error = crate::worktree::checkout_git_dirs(&runner, &repo)
            .expect_err("an unresolvable metadata path is never a linked root");
        assert_eq!(error.kind, "git", "{label}: {}", error.message);
    }
}

#[test]
fn checkout_root_preserves_trailing_space_and_overlong_paths_exactly() {
    let _lock = lock_workflow_tests();
    let base = TempDir::new("path-fidelity");
    let runner = ProcessWorktreeRunner::new();
    let init = |root: &Path| {
        std::fs::create_dir_all(root).expect("root directory");
        assert!(
            runner
                .run(&["init", "-q", "-b", "main"], root)
                .map(|output| output.status == 0)
                .unwrap_or(false),
            "git init at {}",
            root.display()
        );
        let _ = runner.run(
            &[
                "-c",
                "user.name=phasegent-test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
            root,
        );
    };

    let spaced_root = base.path().join("repo ");
    let sibling = base.path().join("repo");
    init(&spaced_root);
    init(&sibling);
    let nested = spaced_root.join("sub");
    std::fs::create_dir_all(&nested).expect("nested");
    let expected = spaced_root.canonicalize().expect("canonical spaced root");
    assert_eq!(
        crate::worktree::git::checkout_root(&runner, &nested).expect("spaced root"),
        expected,
        "a trailing space is part of the path, not trimming noise"
    );
    let identity = repo_identity(&runner, &nested).expect("spaced identity");
    assert_eq!(
        identity,
        expected.join(".git").to_string_lossy(),
        "the identity read preserves the same path"
    );
    let (_, common) = crate::worktree::checkout_git_dirs(&runner, &nested).expect("git dirs");
    assert_eq!(Path::new(&common), expected.join(".git"));

    let long_root = base.path().join("a".repeat(120)).join("b".repeat(120));
    init(&long_root);
    let long_nested = long_root.join("sub");
    std::fs::create_dir_all(&long_nested).expect("long nested");
    let long_expected = long_root.canonicalize().expect("canonical long root");
    assert!(long_expected.to_string_lossy().len() > 200);
    assert_eq!(
        crate::worktree::git::checkout_root(&runner, &long_nested).expect("long root"),
        long_expected,
        "a valid path over the display bound is returned exactly"
    );
}

#[cfg(unix)]
#[test]
fn checkout_root_preserves_a_trailing_cr_instead_of_the_trimmed_sibling() {
    let _lock = lock_workflow_tests();
    let base = TempDir::new("path-cr");
    let runner = ProcessWorktreeRunner::new();
    let init = |root: &Path| {
        std::fs::create_dir_all(root).expect("root directory");
        assert!(
            runner
                .run(&["init", "-q", "-b", "main"], root)
                .map(|output| output.status == 0)
                .unwrap_or(false)
        );
        let _ = runner.run(
            &[
                "-c",
                "user.name=phasegent-test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "init",
            ],
            root,
        );
    };
    let cr_root = base.path().join("repo\r");
    let sibling = base.path().join("repo");
    init(&cr_root);
    init(&sibling);
    let nested = cr_root.join("sub");
    std::fs::create_dir_all(&nested).expect("nested");
    let expected = cr_root.canonicalize().expect("canonical CR root");
    assert_eq!(
        crate::worktree::git::checkout_root(&runner, &nested).expect("CR root"),
        expected,
        "a trailing CR is filename content on Unix"
    );
    let identity = repo_identity(&runner, &nested).expect("CR identity");
    assert_eq!(identity, expected.join(".git").to_string_lossy());
    let (_, common) = crate::worktree::checkout_git_dirs(&runner, &nested).expect("git dirs");
    assert_eq!(Path::new(&common), expected.join(".git"));
}

#[test]
fn worktree_add_surfaces_git_failure_as_structured_error() {
    let _lock = lock_workflow_tests();
    let runner = FakeWorktreeRunner::new(vec![FakeResponse {
        args: vec!["worktree".to_string(), "add".to_string()],
        status: 128,
        stdout: "fatal: bad ref".to_string(),
    }]);
    let error = worktree_add(
        &runner,
        Path::new("/tmp/repo"),
        Path::new("/tmp/repo/wt"),
        "phasegent/1-aaaaaa",
    )
    .unwrap_err();
    assert_eq!(error.kind, "git");
}

#[test]
fn a_cache_only_worktree_is_accepted_and_cleared_before_no_force_removal() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cache-only") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let target = repo.dir.path().join("wt-cache");
    worktree_add(&runner, repo.dir.path(), &target, "phasegent/1196-aaaaaa").expect("worktree add");
    std::fs::create_dir_all(target.join(".cache/ai")).expect("cache dir");
    std::fs::write(target.join(".cache/ai/scratch"), "scratch").expect("cache file");
    assert!(!is_clean(&runner, &target).expect("dirty"));
    assert!(matches!(
        crate::worktree::cache::inspect(&runner, &target).expect("inspect"),
        crate::worktree::cache::CacheInspection::CacheOnly(_)
    ));
    let expected = match crate::worktree::cache::inspect(&runner, &target).expect("inspect") {
        crate::worktree::cache::CacheInspection::CacheOnly(inventory) => inventory,
        other => panic!("expected a cache-only candidate, got {other:?}"),
    };
    let removed =
        crate::worktree::cache::clear_cache_only(&runner, &target, Some(&expected)).expect("clear");
    assert_eq!(removed, Some(2));
    assert!(!target.join(".cache").exists());
    assert!(is_clean(&runner, &target).expect("clean after cache clear"));
    worktree_remove(&runner, repo.dir.path(), &target).expect("worktree remove");
    assert!(!target.exists());
}

#[test]
fn cache_inventory_detects_same_length_file_replacement() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cache-replacement") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let cache = repo.dir.path().join(".cache");
    std::fs::create_dir(&cache).unwrap();
    std::fs::write(cache.join("file"), "old").unwrap();
    let before = crate::worktree::cache::inspect(&runner, repo.dir.path()).unwrap();
    std::fs::write(cache.join("replacement"), "new").unwrap();
    std::fs::rename(cache.join("replacement"), cache.join("file")).unwrap();
    let after = crate::worktree::cache::inspect(&runner, repo.dir.path()).unwrap();
    assert_ne!(before, after, "replacement must change the cache inventory");
}

#[test]
fn other_untracked_content_is_never_part_of_the_cache_exception() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cache-other") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let target = repo.dir.path().join("wt-cache-other");
    worktree_add(&runner, repo.dir.path(), &target, "phasegent/1196-bbbbbb").expect("worktree add");
    std::fs::create_dir_all(target.join(".cache")).expect("cache dir");
    std::fs::write(target.join(".cache/scratch"), "scratch").expect("cache file");
    std::fs::write(target.join("notes.txt"), "user content").expect("other file");
    assert!(matches!(
        crate::worktree::cache::inspect(&runner, &target).expect("inspect"),
        crate::worktree::cache::CacheInspection::NotCacheOnly
    ));
    let error = crate::worktree::cache::clear_cache_only(&runner, &target, None).unwrap_err();
    assert!(
        error.contains("became dirty after the local guard"),
        "got: {error}"
    );
    assert!(target.join("notes.txt").exists());
    assert!(target.join(".cache/scratch").exists());
}

#[test]
fn a_tracked_cache_path_disables_the_exception() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cache-tracked") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let target = repo.dir.path().join("wt-cache-tracked");
    worktree_add(&runner, repo.dir.path(), &target, "phasegent/1196-cccccc").expect("worktree add");
    std::fs::create_dir_all(target.join(".cache")).expect("cache dir");
    std::fs::write(target.join(".cache/tracked.txt"), "tracked").expect("tracked file");
    let _ = Command::new("git")
        .arg("-C")
        .arg(target.to_string_lossy().to_string())
        .args(["add", ".cache/tracked.txt"])
        .status();
    std::fs::write(target.join(".cache/untracked.txt"), "untracked").expect("untracked file");
    assert!(matches!(
        crate::worktree::cache::inspect(&runner, &target).expect("inspect"),
        crate::worktree::cache::CacheInspection::NotCacheOnly
    ));
}

#[test]
fn a_cache_inventory_change_between_the_two_reads_keeps_the_worktree() {
    struct MutatingRunner {
        inner: ProcessWorktreeRunner,
        worktree: std::path::PathBuf,
        status_calls: std::cell::Cell<usize>,
    }
    impl WorktreeRunner for MutatingRunner {
        fn run(
            &self,
            args: &[&str],
            workdir: &Path,
        ) -> Result<crate::worktree::GitOutput, crate::worktree::WorktreeError> {
            if let ["ls-files", "--", ".cache"] = args {
                let calls = self.status_calls.get() + 1;
                self.status_calls.set(calls);
                if calls == 2 {
                    std::fs::write(self.worktree.join(".cache/late.txt"), "late").expect("late");
                }
            }
            self.inner.run(args, workdir)
        }
    }
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cache-race") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let target = repo.dir.path().join("wt-cache-race");
    worktree_add(&runner, repo.dir.path(), &target, "phasegent/1196-dddddd").expect("worktree add");
    std::fs::create_dir_all(target.join(".cache")).expect("cache dir");
    std::fs::write(target.join(".cache/scratch"), "scratch").expect("cache file");
    let mutating = MutatingRunner {
        inner: ProcessWorktreeRunner::new(),
        worktree: target.clone(),
        status_calls: std::cell::Cell::new(0),
    };
    let expected = match crate::worktree::cache::inspect(&mutating, &target).expect("inspect") {
        crate::worktree::cache::CacheInspection::CacheOnly(inventory) => inventory,
        other => panic!("expected a cache-only candidate, got {other:?}"),
    };
    let error =
        crate::worktree::cache::clear_cache_only(&mutating, &target, Some(&expected)).unwrap_err();
    assert!(error.contains("changed before cleanup"), "got: {error}");
    assert!(target.join(".cache/scratch").exists());
    assert!(target.join(".cache/late.txt").exists());
}

#[test]
fn a_cache_file_is_not_a_directory_exception_and_an_over_bound_cache_keeps() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cache-limits") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let target = repo.dir.path().join("wt-cache-limits");
    worktree_add(&runner, repo.dir.path(), &target, "phasegent/1196-eeeeee").expect("worktree add");
    std::fs::write(target.join(".cache"), "not a directory").expect("cache file");
    assert!(matches!(
        crate::worktree::cache::inspect(&runner, &target).expect("inspect"),
        crate::worktree::cache::CacheInspection::NotCacheOnly
    ));
    std::fs::remove_file(target.join(".cache")).expect("remove file");
    std::fs::create_dir_all(target.join(".cache")).expect("cache dir");
    for index in 0..=crate::worktree::cache::MAX_CACHE_ENTRIES {
        std::fs::write(target.join(format!(".cache/{index:04}")), "").expect("entry");
    }
    let error = crate::worktree::cache::inspect(&runner, &target).unwrap_err();
    assert!(error.contains("inventory limit"), "got: {error}");
}

#[test]
fn clear_rejects_a_same_length_replacement_against_the_earlier_baseline() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cache-baseline-replacement") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let cache = repo.dir.path().join(".cache");
    std::fs::create_dir(&cache).unwrap();
    std::fs::write(cache.join("file"), "old").unwrap();
    let baseline = match crate::worktree::cache::inspect(&runner, repo.dir.path()).unwrap() {
        crate::worktree::cache::CacheInspection::CacheOnly(inventory) => inventory,
        other => panic!("expected cache-only, got {other:?}"),
    };
    std::fs::write(cache.join("replacement"), "new").unwrap();
    std::fs::rename(cache.join("replacement"), cache.join("file")).unwrap();
    let error = crate::worktree::cache::clear_cache_only(&runner, repo.dir.path(), Some(&baseline))
        .unwrap_err();
    assert!(error.contains("changed before cleanup"), "got: {error}");
    assert_eq!(std::fs::read(cache.join("file")).unwrap(), b"new");
}

#[test]
fn clear_rejects_a_root_replacement_against_the_earlier_baseline() {
    let _lock = lock_workflow_tests();
    let Some(repo) = TempRepo::init("cache-baseline-root") else {
        return;
    };
    let runner = ProcessWorktreeRunner::new();
    let cache = repo.dir.path().join(".cache");
    std::fs::create_dir(&cache).unwrap();
    std::fs::write(cache.join("file"), "old").unwrap();
    let baseline = match crate::worktree::cache::inspect(&runner, repo.dir.path()).unwrap() {
        crate::worktree::cache::CacheInspection::CacheOnly(inventory) => inventory,
        other => panic!("expected cache-only, got {other:?}"),
    };
    let moved = crate::test_scratch::root().join(format!(
        "phasegent-1196-cache-baseline-{}-{}",
        std::process::id(),
        now_unix_secs()
    ));
    std::fs::rename(&cache, &moved).unwrap();
    std::fs::create_dir(&cache).unwrap();
    std::fs::write(cache.join("file"), "old").unwrap();
    let error = crate::worktree::cache::clear_cache_only(&runner, repo.dir.path(), Some(&baseline))
        .unwrap_err();
    assert!(error.contains("changed before cleanup"), "got: {error}");
    assert!(cache.exists());
    assert!(moved.exists());
    let _ = std::fs::remove_dir_all(&moved);
}
