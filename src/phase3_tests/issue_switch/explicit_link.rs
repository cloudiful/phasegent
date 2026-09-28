//! Explicit `--branch` durable links (issue 628).
//!
//! Moved verbatim from the parent `issue_switch` module so explicit-flow
//! coverage lives on its own. Shared fixtures stay in the parent.

use super::super::{canonical_key, current_branch, pin_temp_db, switch_repo};
use super::{
    bare_repo, branch_exists, explicit_params_for, fallback_key, linked_entries, seed_link,
};
use crate::branch_context::GitRunner;
use crate::infra::storage::test_support::lock_workflow_tests;
use crate::lifecycle::{self, ExplicitLinkOutcome};

#[test]
fn explicit_link_creates_branch_and_records_scoped_link() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("explicit-create") else {
        return;
    };
    let _env = pin_temp_db(&db);
    let params = explicit_params_for(9, "feat/9", Some("tools-phasegent"));

    let outcome = lifecycle::link_explicit_branch(&repo.runner(), &params);
    assert_eq!(
        outcome,
        ExplicitLinkOutcome::Linked {
            branch: "feat/9".to_owned(),
            issue_id: 9,
            created: true,
            shared: None,
        }
    );
    assert!(outcome.warning().is_none());
    assert!(branch_exists(&repo, "feat/9"));
    assert_eq!(
        current_branch(&repo),
        "main",
        "explicit linking never switches the checkout"
    );
    let rows = linked_entries(&db, &canonical_key(), "feat/9");
    assert_eq!(rows.len(), 1, "exactly one durable row: {rows:?}");
    assert_eq!(rows[0].issue.provider, "redmine");
    assert_eq!(rows[0].issue.project, "tools-phasegent");
    assert_eq!(rows[0].issue_number, 9);
}

#[test]
fn explicit_link_adds_link_to_shared_branch_with_warning() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("explicit-shared") else {
        return;
    };
    let _env = pin_temp_db(&db);
    repo.runner()
        .run(&["branch", "feat/9", "HEAD"])
        .expect("pre-create works");
    seed_link(
        &db,
        &canonical_key(),
        "feat/9",
        "forgejo",
        "acme/widgets",
        9,
    );
    let params = explicit_params_for(9, "feat/9", Some("tools-phasegent"));

    let outcome = lifecycle::link_explicit_branch(&repo.runner(), &params);
    let shared_note = outcome.warning();
    let ExplicitLinkOutcome::Linked {
        created, shared, ..
    } = outcome
    else {
        panic!("shared existing branches still link: {outcome:?}");
    };
    assert!(!created, "the branch already existed");
    let note = shared.expect("sharing must surface");
    assert!(
        note.contains("forgejo") && note.contains("acme/widgets"),
        "warning must name the other scope: {note}"
    );
    assert!(
        shared_note.is_some_and(|warning| warning.contains("another link")),
        "warning() must carry the sharing note"
    );
    let rows = linked_entries(&db, &canonical_key(), "feat/9");
    assert_eq!(rows.len(), 2, "history is added, never replaced");
}

#[test]
fn explicit_link_same_scope_sharing_stays_quiet() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("explicit-same-scope") else {
        return;
    };
    let _env = pin_temp_db(&db);
    seed_link(
        &db,
        &canonical_key(),
        "feat/9",
        "redmine",
        "tools-phasegent",
        8,
    );
    let params = explicit_params_for(9, "feat/9", Some("tools-phasegent"));

    let outcome = lifecycle::link_explicit_branch(&repo.runner(), &params);
    assert!(
        matches!(outcome, ExplicitLinkOutcome::Linked { shared: None, .. }),
        "same-scope sharing is normal many-to-many: {outcome:?}"
    );
    assert!(outcome.warning().is_none());
}

#[test]
fn explicit_link_blocked_without_project_writes_nothing() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("explicit-blocked") else {
        return;
    };
    let _env = pin_temp_db(&db);
    let params = explicit_params_for(9, "feat/9", None);

    let outcome = lifecycle::link_explicit_branch(&repo.runner(), &params);
    let ExplicitLinkOutcome::Blocked { reason } = outcome else {
        panic!("missing project must block: {outcome:?}");
    };
    assert!(reason.contains("project"), "{reason}");
    assert!(!branch_exists(&repo, "feat/9"), "blocked must not create");
    assert!(
        linked_entries(&db, &canonical_key(), "feat/9").is_empty(),
        "blocked must not link"
    );
}

#[test]
fn explicit_link_skips_silently_on_repository_mismatch() {
    let _lock = lock_workflow_tests();
    let Some((repo, _db)) = switch_repo("explicit-mismatch") else {
        return;
    };
    // No DB pin needed: the mismatch gate fires before any store IO.
    let mut params = explicit_params_for(9, "feat/9", Some("tools-phasegent"));
    params.explicit_repository = Some("other/tools");

    let outcome = lifecycle::link_explicit_branch(&repo.runner(), &params);
    assert!(
        matches!(outcome, ExplicitLinkOutcome::Skipped { .. }),
        "foreign checkout must skip: {outcome:?}"
    );
    assert!(outcome.warning().is_none());
    assert!(!branch_exists(&repo, "feat/9"));
}

#[test]
fn explicit_link_refuses_conventional_default_when_unknown() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = bare_repo("explicit-conventional") else {
        return;
    };
    let _env = pin_temp_db(&db);
    for protected in ["main", "master"] {
        let params = explicit_params_for(9, protected, Some("tools-phasegent"));
        let outcome = lifecycle::link_explicit_branch(&repo.runner(), &params);
        let ExplicitLinkOutcome::Warning { reason } = outcome else {
            panic!("{protected} with unknown HEAD must warn: {outcome:?}");
        };
        assert!(
            reason.contains("conventional"),
            "warning must name the rule: {reason}"
        );
    }
    assert!(
        linked_entries(&db, &fallback_key(&repo), "main").is_empty(),
        "refused names must not link"
    );
}

#[test]
fn explicit_link_refuses_detected_default() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = switch_repo("explicit-detected") else {
        return;
    };
    let _env = pin_temp_db(&db);
    let params = explicit_params_for(9, "main", Some("tools-phasegent"));

    let outcome = lifecycle::link_explicit_branch(&repo.runner(), &params);
    let ExplicitLinkOutcome::Warning { reason } = outcome else {
        panic!("detected default must warn: {outcome:?}");
    };
    assert!(
        reason.contains("detected default"),
        "warning must name the rule: {reason}"
    );
    assert!(
        linked_entries(&db, &canonical_key(), "main").is_empty(),
        "refused names must not link"
    );
}

#[test]
fn explicit_link_allows_normal_names_when_default_unknown() {
    let _lock = lock_workflow_tests();
    let Some((repo, db)) = bare_repo("explicit-normal") else {
        return;
    };
    let _env = pin_temp_db(&db);
    let params = explicit_params_for(9, "feat/9", Some("tools-phasegent"));

    let outcome = lifecycle::link_explicit_branch(&repo.runner(), &params);
    assert!(
        matches!(outcome, ExplicitLinkOutcome::Linked { .. }),
        "unknown detection blocks only conventional names: {outcome:?}"
    );
    assert_eq!(linked_entries(&db, &fallback_key(&repo), "feat/9").len(), 1);
}
