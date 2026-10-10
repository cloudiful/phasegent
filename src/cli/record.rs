//! `record create|get|list`: the execution layer over
//! [`crate::record`], expressed through the existing `IssueProvider`
//! comment primitives.
//!
//! Gate order is deliberate and matches the ordinary comment flow: the
//! role and field gates run first, then the one-shot `--body-file` is
//! read and validated locally, and only then is a provider resolved. An
//! invalid request therefore never consumes input or reaches the network.
//! The generated header is the CLI's; the agent supplies metadata and a
//! plain note body only.

mod request;

use crate::command::RecordCommand;
use crate::policy::Role;
use crate::providers::ProviderKind;
use crate::record::{self, RecordFilter};

use request::Gate;

pub(crate) fn execute_record(
    role_value: Option<Role>,
    provider_kind: Option<ProviderKind>,
    api_base: Option<&str>,
    repository: Option<&str>,
    project_id: Option<&str>,
    close_status_id: Option<&str>,
    command: RecordCommand,
) -> i32 {
    let role = super::required_role(role_value);
    if !Role::RECORD_READ_ROLES.contains(&role) {
        return super::structured_error(
            serde_json::json!({
                "kind":"permission",
                "role":role.as_str(),
                "operation":"record",
                "message":format!("role '{}' is not allowed to perform record commands", role)
            }),
            3,
        );
    }
    match command {
        RecordCommand::Create {
            issue,
            kind,
            key,
            phase,
            attempt,
            review,
            recon,
            body,
            body_file,
            keep_body_file,
            authorized,
        } => {
            let spec = match request::authorize(
                role,
                request::GateInput {
                    kind,
                    key: &key,
                    phase: phase.as_deref(),
                    attempt,
                    review: review.as_deref(),
                    recon: recon.as_deref(),
                },
                authorized,
            ) {
                Ok(spec) => spec,
                Err(Gate::Argument(message)) => {
                    return super::structured_error(
                        serde_json::json!({
                            "kind":"argument",
                            "operation":"record create",
                            "message":message
                        }),
                        2,
                    );
                }
                Err(Gate::Permission(message)) => {
                    return super::structured_error(
                        serde_json::json!({
                            "kind":"permission",
                            "role":role.as_str(),
                            "operation":"record create",
                            "message":message
                        }),
                        3,
                    );
                }
            };
            // Reuse the ordinary one-shot body helpers so the file
            // lifecycle is byte-for-byte the comment flow's: validated
            // before any provider access, deleted only after a confirmed
            // write, kept on every failure.
            let body_input = match crate::body_file::resolve(
                Some(body.as_str()),
                body_file.as_deref(),
                keep_body_file,
            ) {
                Ok(Some(resolved)) => resolved,
                Ok(None) => unreachable!("--body or --body-file is required"),
                Err(message) => {
                    return super::structured_error(
                        serde_json::json!({
                            "kind":"argument",
                            "operation":"record create",
                            "message":message
                        }),
                        2,
                    );
                }
            };
            let (text, body_file) = body_input;
            let provider = match super::provider_for(
                role,
                provider_kind,
                api_base,
                repository,
                project_id,
                close_status_id,
            ) {
                Ok(provider) => provider,
                Err(error) => return super::provider_error(error),
            };
            if let Err(error) = super::project_resolution::verify_redmine_scope_before_write(
                role,
                api_base,
                repository,
                project_id,
                close_status_id,
                provider.kind(),
                &provider,
                issue,
            ) {
                return super::provider_error(error);
            }
            let exit = super::print_result(record::create(&provider, issue, &spec, text.as_str()));
            if exit == 0
                && let Some(body_file) = body_file.as_ref()
                && let Some(warning) = body_file.cleanup_after_success()
            {
                super::report_local_warnings("record create", Some(warning));
            }
            exit
        }
        RecordCommand::Get { issue, record } => {
            let provider = match super::provider_for(
                role,
                provider_kind,
                api_base,
                repository,
                project_id,
                close_status_id,
            ) {
                Ok(provider) => provider,
                Err(error) => return super::provider_error(error),
            };
            super::print_result(record::get(&provider, issue, record))
        }
        RecordCommand::List {
            issue,
            kind,
            phase,
            recon,
        } => {
            let provider = match super::provider_for(
                role,
                provider_kind,
                api_base,
                repository,
                project_id,
                close_status_id,
            ) {
                Ok(provider) => provider,
                Err(error) => return super::provider_error(error),
            };
            match record::list(&provider, issue, RecordFilter { kind, phase, recon }) {
                Ok(records) => super::print_json(&records),
                Err(error) => super::provider_error(error),
            }
        }
    }
}
