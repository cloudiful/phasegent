use crate::command::IssueCommand;

pub(crate) fn execute_branch_context(command: IssueCommand) -> i32 {
    let runner = crate::branch_context::ProcessGitRunner::new();
    let result = match command {
        IssueCommand::Bind {
            issue_id,
            replace,
            session,
        } => {
            // Issue 18: a successful bind is the session's task identity
            // becoming known, so run the shared best-effort auto-acquire hook.
            // It reports a created worktree (or a reuse warning) on stderr via
            // `report_local_warnings`; the stdout bind document is unchanged,
            // and no branch or worktree is ever deleted.
            let outcome = crate::branch_context::execute_bind(&runner, issue_id, replace);
            if outcome.is_ok() {
                crate::cli::report_local_warnings(
                    "issue bind",
                    crate::worktree::auto_acquire_after_bind(issue_id, session.as_deref()),
                );
            }
            outcome
        }
        IssueCommand::Unbind => crate::branch_context::execute_unbind(&runner),
        IssueCommand::StatusBranch => crate::branch_context::execute_status(&runner),
        _ => unreachable!("branch context dispatch handles only local issue commands"),
    };
    match result {
        Ok(value) => super::print_json(&value),
        Err(error) => super::structured_error(error.json(), 1),
    }
}
