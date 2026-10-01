//! Skill consistency tests.
//!
//! Pins the shipped `skills/phasegent` skills against the code and the protocol
//! contract: the SKILL frontmatter identity, the reviewer VERDICT vocabulary,
//! the role capability table's agreement with `src/policy.rs`, the
//! single-file self-contained shape, and the issue #602 split between the
//! shared protocol in `SKILL.md` and the always-on role boundaries in
//! `SKILL.<role>.md`. Issue 665 adds `SKILL.explore.md` to that role set and
//! asserts every role skill is embedded by the generated adapter. Issue 669
//! pins the default explorer-session reuse protocol: one retained `sessionID`
//! handle per parent session, incremental deltas, and a recorded isolation
//! reason for the limited fresh-child triggers. Issue 671 extends that into a
//! bounded nested-explorer level: executor/reviewer may launch only `explore`,
//! the explorer cannot recurse and stays non-audited, reuse is parent-scoped,
//! and orchestrator ownership plus the independent verdict contract stand.
//! Issue 679 adds the `tester` role skill: it embeds and binds like the other
//! protocol roles, owns a test-only write boundary and independent verification
//! duty, and reuses the shared status vocabulary plus explicit test-result
//! evidence without inventing a verdict token. Issue 679 P3 adds the risk-based
//! review policy: one risk class per phase with a `reviewer_policy`
//! (`final-only` by default, `checkpoint-and-final` only for a `high-risk` or
//! `irreversible` phase whose plan names the checkpoint), an executor test
//! disposition, compact evidence, and serial-by-default bounded parallelism that
//! never authorizes overlapping write owners or a mutable shared tree.
//! Issue 692 replaces the issue-bound phasegent explorer backend with the
//! generic research backend and documents the native `explore` child as a
//! separate native capability, so no shipped skill reintroduces the removed
//! `explorer_*` tools or an issue/worktree lease prerequisite.
//! Pure filesystem + policy reads; no network, credentials, HOME, or SQLite
//! access.

use crate::policy::{Capability, Role};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

fn skill_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills/phasegent")
}

fn read_skill(relative: &str) -> String {
    let path = skill_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "expected skill file {} to be readable: {err}",
            path.display()
        )
    })
}

#[test]
fn skill_file_exists_with_phasegent_frontmatter() {
    let skill = read_skill("SKILL.md");
    // The frontmatter is the YAML block wrapped by leading `---` marker lines.
    let frontmatter: String = skill
        .lines()
        .skip_while(|line| line.trim() != "---")
        .skip(1)
        .take_while(|line| line.trim() != "---")
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !frontmatter.is_empty(),
        "SKILL.md must open with a `---` frontmatter block"
    );
    assert!(
        frontmatter
            .lines()
            .any(|line| line.trim() == "name: phasegent"),
        "frontmatter must carry name: phasegent\n---\n{frontmatter}\n---"
    );
    // opencode parses the frontmatter as YAML and drops the skill when it
    // fails: an unquoted plain scalar must not contain a bare `: ` sequence.
    let description_line = frontmatter
        .lines()
        .find(|line| line.trim_start().starts_with("description:"))
        .unwrap_or_else(|| panic!("frontmatter must carry a description\n---\n{frontmatter}\n---"));
    let value = description_line
        .trim_start()
        .strip_prefix("description:")
        .unwrap_or_default();
    assert!(
        !value.contains(": "),
        "description must stay a valid YAML plain scalar (no bare `: `)\n{description_line}"
    );
}

#[test]
fn verdict_vocabulary_matches_reviewer_contract() {
    let skill = read_skill("SKILL.md");
    // The verdict line is a hard protocol boundary; assert the exact tokens and
    // the single-token selection rule. The rule sentence wraps across source
    // lines, so compare against a whitespace-normalised form.
    assert!(
        skill.contains("`PASS` · `FAIL` · `REQUEST_CHANGES` · `BLOCKED` · `AUDIT_FAILED`"),
        "SKILL.md must delimit the five-token reviewer VERDICT vocabulary"
    );
    let normalised: String = skill.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalised.contains(
            "Use exactly one of these five case-sensitive tokens on the note's `VERDICT:` line"
        ),
        "SKILL.md must state the single-token selection rule"
    );
}

/// The role matrix section in `SKILL.md` is the human-readable mirror of
/// `src/policy.rs`. Assert every capability row against `Role::allows` so a
/// drift in either direction surfaces as a test failure, and confirm the
/// row/operation counts agree between the table and the policy enum.
#[test]
fn role_table_agrees_with_policy() {
    let roles = read_skill("SKILL.md");
    let lines: Vec<&str> = roles.lines().collect();
    let header_idx = lines
        .iter()
        .position(|line| line.starts_with("| Capability |"))
        .expect("SKILL.md must carry the Capability header row");
    let header: Vec<String> = lines[header_idx]
        .split('|')
        .map(str::trim)
        .map(str::to_owned)
        .collect();
    let column = |name: &str| -> usize {
        header
            .iter()
            .position(|cell| cell == name)
            .unwrap_or_else(|| panic!("SKILL.md must include the {name} column"))
    };
    let admin_col = column("admin");
    let orchestrator_col = column("orchestrator");
    let executor_col = column("executor");
    let reviewer_col = column("reviewer");
    let tester_col = column("tester");
    let operation_col = column("Operation");

    let parse_cell = |cell: &str| match cell.trim() {
        "✓" => true,
        "—" => false,
        other => panic!("unexpected capability cell {other:?} in SKILL.md"),
    };

    // (capability, operation, admin, orchestrator, executor, reviewer, tester)
    let mut table: HashMap<String, (String, bool, bool, bool, bool, bool)> = HashMap::new();
    for line in &lines[header_idx + 1..] {
        let line = line.trim();
        if line.starts_with("##") {
            break;
        }
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        if cells.len() <= 1 || cells.iter().all(|cell| cell.is_empty()) {
            continue;
        }
        // Skip the header/separator row (all-dash cells) and any trailing empty.
        if cells.iter().all(|cell| {
            cell.is_empty() || (cell.chars().all(|ch| ch == '-') || cell.starts_with('-'))
        }) {
            continue;
        }
        let capability = cells
            .get(1)
            .map(|cell| cell.trim().to_owned())
            .unwrap_or_default();
        if capability.is_empty() {
            continue;
        }
        let operation = cells
            .get(operation_col)
            .map(|cell| cell.trim().to_owned())
            .unwrap_or_default();
        table.insert(
            capability.clone(),
            (
                operation,
                parse_cell(cells[admin_col]),
                parse_cell(cells[orchestrator_col]),
                parse_cell(cells[executor_col]),
                parse_cell(cells[reviewer_col]),
                parse_cell(cells[tester_col]),
            ),
        );
    }

    let all: &[(&str, Capability)] = &[
        ("IssueRead", Capability::IssueRead),
        ("IssueSearch", Capability::IssueSearch),
        ("IssueCreate", Capability::IssueCreate),
        ("IssueUpdateBody", Capability::IssueUpdateBody),
        ("IssueClose", Capability::IssueClose),
        ("IssueAttachmentUpload", Capability::IssueAttachmentUpload),
        ("RepoCreate", Capability::RepoCreate),
        ("CommentCreate", Capability::CommentCreate),
        ("CommentRead", Capability::CommentRead),
        ("CommentFindMarker", Capability::CommentFindMarker),
        ("Notify", Capability::Notify),
        ("ProjectRead", Capability::ProjectRead),
        ("ProjectCreate", Capability::ProjectCreate),
        ("IssueStatusRead", Capability::IssueStatusRead),
        ("VersionRead", Capability::VersionRead),
        ("RelationRead", Capability::RelationRead),
        ("RelationCreate", Capability::RelationCreate),
        ("RelationDelete", Capability::RelationDelete),
    ];

    assert_eq!(
        table.len(),
        all.len(),
        "SKILL.md and the policy enum must cover the same capability set"
    );

    for (name, capability) in all {
        let (operation, admin, orchestrator, executor, reviewer, tester) = table
            .get(*name)
            .unwrap_or_else(|| panic!("SKILL.md table must contain {name}"))
            .clone();
        assert_eq!(
            operation,
            capability.operation(),
            "operation mismatch for {name}"
        );
        assert_eq!(
            orchestrator,
            Role::Orchestrator.allows(*capability),
            "orchestrator row for {name}"
        );
        assert_eq!(
            admin,
            Role::Admin.allows(*capability),
            "admin row for {name}"
        );
        assert_eq!(
            executor,
            Role::Executor.allows(*capability),
            "executor row for {name}"
        );
        assert_eq!(
            reviewer,
            Role::Reviewer.allows(*capability),
            "reviewer row for {name}"
        );
        assert_eq!(
            tester,
            Role::Tester.allows(*capability),
            "tester row for {name}"
        );
    }
}

/// The issue 337 refactor removed `worktree release-stale` (folded into
/// `worktree prune --release-stale --reason TEXT`), the `worktree prune
/// --dry-run` flag, and `issue update-body` (renamed to `issue update`).
/// Shipped docs and the skill must never advertise the removed surface as an
/// available entry point; `--release-stale` itself remains a valid flag.
#[test]
fn docs_and_skill_do_not_advertise_removed_issue_337_surface() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut corpus = String::new();
    for relative in ["README.md", "README.zh-CN.md", "skills/phasegent/SKILL.md"] {
        let text = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|err| panic!("expected {relative} to be readable: {err}"));
        corpus.push_str(&text);
        corpus.push('\n');
    }
    for forbidden in [
        "worktree release-stale",
        "issue update-body",
        "--dry-run",
        "--apply",
    ] {
        assert!(
            !corpus.contains(forbidden),
            "removed issue 337 command surface {forbidden:?} must not appear in the shipped docs or skill"
        );
    }
}

/// Issue 337 Phase 4 synced the shipped READMEs with the provider-neutral
/// tracking vocabulary and the config-resolved provider default. The READMEs
/// must name the current tracking mode and the current `issue update` /
/// `worktree prune` surface, and must not hard-code the legacy tracking alias
/// or a single default provider.
#[test]
fn readmes_carry_provider_neutral_tracking_and_current_surface() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    for relative in ["README.md", "README.zh-CN.md"] {
        let text = fs::read_to_string(root.join(relative))
            .unwrap_or_else(|err| panic!("expected {relative} to be readable: {err}"));
        assert!(
            text.contains("TRACKED_ISSUE"),
            "{relative} must name the current TRACKED_ISSUE tracking mode"
        );
        assert!(
            !text.contains("REDMINE_ISSUE"),
            "{relative} must not advertise the legacy REDMINE_ISSUE alias"
        );
        assert!(
            text.contains("issue update") && text.contains("worktree prune"),
            "{relative} must document the current issue update and worktree prune surface"
        );
        assert!(
            text.contains("--provider") && text.contains("Forgejo"),
            "{relative} must document that the provider is resolved from configuration with a Forgejo fallback"
        );
    }
}

/// The merged skill ships as one self-contained file: `skills/` holds exactly
/// the `phasegent` skill directory, and the SKILL must not link to a separate
/// reference path that would be a dead link.
#[test]
fn skill_is_a_single_self_contained_file() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut entries: Vec<String> = fs::read_dir(root.join("skills"))
        .expect("skills/ must be readable")
        .map(|entry| {
            entry
                .expect("skills/ entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        vec!["phasegent".to_owned()],
        "skills/ must contain exactly the phasegent skill directory"
    );
    let skill = read_skill("SKILL.md");
    assert!(
        !skill.contains("references/"),
        "SKILL.md must not link to a separate reference file"
    );
}

/// Issue 665 adds the `explore` recon skill and issue 679 adds the `tester`
/// verification skill: every `SKILL.<role>.md` in the skill directory must be
/// embedded by the generated adapter under its `phasegent-<role>` id, so a new
/// role prompt cannot ship without its binding.
#[test]
fn every_role_skill_ships_embedded_in_the_adapter() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut role_files: Vec<String> = fs::read_dir(root.join("skills/phasegent"))
        .expect("skills/phasegent must be readable")
        .map(|entry| {
            entry
                .expect("skill entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .filter(|name| name.starts_with("SKILL.") && name.ends_with(".md") && name != "SKILL.md")
        .collect();
    role_files.sort();
    assert_eq!(
        role_files,
        vec![
            "SKILL.executor.md".to_owned(),
            "SKILL.explore.md".to_owned(),
            "SKILL.orchestrator.md".to_owned(),
            "SKILL.reviewer.md".to_owned(),
            "SKILL.tester.md".to_owned(),
        ],
        "the protocol role skills are the orchestrator/executor/reviewer/tester/explore set"
    );

    let adapter = fs::read_to_string(root.join("assets/opencode/phasegent-worktree.js"))
        .expect("the generated adapter must be readable");
    for file in &role_files {
        let role = file
            .trim_start_matches("SKILL.")
            .trim_end_matches(".md")
            .to_owned();
        assert!(
            adapter.contains(&format!("id: \"phasegent-{role}\"")),
            "the adapter must embed {file} as phasegent-{role}"
        );
    }
}

/// Issue 665 P2 restores the host-side workflow guidance under Phasegent
/// ownership: the orchestrator skill carries the explore-first recon delegation
/// and the delegation-role contract, and the explore skill keeps the
/// parent-request/applicable-AGENTS read rule.
#[test]
fn role_skills_own_recon_delegation_and_explore_read_rule() {
    let orchestrator = read_skill("SKILL.orchestrator.md");
    let normalised: String = orchestrator
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for phrase in [
        "## Recon delegation (explore-first)",
        "`task(explore)` first",
        "three or more expected greps or file opens",
        "at most two hops or two files per question",
        "reserve `general` for standalone work outside this workflow",
        "never infer a permission or a contract from another role",
        "## Git delivery",
        "## Branch binding & lease checklist",
    ] {
        assert!(
            normalised.contains(phrase),
            "SKILL.orchestrator.md must own the recon delegation guidance {phrase:?}"
        );
    }

    let explore = read_skill("SKILL.explore.md");
    assert!(
        explore.contains("Read the parent request and the applicable `AGENTS.md` files"),
        "SKILL.explore.md must keep the parent-request/applicable-AGENTS read rule"
    );
}

/// Issue 692 replaces the issue-bound phasegent explorer backend with the
/// generic research backend and documents the native `explore` child as a
/// separate native capability rather than an equivalent backend. The shared
/// skill must name the research lifecycle tools, state the phasegent backend
/// runs in a private scratch directory with no issue/worktree selection, keep
/// the native fallback honest, and never reintroduce the removed `explorer_*`
/// tool names or an issue/worktree lease prerequisite.
#[test]
fn skill_documents_generic_research_and_the_native_fallback() {
    let shared = read_skill("SKILL.md");
    let normalised: String = shared.split_whitespace().collect::<Vec<_>>().join(" ");
    for phrase in [
        "## Research delegation backends",
        "`research_start`",
        "`research_wait`",
        "`research_status`",
        "`research_cancel`",
        "`research_resume`",
        "fixed read-only research system instruction it owns",
        "private server-created scratch directory that never holds the phasegent repository or a resolved worktree",
        "separate native capability, not an equivalent phasegent backend",
        "never calls the phasegent MCP research tools or any phasegent issue/worktree binding",
    ] {
        assert!(
            normalised.contains(phrase),
            "SKILL.md must document the generic research contract {phrase:?}"
        );
    }
    // The removed issue-bound explorer contract must not reappear anywhere in
    // the shipped skill text.
    for removed in [
        "explorer_start",
        "explorer_wait",
        "explorer_status",
        "explorer_cancel",
        "explorer_resume",
        "active worktree lease for the selected issue",
        "the issue number is a selector",
    ] {
        assert!(
            !shared.contains(removed),
            "SKILL.md must not advertise the removed explorer contract {removed:?}"
        );
    }
}

/// Issue 669 makes explorer-session reuse the default: the orchestrator retains
/// the `sessionID` returned by its first `task(explore)` call for the rest of
/// the parent session and continues that one child with incremental deltas, and
/// it opens a fresh explore only on one of the approved isolation triggers whose
/// reason it records. The explore skill resumes accordingly, starts an isolated
/// child with a clean context, and keeps its read-only boundary, compact
/// evidence brief, and result contract.
#[test]
fn orchestrator_reuses_one_explore_session_and_explore_resumes_it() {
    let orchestrator = read_skill("SKILL.orchestrator.md");
    let normalised: String = orchestrator
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for phrase in [
        "Explorer sessions are reused by default",
        "retain that one handle for the rest of the parent session",
        "pass it back as the `sessionID` continuation on every later explore delegation",
        "send only the incremental ask",
        "A new topic is not by itself a reason for a fresh child",
        "Isolation triggers are the only reasons to open a fresh explore",
        "the parent request explicitly asks for an isolated, fresh, or independent context",
        "independent conflicting evidence requires a clean read",
        "the retained child's context is saturated or contaminated",
        "Record the trigger and the reason",
    ] {
        assert!(
            normalised.contains(phrase),
            "SKILL.orchestrator.md must own the explore-session reuse protocol {phrase:?}"
        );
    }
    // Only the approved isolation triggers: the plan never authorizes a
    // re-scope on its own to abandon the retained child.
    assert!(
        !normalised.contains("re-scope that invalidates"),
        "SKILL.orchestrator.md must not add an unapproved isolation trigger"
    );

    let explore = read_skill("SKILL.explore.md");
    let normalised: String = explore.split_whitespace().collect::<Vec<_>>().join(" ");
    for phrase in [
        "You are normally resumed, not replaced",
        "returns only incremental findings beyond the prior brief",
        "explicit isolation trigger",
        "the parent request asks for an isolated, fresh, or independent context",
        "independent conflicting evidence needs a clean read",
        "your context is saturated or contaminated",
        "records the reason",
        "An isolated child starts with a clean context",
        "it does not inherit the retained session's reads or evidence",
    ] {
        assert!(
            normalised.contains(phrase),
            "SKILL.explore.md must own the resumed-session behavior {phrase:?}"
        );
    }
    // The resumed-session contract changes the follow-up budget only; the
    // read-only boundary, the compact evidence brief, and the incremental
    // no-repetition rule all stay intact.
    for kept in [
        "You are read-only by tool block, not only by convention",
        "Return a concise evidence brief",
        "do not repeat already-covered facts",
    ] {
        assert!(
            normalised.contains(kept),
            "SKILL.explore.md must keep {kept:?} after the reuse change"
        );
    }
}

/// Issue 671 adds one bounded nested reconnaissance level: executor and
/// reviewer may launch only `explore`, the explorer cannot recurse, the
/// executor/reviewer stays the sole write and note owner, the reviewer keeps
/// its independent single-verdict contract, and the orchestrator remains the
/// only plan/status/timer/worktree/closure owner. It also pins the parent-scoped
/// reuse clarification in the orchestrator skill and the non-audited explorer
/// note ownership in the shared skill.
#[test]
fn executor_and_reviewer_may_launch_only_explore_without_weakening_ownership() {
    for (relative, round_scope, closure) in [
        (
            "SKILL.executor.md",
            "Reuse one explorer child for the whole phase",
            "You remain the only write owner for the phase and the sole publisher of its terminal note",
        ),
        (
            "SKILL.reviewer.md",
            "Reuse one explorer child for the whole round",
            "your terminal note and its single VERDICT remain yours alone",
        ),
    ] {
        let role = read_skill(relative);
        let normalised: String = role.split_whitespace().collect::<Vec<_>>().join(" ");
        for phrase in [
            "## Nested explorer assistance",
            "`explore` is the only nested child you may launch",
            "every other agent stays closed",
            "cannot recurse",
            "retain the `sessionID` returned by your first call",
            "only on the shared isolation triggers",
            "owns no audit note or VERDICT",
        ] {
            assert!(
                normalised.contains(phrase),
                "{relative} must own the nested-explore boundary {phrase:?}"
            );
        }
        assert!(
            normalised.contains(round_scope),
            "{relative} must scope nested reuse to its own {round_scope:?}"
        );
        assert!(
            normalised.contains(closure),
            "{relative} must keep its phase-terminal ownership {closure:?}"
        );
        // The allowlist is exactly one child: no sibling agent id may appear as
        // a launchable nested child.
        for sibling in [
            "`tester`",
            "`general`",
            "subagent=tester",
            "subagent=general",
        ] {
            assert!(
                !normalised.contains(sibling),
                "{relative} must not widen the nested allowlist with {sibling:?}"
            );
        }
    }

    let explore = read_skill("SKILL.explore.md");
    let normalised: String = explore.split_whitespace().collect::<Vec<_>>().join(" ");
    for phrase in [
        "the primary orchestrator, or the executor/reviewer that owns a phase or round",
        "a nested explorer cannot recurse and never invokes the `subagent` tool",
        "the parent retains the handle of the session that started this one",
        "scoped to the orchestrator's objective or to the executor/reviewer's phase or round",
        "You publish no audit note, marker, or VERDICT",
    ] {
        assert!(
            normalised.contains(phrase),
            "SKILL.explore.md must state {phrase:?}"
        );
    }

    let orchestrator = read_skill("SKILL.orchestrator.md");
    let normalised: String = orchestrator
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for phrase in [
        "Reuse is parent-scoped: the retained handle belongs to one parent session",
        "the phase or round of a nested executor/reviewer parent",
        "a nested explorer child follows the same reuse rule inside that parent's scope",
    ] {
        assert!(
            normalised.contains(phrase),
            "SKILL.orchestrator.md must scope explorer reuse {phrase:?}"
        );
    }

    let shared = read_skill("SKILL.md");
    let normalised: String = shared.split_whitespace().collect::<Vec<_>>().join(" ");
    for phrase in [
        "Nested explorer assistance changes no contract",
        "stays read-only, non-audited, and unable to recurse",
        "publishes no marker or VERDICT",
    ] {
        assert!(
            normalised.contains(phrase),
            "SKILL.md must own the nested-explorer result note {phrase:?}"
        );
    }
}

/// Issue 679 makes the `tester` role a first-class protocol role: its skill
/// carries the tester marker, the test-only write boundary and independent
/// verification duty, the no-production-write rules, and the shared status
/// vocabulary plus explicit test-result evidence instead of a new verdict token.
#[test]
fn tester_skill_owns_independent_verification_and_test_only_writes() {
    let tester = read_skill("SKILL.tester.md");
    let normalised: String = tester.split_whitespace().collect::<Vec<_>>().join(" ");

    assert!(
        tester.contains("<!-- ai-tester issue="),
        "SKILL.tester.md must carry the tester marker shape"
    );
    for phrase in [
        "independently verify",
        "Write only the test, fixture, and harness paths the orchestrator allowlists",
        "Never modify production code",
        "never weaken or delete a failing test",
        "never substitutes for your independent verification",
        "the exact commands run, the observed pass/fail outcome",
        "no new verdict token",
        "`DONE`/`PARTIAL`/`BLOCKED`/`FAILED`",
        "`comment-allowed=true` is audit-incomplete",
    ] {
        assert!(
            normalised.contains(phrase),
            "SKILL.tester.md must own the tester protocol {phrase:?}"
        );
    }

    // Tester keeps its restricted capability surface: no project/status/
    // version/relation data, no status/timer/worktree/issue-body writes, and
    // `notify send` stays manual-only.
    for boundary in [
        "project, status, version, and relation data stay out of reach",
        "Never `issue update`/`close`/`search`",
        "`status *`",
        "`timer *`",
        "`worktree acquire`/`release`/`prune`",
        "`notify send` stays manual-only",
    ] {
        assert!(
            normalised.contains(boundary),
            "SKILL.tester.md must keep the tester boundary {boundary:?}"
        );
    }

    // The tester note reuses the shared status vocabulary and never imports the
    // reviewer verdict tokens.
    for forbidden in ["VERDICT", "AUDIT_FAILED", "REQUEST_CHANGES"] {
        assert!(
            !tester.contains(forbidden),
            "SKILL.tester.md must not import the reviewer verdict vocabulary {forbidden:?}"
        );
    }
}

/// Issue 679 P3 adds the risk-based review policy: one risk class per phase with
/// a `reviewer_policy` (`final-only` by default, `checkpoint-and-final` only for
/// a `high-risk` or `irreversible` phase whose issue plan names the checkpoint),
/// an executor test disposition that never substitutes for tester verification,
/// compact evidence, and serial-by-default bounded parallelism that never
/// authorizes overlapping write owners or a mutable shared tree.
#[test]
fn risk_based_reviewer_policy_and_bounded_parallelism_are_documented() {
    let shared = read_skill("SKILL.md");
    let normalised: String = shared.split_whitespace().collect::<Vec<_>>().join(" ");
    for phrase in [
        "## Risk classes and reviewer policy",
        "`reviewer_policy`",
        "`final-only` is the default for `standard` work",
        "a checkpoint review never replaces the final one",
        "Without a named checkpoint boundary in the issue plan the policy stays `final-only`",
        "## Bounded parallelism (serial by default)",
        "immutable revision or snapshot in a separate worktree",
        "Overlapping write owners are never allowed",
        "## Test disposition and compact evidence",
        "never substitutes for the tester's independent verification",
    ] {
        assert!(
            normalised.contains(phrase),
            "SKILL.md must own the risk-based review policy {phrase:?}"
        );
    }
    // The policy never authorizes overlapping write owners or a reviewer and
    // executor on one mutable tree.
    assert!(
        normalised.contains("never work the same mutable tree at the same time"),
        "SKILL.md must forbid a reviewer and executor sharing a mutable tree"
    );

    let orchestrator = read_skill("SKILL.orchestrator.md");
    let normalised: String = orchestrator
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for phrase in [
        "## Risk class, reviewer policy, and parallelism",
        "`checkpoint-and-final` is allowed only for `high-risk` or `irreversible` work",
        "without that boundary the policy stays `final-only`",
        "Keep orchestration serial by default: one write owner per phase",
        "explicit recorded decision, never automatic",
    ] {
        assert!(
            normalised.contains(phrase),
            "SKILL.orchestrator.md must own the phase policy {phrase:?}"
        );
    }

    let executor = read_skill("SKILL.executor.md");
    let normalised: String = executor.split_whitespace().collect::<Vec<_>>().join(" ");
    for phrase in [
        "Declare a test disposition in your note",
        "never substitute for the tester's independent verification",
        "You remain the only write owner for the phase",
    ] {
        assert!(
            normalised.contains(phrase),
            "SKILL.executor.md must own the test disposition {phrase:?}"
        );
    }

    let reviewer = read_skill("SKILL.reviewer.md");
    let normalised: String = reviewer.split_whitespace().collect::<Vec<_>>().join(" ");
    for phrase in [
        "## Risk class and reviewer policy",
        "The default is one `final-only` audit of `standard` work",
        "never treat a checkpoint review as a replacement for the final one",
        "do not repeat the tester report",
        "`REVIEW:` line beside the `VERDICT:` line",
        "the pointer's `review` field",
    ] {
        assert!(
            normalised.contains(phrase),
            "SKILL.reviewer.md must own the risk-based review policy {phrase:?}"
        );
    }
}

#[test]
fn branch_lifecycle_is_one_liner_with_main_merge_type_id_and_bind_fallback() {
    let skill = read_skill("SKILL.md");
    let marker = "## Branch binding lifecycle";
    let start = skill
        .find(marker)
        .expect("SKILL.md must keep the Branch binding lifecycle section");
    let body = &skill[start + marker.len()..];
    let section = body.find("## ").map(|end| &body[..end]).unwrap_or(body);
    assert!(
        !section.contains("merge-only")
            && !section.to_lowercase().contains("never commit directly"),
        "lifecycle one-liner must not state a main merge-only restriction; got: {section}"
    );
    assert!(
        section.contains("feat/452") && section.contains("<type>/<id>"),
        "lifecycle one-liner must name the <type>/<id> branch convention; got: {section}"
    );
    assert!(
        section.contains("bind") && section.contains("fallback"),
        "lifecycle one-liner must keep bind as a background fallback; got: {section}"
    );
    assert!(
        section.contains("issue create") && section.contains("never creates a worktree implicitly"),
        "lifecycle one-liner must state that create/bind never creates a worktree implicitly; got: {section}"
    );
    assert!(
        !section.contains("- "),
        "lifecycle must stay a one-liner without a bullet list; got: {section}"
    );
}

/// Issue #602 splits the protocol in two: `SKILL.md` is the single source of
/// the shared guidance, and each `SKILL.<role>.md` is the always-on system
/// prefix of its agent, carrying only its own marker and its own safety
/// boundaries before pointing back at the shared skill. A shared block copied
/// back into a role prompt is what this test forbids.
#[test]
fn role_skills_defer_shared_protocol_to_the_general_skill() {
    let shared = read_skill("SKILL.md");
    for shared_block in [
        "# Phasegent",
        "## Tracking modes (decision tree)",
        "legacy alias `REDMINE_ISSUE`",
        "## Marker protocol",
        "## Result contracts",
        "```json",
        "`PASS` · `FAIL` · `REQUEST_CHANGES` · `BLOCKED` · `AUDIT_FAILED`",
        "New → In Progress → In Review → Resolved → Closed",
        "PHASEGENT_SESSION_ID",
    ] {
        assert!(
            shared.contains(shared_block),
            "SKILL.md must stay the single source for {shared_block:?}"
        );
    }

    // (role skill, its own marker family; the orchestrator and explore carry none)
    let roles: &[(&str, &str)] = &[
        ("SKILL.orchestrator.md", ""),
        ("SKILL.executor.md", "ai-executor"),
        ("SKILL.reviewer.md", "ai-reviewer"),
        ("SKILL.tester.md", "ai-tester"),
        ("SKILL.explore.md", ""),
    ];
    for &(relative, own_marker) in roles {
        let role = read_skill(relative);
        // Every role prompt keeps its critical safety boundaries.
        for boundary in ["`status *`", "`timer *`", "human-operator only"] {
            assert!(
                role.contains(boundary),
                "{relative} must keep the safety boundary {boundary:?}"
            );
        }
        if !own_marker.is_empty() {
            assert!(
                role.contains("Never commit, push, tag, or mutate refs"),
                "{relative} must keep the delivery boundary"
            );
        }
        // ...and its own marker shape only, never a sibling's.
        if own_marker.is_empty() {
            assert!(
                !role.contains("<!-- ai-"),
                "{relative} must not carry a child marker shape"
            );
        } else {
            assert!(
                role.contains(&format!("<!-- {own_marker} issue=")),
                "{relative} must keep its own marker shape"
            );
            for sibling in ["ai-executor", "ai-reviewer", "ai-tester"] {
                if sibling != own_marker {
                    assert!(
                        !role.contains(sibling),
                        "{relative} must not repeat the {sibling} marker"
                    );
                }
            }
        }
        // ...and no copy of a shared block; the pointer carries the rest.
        for shared_only in [
            "REDMINE_ISSUE",
            "```json",
            "`PASS` · `FAIL`",
            "New → In Progress",
            "PHASEGENT_SESSION_ID",
            "admin auth setup",
            "`doctor`",
            "`config show`",
            "`config provider get`",
        ] {
            assert!(
                !role.contains(shared_only),
                "{relative} must defer {shared_only:?} to SKILL.md"
            );
        }
        let normalised: String = role.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            normalised.contains("shared `phasegent` skill"),
            "{relative} must point at the shared `phasegent` skill"
        );
        assert!(
            role.contains("only for the command you are about to run"),
            "{relative} must scope help lookup to the command at hand"
        );
    }
}

/// Issue #602: help lookup stays scoped to the command in hand instead of
/// preloading the whole surface, and the configuration/provider self-check is
/// one conditional statement in the shared skill rather than a per-role note.
#[test]
fn help_lookup_is_scoped_and_self_checks_stay_conditional() {
    let shared = read_skill("SKILL.md");
    let normalised: String = shared.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalised.contains(
            "only for the command you are about to run, and defer every other command's help until it is selected"
        ),
        "SKILL.md must scope help lookup to the selected command"
    );
    assert!(
        normalised
            .contains("Only when a configuration or provider problem actually blocks the task"),
        "the shared skill must gate the configuration/provider self-check on an actual blocker"
    );
    for command in ["`doctor`", "`config show`", "`config provider get`"] {
        assert!(
            shared.contains(command),
            "SKILL.md must keep the read-only self-check {command}"
        );
    }
    for relative in [
        "SKILL.orchestrator.md",
        "SKILL.executor.md",
        "SKILL.reviewer.md",
        "SKILL.tester.md",
        "SKILL.explore.md",
    ] {
        let role = read_skill(relative);
        assert!(
            role.contains("only for the command you are about to run"),
            "{relative} must scope help lookup to the command at hand"
        );
        for duplicated in ["`doctor`", "`config show`", "`config provider get`"] {
            assert!(
                !role.contains(duplicated),
                "{relative} must not repeat the shared configuration self-check {duplicated}"
            );
        }
    }
}

/// Issue #602: the human-operator-only boundary must be unambiguous in every
/// prompt that describes CLI role gates, while the full `admin` group list
/// stays in the shared skill.
#[test]
fn admin_boundary_is_human_operator_only_in_every_prompt() {
    for relative in [
        "SKILL.md",
        "SKILL.orchestrator.md",
        "SKILL.executor.md",
        "SKILL.reviewer.md",
        "SKILL.tester.md",
        "SKILL.explore.md",
    ] {
        let text = read_skill(relative);
        assert!(
            text.contains("human-operator only"),
            "{relative} must state the human-operator-only admin boundary"
        );
        assert!(
            text.contains("`admin` group"),
            "{relative} must name the `admin` group the boundary protects"
        );
    }
    let shared = read_skill("SKILL.md");
    for entry in [
        "`admin auth setup`",
        "`admin config set/clear`",
        "`admin config provider set/clear`",
        "`admin workflow bootstrap`",
    ] {
        assert!(
            shared.contains(entry),
            "SKILL.md must keep the human-operator `admin` entry point {entry}"
        );
    }
}

/// The per-command role-gate catalogue lives in the role-filtered
/// `phasegent --help`, not in the skill: `SKILL.md` keeps exactly the
/// capability matrix and no other table that would mirror it.
#[test]
fn skill_does_not_reproduce_the_command_catalogue() {
    let shared = read_skill("SKILL.md");
    assert!(
        !shared.contains("| Command |"),
        "the command/role-gate catalogue must stay in `phasegent --help`"
    );
    let pipe_rows = shared
        .lines()
        .filter(|line| line.trim_start().starts_with('|'))
        .count();
    assert_eq!(
        pipe_rows, 20,
        "SKILL.md must carry exactly the capability table (header + separator + 18 rows)"
    );
}
