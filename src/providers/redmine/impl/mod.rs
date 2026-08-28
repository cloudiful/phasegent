pub mod capabilities;
pub mod comments;
pub mod issues;
pub mod mirror;
pub mod projects;
pub mod relations;
pub mod selectors;
pub mod status;
pub mod time;

pub(crate) const PAGE_SIZE: usize = 100;
pub(crate) const MAX_PAGES: usize = 10_000;
