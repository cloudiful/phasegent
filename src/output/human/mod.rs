//! The curated human views.
//!
//! Every view renders the data the command already produced: the same
//! redacted values the JSON document carries, in a readable shape. No view
//! calls a provider, reads a credential, or mutates the filesystem, and every
//! view falls back to the generic rendering when the payload does not carry
//! the fields it knows.

mod config;
mod doctor;
mod generic;
mod issue;
mod status;
mod worktree;

use serde_json::Value;

use crate::output::context::{HumanView, OutputContext};

/// Render one result in the view selected for the command.
pub(crate) fn render(view: HumanView, value: &Value, context: &OutputContext) -> String {
    match view {
        HumanView::Generic => generic::render(value, context),
        HumanView::WorktreeLeaseList => worktree::render_lease_list(value, context),
        HumanView::WorktreeProbe => worktree::render_probe(value, context),
        HumanView::WorktreeAcquire => worktree::render_acquire(value, context),
        HumanView::WorktreePrune => worktree::render_prune(value, context),
        HumanView::IssueDetail => issue::render_detail(value, context),
        HumanView::IssueSearch => issue::render_search(value, context),
        HumanView::IssueWrite => issue::render_write(value, context),
        HumanView::StatusNext => status::render_next(value, context),
        HumanView::StatusTransition => status::render_transition(value, context),
        HumanView::StatusList => status::render_list(value, context),
        HumanView::Config => config::render(value, context),
        HumanView::Doctor => doctor::render(value, context),
    }
}
