//! The curated worktree views: the directory list, the probe, the acquire
//! outcome, and the prune report.
//!
//! Every view renders the directory identities, associations, paths, and
//! outcomes the command already resolved. A path is shortened relative to the
//! working directory or the home directory only when the shorter form still
//! names the same path; otherwise the full path stays, on its own line.

use serde_json::Value;

use crate::output::context::OutputContext;
use crate::output::render::Lines;

/// The curated `worktree list` and `worktree status` rendering.
pub(crate) fn render_directory_list(value: &Value, context: &OutputContext) -> String {
    let paged = value.get("page").is_some();
    let Some(directories) = value
        .get(if paged { "items" } else { "directories" })
        .and_then(Value::as_array)
    else {
        return super::generic::render(value, context);
    };
    let mut lines = Lines::new(context);
    lines.heading(&directory_summary(directories));
    if paged {
        if let Some(identity) = value.get("repo_identity").and_then(Value::as_str) {
            lines.field(0, "Repository", identity);
        }
        if let Some(issue) = value.get("issue").and_then(Value::as_u64) {
            lines.field(0, "Issue", &format!("#{issue}"));
        }
    }
    if directories.is_empty() {
        lines.blank();
        lines.text(
            0,
            if paged
                && value
                    .get("total_count")
                    .and_then(Value::as_u64)
                    .is_some_and(|total| total > 0)
            {
                "No directories on this page."
            } else {
                "No worktree directories are recorded for this scope."
            },
        );
        return lines.finish();
    }
    lines.blank();
    // A narrow terminal stacks the entries instead of overflowing a table.
    let stacked = !entries_fit(directories, context.width);
    for directory in directories {
        render_directory(&mut lines, directory, stacked);
    }
    lines.finish()
}

/// The count and association summary of a directory list.
fn directory_summary(directories: &[Value]) -> String {
    let total = directories.len();
    let noun = if total == 1 {
        "directory"
    } else {
        "directories"
    };
    let tasks: usize = directories
        .iter()
        .map(|directory| {
            directory
                .get("tasks")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        })
        .sum();
    let sessions: usize = directories
        .iter()
        .map(|directory| {
            directory
                .get("sessions")
                .and_then(Value::as_array)
                .map_or(0, Vec::len)
        })
        .sum();
    format!("{total} {noun}: {tasks} task association(s), {sessions} session(s)")
}

/// Whether the horizontal entry layout fits the terminal width.
fn entries_fit(directories: &[Value], width: usize) -> bool {
    directories.iter().all(|directory| {
        let line = entry_line(directory);
        console::measure_text_width(&line) <= width
    })
}

/// The horizontal entry line of one directory.
fn entry_line(directory: &Value) -> String {
    let id = directory
        .get("directory_id")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let issues = directory_issues(directory);
    let branch = directory
        .get("branch")
        .and_then(Value::as_str)
        .unwrap_or("-");
    if issues.is_empty() {
        format!("{id}  {branch}")
    } else {
        format!("{id}  {}  {branch}", issue_list(&issues))
    }
}

fn directory_issues(directory: &Value) -> Vec<u64> {
    let mut issues: Vec<u64> = directory
        .get("tasks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|task| task.get("issue").and_then(Value::as_u64))
        .chain(
            directory
                .get("legacy_issues")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|issue| issue.get("issue").and_then(Value::as_u64)),
        )
        .collect();
    issues.sort_unstable();
    issues.dedup();
    issues
}

fn issue_list(issues: &[u64]) -> String {
    issues
        .iter()
        .map(|issue| format!("#{issue}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render one directory entry.
fn render_directory(lines: &mut Lines, directory: &Value, stacked: bool) {
    let id = directory
        .get("directory_id")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let issues = directory_issues(directory);
    let branch = directory
        .get("branch")
        .and_then(Value::as_str)
        .unwrap_or("-");
    let path = directory.get("worktree_path").and_then(Value::as_str);
    let sessions = directory
        .get("sessions")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    if stacked {
        lines.row(id);
        if !issues.is_empty() {
            lines.field(1, "Issues", &issue_list(&issues));
        }
        lines.field(1, "Branch", branch);
        lines.field(1, "Sessions", &sessions.to_string());
        if let Some(path) = path {
            lines.field(1, "Path", &shorten_path(path));
        }
    } else {
        lines.raw(entry_line(directory));
        if let Some(path) = path {
            lines.text(1, &shorten_path(path));
        }
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
    if let Some(directory) = value
        .get("directory")
        .filter(|directory| directory.is_object())
    {
        let id = directory
            .get("directory_id")
            .and_then(Value::as_str)
            .unwrap_or("-");
        let issues = directory_issues(directory);
        let detail = if issues.is_empty() {
            id.to_owned()
        } else {
            format!("{id} ({})", issue_list(&issues))
        };
        lines.field(0, "Directory", &detail);
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
    lines.heading("Worktree directory");
    lines.blank();
    if let Some(id) = value.get("directory_id").and_then(Value::as_str) {
        lines.field(0, "Directory", id);
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
///
/// The default view is the fixed-size summary: the aggregate counts, the
/// kept-reason breakdown, and at most a few failure examples. The per
/// directory rows are the verbose detail, selected by the `__verbose` marker
/// the prune command sets on the rendered copy (the machine document never
/// carries it).
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
        lines.text(0, "no repositories with recorded worktree directories");
    }
    let totals = value.get("totals");
    if let Some(totals) = totals {
        let count = |key: &str| totals.get(key).and_then(Value::as_u64).unwrap_or(0);
        let removed = count("removed");
        let eligible = count("eligible");
        let absent = count("absent");
        let kept = count("kept_protected") + count("kept_issue");
        let failed = count("failed");
        // A preview reports what it would remove; it never claims a removal.
        let counts = if dry_run {
            format!(
                "would remove {eligible}, absent {absent}, kept {kept}, removal failures {failed}"
            )
        } else {
            format!(
                "removed {removed}, eligible {eligible}, absent {absent}, kept {kept}, removal \
                 failures {failed}"
            )
        };
        lines.row(counts);
        let mut reasons = std::collections::BTreeMap::<&str, usize>::new();
        for repo in repos.into_iter().flatten() {
            let Some(actions) = repo
                .get("worktrees")
                .and_then(|worktrees| worktrees.get("actions"))
                .and_then(Value::as_array)
            else {
                continue;
            };
            for action in actions {
                if matches!(
                    action.get("result").and_then(Value::as_str),
                    Some("skipped_protected" | "skipped_issue")
                ) {
                    let code = action
                        .get("reason_code")
                        .and_then(Value::as_str)
                        .unwrap_or("other_protected");
                    *reasons.entry(code).or_default() += 1;
                }
            }
        }
        if !reasons.is_empty() {
            lines.row(format!(
                "kept reasons: {}",
                reasons
                    .iter()
                    .map(|(code, count)| format!("{code} {count}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
            for category in [
                crate::cli::issue_closure::LookupFailure::MissingConfiguration,
                crate::cli::issue_closure::LookupFailure::AuthenticationDenied,
                crate::cli::issue_closure::LookupFailure::ScopeMismatch,
                crate::cli::issue_closure::LookupFailure::ProviderUnavailable,
            ] {
                if reasons.contains_key(category.code()) {
                    lines.row(format!("hint: {}", category.hint()));
                }
            }
        }
    }
    let verbose = value
        .get(VERBOSE_MARKER)
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if verbose {
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
    } else {
        render_prune_failures(&mut lines, repos.into_iter().flatten());
    }
    let totals = value.get("totals");
    if let Some(totals) = totals {
        let count = |key: &str| totals.get(key).and_then(Value::as_u64).unwrap_or(0);
        let absent = count("absent");
        if absent > 0 {
            lines.row(format!(
                "absent: {absent} registration(s) {}",
                if dry_run {
                    "would be reclaimed"
                } else {
                    "reclaimed"
                }
            ));
        }
    }
    let skipped = repos
        .into_iter()
        .flatten()
        .filter(|repo| repo.get("skipped_reason").is_some())
        .count();
    if skipped > 0 {
        lines.row(format!("skipped repositories: {skipped}"));
    }
    if verbose {
        render_prune_timings(&mut lines, value.get("timings"));
    }
    lines.finish()
}

/// The bounded timing breakdown of the verbose rendering: the wall clock of
/// the pass, the fixed stage list, the fixed operation list, and the note
/// that the two families overlap instead of adding up.
fn render_prune_timings(lines: &mut Lines, timings: Option<&Value>) {
    let Some(timings) = timings else {
        return;
    };
    let total = timings
        .get("total_ms")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    lines.row(format!("timing: total {total:.3}ms"));
    lines.row(format!(
        "timing stages: {}",
        timing_entries(timings.get("stages"))
    ));
    lines.row(format!(
        "timing operations: {}",
        timing_entries(timings.get("operations"))
    ));
    lines
        .row("timing: stages and operations overlap, so their durations are not an additive total");
}

/// The measured entries of one timing family, in the fixed order the report
/// lists them.
fn timing_entries(entries: Option<&Value>) -> String {
    let Some(entries) = entries.and_then(Value::as_object) else {
        return "none".to_owned();
    };
    entries
        .iter()
        .map(|(name, entry)| {
            let calls = entry.get("calls").and_then(Value::as_u64).unwrap_or(0);
            let duration = entry
                .get("duration_ms")
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            format!("{name} {calls} call(s) {duration:.3}ms")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The marker the verbose prune rendering is selected with. The prune command
/// sets it on the rendered copy only, so the machine document never carries it.
const VERBOSE_MARKER: &str = "__verbose";

/// The maximum number of failure examples the default summary shows.
const MAX_FAILURE_EXAMPLES: usize = 5;

/// The bounded failure examples of the default summary: the actionable
/// removals that failed, with the omitted count as a hint.
fn render_prune_failures<'a>(lines: &mut Lines, repos: impl Iterator<Item = &'a Value>) {
    let mut failures: Vec<(&str, &str)> = Vec::new();
    for repo in repos {
        let Some(actions) = repo
            .get("worktrees")
            .and_then(|worktrees| worktrees.get("actions"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for action in actions {
            if action.get("result").and_then(Value::as_str) != Some("failed") {
                continue;
            }
            let path = action
                .get("worktree_path")
                .and_then(Value::as_str)
                .unwrap_or("-");
            let message = action
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("removal failed");
            failures.push((path, message));
        }
    }
    for (path, message) in failures.iter().take(MAX_FAILURE_EXAMPLES) {
        lines.row(format!("failed {}: {message}", shorten_path(path)));
    }
    let omitted = failures.len().saturating_sub(MAX_FAILURE_EXAMPLES);
    if omitted > 0 {
        lines.row(format!(
            "{omitted} further removal failure(s) not shown; run with --verbose for every one"
        ));
    }
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
        "skipped_protected" | "skipped_issue" => match message {
            Some(message) => lines.row(format!("kept {path}: {message}")),
            None => lines.row(format!("kept {path}")),
        },
        // Registered directory paths that no longer exist are complete detail
        // in verbose mode; the compact default still reports them by count only.
        "absent" => match message {
            Some(message) => lines.row(format!("absent {path}: {message}")),
            None => lines.row(format!("absent {path}")),
        },
        // The no-op rows are reported by the counts.
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
