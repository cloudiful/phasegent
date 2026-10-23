//! The curated worktree views: the lease list, the probe, the acquire
//! outcome, and the prune report.
//!
//! Every view renders the lease identities, paths, and outcomes the command
//! already resolved. A path is shortened relative to the working directory or
//! the home directory only when the shorter form still names the same path;
//! otherwise the full path stays, on its own line.

use serde_json::Value;

use crate::output::context::OutputContext;
use crate::output::render::Lines;

/// The curated `worktree list` and `worktree status` rendering.
pub(crate) fn render_lease_list(value: &Value, context: &OutputContext) -> String {
    let Some(leases) = value.get("leases").and_then(Value::as_array) else {
        return super::generic::render(value, context);
    };
    let mut lines = Lines::new(context);
    lines.heading(&lease_summary(leases));
    if leases.is_empty() {
        lines.blank();
        lines.text(0, "No worktree leases are recorded for this scope.");
        return lines.finish();
    }
    lines.blank();
    // A narrow terminal stacks the entries instead of overflowing a table.
    let stacked = !entries_fit(leases, context.width);
    for lease in leases {
        render_lease(&mut lines, lease, stacked);
    }
    lines.finish()
}

/// The count and state summary of a lease list.
fn lease_summary(leases: &[Value]) -> String {
    let total = leases.len();
    let noun = if total == 1 { "lease" } else { "leases" };
    let mut counts: Vec<(String, usize)> = Vec::new();
    for lease in leases {
        let state = lease
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        match counts.iter_mut().find(|(name, _)| *name == state) {
            Some((_, count)) => *count += 1,
            None => counts.push((state, 1)),
        }
    }
    // The known states keep their workflow order; the rest follow in
    // alphabetical order so the summary never changes shape between runs.
    let mut ordered: Vec<(String, usize)> = Vec::new();
    for state in ["active", "retained", "released"] {
        if let Some(entry) = counts.iter().find(|(name, _)| name == state) {
            ordered.push(entry.clone());
        }
    }
    let mut others: Vec<(String, usize)> = counts
        .into_iter()
        .filter(|(name, _)| !["active", "retained", "released"].contains(&name.as_str()))
        .collect();
    others.sort_by(|left, right| left.0.cmp(&right.0));
    ordered.extend(others);
    if ordered.is_empty() {
        return format!("{total} {noun}");
    }
    let states: Vec<String> = ordered
        .iter()
        .map(|(name, count)| format!("{count} {name}"))
        .collect();
    format!("{total} {noun}: {}", states.join(", "))
}

/// Whether the horizontal entry layout fits the terminal width.
fn entries_fit(leases: &[Value], width: usize) -> bool {
    leases.iter().all(|lease| {
        let line = entry_line(lease);
        console::measure_text_width(&line) <= width
    })
}

/// The horizontal entry line of one lease.
fn entry_line(lease: &Value) -> String {
    let id = lease.get("lease_id").and_then(Value::as_str).unwrap_or("-");
    let issue = lease.get("issue").and_then(Value::as_u64);
    let state = lease
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let branch = lease.get("branch").and_then(Value::as_str).unwrap_or("-");
    match issue {
        Some(issue) => format!("{id}  #{issue}  {state}  {branch}"),
        None => format!("{id}  {state}  {branch}"),
    }
}

/// Render one lease entry.
fn render_lease(lines: &mut Lines, lease: &Value, stacked: bool) {
    let id = lease.get("lease_id").and_then(Value::as_str).unwrap_or("-");
    let issue = lease.get("issue").and_then(Value::as_u64);
    let state = lease
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let branch = lease.get("branch").and_then(Value::as_str).unwrap_or("-");
    let path = lease.get("worktree_path").and_then(Value::as_str);
    if stacked {
        lines.row(id);
        if let Some(issue) = issue {
            lines.field(1, "Issue", &format!("#{issue}"));
        }
        lines.field(1, "State", state);
        lines.field(1, "Branch", branch);
        if let Some(path) = path {
            lines.field(1, "Path", &shorten_path(path));
        }
    } else {
        lines.raw(entry_line(lease));
        if let Some(path) = path {
            lines.text(1, &shorten_path(path));
        }
    }
    if let Some(reason) = lease.get("release_reason").and_then(Value::as_str) {
        lines.field(1, "Release reason", reason);
    }
}

/// The curated `worktree probe` rendering.
pub(crate) fn render_probe(value: &Value, context: &OutputContext) -> String {
    let mut lines = Lines::new(context);
    lines.heading("Worktree probe");
    lines.blank();
    let resolved = value
        .get("resolved")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !resolved {
        lines.text(0, "No worktree matched the selector.");
        render_errors(&mut lines, value);
        return lines.finish();
    }
    if let Some(path) = value.get("path").and_then(Value::as_str) {
        lines.field(0, "Path", &shorten_path(path));
    }
    if let Some(branch) = value.get("branch").and_then(Value::as_str) {
        lines.field(0, "Branch", branch);
    }
    if let Some(head) = value.get("head").and_then(Value::as_str) {
        lines.field(0, "Head", head);
    }
    lines.field(0, "State", &probe_state(value));
    if let Some(lease) = value.get("lease").filter(|lease| lease.is_object()) {
        let id = lease.get("lease_id").and_then(Value::as_str).unwrap_or("-");
        let issue = lease.get("issue").and_then(Value::as_u64);
        let state = lease
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let detail = match issue {
            Some(issue) => format!("{id} (issue #{issue}, {state})"),
            None => format!("{id} ({state})"),
        };
        lines.field(0, "Lease", &detail);
    }
    render_errors(&mut lines, value);
    lines.finish()
}

/// The readable state of a probed directory.
fn probe_state(value: &Value) -> String {
    let exists = value
        .get("exists")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !exists {
        return "missing".to_owned();
    }
    let is_worktree = value
        .get("is_git_worktree")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !is_worktree {
        return "not a Git worktree".to_owned();
    }
    match value.get("clean").and_then(Value::as_bool) {
        Some(true) => "clean worktree".to_owned(),
        Some(false) => "dirty worktree".to_owned(),
        None => "Git worktree".to_owned(),
    }
}

/// Render the bounded errors of a probe.
fn render_errors(lines: &mut Lines, value: &Value) {
    let Some(errors) = value.get("errors").and_then(Value::as_array) else {
        return;
    };
    for error in errors {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        lines.text(0, &format!("error: {message}"));
    }
}

/// The curated `worktree acquire` rendering.
pub(crate) fn render_acquire(value: &Value, context: &OutputContext) -> String {
    let mut lines = Lines::new(context);
    lines.heading("Worktree lease");
    lines.blank();
    if let Some(id) = value.get("lease_id").and_then(Value::as_str) {
        lines.field(0, "Lease", id);
    }
    if let Some(path) = value.get("path").and_then(Value::as_str) {
        lines.field(0, "Path", &shorten_path(path));
    }
    if let Some(branch) = value.get("branch").and_then(Value::as_str) {
        lines.field(0, "Branch", branch);
    }
    if let Some(repository) = value.get("repo_identity").and_then(Value::as_str) {
        lines.field(0, "Repository", repository);
    }
    let created = value
        .get("created")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    lines.field(0, "Outcome", if created { "created" } else { "reused" });
    if let Some(reason) = value.get("reason").and_then(Value::as_str) {
        lines.field(0, "Reason", reason);
    }
    lines.finish()
}

/// The curated `worktree prune` rendering.
pub(crate) fn render_prune(value: &Value, context: &OutputContext) -> String {
    let mut lines = Lines::new(context);
    let dry_run = value
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mode = if dry_run {
        "dry-run preview"
    } else {
        "removal"
    };
    lines.heading(&format!("worktree prune ({mode})"));
    let repos = value.get("repos").and_then(Value::as_array);
    if repos.is_some_and(|repos| repos.is_empty()) {
        lines.text(0, "no repositories with recorded worktree leases");
    }
    for repo in repos.into_iter().flatten() {
        let Some(actions) = repo
            .get("worktrees")
            .and_then(|worktrees| worktrees.get("actions"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for action in actions {
            render_prune_action(&mut lines, action);
        }
    }
    let totals = value.get("totals");
    if let Some(totals) = totals {
        let count = |key: &str| totals.get(key).and_then(Value::as_u64).unwrap_or(0);
        let removed = count("removed");
        let eligible = count("eligible");
        let absent = count("absent");
        let kept = count("kept_active")
            + count("kept_unknown")
            + count("kept_protected")
            + count("kept_issue");
        let failed = count("failed");
        lines.row(format!(
            "removed {removed}, eligible {eligible}, absent {absent}, kept {kept}, removal failures {failed}"
        ));
        lines.row(format!(
            "kept reasons: active {}, protected {}, issue-state {}, unknown {}",
            count("kept_active"),
            count("kept_protected"),
            count("kept_issue"),
            count("kept_unknown")
        ));
    }
    // The stale-lease recovery stays visible in the aggregate.
    let (candidates, released) = repos
        .into_iter()
        .flatten()
        .filter_map(|repo| repo.get("leases"))
        .fold((0u64, 0u64), |(candidates, released), leases| {
            (
                candidates
                    + leases
                        .get("candidates")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                released + leases.get("released").and_then(Value::as_u64).unwrap_or(0),
            )
        });
    if candidates > 0 || released > 0 {
        lines.row(format!(
            "leases: {candidates} stale candidate(s), {released} released"
        ));
    }
    let skipped = repos
        .into_iter()
        .flatten()
        .filter(|repo| repo.get("skipped_reason").is_some())
        .count();
    if skipped > 0 {
        lines.row(format!("skipped repositories: {skipped}"));
    }
    lines.finish()
}

/// Render one prune action. The removals, the failures, and the kept
/// directories keep their actionable reason, and every row wraps to the
/// terminal width without truncating the path or the reason.
fn render_prune_action(lines: &mut Lines, action: &Value) {
    let path = action
        .get("worktree_path")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let path = shorten_path(path);
    let result = action.get("result").and_then(Value::as_str).unwrap_or("");
    let message = action
        .get("message")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty());
    match result {
        "pruned" => lines.row(format!("removed {path}")),
        "prunable" => lines.row(format!("prunable {path}")),
        "failed" => match message {
            Some(message) => lines.row(format!("failed {path}: {message}")),
            None => lines.row(format!("failed {path}")),
        },
        "skipped_active" | "skipped_unknown" | "skipped_protected" | "skipped_issue" => {
            match message {
                Some(message) => lines.row(format!("kept {path}: {message}")),
                None => lines.row(format!("kept {path}")),
            }
        }
        // The absent directories and the no-op rows are reported by the
        // counts; they carry no action of their own.
        _ => {}
    }
}

/// Shorten a path relative to the working directory or the home directory
/// when the shorter form still names the same path. The lookup is read-only.
fn shorten_path(path: &str) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        let cwd = cwd.display().to_string();
        if let Some(rest) = path
            .strip_prefix(&cwd)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            return format!("./{rest}");
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy().into_owned();
        if let Some(rest) = path
            .strip_prefix(&home)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            return format!("~/{rest}");
        }
    }
    path.to_owned()
}
