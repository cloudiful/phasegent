//! Skill consistency tests.
//!
//! Pins the shipped `skills/phasegent` skill against the code and the protocol
//! contract: the SKILL frontmatter identity, the reviewer VERDICT vocabulary,
//! the role capability table's agreement with `src/policy.rs`, and the
//! single-file self-contained shape. Pure filesystem + policy reads; no
//! network, credentials, HOME, or SQLite access.

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
        section.contains("issue create") && section.contains("auto-acquires a worktree"),
        "lifecycle one-liner must state that create/bind auto-acquires a worktree; got: {section}"
    );
    assert!(
        !section.contains("- "),
        "lifecycle must stay a one-liner without a bullet list; got: {section}"
    );
}
