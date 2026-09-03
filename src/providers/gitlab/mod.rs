//! GitLab provider implementation: issue CRUD, note lifecycle, label
//! management, tracker mapping, and workflow status updates.
//!
//! The provider is the only place that knows about GitLab's HTTP
//! shape. Higher layers (`provider_dispatch.rs`,
//! `redmine_planning_cli.rs`) interact with it through the shared
//! `IssueProvider` trait and a small set of GitLab-specific helpers
//! for label / workflow operations.
//!
//! Phase 2 deliberately leaves a handful of capabilities as
//! structured not-supported stubs:
//!   - project enumeration and creation (Phase 3)
//!   - planning fields (parent issue, fixed version, dates, estimates,
//!     done ratio) - mapped to a Redmine-only planning CLI today, so a
//!     caller that asks for one against GitLab gets a structured
//!     not-supported error before any network access.

pub mod http;
pub mod r#impl;
pub mod model;

#[cfg(test)]
mod contract_tests;

pub use r#impl::core::GitlabProvider;
#[allow(unused_imports)]
pub(crate) use r#impl::repo::ResolvedNamespace;
