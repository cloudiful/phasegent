//! Black-box contract for the `record` command group.
//!
//! `record` is a CLI-owned envelope over the existing comment/note
//! primitives, so the acceptance surface is behavioral: the role and
//! kind gates must fire before any provider or `--body-file` access, the
//! one-shot body file must follow the comment flow's lifecycle, and
//! create/get/list must round-trip through a real provider. The local
//! SQLite provider needs no credentials or network, so it stands in for
//! the "mocked" provider here; the Redmine wire contract lives in the
//! unit-level `contract_tests::records` module. The scenarios are split
//! into cohesive child modules under `record_cli/`; the shared
//! scratch/local-store fixtures live in `fixtures`.

// This shared fixture module serves several integration tests; this test
// intentionally uses only its binary and scratch helpers.
#[path = "support/mod.rs"]
mod support;

// The crate root is this file, so its modules are looked up next to it;
// the scenario and fixture modules live in the `record_cli/` directory.
#[path = "record_cli/body_file.rs"]
mod body_file;
#[path = "record_cli/denials.rs"]
mod denials;
#[path = "record_cli/fixtures.rs"]
mod fixtures;
#[path = "record_cli/role.rs"]
mod role;
#[path = "record_cli/roundtrip.rs"]
mod roundtrip;
#[path = "record_cli/unconfirmed.rs"]
mod unconfirmed;
