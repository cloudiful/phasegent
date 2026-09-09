//! Branch name, fingerprint, slug, and cache-root helpers for the
//! worktree leasing flow.
//!
//! The helpers are kept in a focused module so the main
//! [`crate::worktree`] surface stays readable. Nothing here touches
//! the database or the network; the inputs and outputs are pure
//! strings and filesystem paths.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::worktree::{WorktreeError, now_unix_secs};

/// Length of the short hex suffix appended to `phasegent/<issue>-` so
/// collisions on issue id are astronomically unlikely within a
/// single-machine cache.
#[allow(dead_code)]
pub(super) const SHORT_SUFFIX_HEX: usize = 6;
/// Width of the FNV-1a fingerprint used to scope per-repo cache
/// directories. 12 hex chars (48 bits) is well past the practical
/// number of repos a single user / machine ever holds; collisions
/// remain observable but cause only a shared cache directory, not
/// data loss.
pub(super) const FINGERPRINT_HEX: usize = 12;
/// Prefix on auto-generated branch names so Phase 2 cleanup can
/// recognise them with a single prefix scan.
pub(super) const BRANCH_NAMESPACE: &str = "phasegent";

/// Ref-format validation aligned with the `check-ref-format` rules the
/// Phase 1 scope calls out: lowercase alnum + `-`, `_`, `/`; no
/// control or space; <=128 chars; no leading `-`; no trailing `.`;
/// no `..` segment; no `@{` sequence; no `~`, `^`, `:`, `?`, `*`,
/// `[`. The check is intentionally strict so a generated
/// `phasegent/<issue>-<short6hex>` branch is never accepted in a form
/// that Git would later reject on `git worktree add`.
pub fn validate_ref_format(name: &str) -> Result<(), WorktreeError> {
    const MAX_REF_CHARS: usize = 128;
    if name.is_empty() {
        return Err(WorktreeError::new("argument", "ref name must not be empty"));
    }
    if name.chars().count() > MAX_REF_CHARS {
        return Err(WorktreeError::new(
            "argument",
            format!("ref name exceeds {MAX_REF_CHARS} chars"),
        ));
    }
    if name.starts_with('-') {
        return Err(WorktreeError::new(
            "argument",
            "ref name must not start with '-'",
        ));
    }
    if name.ends_with('.') || name.ends_with(".lock") || name.ends_with('/') {
        return Err(WorktreeError::new(
            "argument",
            "ref name must not end with '.', '.lock', or '/'",
        ));
    }
    if name.contains("..") || name.contains("@{") || name.contains('\\') {
        return Err(WorktreeError::new(
            "argument",
            "ref name must not contain '..', '@{', or '\\'",
        ));
    }
    for byte in name.bytes() {
        let is_lower_alnum = byte.is_ascii_digit() || byte.is_ascii_lowercase();
        let allowed = matches!(byte, b'-' | b'_' | b'/');
        if !is_lower_alnum && !allowed {
            return Err(WorktreeError::new(
                "argument",
                "ref name must use lowercase alnum and -/_ only",
            ));
        }
    }
    Ok(())
}

/// FNV-1a 64-bit hash, formatted as `FINGERPRINT_HEX` lowercase hex
/// chars. Deterministic, no external crate, and stable across
/// processes / versions because it has no `RandomState` initialiser.
pub fn compute_fingerprint(repo_identity: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET;
    for byte in repo_identity.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    let mut out = String::with_capacity(FINGERPRINT_HEX);
    for shift in (0..FINGERPRINT_HEX).rev() {
        let nibble = ((hash >> (shift * 4)) & 0x0f) as u8;
        let ch = char::from_digit(u32::from(nibble), 16).unwrap_or('0');
        out.push(ch);
    }
    out
}

/// Slug for the cache directory: replace `/` with `-` so a single
/// `phasegent/<issue>-<short6hex>` ref becomes a flat, single-segment
/// directory name. The result is itself revalidated against the
/// ref-format rules so a malformed branch never reaches the cache.
pub fn slug_from_branch(branch: &str) -> Result<String, WorktreeError> {
    let slug = branch.replace('/', "-");
    validate_ref_format(&slug)?;
    Ok(slug)
}

/// Build a `phasegent/<issue>-<short6hex>` branch name. The
/// short-hex suffix is a per-call counter + nanosecond mix rendered
/// as six lowercase hex characters; collisions on the same issue
/// across rapid retries are still unique on the per-call basis. The
/// result passes [`validate_ref_format`] before being returned.
pub fn generate_branch(issue: u64) -> Result<(String, String), WorktreeError> {
    let short = short_hex_suffix();
    let branch = format!("{BRANCH_NAMESPACE}/{issue}-{short}");
    validate_ref_format(&branch)?;
    Ok((branch, short))
}

fn short_hex_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.subsec_nanos() as u64)
        .unwrap_or(0);
    // Mix in a per-process counter so two calls in the same nanosecond
    // still get a different suffix. The counter resets when the
    // process restarts, which is fine because a fresh process will
    // also have a different nanosecond baseline.
    let counter = SHORT_SUFFIX_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mix = nanos.wrapping_add(counter.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    format!("{:06x}", (mix & 0x00ff_ffff) as u32)
}

static SHORT_SUFFIX_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a unique lease id of the shape `lease-<nanos_hex>-<counter_hex>`.
/// Combines wall-clock seconds with the same per-process counter
/// `short_hex_suffix` uses so two leases minted in the same instant
/// still differ.
pub(super) fn new_lease_id() -> String {
    let nanos = now_unix_secs();
    let counter = SHORT_SUFFIX_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("lease-{nanos:x}-{counter:x}")
}

/// Compute the per-repo cache root used for worktree directories.
/// Returns `<cache_dir>/worktrees/<fingerprint>`. Created with
/// private (0700) permissions on Unix when missing. The
/// `PHASEGENT_WORKTREE_CACHE_DIR` environment variable, when set
/// to an absolute path, overrides the OS cache root so tests can
/// point worktree creation at a temp directory; production never
/// sets it.
pub fn cache_root(fingerprint: &str) -> Result<PathBuf, WorktreeError> {
    let base = cache_base_dir()
        .ok_or_else(|| WorktreeError::new("storage", "could not resolve phasegent cache dir"))?;
    cache_root_in(&base, fingerprint)
}

/// Test / Phase-2 helper that lets a caller pick the cache base
/// explicitly without relying on the `PHASEGENT_WORKTREE_CACHE_DIR`
/// env var. Same layout as [`cache_root`]: `<base>/worktrees/<fingerprint>`.
#[allow(dead_code)]
pub fn cache_root_in(base: &Path, fingerprint: &str) -> Result<PathBuf, WorktreeError> {
    let cache = base.join("worktrees").join(fingerprint);
    std::fs::create_dir_all(&cache).map_err(|error| {
        WorktreeError::new(
            "storage",
            format!("could not create worktree cache dir: {error}"),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700));
    }
    Ok(cache)
}

#[allow(dead_code)]
fn cache_base_dir() -> Option<PathBuf> {
    if let Some(override_path) = std::env::var_os("PHASEGENT_WORKTREE_CACHE_DIR") {
        let path = PathBuf::from(override_path);
        if path.is_absolute() {
            return Some(path);
        }
    }
    let dirs = directories::ProjectDirs::from("com", "Cloud1ful", "phasegent")?;
    Some(dirs.cache_dir().to_path_buf())
}
