// Isolated bridge-to-CLI subprocess integration for the managed shell
// execution context (issue #859 P1).
//
// Each scenario drives the checked-in generated plugin through Bun with
// isolated hook callbacks, then executes the hook-mutated env against an
// isolated local-provider scratch store via the compiled `phasegent`
// binary. Tests write only their isolated scratch local issue/record data
// and never touch a production provider or API.

#[path = "support/mod.rs"]
mod support;

#[path = "managed_shell_cli/fixtures.rs"]
mod fixtures;

#[path = "managed_shell_cli/scenarios.rs"]
mod scenarios;
