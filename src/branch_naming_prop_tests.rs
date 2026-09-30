//! Property tests for branch-name issue resolution.
//!
//! The properties pin the [`parse_issue_id_from_branch_name`] precedence
//! contract (rule 1 `type/<id>` beats rule 2 trailing `-`/`_id` beats rule
//! 3 bare digits) and the shapes the worktree/create flows really produce
//! (`phasegent/<issue>-<short6hex>` from [`generate_branch`],
//! `<type>/<id>` from [`branch_name_for_issue`]).
//!
//! Every property runs with a fixed RNG seed, and failing cases persist
//! under `.scratch/` so a shrunk counterexample is reproducible without
//! `PROPTEST_RNG_SEED` and can never land at the repository root.

use crate::branch_naming::parse_issue_id_from_branch_name;
use crate::lifecycle::branch_name_for_issue;
use crate::worktree::{generate_branch, validate_ref_format};
use proptest::prelude::*;
use proptest::test_runner::{Config, FileFailurePersistence, RngAlgorithm, RngSeed};

/// Fixed-seed config: one seed per property group, failure cases written
/// under the gitignored `.scratch/` tree.
fn prop_config(seed: u64) -> Config {
    Config {
        cases: 128,
        failure_persistence: Some(Box::new(FileFailurePersistence::Direct(
            ".scratch/proptest-regressions/branch_naming.txt",
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
/// padded with whitespace, because branch names never carry outer padding.
fn branch_shaped() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 1..24).prop_filter_map("non-blank branch text", |chars| {
        let text: String = chars.into_iter().collect();
        (!text.is_empty() && text.trim() == text).then_some(text)
    })
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
}
