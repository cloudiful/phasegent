pub mod http;
pub mod model;
pub mod planning;
pub mod relations;

#[cfg(test)]
mod contract_tests;

pub mod r#impl;

pub use r#impl::mirror::register_git_mirror;
pub use model::mirror::{RedmineDiscoveredProject, RedmineDiscovery};
