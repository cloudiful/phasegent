//! Focused CLI execution helpers for Redmine issue relations.
//!
//! This module keeps the relation list/create/delete paths out of `cli.rs`:
//! it validates the raw `--to`, `--type`, and `--delay` values and issues a
//! single request per operation. The local backend rejects every relation
//! operation with a structured not-supported error before any network
//! access.

use crate::command::RelationCommand;
use crate::providers::api::PhasegentError;
use crate::providers::redmine::model::{RedmineRelationType, RelationSummary};
use crate::providers::{ProviderDispatcher, RedmineProvider};

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
/// to the concrete provider. Local dispatchers never reach the network: this
/// returns a structured not-supported error instead.
pub(crate) fn execute(
    provider: &ProviderDispatcher,
    command: &RelationCommand,
) -> Result<RelationResult, PhasegentError> {
    match provider {
        ProviderDispatcher::Redmine(redmine) => execute_redmine(redmine, command),
        // Local backend has no relations.
        ProviderDispatcher::Local(_) => {
            Err(PhasegentError::not_supported("local", "issue relations"))
        }
    }
}

fn execute_redmine(
    redmine: &RedmineProvider,
    command: &RelationCommand,
) -> Result<RelationResult, PhasegentError> {
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
                return Err(PhasegentError::config(
                    "relation create --to requires a positive issue id",
                ));
            }
            if *to == *issue {
                return Err(PhasegentError::config(
                    "relation create cannot relate an issue to itself",
                ));
            }
            // `delay` is only meaningful for `precedes`; rejecting it for the
            // other canonical types keeps the serialized payload minimal and
            // prevents a contradictory `blocks` + `delay` request.
            if *relation_type != RedmineRelationType::Precedes && delay.is_some() {
                return Err(PhasegentError::config(
                    "relation create --delay is only valid with --type precedes",
                ));
            }
            let summary = redmine.create_relation(*issue, *to, *relation_type, *delay)?;
            Ok(RelationResult::Created(summary))
        }
        RelationCommand::Delete { relation_id } => {
            if *relation_id == 0 {
                return Err(PhasegentError::config(
                    "relation delete requires a positive relation id",
                ));
            }
            redmine.delete_relation(*relation_id)?;
            Ok(RelationResult::Deleted(*relation_id))
        }
    }
}

fn validate_issue(issue: u64, operation: &str) -> Result<(), PhasegentError> {
    if issue == 0 {
        return Err(PhasegentError::config(format!(
            "{operation} requires a positive issue id"
        )));
    }
    Ok(())
}
