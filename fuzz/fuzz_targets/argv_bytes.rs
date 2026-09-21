//! libFuzzer target for the argv walker.
//!
//! Any panic escaping the harness entry aborts the run and libFuzzer writes the
//! reproducer to `fuzz/artifacts/argv_bytes/`. See `fuzz/README.md`.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = phasegent_fuzz::parse_argv_bytes(data);
});
