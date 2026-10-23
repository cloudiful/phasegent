use crate::providers::redmine::model::RedmineRelationType;

#[derive(Debug)]
pub enum CommentCommand {
    Create {
        issue: u64,
        body: String,
        /// Optional one-shot Markdown file input (`--body-file`); mutually
        /// exclusive with `--body`, deleted after a successful write unless
        /// `--keep-body-file` was supplied.
        body_file: Option<String>,
        keep_body_file: bool,
        marker: String,
        authorized: bool,
    },
    Get {
        issue: u64,
        comment: u64,
    },
    /// Paged comment bodies for an issue, in provider order. The approved
    /// bulk-read path; successive pages expose all notes without raw
    /// `?include=journals` calls.
    List {
        issue: u64,
        page: Option<crate::pagination::PageRequest>,
    },
    FindMarker {
        issue: u64,
        marker: String,
    },
}

#[derive(Debug)]
pub enum ProjectCommand {
    List {
        page: Option<crate::pagination::PageRequest>,
    },
    Create {
        name: String,
        identifier: String,
        description: Option<String>,
        confirmed: bool,
    },
}

#[derive(Debug)]
pub enum StatusCommand {
    List,
    Next {
        number: u64,
    },
    Set {
        number: u64,
        status: String,
    },
    /// Redmine-only policy-preflighted transition: idempotent for the
    /// same status, rejected before any write for a canonical illegal
    /// edge, advisory for custom statuses. Orchestrator-only.
    Advance {
        number: u64,
        status: String,
    },
}

#[derive(Debug)]
pub enum VersionCommand {
    List {
        page: Option<crate::pagination::PageRequest>,
    },
}

#[derive(Debug)]
pub enum RelationCommand {
    List {
        issue: u64,
        page: Option<crate::pagination::PageRequest>,
    },
    Create {
        issue: u64,
        to: u64,
        relation_type: RedmineRelationType,
        delay: Option<u64>,
    },
    Delete {
        relation_id: u64,
    },
}

#[derive(Debug)]
pub enum TimerCommand {
    Start {
        issue: u64,
        phase: String,
        agent_role: String,
        attempt: u64,
        run_id: Option<String>,
        owner_session_id: Option<String>,
        owner_call_id: Option<String>,
    },
    Finish {
        run_id: String,
        result: String,
    },
    List {
        status: String,
        limit: u32,
        page: Option<crate::pagination::PageRequest>,
    },
    /// Read-only inspection of a single row. Returns a structured error
    /// when the run id is unknown; never mutates state.
    Get {
        run_id: String,
    },
    /// Explicit `FAILED` recovery for a known orphan. Equivalent to a
    /// user-authorized `timer finish --result FAILED` and then a same-run
    /// provider projection (with `sync_status` reconciliation). If the
    /// row is already terminal, the operation is rejected so a recovered
    /// run cannot overwrite a previous outcome.
    Recover {
        run_id: String,
    },
}

#[derive(Debug)]
pub enum WorkflowCommand {
    Bootstrap {
        repository: Option<String>,
        close_status_id: Option<String>,
        close_status_name: Option<String>,
        /// Explicit global reconciliation across every existing Redmine
        /// project instead of the current repository's project.
        all_projects: bool,
        /// Read-only preview; valid only together with `all_projects`.
        dry_run: bool,
    },
}
