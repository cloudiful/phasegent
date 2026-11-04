//! The one bounded cache-only exemption shared by `issue close`, `issue sync`,
//! and `worktree prune`.
//!
//! A worktree whose only untracked content is the root `.cache` directory is
//! still removable: the contents are treated as regenerable scratch, never as
//! user data. The exception is deliberately narrow. It applies only when
//! `.cache` is a real directory under the candidate root (never a file or a
//! symlink and never a nested `other/.cache`), Git tracks nothing below it, the
//! complete porcelain-v1 status names only `??` paths under that exact root,
//! and a bounded, symlink-free inventory can be read twice identically. Any
//! uncertainty keeps the directory.
//!
//! Global [`super::is_clean`] is unchanged: it still reports a dirty worktree,
//! and only the cleanup callers consult this module after that verdict. The
//! removal is a guarded filesystem `remove_dir_all` of the single root
//! `.cache`, never `git clean` and never a force reset, and the normal
//! `git worktree remove` still runs without `--force` afterwards.

use std::path::{Path, PathBuf};

use super::{WorktreeRunner, bounded};

/// The maximum number of filesystem entries one cache inventory may contain.
/// A larger cache is uncertainty, not a cleanup candidate.
pub(crate) const MAX_CACHE_ENTRIES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FileIdentity {
    dev: u64,
    ino: u64,
    mtime: i64,
    mtime_nsec: i64,
    ctime: i64,
    ctime_nsec: i64,
    len: u64,
    directory: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CacheEntry {
    path: PathBuf,
    identity: FileIdentity,
}

/// One complete, bounded, symlink-free inventory of a root `.cache`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CacheInventory {
    root: FileIdentity,
    entries: Vec<CacheEntry>,
}

impl CacheInventory {
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }
}

/// The read-only classification of a dirty worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CacheInspection {
    /// The only untracked content is the exact root `.cache` directory.
    CacheOnly(CacheInventory),
    /// The worktree has other changes; the ordinary dirty rule applies.
    NotCacheOnly,
}

/// Classify a worktree's dirty state. `Err` is a bounded keep reason: the
/// inspection itself could not prove the exception safe.
pub(crate) fn inspect(
    runner: &dyn WorktreeRunner,
    worktree: &Path,
) -> Result<CacheInspection, String> {
    let cache = worktree.join(".cache");
    let metadata = match std::fs::symlink_metadata(&cache) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CacheInspection::NotCacheOnly);
        }
        Err(error) => {
            return Err(format!(
                "the worktree .cache could not be inspected: {}",
                bounded(&error.to_string())
            ));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err("the worktree .cache is a symlink, not a real directory".to_owned());
    }
    if !metadata.is_dir() {
        return Ok(CacheInspection::NotCacheOnly);
    }
    if !same_filesystem(worktree, &cache) {
        return Err("the worktree .cache is on a different filesystem".to_owned());
    }
    let tracked = runner
        .run(&["ls-files", "--", ".cache"], worktree)
        .map_err(|error| {
            format!(
                "the tracked paths under .cache could not be listed: {}",
                bounded(&error.message)
            )
        })?;
    if tracked.status != 0 {
        return Err("git ls-files failed while checking .cache".to_owned());
    }
    if !tracked.stdout.trim().is_empty() {
        return Ok(CacheInspection::NotCacheOnly);
    }
    // Two bounded porcelain-v1 queries prove the exact shape: nothing outside
    // the root `.cache` is dirty, and the cache's own entries are untracked.
    // The runner may bound long Git output, so emptiness and the first record
    // are compared instead of walking a possibly truncated list.
    let outside = runner
        .run(
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--",
                ".",
                ":(exclude).cache",
            ],
            worktree,
        )
        .map_err(|error| {
            format!(
                "the worktree status outside .cache could not be read: {}",
                bounded(&error.message)
            )
        })?;
    if outside.status != 0 {
        return Err("git status failed while checking paths outside .cache".to_owned());
    }
    if !outside.stdout.trim().is_empty() {
        return Ok(CacheInspection::NotCacheOnly);
    }
    let inside = runner
        .run(
            &[
                "status",
                "--porcelain=v1",
                "-z",
                "--untracked-files=all",
                "--",
                ".cache",
            ],
            worktree,
        )
        .map_err(|error| {
            format!(
                "the worktree status of .cache could not be read: {}",
                bounded(&error.message)
            )
        })?;
    if inside.status != 0 {
        return Err("git status failed while checking .cache".to_owned());
    }
    let inside = inside.stdout.trim();
    if inside.is_empty() || !inside.starts_with("?? ") {
        return Ok(CacheInspection::NotCacheOnly);
    }
    let inventory = inventory(&cache)?;
    if inventory.len() > MAX_CACHE_ENTRIES {
        return Err(format!(
            "the worktree .cache exceeds the {MAX_CACHE_ENTRIES}-entry inventory limit"
        ));
    }
    Ok(CacheInspection::CacheOnly(inventory))
}

/// Remove the exact root `.cache` of a dirty worktree, after re-reading the
/// inventory immediately before the deletion and re-running the Git status
/// afterwards. `Ok(None)` means the worktree was already clean; `Ok(Some(n))`
/// reports the entries removed. `Err` is a bounded keep reason and no cache
/// content is mutated.
pub(crate) fn clear_cache_only(
    runner: &dyn WorktreeRunner,
    worktree: &Path,
    expected: Option<&CacheInventory>,
) -> Result<Option<usize>, String> {
    let Some(expected) = expected else {
        return match super::is_clean(runner, worktree) {
            Ok(true) => Ok(None),
            Ok(false) => Err("the worktree became dirty after the local guard".to_owned()),
            Err(error) => Err(format!(
                "cleanliness probe failed: {}",
                bounded(&error.message)
            )),
        };
    };
    match inspect(runner, worktree)? {
        CacheInspection::CacheOnly(fresh) if fresh == *expected => {}
        CacheInspection::CacheOnly(_) => {
            return Err("the root .cache contents changed before cleanup".to_owned());
        }
        CacheInspection::NotCacheOnly => {
            return Err("the worktree has changes other than an untracked root .cache".to_owned());
        }
    }
    let cache = worktree.join(".cache");
    std::fs::remove_dir_all(&cache).map_err(|error| {
        format!(
            "removing the root .cache failed: {}",
            bounded(&error.to_string())
        )
    })?;
    match super::is_clean(runner, worktree) {
        Ok(true) => Ok(Some(expected.len())),
        Ok(false) => Err("the worktree became dirty after the cache cleanup".to_owned()),
        Err(error) => Err(format!(
            "cleanliness probe after cache cleanup failed: {}",
            bounded(&error.message)
        )),
    }
}

fn inventory(cache: &Path) -> Result<CacheInventory, String> {
    let root_metadata = std::fs::symlink_metadata(cache).map_err(|error| {
        format!(
            "the worktree .cache could not be inspected: {}",
            bounded(&error.to_string())
        )
    })?;
    let root = identity_of(&root_metadata)?;
    let mut entries: Vec<CacheEntry> = Vec::new();
    let mut pending: Vec<(PathBuf, PathBuf)> = vec![(PathBuf::new(), cache.to_path_buf())];
    while let Some((relative, directory)) = pending.pop() {
        let read = std::fs::read_dir(&directory).map_err(|error| {
            format!(
                "the worktree .cache could not be read: {}",
                bounded(&error.to_string())
            )
        })?;
        for entry in read {
            let entry = entry.map_err(|error| {
                format!(
                    "the worktree .cache could not be read: {}",
                    bounded(&error.to_string())
                )
            })?;
            if entries.len() >= MAX_CACHE_ENTRIES {
                return Err(format!(
                    "the worktree .cache exceeds the {MAX_CACHE_ENTRIES}-entry inventory limit"
                ));
            }
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                format!(
                    "a worktree .cache entry could not be inspected: {}",
                    bounded(&error.to_string())
                )
            })?;
            if metadata.file_type().is_symlink() {
                return Err("the worktree .cache contains a symlink".to_owned());
            }
            #[cfg(unix)]
            if device_of(&metadata) != device_of(&root_metadata) {
                return Err("the worktree .cache contains a different filesystem".to_owned());
            }
            let identity = identity_of(&metadata)?;
            let child = relative.join(entry.file_name());
            if identity.directory {
                pending.push((child.clone(), path));
            }
            entries.push(CacheEntry {
                path: child,
                identity,
            });
        }
    }
    entries.sort();
    Ok(CacheInventory { root, entries })
}

/// Reliable identity and change evidence for one filesystem entry. Platforms
/// without stable device/inode and high-resolution timestamps cannot prove the
/// explicit cache exception, so they fail closed instead of comparing names and
/// lengths only.
fn identity_of(metadata: &std::fs::Metadata) -> Result<FileIdentity, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(FileIdentity {
            dev: metadata.dev(),
            ino: metadata.ino(),
            mtime: metadata.mtime(),
            mtime_nsec: metadata.mtime_nsec(),
            ctime: metadata.ctime(),
            ctime_nsec: metadata.ctime_nsec(),
            len: metadata.len(),
            directory: metadata.is_dir(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err("reliable cache identity is unavailable on this platform".to_owned())
    }
}

#[cfg(unix)]
fn device_of(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.dev()
}

fn same_filesystem(worktree: &Path, cache: &Path) -> bool {
    #[cfg(unix)]
    {
        let Ok(left) = std::fs::metadata(worktree) else {
            return false;
        };
        let Ok(right) = std::fs::metadata(cache) else {
            return false;
        };
        device_of(&left) == device_of(&right)
    }
    #[cfg(not(unix))]
    {
        let _ = (worktree, cache);
        true
    }
}
