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
            parent_id: None,
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
        parent_id: None,
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
        parent_id: None,
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
    let (warnings, setup) = super::taxi_sync_in(
        crate::policy::Role::Orchestrator,
        &crate::cli::provider_context::ProviderContext::default(),
        &runner,
        storage,
        root.path(),
        || {
            providers.set(providers.get() + 1);
            Ok((None, true, |issue| {
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
            }))
        },
    );
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
fn scope_sync_skips_mixed_candidates_before_provider_reads() {
    use crate::infra::storage::Storage;
    use crate::worktree::leases::{NewLease, ensure_schema, insert_lease, list_all_for_repo};
    use crate::worktree::{GitOutput, WorktreeError, WorktreeRunner};
    struct Runner;
    impl WorktreeRunner for Runner {
        fn run(&self, args: &[&str], _: &Path) -> Result<GitOutput, WorktreeError> {
            assert_eq!(args, ["rev-parse", "--git-common-dir"]);
            Ok(GitOutput {
                status: 0,
                stdout: "/fixture/.git".into(),
            })
        }
    }
    let root = Scratch::new("scope-skip");
    let storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    ensure_schema(&storage).unwrap();
    for (id, issue) in [("legacy", 7), ("scoped", 8)] {
        insert_lease(
            &storage,
            NewLease {
                lease_id: id,
                identity: "/fixture/.git",
                issue,
                session: id,
                checkout_path: "/fixture",
                worktree_path: root.path().to_str().unwrap(),
                branch: "branch",
                status: "retained",
                created_at: 1,
                heartbeat_at: 1,
            },
        )
        .unwrap();
    }
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, created_at) VALUES ('scoped','forgejo','owner/repo',8,1)",
            [],
        )
        .unwrap();
    let mut setup = super::engine::resolve_setup(&Runner, storage, false, root.path()).unwrap();
    assert_eq!(setup.scopes[0].candidates.len(), 2);
    assert_eq!(setup.skipped.len(), 0);
    let calls = Cell::new(0);
    let report = super::engine::run_sync_resolved(
        &|_| {
            calls.set(calls.get() + 1);
            panic!("must not query provider")
        },
        None,
        super::engine::SyncProviderScope {
            scope: None,
            legacy_numeric_leases: true,
        },
        &Runner,
        &mut setup,
        super::SyncMode::Clean,
        false,
    )
    .unwrap();
    assert_eq!(calls.get(), 0);
    assert_eq!(report.checked, 0);
    assert!(
        report
            .skipped_repos
            .iter()
            .all(|s| s.reason.contains("scoped provider lease"))
    );
    assert!(
        list_all_for_repo(&setup.storage, "/fixture/.git")
            .unwrap()
            .iter()
            .all(|row| row.status == "retained")
    );
}

#[test]
fn scope_sync_processes_a_matching_provider_group_and_releases_its_exact_scope() {
    use crate::infra::storage::Storage;
    use crate::worktree::leases::{NewLease, ensure_schema, insert_lease, list_all_for_repo};
    use crate::worktree::{GitOutput, WorktreeError, WorktreeRunner};
    struct Runner;
    impl WorktreeRunner for Runner {
        fn run(&self, _: &[&str], _: &Path) -> Result<GitOutput, WorktreeError> {
            Ok(GitOutput {
                status: 0,
                stdout: "/fixture/.git".into(),
            })
        }
    }
    let root = Scratch::new("scope-process-match");
    let storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    ensure_schema(&storage).unwrap();
    let scoped_dir = root.path().join("scoped-dir");
    std::fs::create_dir_all(&scoped_dir).unwrap();
    insert_lease(
        &storage,
        NewLease {
            lease_id: "scoped",
            identity: "/fixture/.git",
            issue: 8,
            session: "s1",
            checkout_path: "/fixture",
            worktree_path: scoped_dir.to_str().unwrap(),
            branch: "branch",
            status: "active",
            created_at: 1,
            heartbeat_at: 1,
        },
    )
    .unwrap();
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, created_at) VALUES ('scoped','forgejo','owner/repo',8,1)",
            [],
        )
        .unwrap();
    let mut setup = super::engine::resolve_setup(&Runner, storage, false, root.path()).unwrap();
    let asked = Cell::new(Vec::new());
    let report = super::engine::run_sync_resolved(
        &|issue| {
            asked.set({
                let mut asked = asked.take();
                asked.push(issue);
                asked
            });
            Ok(crate::providers::IssueSummary {
                id: issue,
                number: issue,
                title: String::new(),
                body: String::new(),
                state: "closed".to_owned(),
                html_url: None,
                project: None,
            })
        },
        None,
        super::engine::SyncProviderScope {
            scope: Some(("forgejo", "owner/repo")),
            legacy_numeric_leases: false,
        },
        &Runner,
        &mut setup,
        super::SyncMode::Clean,
        false,
    )
    .unwrap();
    assert_eq!(asked.take(), vec![8], "the scoped issue is read once");
    assert_eq!(report.checked, 1);
    assert_eq!(report.released_leases, 1);
    assert!(
        list_all_for_repo(&setup.storage, "/fixture/.git")
            .unwrap()
            .iter()
            .all(|row| row.status == "retained")
    );
}

#[test]
fn scope_sync_keeps_a_group_when_the_provider_scope_differs() {
    use crate::infra::storage::Storage;
    use crate::worktree::leases::{NewLease, ensure_schema, insert_lease};
    use crate::worktree::{GitOutput, WorktreeError, WorktreeRunner};
    struct Runner;
    impl WorktreeRunner for Runner {
        fn run(&self, _: &[&str], _: &Path) -> Result<GitOutput, WorktreeError> {
            Ok(GitOutput {
                status: 0,
                stdout: "/fixture/.git".into(),
            })
        }
    }
    let root = Scratch::new("scope-process-foreign");
    let storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    ensure_schema(&storage).unwrap();
    let scoped_dir = root.path().join("scoped-dir");
    std::fs::create_dir_all(&scoped_dir).unwrap();
    insert_lease(
        &storage,
        NewLease {
            lease_id: "scoped",
            identity: "/fixture/.git",
            issue: 8,
            session: "s1",
            checkout_path: "/fixture",
            worktree_path: scoped_dir.to_str().unwrap(),
            branch: "branch",
            status: "retained",
            created_at: 1,
            heartbeat_at: 1,
        },
    )
    .unwrap();
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, created_at) VALUES ('scoped','forgejo','owner/other',8,1)",
            [],
        )
        .unwrap();
    let mut setup = super::engine::resolve_setup(&Runner, storage, false, root.path()).unwrap();
    let calls = Cell::new(0);
    let report = super::engine::run_sync_resolved(
        &|_| {
            calls.set(calls.get() + 1);
            panic!("must not query the provider for a foreign scope")
        },
        None,
        super::engine::SyncProviderScope {
            scope: Some(("forgejo", "owner/repo")),
            legacy_numeric_leases: false,
        },
        &Runner,
        &mut setup,
        super::SyncMode::Clean,
        false,
    )
    .unwrap();
    assert_eq!(calls.get(), 0);
    assert_eq!(report.checked, 0);
    assert!(
        report
            .skipped_repos
            .iter()
            .all(|s| s.reason.contains("scoped provider lease"))
    );
}

#[test]
fn scope_sync_rechecks_new_scoped_rows_before_provider_read() {
    use crate::infra::storage::Storage;
    use crate::worktree::leases::{NewLease, ensure_schema, insert_lease};
    use crate::worktree::{GitOutput, WorktreeError, WorktreeRunner};
    struct Runner;
    impl WorktreeRunner for Runner {
        fn run(&self, _: &[&str], _: &Path) -> Result<GitOutput, WorktreeError> {
            Ok(GitOutput {
                status: 0,
                stdout: "/fixture/.git".into(),
            })
        }
    }
    let root = Scratch::new("scope-recheck");
    let storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    ensure_schema(&storage).unwrap();
    let lease = |id, issue| NewLease {
        lease_id: id,
        identity: "/fixture/.git",
        issue,
        session: id,
        checkout_path: "/fixture",
        worktree_path: root.path().to_str().unwrap(),
        branch: "branch",
        status: "retained",
        created_at: 1,
        heartbeat_at: 1,
    };
    insert_lease(&storage, lease("legacy", 7)).unwrap();
    let mut setup = super::engine::resolve_setup(&Runner, storage, false, root.path()).unwrap();
    assert_eq!(setup.scopes[0].candidates.len(), 1);
    insert_lease(&setup.storage, lease("later", 8)).unwrap();
    setup
        .storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, created_at) VALUES ('later','forgejo','owner/repo',8,1)",
            [],
        )
        .unwrap();
    let calls = Cell::new(0);
    let report = super::engine::run_sync_resolved(
        &|_| {
            calls.set(calls.get() + 1);
            panic!("must not query provider")
        },
        None,
        super::engine::SyncProviderScope {
            scope: None,
            legacy_numeric_leases: true,
        },
        &Runner,
        &mut setup,
        super::SyncMode::Report,
        false,
    )
    .unwrap();
    assert_eq!(calls.get(), 0);
    assert_eq!(report.checked, 0);
    assert_eq!(report.skipped_repos.len(), 1);
    setup
        .storage
        .connection
        .execute("DROP TABLE worktree_lease_scopes", [])
        .unwrap();
    let failed = super::engine::run_sync_resolved(
        &|_| {
            calls.set(calls.get() + 1);
            panic!("must not query provider")
        },
        None,
        super::engine::SyncProviderScope {
            scope: None,
            legacy_numeric_leases: true,
        },
        &Runner,
        &mut setup,
        super::SyncMode::Report,
        false,
    );
    assert!(failed.is_err());
    assert_eq!(calls.get(), 0);
}

#[test]
fn scope_sync_proves_directories_of_new_exact_scope_leases() {
    use crate::infra::storage::Storage;
    use crate::worktree::leases::{NewLease, ensure_schema, insert_lease, list_all_for_repo};

    let root = Scratch::new("scope-new-directory-review");
    let storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    ensure_schema(&storage).unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    let insert = |id: &str, issue, path: &Path, status: &str| {
        insert_lease(
            &storage,
            NewLease {
                lease_id: id,
                identity: "/fixture/.git",
                issue,
                session: id,
                checkout_path: "/fixture",
                worktree_path: path.to_str().unwrap(),
                branch: "branch",
                status,
                created_at: 1,
                heartbeat_at: 1,
            },
        )
        .unwrap();
    };
    insert("first", 8, &first, "active");
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, created_at) VALUES ('first','forgejo','owner/repo',8,1)",
            [],
        )
        .unwrap();
    let candidate = list_all_for_repo(&storage, "/fixture/.git").unwrap();
    insert("legacy", 7, &second, "retained");
    insert("later", 8, &second, "active");
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, created_at) VALUES ('later','forgejo','owner/repo',8,1)",
            [],
        )
        .unwrap();
    let all = list_all_for_repo(&storage, "/fixture/.git").unwrap();
    let scopes =
        crate::worktree::lease_scope::scope_map_for_repo(&storage, "/fixture/.git").unwrap();
    assert!(
        matches!(
            super::engine::scoped_processing(
                &super::engine::group_for(8, &all, &scopes),
                &candidate,
                &all,
                &scopes,
                Some(("forgejo", "owner/repo")),
                false,
            ),
            super::engine::ScopedProcessing::Keep(_)
        ),
        "the newly added exact-scope lease shares a directory with legacy history and must keep"
    );
}

#[test]
fn legacy_sync_is_not_blocked_by_scoped_lease_on_another_path() {
    use crate::infra::storage::Storage;
    use crate::worktree::leases::{NewLease, ensure_schema, insert_lease, list_all_for_repo};

    let root = Scratch::new("legacy-isolation-review");
    let storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    ensure_schema(&storage).unwrap();
    for (id, issue) in [("legacy", 7), ("forgejo", 8)] {
        let path = root.path().join(id);
        std::fs::create_dir_all(&path).unwrap();
        insert_lease(
            &storage,
            NewLease {
                lease_id: id,
                identity: "/fixture/.git",
                issue,
                session: id,
                checkout_path: "/fixture",
                worktree_path: path.to_str().unwrap(),
                branch: "branch",
                status: "retained",
                created_at: 1,
                heartbeat_at: 1,
            },
        )
        .unwrap();
    }
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, created_at) VALUES ('forgejo','forgejo','owner/repo',8,1)",
            [],
        )
        .unwrap();
    let rows = list_all_for_repo(&storage, "/fixture/.git").unwrap();
    let candidate: Vec<_> = rows.iter().filter(|row| row.issue == 7).cloned().collect();
    let scopes =
        crate::worktree::lease_scope::scope_map_for_repo(&storage, "/fixture/.git").unwrap();
    assert!(
        matches!(
            super::engine::scoped_processing(
                &super::engine::group_for(7, &rows, &scopes),
                &candidate,
                &rows,
                &scopes,
                None,
                true,
            ),
            super::engine::ScopedProcessing::Unscoped
        ),
        "an unrelated scoped directory must not suppress the legacy sync path"
    );
}

#[test]
fn verified_retention_keeps_when_legacy_history_arrives_after_proof() {
    use crate::infra::storage::Storage;
    use crate::worktree::leases::{
        NewLease, ensure_schema, insert_lease, list_all_for_repo,
        retain_active_leases_for_scope_ids,
    };

    let root = Scratch::new("retain-late-legacy-review");
    let storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    ensure_schema(&storage).unwrap();
    let path = root.path().join("shared");
    std::fs::create_dir_all(&path).unwrap();
    let insert = |id: &str, issue, status: &str| {
        insert_lease(
            &storage,
            NewLease {
                lease_id: id,
                identity: "/fixture/.git",
                issue,
                session: id,
                checkout_path: "/fixture",
                worktree_path: path.to_str().unwrap(),
                branch: "branch",
                status,
                created_at: 1,
                heartbeat_at: 1,
            },
        )
        .unwrap();
    };
    insert("scoped", 8, "active");
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, created_at) VALUES ('scoped','forgejo','owner/repo',8,1)",
            [],
        )
        .unwrap();
    let verified_ids = vec!["scoped".to_owned()];
    let scope = crate::worktree::LeaseScope::new("forgejo", "owner/repo", 8).unwrap();
    let approved_rows = list_all_for_repo(&storage, "/fixture/.git").unwrap();
    let approved_scopes =
        crate::worktree::lease_scope::scope_map_for_repo(&storage, "/fixture/.git").unwrap();
    let approved =
        crate::worktree::leases::scoped_retention_proof(8, &approved_rows, &approved_scopes);
    insert("legacy", 7, "retained");
    let result = retain_active_leases_for_scope_ids(
        &storage,
        "/fixture/.git",
        None,
        &scope,
        &verified_ids,
        &approved,
        "fixture",
        2,
    );
    assert!(
        result.is_err() || matches!(result, Ok(0)),
        "new mixed history after verification must prevent retention: {result:?}"
    );
    let rows = list_all_for_repo(&storage, "/fixture/.git").unwrap();
    assert_eq!(
        rows.iter()
            .find(|row| row.lease_id == "scoped")
            .unwrap()
            .status,
        "active"
    );
}

#[test]
fn scoped_sync_keeps_same_number_legacy_history_on_another_path() {
    use crate::infra::storage::Storage;
    use crate::worktree::leases::{NewLease, ensure_schema, insert_lease, list_all_for_repo};

    let root = Scratch::new("same-number-scope-review");
    let storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    ensure_schema(&storage).unwrap();
    for id in ["legacy", "forgejo"] {
        let path = root.path().join(id);
        std::fs::create_dir_all(&path).unwrap();
        insert_lease(
            &storage,
            NewLease {
                lease_id: id,
                identity: "/fixture/.git",
                issue: 7,
                session: id,
                checkout_path: "/fixture",
                worktree_path: path.to_str().unwrap(),
                branch: "branch",
                status: "retained",
                created_at: 1,
                heartbeat_at: 1,
            },
        )
        .unwrap();
    }
    storage
        .connection
        .execute(
            "INSERT INTO worktree_lease_scopes (lease_id, provider, project, external_id, created_at) VALUES ('forgejo','forgejo','owner/repo',7,1)",
            [],
        )
        .unwrap();
    let rows = list_all_for_repo(&storage, "/fixture/.git").unwrap();
    let scopes =
        crate::worktree::lease_scope::scope_map_for_repo(&storage, "/fixture/.git").unwrap();
    assert!(
        matches!(
            super::engine::scoped_processing(
                &super::engine::group_for(7, &rows, &scopes),
                &rows,
                &rows,
                &scopes,
                Some(("forgejo", "owner/repo")),
                false,
            ),
            super::engine::ScopedProcessing::Keep(_)
        ),
        "same-number unscoped legacy history must prevent provider dispatch even on another path"
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
    let (warnings, setup) = super::taxi_sync_in(
        crate::policy::Role::Orchestrator,
        &crate::cli::provider_context::ProviderContext::default(),
        &Runner,
        storage,
        root.path(),
        || {
            std::fs::remove_dir(&candidate).unwrap();
            Ok((None, true, |issue| {
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
            }))
        },
    );
    assert!(warnings.is_empty());
    assert!(setup.is_some());
    assert_eq!(
        reads.get(),
        1,
        "engine consumes the preflight grouping rather than rescanning paths"
    );
    let setup = setup.unwrap();
    let resolutions = Cell::new(0);
    let (warnings, _) = super::taxi_sync_in(
        crate::policy::Role::Orchestrator,
        &crate::cli::provider_context::ProviderContext::default(),
        &Runner,
        setup.storage,
        root.path(),
        || {
            resolutions.set(resolutions.get() + 1);
            Ok((None, true, |_| unreachable!()))
        },
    );
    assert_eq!(
        resolutions.get(),
        0,
        "no candidates must not resolve credentials or a provider"
    );
    assert!(warnings.is_empty());
}

/// One durable task with its canonical scope, for the group-identity tests.
fn task_binding(
    storage: &crate::infra::storage::Storage,
    number: &str,
    instance: &str,
) -> crate::task_identity::TaskBinding {
    let key =
        crate::task_identity::TaskKey::new("forgejo", instance, "owner/repo", number).unwrap();
    let now = crate::worktree::now_unix_secs();
    let binding = crate::task_identity::ensure_binding(storage, &key, now).unwrap();
    crate::task_identity::add_repo_membership(
        storage,
        "forgejo.test/owner/repo",
        binding.task_id(),
        now,
    )
    .unwrap();
    binding
}

#[test]
fn one_number_becomes_one_group_per_durable_task() {
    use crate::infra::storage::Storage;
    use crate::worktree::leases::{NewLease, ensure_schema, insert_lease_scoped};

    let root = Scratch::new("task-groups");
    let storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    ensure_schema(&storage).unwrap();
    let first = task_binding(&storage, "7", "https://forgejo.test/one");
    let second = task_binding(&storage, "7", "https://forgejo.test/two");
    fn lease<'a>(id: &'a str, path: &'a str) -> NewLease<'a> {
        NewLease {
            lease_id: id,
            identity: "/fixture/.git",
            issue: 7,
            session: id,
            checkout_path: "/fixture",
            worktree_path: path,
            branch: "branch",
            status: "active",
            created_at: 1,
            heartbeat_at: 1,
        }
    }
    let one_path = root.path().join("one");
    let two_path = root.path().join("two");
    std::fs::create_dir_all(&one_path).unwrap();
    std::fs::create_dir_all(&two_path).unwrap();
    fn scope_of(binding: &crate::task_identity::TaskBinding) -> crate::worktree::LeaseScope {
        crate::worktree::LeaseScope::for_task(
            "forgejo",
            "owner/repo",
            7,
            binding.task_id(),
            binding.key().instance(),
        )
        .unwrap()
    }
    insert_lease_scoped(
        &storage,
        lease("one", one_path.to_str().unwrap()),
        &scope_of(&first),
        Some("forgejo.test/owner/repo"),
    )
    .unwrap();
    insert_lease_scoped(
        &storage,
        lease("two", two_path.to_str().unwrap()),
        &scope_of(&second),
        Some("forgejo.test/owner/repo"),
    )
    .unwrap();
    let rows = crate::worktree::leases::list_all_for_repo(&storage, "/fixture/.git").unwrap();
    let scopes = crate::worktree::lease_scope::scope_map_for_repo_with_key(
        &storage,
        "/fixture/.git",
        Some("forgejo.test/owner/repo"),
    )
    .unwrap();
    let groups = super::engine::lease_groups(7, &rows, &scopes);
    assert_eq!(
        groups.len(),
        2,
        "two same-number tasks on two instances are two groups"
    );
    let mut task_ids: Vec<i64> = groups.iter().filter_map(|group| group.task_id()).collect();
    task_ids.sort_unstable();
    assert_eq!(task_ids, vec![first.task_id(), second.task_id()]);
}

#[test]
fn a_same_number_task_is_not_legacy_ambiguity_for_its_own_group() {
    use crate::infra::storage::Storage;
    use crate::worktree::leases::{NewLease, ensure_schema, insert_lease_scoped};

    let root = Scratch::new("task-ambiguity");
    let storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    ensure_schema(&storage).unwrap();
    let first = task_binding(&storage, "7", "https://forgejo.test/one");
    let second = task_binding(&storage, "7", "https://forgejo.test/two");
    fn lease<'a>(id: &'a str, path: &'a str) -> NewLease<'a> {
        NewLease {
            lease_id: id,
            identity: "/fixture/.git",
            issue: 7,
            session: id,
            checkout_path: "/fixture",
            worktree_path: path,
            branch: "branch",
            status: "active",
            created_at: 1,
            heartbeat_at: 1,
        }
    }
    fn scope_of(binding: &crate::task_identity::TaskBinding) -> crate::worktree::LeaseScope {
        crate::worktree::LeaseScope::for_task(
            "forgejo",
            "owner/repo",
            7,
            binding.task_id(),
            binding.key().instance(),
        )
        .unwrap()
    }
    let one = root.path().join("one");
    let two = root.path().join("two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    insert_lease_scoped(
        &storage,
        lease("one", one.to_str().unwrap()),
        &scope_of(&first),
        Some("forgejo.test/owner/repo"),
    )
    .unwrap();
    insert_lease_scoped(
        &storage,
        lease("two", two.to_str().unwrap()),
        &scope_of(&second),
        Some("forgejo.test/owner/repo"),
    )
    .unwrap();
    let rows = crate::worktree::leases::list_all_for_repo(&storage, "/fixture/.git").unwrap();
    let scopes = crate::worktree::lease_scope::scope_map_for_repo_with_key(
        &storage,
        "/fixture/.git",
        Some("forgejo.test/owner/repo"),
    )
    .unwrap();
    let groups = super::engine::lease_groups(7, &rows, &scopes);
    for group in &groups {
        assert!(
            matches!(
                super::engine::scoped_processing(group, &rows, &rows, &scopes, None, true),
                super::engine::ScopedProcessing::Provider
            ),
            "a same-number task on another path is not legacy ambiguity"
        );
    }
}

#[test]
fn a_task_group_sharing_a_directory_with_a_legacy_row_keeps() {
    use crate::infra::storage::Storage;
    use crate::worktree::leases::{NewLease, ensure_schema, insert_lease_scoped};

    let root = Scratch::new("task-mixed");
    let storage = Storage::open_at(&root.path().join("leases.sqlite3")).unwrap();
    ensure_schema(&storage).unwrap();
    let binding = task_binding(&storage, "7", "https://forgejo.test/one");
    let shared = root.path().join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    fn lease<'a>(id: &'a str, issue: u64, path: &'a str, status: &'a str) -> NewLease<'a> {
        NewLease {
            lease_id: id,
            identity: "/fixture/.git",
            issue,
            session: id,
            checkout_path: "/fixture",
            worktree_path: path,
            branch: "branch",
            status,
            created_at: 1,
            heartbeat_at: 1,
        }
    }
    let shared_path = shared.to_str().unwrap();
    let scope = crate::worktree::LeaseScope::for_task(
        "forgejo",
        "owner/repo",
        7,
        binding.task_id(),
        binding.key().instance(),
    )
    .unwrap();
    insert_lease_scoped(
        &storage,
        lease("task-seven", 7, shared_path, "active"),
        &scope,
        Some("forgejo.test/owner/repo"),
    )
    .unwrap();
    crate::worktree::leases::insert_lease(
        &storage,
        lease("legacy-eight", 8, shared_path, "retained"),
    )
    .unwrap();
    let rows = crate::worktree::leases::list_all_for_repo(&storage, "/fixture/.git").unwrap();
    let scopes = crate::worktree::lease_scope::scope_map_for_repo_with_key(
        &storage,
        "/fixture/.git",
        Some("forgejo.test/owner/repo"),
    )
    .unwrap();
    let groups = super::engine::lease_groups(7, &rows, &scopes);
    assert_eq!(groups.len(), 1);
    assert!(
        matches!(
            super::engine::scoped_processing(&groups[0], &rows, &rows, &scopes, None, true),
            super::engine::ScopedProcessing::Keep(_)
        ),
        "a task lease sharing a directory with legacy ownership keeps"
    );
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
