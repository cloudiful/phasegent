//! Skill consistency tests.
//!
//! Pins the shipped `skills/phasegent-workflow` skill against the code and the
//! protocol contract: the SKILL frontmatter identity, the reviewer VERDICT
//! vocabulary, the role capability table's agreement with `src/policy.rs`, and
//! the references-chain files the SKILL links to. Pure filesystem + policy
//! reads; no network, credentials, HOME, or SQLite access.

use crate::policy::{Capability, Role};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

fn skill_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("skills/phasegent-workflow")
}

fn read_skill(relative: &str) -> String {
    let path = skill_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("expected skill file {} to be readable: {err}", path.display())
    })
}

#[test]
fn skill_file_exists_with_phasegent_workflow_frontmatter() {
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
            .any(|line| line.trim() == "name: phasegent-workflow"),
        "frontmatter must carry name: phasegent-workflow\n---\n{frontmatter}\n---"
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
    let normalised: String = skill
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        normalised.contains(
            "Use exactly one of these five case-sensitive tokens on the note's `VERDICT:` line"
        ),
        "SKILL.md must state the single-token selection rule"
    );
}

/// The role matrix in `references/roles.md` is the human-readable mirror of
/// `src/policy.rs`. Assert every capability row against `Role::allows` so a
/// drift in either direction surfaces as a test failure, and confirm the
/// row/operation counts agree between the table and the policy enum.
#[test]
fn role_table_agrees_with_policy() {
    let roles = read_skill("references/roles.md");
    let lines: Vec<&str> = roles.lines().collect();
    let header_idx = lines
        .iter()
        .position(|line| line.starts_with("| Capability |"))
        .expect("roles.md must carry the Capability header row");
    let header: Vec<String> = lines[header_idx]
        .split('|')
        .map(str::trim)
        .map(str::to_owned)
        .collect();
    let column = |name: &str| -> usize {
        header
            .iter()
            .position(|cell| cell == name)
            .unwrap_or_else(|| panic!("roles.md must include the {name} column"))
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
        other => panic!("unexpected capability cell {other:?} in roles.md"),
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
        let capability = cells.get(1).map(|cell| cell.trim().to_owned()).unwrap_or_default();
        if capability.is_empty() {
            continue;
        }
        let operation = cells.get(operation_col).map(|cell| cell.trim().to_owned()).unwrap_or_default();
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
        "roles.md and the policy enum must cover the same capability set"
    );

    for (name, capability) in all {
        let (operation, admin, orchestrator, executor, reviewer, tester) = table
            .get(*name)
            .unwrap_or_else(|| panic!("roles.md table must contain {name}"))
            .clone();
        assert_eq!(operation, capability.operation(), "operation mismatch for {name}");
        assert_eq!(orchestrator, Role::Orchestrator.allows(*capability), "orchestrator row for {name}");
        assert_eq!(admin, Role::Admin.allows(*capability), "admin row for {name}");
        assert_eq!(executor, Role::Executor.allows(*capability), "executor row for {name}");
        assert_eq!(reviewer, Role::Reviewer.allows(*capability), "reviewer row for {name}");
        assert_eq!(tester, Role::Tester.allows(*capability), "tester row for {name}");
    }
}

#[test]
fn reference_chain_files_exist_and_are_linked() {
    let skill = read_skill("SKILL.md");
    for (file, link) in [
        ("references/roles.md", "references/roles.md"),
        ("references/contracts.md", "references/contracts.md"),
    ] {
        let path = skill_root().join(file);
        assert!(path.is_file(), "reference chain must include {file}");
        assert!(
            !fs::read_to_string(&path).unwrap_or_default().is_empty(),
            "{file} must not be empty"
        );
        assert!(
            skill.contains(link),
            "SKILL.md must link to {link}"
        );
    }
}
