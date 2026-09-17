//! Test-only scratch root: one accessor, guaranteed to exist.

use std::path::PathBuf;

/// Absolute test scratch root, created if it is missing.
///
/// Tests build scratch *files* at `<root>/<name>` directly, and unlike a
/// scratch directory such a path has no `create_dir_all` of its own; under
/// the old `/tmp` root the directory happened to pre-exist, which is exactly
/// the hidden dependency this removes.
pub(crate) fn root() -> PathBuf {
    let root = std::env::temp_dir();
    std::fs::create_dir_all(&root).unwrap_or_else(|error| {
        panic!("create test scratch root {}: {error}", root.display());
    });
    root
}
