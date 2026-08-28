use crate::command::IssueCommand;

pub(crate) fn execute_branch_context(command: IssueCommand) -> i32 {
    let runner = crate::branch_context::ProcessGitRunner::new();
    let result = match command {
        IssueCommand::Bind { issue_id, replace } => {
            crate::branch_context::execute_bind(&runner, issue_id, replace)
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
