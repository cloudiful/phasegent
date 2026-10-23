use std::cell::{Cell, RefCell};
use std::path::Path;

use super::host_cache::{SyncHostCache, SyncOccupancyGuard};
use crate::lifecycle::CleanupMode;
use crate::lifecycle::cleanup::candidate::OccupancyGuard;
use crate::lifecycle::cleanup::target::TargetResolver;
use crate::worktree::opencode::{ApiError, ListRequest, LocatedSession, OpenCodeApi, SessionPage};

#[derive(Default)]
struct Host {
    sessions: Cell<usize>,
    lists: Cell<usize>,
    fail: Cell<bool>,
    directory: RefCell<String>,
    occupant: RefCell<Option<LocatedSession>>,
}

impl OpenCodeApi for Host {
    fn session(&self, id: &str) -> Result<LocatedSession, ApiError> {
        self.sessions.set(self.sessions.get() + 1);
        if self.fail.get() {
            return Err(ApiError::plain("unknown session"));
        }
        Ok(LocatedSession {
            id: id.to_owned(),
            directory: self.directory.borrow().clone(),
        })
    }

    fn list(&self, _: &ListRequest) -> Result<SessionPage, ApiError> {
        self.lists.set(self.lists.get() + 1);
        if self.fail.get() {
            return Err(ApiError::plain("unavailable listing"));
        }
        Ok(SessionPage {
            sessions: self.occupant.borrow().clone().into_iter().collect(),
            next_cursor: None,
        })
    }

    fn move_session(&self, _: &str, _: &Path) -> Result<(), ApiError> {
        panic!("sync must not forward moves")
    }
}

fn check(guard: &dyn OccupancyGuard, directory: &str) -> Option<String> {
    let target = TargetResolver::new(Path::new("/repo"), "/repo/.git");
    guard.keep_reason(
        CleanupMode::Sync,
        &target,
        Path::new(directory),
        &["ses_a".to_owned()],
        None,
    )
}

#[test]
fn all_fresh_free_path_has_no_redundant_occupancy_pass() {
    let host = Host::default();
    *host.directory.borrow_mut() = "/main".to_owned();
    let cache = SyncHostCache::new(&host);
    let guard = SyncOccupancyGuard::new(&cache, &host);
    assert_eq!(check(&guard, "/one"), None);
    assert_eq!((host.sessions.get(), host.lists.get()), (1, 1));
    assert_eq!(check(&guard, "/two"), None);
    assert_eq!(
        (host.sessions.get(), host.lists.get()),
        (2, 2),
        "second directory uses cache plus one direct freshness pass"
    );
}

#[test]
fn listing_is_reused_across_directories_when_cached_evidence_keeps() {
    let host = Host::default();
    *host.directory.borrow_mut() = "/main".to_owned();
    *host.occupant.borrow_mut() = Some(LocatedSession {
        id: "ses_foreign".to_owned(),
        directory: "/two".to_owned(),
    });
    let cache = SyncHostCache::new(&host);
    let guard = SyncOccupancyGuard::new(&cache, &host);
    assert_eq!(check(&guard, "/one"), None);
    assert!(check(&guard, "/two").unwrap().contains("ses_foreign"));
    assert_eq!((host.sessions.get(), host.lists.get()), (1, 1));
}

#[test]
fn cached_free_with_new_foreign_occupant_keeps() {
    let host = Host::default();
    *host.directory.borrow_mut() = "/main".to_owned();
    let cache = SyncHostCache::new(&host);
    let guard = SyncOccupancyGuard::new(&cache, &host);
    assert_eq!(check(&guard, "/one"), None);
    *host.occupant.borrow_mut() = Some(LocatedSession {
        id: "ses_foreign".to_owned(),
        directory: "/two".to_owned(),
    });
    assert!(check(&guard, "/two").unwrap().contains("ses_foreign"));
    assert_eq!((host.sessions.get(), host.lists.get()), (2, 2));
}

#[test]
fn cached_free_with_failed_fresh_evidence_keeps() {
    let host = Host::default();
    *host.directory.borrow_mut() = "/main".to_owned();
    let cache = SyncHostCache::new(&host);
    let guard = SyncOccupancyGuard::new(&cache, &host);
    assert_eq!(check(&guard, "/one"), None);
    host.fail.set(true);
    assert!(
        check(&guard, "/two")
            .unwrap()
            .contains("could not be located")
    );
}

#[test]
fn cached_keep_is_conservative_and_does_not_refresh() {
    let host = Host::default();
    *host.directory.borrow_mut() = "/one".to_owned();
    let cache = SyncHostCache::new(&host);
    let guard = SyncOccupancyGuard::new(&cache, &host);
    assert!(check(&guard, "/one").is_some());
    *host.directory.borrow_mut() = "/main".to_owned();
    assert!(check(&guard, "/one").is_some());
    assert_eq!((host.sessions.get(), host.lists.get()), (1, 1));
}

#[test]
fn failed_reads_are_not_cached_and_moves_are_denied() {
    let host = Host::default();
    let cache = SyncHostCache::new(&host);
    let request = ListRequest {
        directory: String::new(),
        cursor: None,
    };
    host.fail.set(true);
    for _ in 0..2 {
        assert!(cache.session("ses_unknown").is_err());
        assert!(cache.list(&request).is_err());
    }
    assert_eq!((host.sessions.get(), host.lists.get()), (2, 2));
    assert!(cache.move_session("ses_a", Path::new("/main")).is_err());
}

struct Scratch(std::path::PathBuf);
impl Scratch {
    fn path(&self) -> &Path {
        &self.0
    }
    fn new(label: &str) -> Self {
        let root = Self(crate::test_scratch::root().join(format!(
                "sync-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            )));
        std::fs::create_dir_all(root.path()).unwrap();
        root
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn taxi_list_and_acquire_reuse_identity_store_and_read_each_issue_once() {
    use crate::infra::storage::Storage;
    use crate::worktree::leases::{NewLease, ensure_schema, insert_lease, list_all_for_repo};
    use crate::worktree::{GitOutput, WorktreeError, WorktreeRunner};

    struct Runner(Cell<usize>);
    impl WorktreeRunner for Runner {
        fn run(&self, args: &[&str], _: &Path) -> Result<GitOutput, WorktreeError> {
            let stdout = match args {
                ["rev-parse", "--git-common-dir"] => {
                    self.0.set(self.0.get() + 1);
                    "/fixture/.git"
                }
                ["rev-parse", "--git-dir"] => "/fixture/.git/worktrees/one",
                ["status", "--porcelain"] => "?? dirty",
                ["symbolic-ref", "--quiet", "--short", "HEAD"] => "fixture-branch",
                _ => panic!("unexpected Git call: {args:?}"),
            };
            Ok(GitOutput {
                status: 0,
                stdout: stdout.to_owned(),
            })
        }
    }
    let root = Scratch::new("reuse");
    let mut storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    ensure_schema(&storage).unwrap();
    storage.path = root.path().to_path_buf();
    for (lease, issue) in [("one", 1), ("two", 2), ("three", 2)] {
        insert_lease(
            &storage,
            NewLease {
                lease_id: lease,
                identity: "/fixture/.git",
                issue,
                session: lease,
                checkout_path: "/fixture",
                worktree_path: root.path().to_str().unwrap(),
                branch: "branch",
                status: if lease == "one" { "active" } else { "retained" },
                created_at: 1,
                heartbeat_at: 1,
            },
        )
        .unwrap();
    }
    let runner = Runner(Cell::new(0));
    let reads = RefCell::new(Vec::new());
    let providers = Cell::new(0);
    let (warnings, setup) = super::taxi_sync_in(&runner, storage, root.path(), || {
        providers.set(providers.get() + 1);
        Ok(|issue| {
            reads.borrow_mut().push(issue);
            Ok(crate::providers::IssueSummary {
                id: issue,
                number: issue,
                title: String::new(),
                body: String::new(),
                state: "closed".to_owned(),
                html_url: None,
                project: None,
            })
        })
    });
    let setup = setup.unwrap();
    assert_eq!(providers.get(), 1);
    assert!(
        warnings
            .iter()
            .any(|warning| warning.contains("released 1 lease"))
    );
    assert_eq!(*reads.borrow(), vec![1, 2]);
    assert_eq!(runner.0.get(), 1);
    let identity = &setup.scopes[0].repo_identity;
    assert_eq!(
        crate::cli::worktree::query::execute_list_seeded(None, Some(&setup), &runner),
        0
    );
    let payload = crate::cli::worktree::query::list_payload(&setup.storage, identity).unwrap();
    assert!(
        payload["leases"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["status"] == "retained")
    );
    assert_eq!(
        crate::cli::worktree::acquire::execute_acquire_at(
            3,
            Some("new-owner"),
            "json",
            false,
            None,
            Some(&setup),
            &runner,
            root.path(),
        ),
        0
    );
    assert_eq!(
        runner.0.get(),
        1,
        "taxi + list + acquire share the resolved identity"
    );
    let payload = crate::cli::worktree::query::list_payload(&setup.storage, identity).unwrap();
    assert!(
        payload["leases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["issue"] == 3 && row["status"] == "active")
    );
    assert_eq!(
        list_all_for_repo(&setup.storage, "/fixture/.git")
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn legacy_sync_wrapper_uses_the_supplied_connection() {
    use crate::infra::storage::Storage;
    use crate::worktree::{GitOutput, WorktreeError, WorktreeRunner};
    struct Runner;
    impl WorktreeRunner for Runner {
        fn run(&self, args: &[&str], _: &Path) -> Result<GitOutput, WorktreeError> {
            assert_eq!(args, ["rev-parse", "--git-common-dir"]);
            Ok(GitOutput {
                status: 0,
                stdout: "/fixture/.git".to_owned(),
            })
        }
    }
    let root = Scratch::new("legacy");
    let mut storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    crate::worktree::leases::ensure_schema(&storage).unwrap();
    storage.path = root.path().to_path_buf();
    let provider = crate::providers::ProviderDispatcher::local(
        crate::providers::local::LocalProvider::open_at(&root.path().join("local.sqlite3"))
            .unwrap(),
    );
    let report = super::run_sync(
        &provider,
        &Runner,
        &storage,
        super::SyncRequest {
            all: false,
            mode: super::SyncMode::Clean,
            cwd: root.path(),
        },
    )
    .unwrap();
    assert_eq!(report.checked, 0);
}

#[test]
fn candidate_grouping_is_retained_between_preflight_and_engine() {
    use crate::infra::storage::Storage;
    use crate::worktree::{GitOutput, WorktreeError, WorktreeRunner};
    struct Runner;
    impl WorktreeRunner for Runner {
        fn run(&self, args: &[&str], _: &Path) -> Result<GitOutput, WorktreeError> {
            assert_eq!(args, ["rev-parse", "--git-common-dir"]);
            Ok(GitOutput {
                status: 0,
                stdout: "/fixture/.git".to_owned(),
            })
        }
    }
    let root = Scratch::new("grouping");
    let candidate = root.path().join("candidate");
    std::fs::create_dir(&candidate).unwrap();
    let storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    crate::worktree::leases::ensure_schema(&storage).unwrap();
    crate::worktree::leases::insert_lease(
        &storage,
        crate::worktree::leases::NewLease {
            lease_id: "one",
            identity: "/fixture/.git",
            issue: 1,
            session: "cli",
            checkout_path: "/fixture",
            worktree_path: candidate.to_str().unwrap(),
            branch: "branch",
            status: "retained",
            created_at: 1,
            heartbeat_at: 1,
        },
    )
    .unwrap();
    let reads = Cell::new(0);
    let (warnings, setup) = super::taxi_sync_in(&Runner, storage, root.path(), || {
        std::fs::remove_dir(&candidate).unwrap();
        Ok(|issue| {
            reads.set(reads.get() + 1);
            Ok(crate::providers::IssueSummary {
                id: issue,
                number: issue,
                title: String::new(),
                body: String::new(),
                state: "open".to_owned(),
                html_url: None,
                project: None,
            })
        })
    });
    assert!(warnings.is_empty());
    assert!(setup.is_some());
    assert_eq!(
        reads.get(),
        1,
        "engine consumes the preflight grouping rather than rescanning paths"
    );
    let setup = setup.unwrap();
    let resolutions = Cell::new(0);
    let (warnings, _) = super::taxi_sync_in(&Runner, setup.storage, root.path(), || {
        resolutions.set(resolutions.get() + 1);
        Ok(|_| unreachable!())
    });
    assert_eq!(
        resolutions.get(),
        0,
        "no candidates must not resolve credentials or a provider"
    );
    assert!(warnings.is_empty());
}

#[test]
fn successful_pages_are_keyed_by_cursor_and_directory() {
    let host = Host::default();
    let cache = SyncHostCache::new(&host);
    for cursor in [None, Some("next".to_owned())] {
        let request = ListRequest {
            directory: String::new(),
            cursor,
        };
        cache.list(&request).unwrap();
        cache.list(&request).unwrap();
    }
    cache
        .list(&ListRequest {
            directory: "/different".to_owned(),
            cursor: None,
        })
        .unwrap();
    assert_eq!(host.lists.get(), 3);
}
