//! Filesystem-resolved containment for the explorer's read scope.
//!
//! [`super::scope`] decides *which* strings a tool call names; this
//! module decides whether one of them lands inside the worktree. The
//! split is deliberate: only this side has to touch the filesystem.
//!
//! Lexical normalization is not containment. A symlink inside the
//! worktree can point anywhere, so a `read` of `link/phasegent.sqlite3`
//! would reach the credential store through a path that looks
//! entirely inside the workspace. Every component is therefore resolved
//! through the filesystem, and a link is resolved even when it dangles:
//! a target that does not exist yet is still a link, and treating it as
//! a plain name would reopen the same escape.
//!
//! Resolution is a point-in-time check, so a link swapped between the
//! check and the agent's own read is still a window. It is bounded
//! rather than closed; the read-kind allowlist and the process cwd stay
//! the outer bound.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// Upper bound on the components and symlink hops one resolution
/// follows. A real worktree path is orders of magnitude shorter, and a
/// path built to spin through links lands here and is denied.
const MAX_HOPS: usize = 64;

/// One owned path step. Owning the name is what lets a symlink target be
/// spliced into a walk that is already borrowing another path.
enum Segment {
    /// A root or prefix. Pushing it *replaces* what came before, which is
    /// what an absolute symlink target needs.
    Root(OsString),
    Parent,
    Name(OsString),
}

fn segment_of(component: Component<'_>) -> Option<Segment> {
    match component {
        Component::Prefix(_) | Component::RootDir => {
            Some(Segment::Root(component.as_os_str().to_owned()))
        }
        Component::ParentDir => Some(Segment::Parent),
        Component::Normal(name) => Some(Segment::Name(name.to_owned())),
        Component::CurDir => None,
    }
}

fn apply(out: &mut PathBuf, segment: Segment) {
    match segment {
        Segment::Root(root) => out.push(root),
        Segment::Parent => {
            // Only a real name can be popped; a leading `..` has to
            // survive or a relative escape would normalize away into
            // something that looks contained.
            if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                out.pop();
            } else {
                out.push("..");
            }
        }
        Segment::Name(name) => out.push(name),
    }
}

/// Lexical normalization: resolve `.` and `..` without touching the
/// filesystem. A path that climbs above the filesystem root stays
/// outside any worktree, which is the safe answer, and this is the
/// form [`resolve`] starts from.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for segment in path.components().filter_map(segment_of) {
        apply(&mut out, segment);
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Resolve `path` through the filesystem, following every symlink.
/// `None` means the path could not be resolved within the hop budget,
/// which callers must treat as "not contained".
pub(crate) fn resolve(path: &Path) -> Option<PathBuf> {
    let mut resolved = PathBuf::new();
    let mut pending: VecDeque<Segment> = path.components().filter_map(segment_of).collect();
    let mut hops = 0usize;
    while let Some(segment) = pending.pop_front() {
        match segment {
            Segment::Name(name) => {
                hops += 1;
                if hops > MAX_HOPS {
                    return None;
                }
                let candidate = resolved.join(&name);
                match std::fs::symlink_metadata(&candidate) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        let target = std::fs::read_link(&candidate).ok()?;
                        // A relative target continues from the link's own
                        // parent, which is what `resolved` still holds.
                        pending.extend(target.components().filter_map(segment_of));
                    }
                    // A plain name, or one the filesystem cannot stat: it
                    // is carried forward lexically and the final
                    // containment check still applies to it.
                    _ => resolved.push(name),
                }
            }
            other => apply(&mut resolved, other),
        }
    }
    Some(resolved)
}

/// Whether `candidate` is `root` itself or inside it, once both are
/// resolved through symlinks. Fails closed: a relative side, or a
/// resolution the hop budget refuses, is never contained.
pub(crate) fn is_within(root: &Path, candidate: &Path) -> bool {
    if !root.is_absolute() || !candidate.is_absolute() {
        return false;
    }
    let (Some(resolved_root), Some(resolved_candidate)) = (resolve(root), resolve(candidate))
    else {
        return false;
    };
    resolved_candidate == resolved_root || resolved_candidate.starts_with(&resolved_root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn symlink(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).expect("create symlink");
    }

    #[test]
    fn dot_segments_resolve_without_the_filesystem() {
        assert_eq!(normalize(Path::new("/a/b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("/a/./b/")), PathBuf::from("/a/b"));
        assert_eq!(normalize(Path::new("../../x")), PathBuf::from("../../x"));
        assert_eq!(normalize(Path::new(".")), PathBuf::from("."));
        assert_eq!(
            resolve(Path::new("/a/./b/../c")).as_deref(),
            Some(Path::new("/a/c"))
        );
    }

    #[test]
    fn a_relative_side_is_never_contained() {
        assert!(!is_within(Path::new("wt"), Path::new("wt/src/a.rs")));
        assert!(!is_within(Path::new("/wt"), Path::new("src/a.rs")));
    }

    #[test]
    fn sibling_directories_sharing_a_prefix_are_outside() {
        assert!(is_within(
            Path::new("/tmp/wt"),
            Path::new("/tmp/wt/src/a.rs")
        ));
        assert!(!is_within(
            Path::new("/tmp/wt"),
            Path::new("/tmp/wt-secret/x")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_worktree_cannot_reach_outside_it() {
        let base = super::super::tests::scratch_dir("contain-escape");
        let worktree = base.join("worktree");
        let outside = base.join("outside");
        std::fs::create_dir_all(worktree.join("src")).expect("worktree");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(outside.join("token"), "secret").expect("seed");
        // A link to a real directory outside the worktree, and a link to
        // a target that does not exist yet: both are still links.
        symlink(&outside, &worktree.join("escape"));
        symlink(&base.join("not-created-yet"), &worktree.join("dangling"));
        symlink(&worktree.join("src"), &worktree.join("inside"));

        assert!(!is_within(&worktree, &worktree.join("escape/token")));
        assert!(
            !is_within(&worktree, &worktree.join("dangling/token")),
            "a dangling link is a link, not a plain name"
        );
        assert!(
            is_within(&worktree, &worktree.join("inside/main.rs")),
            "a link that stays inside the worktree is still a read"
        );
        assert!(is_within(&worktree, &worktree.join("src/main.rs")));
        assert!(!is_within(&worktree, &outside.join("token")));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_that_climbs_out_through_itself_is_denied() {
        let base = super::super::tests::scratch_dir("contain-climb");
        let worktree = base.join("worktree");
        std::fs::create_dir_all(worktree.join("deep")).expect("worktree");
        symlink(&base.join("outside"), &worktree.join("deep/up"));
        assert!(!is_within(&worktree, &worktree.join("deep/up/token")));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_worktree_root_resolves_both_sides() {
        // The worktree itself reached through a link must not make every
        // read look like an escape, and its real path is still inside.
        let base = super::super::tests::scratch_dir("contain-root-link");
        let worktree = base.join("real");
        std::fs::create_dir_all(&worktree).expect("worktree");
        symlink(&worktree, &base.join("link"));
        assert!(is_within(
            &base.join("link"),
            &base.join("link/src/main.rs")
        ));
        assert!(!is_within(
            &base.join("link"),
            &worktree.join("../elsewhere/x")
        ));
    }
}
