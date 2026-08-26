//! Focused CLI execution helpers for Redmine and GitLab issue relations.
//!
//! This module keeps the relation list/create/delete paths out of `cli.rs`:
//! it validates the raw `--to`, `--type`, and `--delay` values, enforces
//! provider-specific direction and inverse semantics, and issues a single
//! request per operation. Forgejo providers reject every relation
//! operation with a structured not-supported error before any network
//! access.

use crate::command::RelationCommand;
use crate::forgejo_model::ForgejoError;
use crate::provider::{ProviderDispatcher, RedmineProvider};
use crate::redmine_model::{RedmineRelationType, RelationSummary};

/// Distinct result shapes so `cli.rs` can emit the right JSON for each
/// relation subcommand without re-matching the command variant.
#[derive(Debug)]
pub(crate) enum RelationResult {
    List(Vec<RelationSummary>),
    Created(RelationSummary),
    Deleted(u64),
}

/// Validate the raw relation inputs, enforce role-independent invariants
/// (positive ids, no self-relation, delay only with `precedes`), and dispatch
/// to the concrete provider. Forgejo dispatchers never reach the
/// network: this returns a structured not-supported error instead.
pub(crate) fn execute(
    provider: &ProviderDispatcher,
    command: &RelationCommand,
) -> Result<RelationResult, ForgejoError> {
    match provider {
        ProviderDispatcher::Redmine(redmine) => execute_redmine(redmine, command),
        ProviderDispatcher::Gitlab(gitlab) => execute_gitlab(gitlab, command),
        ProviderDispatcher::Forgejo(_) => {
            Err(ForgejoError::not_supported("forgejo", "issue relations"))
        }
    }
}

fn execute_redmine(
    redmine: &RedmineProvider,
    command: &RelationCommand,
) -> Result<RelationResult, ForgejoError> {
    match command {
        RelationCommand::List { issue } => {
            validate_issue(*issue, "relation list")?;
            let relations = redmine.list_relations(*issue)?;
            Ok(RelationResult::List(relations))
        }
        RelationCommand::Create {
            issue,
            to,
            relation_type,
            delay,
        } => {
            validate_issue(*issue, "relation create")?;
            if *to == 0 {
                return Err(ForgejoError::config(
                    "relation create --to requires a positive issue id",
                ));
            }
            if *to == *issue {
                return Err(ForgejoError::config(
                    "relation create cannot relate an issue to itself",
                ));
            }
            // `delay` is only meaningful for `precedes`; rejecting it for the
            // other canonical types keeps the serialized payload minimal and
            // prevents a contradictory `blocks` + `delay` request.
            if *relation_type != RedmineRelationType::Precedes && delay.is_some() {
                return Err(ForgejoError::config(
                    "relation create --delay is only valid with --type precedes",
                ));
            }
            let summary = redmine.create_relation(*issue, *to, *relation_type, *delay)?;
            Ok(RelationResult::Created(summary))
        }
        RelationCommand::Delete {
            relation_id,
            issue: _,
        } => {
            if *relation_id == 0 {
                return Err(ForgejoError::config(
                    "relation delete requires a positive relation id",
                ));
            }
            // Redmine ignores the optional source issue; the field
            // exists only so the GitLab dispatch can be explicit.
            redmine.delete_relation(*relation_id)?;
            Ok(RelationResult::Deleted(*relation_id))
        }
    }
}

fn execute_gitlab(
    gitlab: &crate::gitlab::GitlabProvider,
    command: &RelationCommand,
) -> Result<RelationResult, ForgejoError> {
    match command {
        RelationCommand::List { issue } => {
            validate_issue(*issue, "relation list")?;
            let relations = gitlab.list_issue_links(*issue)?;
            Ok(RelationResult::List(relations))
        }
        RelationCommand::Create {
            issue,
            to,
            relation_type,
            delay,
        } => {
            validate_issue(*issue, "relation create")?;
            if *to == 0 {
                return Err(ForgejoError::config(
                    "relation create --to requires a positive issue id",
                ));
            }
            if *to == *issue {
                return Err(ForgejoError::config(
                    "relation create cannot relate an issue to itself",
                ));
            }
            // GitLab does not implement `precedes`/`follows` and has no
            // notion of a precedence lag. Surface the unsupported flag as
            // a structured config error rather than silently dropping it
            // or mapping it to a different link type.
            if *relation_type == RedmineRelationType::Precedes {
                return Err(ForgejoError::config(
                    "GitLab issue links do not support --type precedes",
                ));
            }
            if delay.is_some() {
                return Err(ForgejoError::config(
                    "GitLab issue links do not support --delay",
                ));
            }
            let summary = gitlab.create_issue_link(*issue, *to, *relation_type)?;
            Ok(RelationResult::Created(summary))
        }
        RelationCommand::Delete { relation_id, issue } => {
            if *relation_id == 0 {
                return Err(ForgejoError::config(
                    "relation delete requires a positive relation id",
                ));
            }
            // GitLab requires the source issue iid in the DELETE
            // URL (the endpoint is scoped per source issue); Redmine
            // and Forgejo ignore the flag. The parser now accepts an
            // explicit `--issue <SOURCE_ISSUE_IID>` so a caller who
            // targets GitLab must supply the source. A missing or
            // zero source on GitLab is rejected by the provider as
            // a structured config error before any HTTP traffic.
            gitlab.delete_issue_link(*issue, *relation_id)?;
            Ok(RelationResult::Deleted(*relation_id))
        }
    }
}

fn validate_issue(issue: u64, operation: &str) -> Result<(), ForgejoError> {
    if issue == 0 {
        return Err(ForgejoError::config(format!(
            "{operation} requires a positive issue id"
        )));
    }
    Ok(())
}
