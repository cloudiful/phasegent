//! Workspace-bounded read scope for the read-only research agent.
//!
//! An allowed tool kind is not a scope: a `read` of any path would
//! hand a model-driven turn the phasegent credential store, the SSH
//! keys, and the agent's own configuration. Every path the agent names
//! in a permission request is therefore resolved — relative paths
//! against the scratch workspace the process was started in, symlinks
//! through the filesystem — and must land inside that workspace. See
//! [`super::contain`] for the resolution and [`super::fetch`] for the
//! one read kind that is not a filesystem read at all.

use std::path::{Path, PathBuf};

use super::wire_client::ToolCallSummary;
use super::{contain, fetch};

/// Bounds on the per-request inspection so a large `rawInput` cannot
/// turn the decision into a hot loop.
const MAX_INSPECTED_STRINGS: usize = 256;
const MAX_INSPECTED_STRING_CHARS: usize = 4_096;

/// One research agent's workspace root: the scratch directory the ACP process
/// runs in.
#[derive(Clone, Debug)]
pub struct WorkspaceScope {
    /// The workspace root as the run manager created it, and the base every
    /// relative candidate is joined to.
    root: PathBuf,
    /// The same path with its symlinks resolved. Containment is checked
    /// against this, so a link inside the workspace cannot point out of
    /// it; a root that cannot be resolved fails closed.
    resolved_root: PathBuf,
}

impl WorkspaceScope {
    pub fn new(cwd: &Path) -> Self {
        let root = contain::normalize(cwd);
        let resolved_root = contain::resolve(&root).unwrap_or_else(|| root.clone());
        Self {
            root,
            resolved_root,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether a read-kind tool call stays inside the workspace. Any
    /// non-empty string in the tool call is treated as a candidate
    /// path: a string that cannot escape (a glob, a regex) is
    /// harmlessly inside the root, so the rule stays fail-closed
    /// without a separate "is this a path" guess.
    ///
    /// `fetch` is the exception: it names no path, so it is bounded by
    /// the URL contract instead of the scratch workspace.
    pub fn permits_read(&self, tool_call: &ToolCallSummary) -> bool {
        let candidates = referenced_candidates(tool_call);
        if tool_call.kind.as_deref() == Some(super::KIND_FETCH) {
            return fetch::permits(&candidates);
        }
        candidates.iter().all(|candidate| self.allows(candidate))
    }

    fn allows(&self, candidate: &str) -> bool {
        let raw = Path::new(candidate);
        let joined = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.root.join(raw)
        };
        contain::is_within(&self.resolved_root, &contain::normalize(&joined))
    }
}

/// Every candidate path string a tool call names: its declared
/// locations plus its raw input values, in a bounded walk.
fn referenced_candidates(tool_call: &ToolCallSummary) -> Vec<String> {
    let mut found = Vec::new();
    for location in &tool_call.locations {
        push_candidate(&mut found, &location.path);
    }
    if let Some(raw) = &tool_call.rawInput {
        collect_strings(raw, &mut found, 0);
    }
    found
}

fn collect_strings(value: &serde_json::Value, found: &mut Vec<String>, depth: usize) {
    if found.len() >= MAX_INSPECTED_STRINGS || depth > 8 {
        return;
    }
    match value {
        serde_json::Value::String(text) => push_candidate(found, text),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_strings(item, found, depth + 1);
            }
        }
        serde_json::Value::Object(fields) => {
            for field in fields.values() {
                collect_strings(field, found, depth + 1);
            }
        }
        _ => {}
    }
}

fn push_candidate(found: &mut Vec<String>, text: &str) {
    if text.is_empty() || found.len() >= MAX_INSPECTED_STRINGS {
        return;
    }
    let trimmed: String = text.chars().take(MAX_INSPECTED_STRING_CHARS).collect();
    if !trimmed.is_empty() {
        found.push(trimmed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(kind: &str, raw_input: serde_json::Value) -> ToolCallSummary {
        ToolCallSummary {
            kind: Some(kind.to_owned()),
            locations: Vec::new(),
            rawInput: Some(raw_input),
        }
    }

    fn read(path: &str) -> ToolCallSummary {
        call("read", serde_json::json!({ "path": path }))
    }

    #[test]
    fn reads_inside_the_worktree_are_permitted() {
        let scope = WorkspaceScope::new(Path::new("/tmp/wt"));
        assert!(scope.permits_read(&read("/tmp/wt/src/main.rs")));
        assert!(scope.permits_read(&read("src/main.rs")));
        assert!(scope.permits_read(&call("search", serde_json::json!({ "pattern": "**/*.rs" }))));
        // A sibling directory sharing a name prefix is outside.
        assert!(!scope.permits_read(&read("/tmp/wt-secret/token")));
    }

    #[test]
    fn relative_climbs_and_absolute_escapes_are_denied() {
        let scope = WorkspaceScope::new(Path::new("/tmp/wt"));
        for escape in [
            "../../../home/dev/.config/phasegent/phasegent.sqlite3",
            "/home/dev/.config/phasegent/phasement.sqlite3",
            "/home/dev/.ssh/id_ed25519",
            "/etc/shadow",
            "src/../../../etc/hosts",
            "/",
        ] {
            assert!(
                !scope.permits_read(&read(escape)),
                "{escape} must be denied"
            );
        }
    }

    #[test]
    fn any_nested_string_is_inspected_not_only_known_path_keys() {
        let scope = WorkspaceScope::new(Path::new("/tmp/wt"));
        assert!(!scope.permits_read(&call(
            "search",
            serde_json::json!({ "filter": {"include": [{"paths": ["/root/.minimax/config.yaml"]}]} })
        )));
        assert!(scope.permits_read(&call(
            "search",
            serde_json::json!({ "filter": {"include": [{"paths": ["src/lib.rs"]}]} })
        )));
    }

    #[test]
    fn declared_locations_are_bounded_too() {
        let scope = WorkspaceScope::new(Path::new("/tmp/wt"));
        let mut summary = read("src/main.rs");
        summary
            .locations
            .push(crate::mcp::agent::wire_client::ToolCallLocation {
                path: "/home/dev/.minimax/auth.json".to_owned(),
            });
        assert!(!scope.permits_read(&summary));
    }

    #[test]
    fn oversized_inputs_stay_bounded() {
        let scope = WorkspaceScope::new(Path::new("/tmp/wt"));
        let wide: Vec<String> = (0..1_000).map(|_| "/etc/shadow".to_owned()).collect();
        assert!(!scope.permits_read(&call("read", serde_json::json!({ "paths": wide }))));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_inside_the_worktree_cannot_reach_outside_it() {
        // The kind is an allowed read, so the one thing that can carry a
        // `read` out of the worktree is a link: `escape/phasegent.sqlite3`
        // looks entirely contained while reaching the credential store.
        let base = super::super::tests::scratch_dir("scope-symlink");
        let worktree = base.join("worktree");
        let outside = base.join("outside");
        std::fs::create_dir_all(worktree.join("src")).expect("worktree");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(outside.join("phasegent.sqlite3"), "rows").expect("seed");
        std::os::unix::fs::symlink(&outside, worktree.join("escape")).expect("link");

        let scope = WorkspaceScope::new(&worktree);
        assert!(scope.permits_read(&read("src/main.rs")));
        let escape = format!("{}/escape/phasegent.sqlite3", worktree.display());
        assert!(
            !scope.permits_read(&read(&escape)),
            "a link out of the worktree must not become an allowed read"
        );
        assert!(
            !scope.permits_read(&read("escape/phasegent.sqlite3")),
            "the same escape is denied relatively too"
        );
    }

    #[test]
    fn a_fetch_is_bounded_by_its_urls_not_by_the_worktree() {
        let scope = WorkspaceScope::new(Path::new("/tmp/wt"));
        assert!(scope.permits_read(&call(
            "fetch",
            serde_json::json!({ "url": "https://docs.example.com/guide" })
        )));
        // No location at all: nothing to bound, so nothing is allowed.
        assert!(!scope.permits_read(&call("fetch", serde_json::json!({ "maxBytes": 4 }))));
        // A filesystem path does not satisfy the URL contract, so it
        // cannot smuggle a read through the fetch kind.
        assert!(!scope.permits_read(&call("fetch", serde_json::json!({ "url": "/etc/shadow" }))));
        assert!(!scope.permits_read(&call(
            "fetch",
            serde_json::json!({ "url": "https://user:pw@example.com/x" })
        )));
    }
}
