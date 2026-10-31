//! Release contract for the CLI-only product.
//!
//! The release workflow publishes one native `phasegent` binary per target,
//! checksummed into `SHA256SUMS`, and gated on the strict numeric tag matching
//! the `Cargo.toml` package version. These tests read the real manifests and
//! the real workflow and pin that contract: the target matrix, the full
//! feature set on every build, the raw artifact names, the checksum and
//! publication lists, the version-check parity, and the job permissions.
//! Workflow YAML is parsed structurally by `assets/opencode/cli-package.test.js`
//! under Bun, so these checks read the file text only where the layout is
//! itself the contract.

#[path = "cli_release_contract/homebrew.rs"]
mod homebrew;
#[path = "cli_release_contract/manifest.rs"]
mod manifest;
#[path = "cli_release_contract/workflow.rs"]
mod workflow;
