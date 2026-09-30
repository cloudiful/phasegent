//! Skill consistency tests.
//!
//! Pins the shipped `skills/phasegent` skills against the code and the protocol
//! contract: the SKILL frontmatter identity, the reviewer VERDICT vocabulary,
//! the role capability table's agreement with `src/policy.rs`, the
//! single-file self-contained shape, and the issue #602 split between the
//! shared protocol in `SKILL.md` and the always-on role boundaries in
//! `SKILL.<role>.md`. Issue 665 adds `SKILL.explore.md` to that role set and
//! asserts every role skill is embedded by the generated adapter. Pure
//! filesystem + policy reads; no network, credentials, HOME, or SQLite access.

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

/// Issue 665 adds the `explore` recon skill: every `SKILL.<role>.md` in the
/// skill directory must be embedded by the generated adapter under its
/// `phasegent-<role>` id, so a new role prompt cannot ship without its binding.
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
        ],
        "the protocol role skills are the orchestrator/executor/reviewer/explore set"
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
