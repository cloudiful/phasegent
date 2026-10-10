//! Redmine wire contract for structured records.
//!
//! Records ride on the ordinary journal primitives, so these tests pin
//! the provider-specific facts the record protocol depends on: the native
//! journal id is the record id and anchors `#change-<id>`, a plain `204 No
//! Content` write leaves the result unknown so the service recovers by
//! reading, and a write is only confirmed by an exact decoded record read.
//!
//! The scenarios are split into cohesive child modules; the shared
//! `spec`/`stored` fixtures live in [`fixtures`].

mod confirmation;
mod error;
mod fixtures;
mod native;
mod read;
mod replay;
