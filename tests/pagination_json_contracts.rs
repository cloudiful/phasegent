use serde_json::Value;
#[path = "support/mod.rs"]
mod support;

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".scratch")
            .join(format!(
                "pagination-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }
    fn run(&self, role: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_phasegent"))
            .args(args)
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("PHASEGENT_DB_PATH", self.root.join("db.sqlite3"))
            .env("PHASEGENT_LOCAL_DB_PATH", self.root.join("local.sqlite3"))
            .env("PHASEGENT_INDEX_DB_PATH", self.root.join("index.sqlite3"))
            .env("PHASEGENT_CONFIG_PATH", self.root.join("missing.toml"))
            .env("PHASEGENT_ROLE", role)
            .env_remove("PHASEGENT_SESSION_ID")
            .env_remove("OPENCODE_SESSION_ID")
            .env_remove("PHASEGENT_PROVIDER")
            .env_remove("PHASEGENT_DEFAULT_PROVIDER")
            .env_remove("CI")
            .env_remove("CONTINUOUS_INTEGRATION")
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
    fn json(&self, role: &str, args: &[&str]) -> (Value, String) {
        let output = self.run(role, args);
        assert!(
            output.status.success(),
            "{:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        (
            serde_json::from_slice(&output.stdout).unwrap(),
            String::from_utf8(output.stderr).unwrap(),
        )
    }
    fn seed(&self) {
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(&self.root)
                .status()
                .unwrap()
                .success()
        );
        self.json("executor", &["worktree", "status", "--issue", "42"]);
        let connection = rusqlite::Connection::open(self.root.join("db.sqlite3")).unwrap();
        let identity = self
            .root
            .join(".git")
            .canonicalize()
            .unwrap()
            .display()
            .to_string();
        for n in 0..305 {
            connection.execute("INSERT INTO worktree_leases (lease_id,repo_identity,issue,session,checkout_path,worktree_path,branch,status,created_at,heartbeat_at) VALUES (?1,?2,?3,'fixture',?4,?5,'fixture-branch',?6,1,1)",
                rusqlite::params![format!("lease{n:03}"), identity, if n < 300 {42} else {99}, self.root.display().to_string(), self.root.join(format!("wt{n}")).display().to_string(), if n < 300 {"active"} else {"retained"}]).unwrap();
        }
        for n in 0..205 {
            connection.execute("INSERT INTO execution_timer_runs (run_id,issue_id,phase,role,attempt,started_at,status) VALUES (?1,42,'P1','executor',1,1,?2)",
                rusqlite::params![format!("run{n:03}"), if n < 200 {"running"} else {"DONE"}]).unwrap();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn keys(value: &Value) -> Vec<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

#[test]
fn default_native_collection_calls_return_only_the_first_fifty_items() {
    let fixture = Fixture::new();
    fixture.seed();
    for (role, args, total) in [
        ("executor", vec!["worktree", "list", "--no-sync"], 305),
        ("executor", vec!["worktree", "status", "--issue", "42"], 300),
        ("orchestrator", vec!["timer", "list"], 205),
    ] {
        let (value, warning) = fixture.json(role, &args);
        assert!(warning.is_empty(), "{warning}");
        assert_eq!(value["page"], 1);
        assert_eq!(value["limit"], 50);
        assert_eq!(value["total_count"], total);
        assert_eq!(value["items"].as_array().unwrap().len(), 50);
        assert_eq!(value["has_more"], true);
        assert!(value.get("leases").is_none() && value.get("runs").is_none());
    }
    let (empty, warning) = fixture.json("executor", &["worktree", "status", "--issue", "99"]);
    assert_eq!(empty["items"], serde_json::json!([]));
    assert_eq!(empty["total_count"], 0);
    assert_eq!(empty["has_more"], false);
    assert!(warning.is_empty());
    let invalid = fixture.run("orchestrator", &["timer", "list", "--limit", "1000"]);
    assert!(!invalid.status.success());
    assert!(invalid.stdout.is_empty());
}

#[test]
fn native_pages_have_complete_metadata_unique_tie_order_and_no_cap() {
    let fixture = Fixture::new();
    fixture.seed();
    for (role, base, count, id) in [
        (
            "executor",
            vec!["worktree", "list", "--no-sync"],
            305,
            "lease_id",
        ),
        (
            "executor",
            vec!["worktree", "status", "--issue", "42"],
            300,
            "lease_id",
        ),
        ("orchestrator", vec!["timer", "list"], 205, "run_id"),
    ] {
        let mut all = Vec::new();
        for page in 1..=4 {
            let page_arg = page.to_string();
            let mut args = base.clone();
            args.extend(["--page", &page_arg, "--limit", "100"]);
            let (value, warning) = fixture.json(role, &args);
            assert!(warning.is_empty(), "{warning}");
            assert_eq!(value["page"], page);
            assert_eq!(value["limit"], 100);
            assert_eq!(value["total_count"], count);
            assert_eq!(value["has_more"], page * 100 < count);
            let expected_keys = if base[0] == "timer" {
                vec!["has_more", "items", "limit", "page", "total_count"]
            } else if base[1] == "status" {
                vec!["has_more", "issue", "items", "limit", "page", "total_count"]
            } else {
                vec![
                    "has_more",
                    "items",
                    "limit",
                    "page",
                    "repo_identity",
                    "total_count",
                ]
            };
            assert_eq!(keys(&value), expected_keys);
            all.extend(
                value["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|row| row[id].as_str().unwrap().to_owned()),
            );
        }
        assert_eq!(all.len(), count as usize);
        assert!(all.windows(2).all(|pair| pair[0] < pair[1]));
    }
    let (page, _) = fixture.json(
        "executor",
        &["worktree", "status", "--issue", "42", "--limit", "2"],
    );
    assert_eq!(page["page"], 1);
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let (page, _) = fixture.json("orchestrator", &["timer", "list", "--page", "1"]);
    assert_eq!(page["limit"], 50);
    let (page, _) = fixture.json(
        "orchestrator",
        &["timer", "list", "--status", "finished", "--page", "1"],
    );
    assert_eq!(page["total_count"], 5);
    assert_eq!(page["items"].as_array().unwrap().len(), 5);
}

#[test]
fn invalid_pages_fail_before_database_or_git_access() {
    let fixture = Fixture::new();
    for base in [
        vec!["worktree", "list"],
        vec!["worktree", "status", "--issue", "42"],
        vec!["timer", "list"],
    ] {
        for suffix in [
            vec!["--page", "0"],
            vec!["--page=-1"],
            vec!["--page", "999999999999999999999999999999999"],
            vec!["--page", "1", "--limit", "0"],
            vec!["--page", "1", "--limit", "101"],
            vec!["--page", "18446744073709551615", "--limit", "100"],
        ] {
            let mut args = base.clone();
            args.extend(suffix);
            let output = fixture.run("orchestrator", &args);
            assert!(!output.status.success(), "{args:?}");
            assert!(output.stdout.is_empty());
            assert!(
                !fixture.root.join("db.sqlite3").exists(),
                "invalid {args:?} accessed storage"
            );
        }
    }
}

#[test]
fn local_catalog_comment_and_record_lists_use_shared_page_envelopes() {
    let fixture = Fixture::new();
    let (initial_projects, _) =
        fixture.json("executor", &["--provider", "local", "project", "list"]);
    assert_eq!(initial_projects["items"].as_array().unwrap().len(), 1);
    let connection = rusqlite::Connection::open(fixture.root.join("local.sqlite3")).unwrap();
    for n in 0..7 {
        connection
            .execute(
                "INSERT INTO local_projects (project,created_at) VALUES (?1,1)",
                [format!("project{n}")],
            )
            .unwrap();
    }
    connection.execute("INSERT INTO local_issues (id,title,status,created_at,updated_at) VALUES (42,'Fixture','New',1,1)", []).unwrap();
    for n in 0..6 {
        connection.execute("INSERT INTO local_comments (issue_id,role,marker,body,created_at) VALUES (42,'executor',?1,?2,?3)", rusqlite::params![format!("key{n}"), format!("body{n}"), n + 1]).unwrap();
    }
    let (projects, _) = fixture.json("executor", &["--provider", "local", "project", "list"]);
    assert_eq!(projects["page"], 1);
    assert_eq!(projects["limit"], 50);
    assert_eq!(projects["total_count"], 8);
    assert_eq!(projects["items"].as_array().unwrap().len(), 8);
    let (page, _) = fixture.json(
        "executor",
        &[
            "--provider",
            "local",
            "project",
            "list",
            "--page",
            "2",
            "--limit",
            "3",
        ],
    );
    assert_eq!(page["total_count"], 8);
    assert_eq!(page["items"].as_array().unwrap().len(), 3);
    assert_eq!(page["has_more"], true);
    let (versions, _) = fixture.json("executor", &["--provider", "local", "version", "list"]);
    assert_eq!(
        keys(&versions),
        ["has_more", "items", "limit", "page", "total_count"]
    );
    assert_eq!(versions["page"], 1);
    assert_eq!(versions["limit"], 50);
    assert_eq!(versions["total_count"], 0);
    assert_eq!(versions["items"], serde_json::json!([]));
    let (comments, _) = fixture.json(
        "executor",
        &["--provider", "local", "comment", "list", "42"],
    );
    assert_eq!(comments["page"], 1);
    assert_eq!(comments["limit"], 50);
    assert_eq!(comments["issue"], 42);
    assert_eq!(comments["total_count"], 6);
    assert_eq!(comments["items"].as_array().unwrap().len(), 6);
    let (page, _) = fixture.json(
        "executor",
        &[
            "--provider",
            "local",
            "comment",
            "list",
            "42",
            "--page",
            "2",
            "--limit",
            "2",
        ],
    );
    assert_eq!(page["total_count"], 6);
    assert_eq!(page["items"][0]["body"], "body2");
    let (records, _) = fixture.json("executor", &["--provider", "local", "record", "list", "42"]);
    assert_eq!(records["page"], 1);
    assert_eq!(records["limit"], 50);
    assert_eq!(records["issue"], 42);
    assert_eq!(records["total_count"], 0);
    assert_eq!(records["items"], serde_json::json!([]));
    let (page, _) = fixture.json(
        "executor",
        &[
            "--provider",
            "local",
            "record",
            "list",
            "42",
            "--page",
            "2",
            "--limit",
            "2",
            "--kind",
            "executor",
        ],
    );
    assert_eq!(page["total_count"], 0);
    assert_eq!(page["items"], serde_json::json!([]));
    assert_eq!(page["has_more"], false);
    let default_relation = fixture.run(
        "executor",
        &["--provider", "local", "relation", "list", "42"],
    );
    let explicit_relation = fixture.run(
        "executor",
        &[
            "--provider",
            "local",
            "relation",
            "list",
            "42",
            "--page",
            "1",
        ],
    );
    assert_eq!(
        default_relation.status.code(),
        explicit_relation.status.code()
    );
    assert!(!default_relation.status.success());
    assert_eq!(default_relation.stderr, explicit_relation.stderr);
}

#[test]
fn branch_pages_keep_full_scope_ambiguity_beyond_visible_rows() {
    let fixture = Fixture::new();
    fixture.seed();
    let (default, _) = fixture.json("executor", &["issue", "branches", "42"]);
    assert_eq!(
        keys(&default),
        [
            "ambiguous",
            "has_more",
            "issue",
            "items",
            "limit",
            "local_only",
            "page",
            "repo_key",
            "total_count"
        ]
    );
    assert_eq!(default["page"], 1);
    assert_eq!(default["limit"], 50);
    assert_eq!(default["total_count"], 0);
    let connection = rusqlite::Connection::open(fixture.root.join("db.sqlite3")).unwrap();
    for n in 0..180 {
        connection.execute("INSERT INTO branch_issue_links (repo_key,branch,provider,project,external_id,issue_number,status,source,created_at,updated_at) VALUES (?1,?2,'redmine',?3,'42',42,'linked','fixture',1,1)",
            rusqlite::params![default["repo_key"].as_str().unwrap(), format!("branch{n:03}"), if n < 179 {"aaa"} else {"zzz"}]).unwrap();
    }
    let mut all = Vec::new();
    for n in 1..=5 {
        let page_number = n.to_string();
        let (page, _) = fixture.json(
            "executor",
            &[
                "issue",
                "branches",
                "42",
                "--page",
                &page_number,
                "--limit",
                "50",
            ],
        );
        assert_eq!(
            keys(&page),
            [
                "ambiguous",
                "has_more",
                "issue",
                "items",
                "limit",
                "local_only",
                "page",
                "repo_key",
                "total_count"
            ]
        );
        assert_eq!(page["total_count"], 180);
        assert_eq!(page["ambiguous"], true);
        assert_eq!(page["repo_key"], default["repo_key"]);
        assert_eq!(page["has_more"], n < 4);
        if n == 1 {
            assert!(
                page["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|item| item["project"] == "aaa")
            );
        }
        all.extend(
            page["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| item["branch"].as_str().unwrap().to_owned()),
        );
    }
    assert_eq!(all.len(), 180);
    assert!(all.windows(2).all(|pair| pair[0] < pair[1]));
}

#[test]
fn remaining_page_parsers_reject_invalid_input_before_opening_stores() {
    let fixture = Fixture::new();
    for args in [
        vec!["project", "list"],
        vec!["version", "list"],
        vec!["comment", "list", "42"],
        vec!["record", "list", "42"],
        vec!["relation", "list", "42"],
        vec!["issue", "branches", "42"],
    ] {
        for suffix in [
            vec!["--page", "0"],
            vec!["--limit", "101"],
            vec!["--page=18446744073709551615", "--limit=100"],
        ] {
            let mut arguments = args.clone();
            arguments.extend(suffix);
            let output = fixture.run("executor", &arguments);
            assert!(!output.status.success(), "{arguments:?}");
            assert!(output.stdout.is_empty());
            assert!(!fixture.root.join("db.sqlite3").exists());
            assert!(!fixture.root.join("local.sqlite3").exists());
        }
    }
}

#[test]
fn local_cli_matrix_covers_default_limit_only_and_out_of_range_pages() {
    let fixture = Fixture::new();
    fixture.seed();
    fixture.json("executor", &["--provider", "local", "project", "list"]);
    let connection = rusqlite::Connection::open(fixture.root.join("local.sqlite3")).unwrap();
    for n in 0..60 {
        connection
            .execute(
                "INSERT INTO local_projects (project,created_at) VALUES (?1,?2)",
                rusqlite::params![format!("project{n:03}"), n + 1],
            )
            .unwrap();
    }
    connection.execute("INSERT INTO local_issues (id,title,status,created_at,updated_at) VALUES (42,'Fixture','New',1,1)", []).unwrap();
    for n in 0..60 {
        let key = format!("record-{n:03}");
        let header = serde_json::json!({"v":1,"kind":"executor","actor":"executor","key":key,"phase":"P1","attempt":1});
        let body = format!("<!-- phasegent-record {header} -->\nbody{n}");
        connection.execute("INSERT INTO local_comments (issue_id,role,phase,attempt,marker,body,created_at) VALUES (42,'executor','P1',1,?1,?2,?3)", rusqlite::params![key, body, n + 1]).unwrap();
    }
    for (role, args, total) in [
        ("executor", vec!["worktree", "list", "--no-sync"], 305),
        ("executor", vec!["worktree", "status", "--issue", "42"], 300),
        ("orchestrator", vec!["timer", "list"], 205),
        (
            "executor",
            vec!["--provider", "local", "project", "list"],
            61,
        ),
        (
            "executor",
            vec!["--provider", "local", "version", "list"],
            0,
        ),
        (
            "executor",
            vec!["--provider", "local", "comment", "list", "42"],
            60,
        ),
        (
            "executor",
            vec![
                "--provider",
                "local",
                "record",
                "list",
                "42",
                "--phase",
                "P1",
            ],
            60,
        ),
        ("executor", vec!["issue", "branches", "42"], 0),
    ] {
        let (default, warning) = fixture.json(role, &args);
        assert!(warning.is_empty(), "{args:?}: {warning}");
        assert_eq!(default["page"], 1, "{args:?}");
        assert_eq!(default["limit"], 50, "{args:?}");
        assert_eq!(default["total_count"], total, "{args:?}");
        assert_eq!(
            default["items"].as_array().unwrap().len(),
            total.min(50) as usize,
            "{args:?}"
        );
        assert_eq!(default["has_more"], total > 50, "{args:?}");
        if args[0] == "worktree" && args[1] == "list" {
            assert!(default["repo_identity"].is_string());
        }
        if (args[0] == "worktree" && args[1] == "status")
            || (args[0] == "--provider" && (args[2] == "comment" || args[2] == "record"))
            || args[0] == "issue"
        {
            assert_eq!(default["issue"], 42, "{args:?}");
        }
        let mut limited_args = args.clone();
        limited_args.extend(["--limit", "1"]);
        let (limited, _) = fixture.json(role, &limited_args);
        assert_eq!(limited["page"], 1, "{args:?}");
        assert_eq!(limited["limit"], 1, "{args:?}");
        assert_eq!(limited["total_count"], total, "{args:?}");
        assert_eq!(
            limited["items"].as_array().unwrap().len(),
            usize::from(total > 0)
        );
        let mut second_args = args.clone();
        second_args.extend(["--page", "2", "--limit", "1"]);
        let (second, _) = fixture.json(role, &second_args);
        assert_eq!(second["page"], 2, "{args:?}");
        assert_eq!(second["limit"], 1, "{args:?}");
        assert_eq!(
            second["items"].as_array().unwrap().len(),
            usize::from(total > 1)
        );
        let mut out_of_range_args = args.clone();
        out_of_range_args.extend(["--page", "9999", "--limit", "1"]);
        let (out_of_range, warning) = fixture.json(role, &out_of_range_args);
        assert!(warning.is_empty(), "{warning}");
        assert_eq!(out_of_range["items"], serde_json::json!([]));
        assert_eq!(out_of_range["total_count"], total);
        assert_eq!(out_of_range["has_more"], false);
    }
}

#[test]
fn redmine_cli_matrix_pages_defaults_without_full_native_traversal() {
    let fixture = Fixture::new();
    fixture.seed();
    let connection = rusqlite::Connection::open(fixture.root.join("db.sqlite3")).unwrap();
    connection.execute("INSERT INTO role_credential (role,provider,credential) VALUES ('executor','redmine','fixture-only-key')", []).unwrap();
    let project = serde_json::json!({"id": 1, "name": "One", "identifier": "one"});
    let version = serde_json::json!({"id": 1, "name": "One", "status": "open"});
    let relation =
        serde_json::json!({"id": 1, "issue_id": 42, "issue_to_id": 99, "relation_type": "relates"});
    for (group, collection, entry, issue) in [
        ("project", "projects", project, false),
        ("version", "versions", version, false),
        ("relation", "relations", relation, true),
    ] {
        for suffix in [
            vec![],
            vec!["--limit", "1"],
            vec!["--page", "1", "--limit", "1"],
            vec!["--page", "9999", "--limit", "1"],
        ] {
            let out_of_range = suffix.contains(&"9999");
            let limit = if suffix.is_empty() { 50 } else { 1 };
            let entry_count = if group == "relation" {
                120
            } else if out_of_range {
                0
            } else {
                limit
            };
            let entries: Vec<serde_json::Value> = (0..entry_count)
                .map(|index| {
                    let mut item = entry.clone();
                    item["id"] = serde_json::json!(index + 1);
                    match group {
                        "project" => {
                            item["name"] = serde_json::json!(format!("Project {index}"));
                            item["identifier"] = serde_json::json!(format!("project-{index}"));
                        }
                        "version" => {
                            item["name"] = serde_json::json!(format!("Version {index}"));
                        }
                        "relation" => {
                            item["issue_id"] = serde_json::json!(42);
                            item["issue_to_id"] = serde_json::json!(index + 100);
                        }
                        _ => unreachable!(),
                    }
                    item
                })
                .collect();
            let response =
                serde_json::json!({collection: entries, "total_count": 120, "limit": limit})
                    .to_string();
            let server = support::start_mock_server(vec![support::MockResponse::ok(response)]);
            let mut args = vec![
                "--provider",
                "redmine",
                "--api-base",
                server.base_url.as_str(),
                "--project-id",
                "42",
                group,
                "list",
            ];
            if issue {
                args.push("42");
            }
            args.extend(suffix.clone());
            let (result, _) = fixture.json("executor", &args);
            assert_eq!(result["page"], if out_of_range { 9999 } else { 1 });
            assert_eq!(result["limit"], limit);
            assert_eq!(result["total_count"], 120);
            assert_eq!(result["has_more"], !out_of_range);
            assert_eq!(
                result["items"].as_array().unwrap().len(),
                if out_of_range { 0 } else { limit }
            );
            if issue {
                assert_eq!(result["issue"], 42);
            }
            let requests = server.requests();
            assert_eq!(requests.len(), 1, "{group}: {requests:?}");
            if group == "relation" {
                assert!(!requests[0].contains("offset="));
            } else {
                assert!(requests[0].contains("offset="));
            }
        }
    }
}

#[test]
fn redmine_comment_and_record_defaults_slice_complete_journals_without_page_fetches() {
    let fixture = Fixture::new();
    fixture.seed();
    let connection = rusqlite::Connection::open(fixture.root.join("db.sqlite3")).unwrap();
    connection.execute("INSERT INTO role_credential (role,provider,credential) VALUES ('executor','redmine','fixture-only-key')", []).unwrap();

    let comments: Vec<_> = (0..120)
        .map(|index| serde_json::json!({"id": index + 1, "notes": format!("comment-{index}")}))
        .collect();
    let response = serde_json::json!({"issue": {"id": 42, "subject": "Fixture", "description": "", "project": {"id": 42, "name": "Fixture", "identifier": "fixture"}, "journals": comments}}).to_string();
    let server = support::start_mock_server(vec![
        support::MockResponse::ok(response.clone()),
        support::MockResponse::ok(response),
    ]);
    let (comments, _) = fixture.json(
        "executor",
        &[
            "--provider",
            "redmine",
            "--api-base",
            &server.base_url,
            "--project-id",
            "42",
            "comment",
            "list",
            "42",
        ],
    );
    assert_eq!(comments["page"], 1);
    assert_eq!(comments["limit"], 50);
    assert_eq!(comments["issue"], 42);
    assert_eq!(comments["total_count"], 120);
    assert_eq!(comments["items"].as_array().unwrap().len(), 50);
    assert_eq!(comments["has_more"], true);
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.contains("include=journals"))
    );

    let records: Vec<_> = (0..120)
        .map(|index| {
            let key = format!("remote-{index:03}");
            let header = serde_json::json!({"v":1,"kind":"executor","actor":"executor","key":key,"phase":"P1","attempt":1});
            serde_json::json!({"id": index + 1, "notes": format!("<!-- phasegent-record {header} -->\nbody-{index}")})
        })
        .collect();
    let response = serde_json::json!({"issue": {"id": 42, "subject": "Fixture", "description": "", "project": {"id": 42, "name": "Fixture", "identifier": "fixture"}, "journals": records}}).to_string();
    let server = support::start_mock_server(vec![support::MockResponse::ok(response)]);
    let (records, _) = fixture.json(
        "executor",
        &[
            "--provider",
            "redmine",
            "--api-base",
            &server.base_url,
            "--project-id",
            "42",
            "record",
            "list",
            "42",
        ],
    );
    assert_eq!(records["page"], 1);
    assert_eq!(records["limit"], 50);
    assert_eq!(records["issue"], 42);
    assert_eq!(records["total_count"], 120);
    assert_eq!(records["items"].as_array().unwrap().len(), 50);
    assert_eq!(records["items"][49]["key"], "remote-049");
    assert_eq!(records["has_more"], true);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests
            .iter()
            .all(|request| request.contains("include=journals"))
    );
}

#[test]
fn human_pages_render_items_scope_and_correct_out_of_range_state() {
    let fixture = Fixture::new();
    fixture.seed();
    let default = fixture.run(
        "executor",
        &["--output", "text", "worktree", "status", "--issue", "42"],
    );
    assert!(default.status.success());
    let default_text = String::from_utf8(default.stdout).unwrap();
    assert!(
        default_text.contains("Page 1, limit 50, total 300, more"),
        "{default_text}"
    );
    assert!(default_text.contains("lease049") && !default_text.contains("lease050"));
    let output = fixture.run(
        "executor",
        &[
            "--output", "text", "worktree", "status", "--issue", "42", "--page", "1", "--limit",
            "2",
        ],
    );
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("Page 1, limit 2, total 300, more"), "{text}");
    assert!(text.contains("lease000") && text.contains("lease001") && text.contains("#42"));
    let output = fixture.run(
        "executor",
        &[
            "--output", "text", "worktree", "status", "--issue", "42", "--page", "999",
        ],
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("No leases on this page."));
    assert!(!text.contains("No worktree leases are recorded"));
}
