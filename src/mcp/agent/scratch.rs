//! Private server-side scratch directories for research runs.
//!
//! A research run never receives the phasegent repository or a resolved
//! worktree: it runs in a fresh directory created for that attempt alone, so
//! the adapter's filesystem reads are bounded to an empty sandbox and no
//! repository path is ever disclosed to the model. The directory lives under a
//! single server-owned root and is removed on terminal settlement.
//!
//! Removal is deliberately paranoid. It resolves the target and only deletes a
//! directory that canonically resolves strictly inside the server-owned root,
//! so a corrupted ledger row or a symlink swap can never turn settlement into
//! an arbitrary recursive delete.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Subdirectory of the system temp directory that owns every research scratch.
const SCRATCH_ROOT_NAME: &str = "phasegent-research";

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

/// The server-owned scratch root. The path stays server-side and is never
/// returned to a caller.
pub fn root() -> PathBuf {
    std::env::temp_dir().join(SCRATCH_ROOT_NAME)
}

/// Create a fresh private scratch directory for one research attempt.
///
/// The directory name carries the (server-generated) run id plus a
/// process-scoped sequence, so two attempts never share a directory even when
/// a run id is reused by a refused start or a resume.
pub fn create(run_id: &str) -> Result<PathBuf, String> {
    let root = root();
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("could not create the research scratch root: {error}"))?;
    restrict(&root);
    let sequence = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
    let name = format!("{run_id}-{:x}-{sequence:x}", std::process::id());
    let path = root.join(name);
    std::fs::create_dir_all(&path)
        .map_err(|error| format!("could not create the research scratch directory: {error}"))?;
    restrict(&path);
    Ok(path)
}

/// Remove a scratch directory created by [`create`]. A path that is missing,
/// that cannot be resolved, or that resolves outside the server-owned root is
/// left alone: cleanup must never reach beyond what phasegent created.
pub fn remove(path: &Path) {
    if !is_inside_root(path) {
        return;
    }
    let _ = std::fs::remove_dir_all(path);
}

/// Whether `path` canonically resolves strictly inside the scratch root.
fn is_inside_root(path: &Path) -> bool {
    let (Ok(target), Ok(root)) = (std::fs::canonicalize(path), std::fs::canonicalize(root()))
    else {
        return false;
    };
    target != root && target.starts_with(root)
}

#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_makes_a_fresh_private_directory_each_time() {
        let first = create("run-a").expect("first scratch");
        let second = create("run-a").expect("second scratch");
        assert!(first.is_dir() && second.is_dir());
        assert_ne!(first, second, "attempts never share a scratch directory");
        assert!(first.starts_with(root()), "{first:?}");
        remove(&first);
        remove(&second);
        assert!(!first.exists());
        assert!(!second.exists());
    }

    #[test]
    fn remove_refuses_any_path_outside_the_scratch_root() {
        let outside = std::env::temp_dir().join("phasegent-scratch-not-ours");
        std::fs::create_dir_all(&outside).expect("seed outside dir");
        std::fs::write(outside.join("keep.txt"), "keep").expect("seed file");
        remove(&outside);
        assert!(
            outside.join("keep.txt").exists(),
            "outside data is never touched"
        );
        std::fs::remove_dir_all(&outside).expect("cleanup outside dir");
    }
}
