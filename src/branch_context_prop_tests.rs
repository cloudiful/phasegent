//! Property tests for branch-name issue resolution (issue #558 Phase 3).
//!
//! The properties pin the [`parse_issue_id_from_branch_name`] precedence
//! contract (rule 1 `type/<id>` beats rule 2 trailing `-`/`_id` beats rule
//! 3 bare digits), the shapes the worktree flow really produces
//! (`phasegent/<issue>-<short6hex>` from [`generate_branch`], `<type>/<id>`
//! from [`branch_name_for_issue`]), and the bound-over-named source
//! precedence in [`status`].
//!
//! Every property runs with a fixed RNG seed, and failing cases persist
//! under `target/tmp/` so a shrunk counterexample is reproducible without
//! `PROPTEST_RNG_SEED` and can never land at the repository root.

use crate::branch_context::{
    BranchContextError, BranchIssueSource, GitOutput, GitRunner, parse_issue_id_from_branch_name,
    status,
};
use crate::lifecycle::branch_name_for_issue;
use crate::worktree::{generate_branch, validate_ref_format};
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, RngAlgorithm, RngSeed};

/// Fixed-seed config: one seed per property group, failure cases written
/// under the gitignored `target/` tree.
fn prop_config(seed: u64) -> Config {
    Config {
        cases: 128,
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            "target/tmp/proptest-regressions/branch_context.txt",
        ))),
        rng_algorithm: RngAlgorithm::ChaCha,
        rng_seed: RngSeed::Fixed(seed),
        ..Config::default()
    }
}

/// Rule-1 reference: the first `/`-segment whose leading digits form a
/// strictly positive `u64`.
fn first_slash_digit_prefix(branch: &str) -> Option<u64> {
    branch.match_indices('/').find_map(|(index, _)| {
        let digits: String = branch[index + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse::<u64>().ok().filter(|id| *id > 0)
    })
}

/// Arbitrary branch-shaped text: any Unicode scalar, never blank and never
/// padded with whitespace, because `current_branch` trims Git output.
fn branch_shaped() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 1..24).prop_filter_map("non-blank branch text", |chars| {
        let text: String = chars.into_iter().collect();
        (!text.is_empty() && text.trim() == text).then_some(text)
    })
}

/// Git runner for the source-precedence properties: a fixed branch name and
/// an optional stored binding.
struct BindingRunner {
    branch: String,
    binding: Option<String>,
}

impl GitRunner for BindingRunner {
    fn run(&self, args: &[&str]) -> Result<GitOutput, BranchContextError> {
        match args.first().copied() {
            Some("symbolic-ref") => Ok(GitOutput {
                status: 0,
                stdout: self.branch.clone(),
            }),
            Some("config") => Ok(match &self.binding {
                Some(value) => GitOutput {
                    status: 0,
                    stdout: value.clone(),
                },
                None => GitOutput {
                    status: 1,
                    stdout: String::new(),
                },
            }),
            other => Err(BranchContextError::new(
                "git",
                format!("unexpected git invocation {other:?}"),
            )),
        }
    }
}

#[test]
fn real_workflow_branch_shapes_resolve_to_the_expected_issue() {
    // Seeds include the worktree branch names minted for issues #544, #552
    // and #558 (`phasegent/<issue>-<short6hex>`), which is the shape the
    // leasing flow produces for every session.
    let cases: &[(&str, Option<u64>)] = &[
        ("phasegent/544-d5dc27", Some(544)),
        ("phasegent/552-c77a19", Some(552)),
        ("phasegent/558-544da0", Some(558)),
        ("feat/452", Some(452)),
        ("feat/452-slug", Some(452)),
        ("feat/help-unify-447", Some(447)),
        ("452", Some(452)),
        ("feat/452-help-447", Some(452)),
        ("feat/0", None),
        ("0", None),
        ("feat/", None),
        ("", None),
    ];
    for (branch, expected) in cases {
        assert_eq!(
            parse_issue_id_from_branch_name(branch),
            *expected,
            "branch {branch:?}"
        );
    }
}

proptest! {
    #![proptest_config(prop_config(0x544))]

    #[test]
    fn rule_one_beats_a_later_trailing_id(
        prefix in "[a-z][a-z0-9_]{0,10}",
        id in 1u64..=999_999,
        middle in "[a-z][a-z0-9_-]{0,10}",
        other in 1u64..=999_999,
    ) {
        let branch = format!("{prefix}/{id}-{middle}-{other}");
        prop_assert_eq!(parse_issue_id_from_branch_name(&branch), Some(id));
        prop_assert_eq!(first_slash_digit_prefix(&branch), Some(id));
    }

    #[test]
    fn rule_one_ignores_a_suffix_after_the_digits(
        lead in "[a-z][a-z0-9_-]{0,8}",
        id in 1u64..=999_999,
        tail in "[a-z0-9_/-]{0,12}",
    ) {
        let branch = format!("{lead}/{id}-{tail}");
        prop_assert_eq!(parse_issue_id_from_branch_name(&branch), Some(id));
    }

    #[test]
    fn rule_two_reads_a_trailing_separator_id(
        lead in "[a-z][a-z0-9_]{0,10}",
        word in "[a-z][a-z0-9_-]{0,10}",
        id in 1u64..=999_999,
    ) {
        let branch = format!("{lead}/{word}-{id}");
        prop_assert_eq!(first_slash_digit_prefix(&branch), None);
        prop_assert_eq!(parse_issue_id_from_branch_name(&branch), Some(id));
    }

    #[test]
    fn rule_two_accepts_a_zero_padded_trailing_id(
        lead in "[a-z][a-z0-9_-]{0,8}",
        id in 1u64..=999_999,
    ) {
        let branch = format!("{lead}_0{id}");
        prop_assert_eq!(parse_issue_id_from_branch_name(&branch), Some(id));
    }

    #[test]
    fn rule_three_agrees_with_parsing_the_digits(digits in "[0-9]{1,20}") {
        let expected = digits.parse::<u64>().ok().filter(|id| *id > 0);
        prop_assert_eq!(parse_issue_id_from_branch_name(&digits), expected);
    }

    #[test]
    fn a_returned_id_never_leaves_the_branch_name(branch in branch_shaped()) {
        if let Some(id) = parse_issue_id_from_branch_name(&branch) {
            prop_assert!(id > 0);
            prop_assert!(
                branch.contains(id.to_string().as_str()),
                "id {} is not a decimal substring of {:?}",
                id,
                branch
            );
        }
    }
}

proptest! {
    #![proptest_config(prop_config(0x558))]

    #[test]
    fn generated_worktree_branch_round_trips(id in 1u64..=4_000_000_000) {
        let (branch, short) = generate_branch(id).expect("generated branch");
        prop_assert_eq!(short.len(), 6);
        prop_assert!(validate_ref_format(&branch).is_ok());
        prop_assert_eq!(parse_issue_id_from_branch_name(&branch), Some(id));
    }

    #[test]
    fn hex_suffixed_phasegent_branch_round_trips(
        id in 1u64..=999_999,
        suffix in "[0-9a-f]{6}",
    ) {
        let branch = format!("phasegent/{id}-{suffix}");
        prop_assert!(validate_ref_format(&branch).is_ok());
        prop_assert_eq!(parse_issue_id_from_branch_name(&branch), Some(id));
    }

    #[test]
    fn tracker_branch_name_round_trips(
        tracker in prop::option::of("[A-Za-z ]{0,12}"),
        id in 1u64..=u32::MAX as u64,
    ) {
        let branch = branch_name_for_issue(tracker.as_deref(), id);
        prop_assert_eq!(parse_issue_id_from_branch_name(&branch), Some(id));
    }

    #[test]
    fn a_stored_binding_wins_over_any_parseable_branch(branch in branch_shaped(), id in 1u64..=999_999) {
        let runner = BindingRunner {
            branch: branch.clone(),
            binding: Some(id.to_string()),
        };
        let resolved = status(&runner).expect("status");
        prop_assert_eq!(resolved.issue_id, Some(id));
        prop_assert_eq!(resolved.source, BranchIssueSource::Bound);
        prop_assert_eq!(resolved.branch.as_str(), branch.trim());
    }

    #[test]
    fn an_unbound_branch_falls_back_to_the_parser(branch in branch_shaped()) {
        let runner = BindingRunner {
            branch: branch.clone(),
            binding: None,
        };
        let resolved = status(&runner).expect("status");
        prop_assert_eq!(resolved.branch.as_str(), branch.trim());
        match parse_issue_id_from_branch_name(&branch) {
            Some(id) => {
                prop_assert_eq!(resolved.issue_id, Some(id));
                prop_assert_eq!(resolved.source, BranchIssueSource::Named);
            }
            None => {
                prop_assert_eq!(resolved.issue_id, None);
                prop_assert_eq!(resolved.source, BranchIssueSource::None);
            }
        }
    }
}
