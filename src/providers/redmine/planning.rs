//! Focused CLI execution helpers for Redmine issue planning fields.
//!
//! This module keeps the tracker/planning-aware create and update paths
//! out of `cli.rs`: it validates the raw `--parent-issue`, `--fixed-version`,
//! `--start-date`, `--due-date`, `--estimated-hours`, and `--done-ratio`
//! values, resolves `--fixed-version` against the configured project's
//! Redmine versions, and issues a single create/PUT request. Invocations
//! without any planning or tracker flag keep the plain shared provider
//! path so legacy payloads stay byte-identical.

use crate::command::PlanningOptions;
use crate::providers::api::{ForgejoError, IssueSummary};
use crate::providers::redmine::model::IssuePlanning;
use crate::providers::{IssueProvider, ProviderDispatcher, RedmineProvider};

/// Validate raw planning values and resolve `--fixed-version` by exact
/// version name or numeric id within the configured project. Numeric
/// ranges and date shapes are rejected before any write; version
/// resolution is a read-only lookup, so a rejected value never reaches an
/// issue write. Forgejo providers reject every Redmine-only planning
/// flag with a structured not-supported error before any network
/// access. GitLab accepts the `--tracker` and `--estimated-hours`
/// flags (the latter maps to GitLab's native `time_estimate` endpoint)
/// but rejects every other Redmine planning field.
pub(crate) fn resolve_planning(
    provider: &ProviderDispatcher,
    options: &PlanningOptions,
) -> Result<IssuePlanning, ForgejoError> {
    if options.is_empty() {
        return Ok(IssuePlanning::default());
    }
    match provider {
        ProviderDispatcher::Gitlab(_) => {
            // GitLab accepts `--estimated-hours` (mapped to the
            // native `time_estimate` endpoint) but rejects every
            // other Redmine planning field. Tracker is a label-only
            // operation that goes through the existing label path,
            // not through `resolve_planning`.
            if options.parent_issue.is_some() {
                return Err(ForgejoError::config(
                    "GitLab issues do not support --parent-issue",
                ));
            }
            if options.fixed_version.is_some() {
                return Err(ForgejoError::config(
                    "GitLab issues do not support --fixed-version",
                ));
            }
            if options.start_date.is_some() {
                return Err(ForgejoError::config(
                    "GitLab issues do not support --start-date",
                ));
            }
            if options.due_date.is_some() {
                return Err(ForgejoError::config(
                    "GitLab issues do not support --due-date",
                ));
            }
            if options.done_ratio.is_some() {
                return Err(ForgejoError::config(
                    "GitLab issues do not support --done-ratio",
                ));
            }
            // `--estimated-hours` is intentionally accepted: the
            // planning CLI returns an empty IssuePlanning for GitLab so
            // the create/update path stays symmetric; the provider
            // caller is responsible for forwarding `estimated_hours`
            // through the `time_estimate` endpoint after the issue
            // body is written.
        }
        ProviderDispatcher::Forgejo(_) => {
            return Err(ForgejoError::not_supported(
                "forgejo",
                "issue planning fields",
            ));
        }
        ProviderDispatcher::Redmine(_) => {}
    }
    let parent_issue_id = match &options.parent_issue {
        None => None,
        Some(value) => Some(parse_positive(value, "parent issue id")?),
    };
    let estimated_hours = match &options.estimated_hours {
        None => None,
        Some(value) => Some(parse_estimated_hours(value)?),
    };
    let done_ratio = match &options.done_ratio {
        None => None,
        Some(value) => Some(parse_done_ratio(value)?),
    };
    let start_date = match &options.start_date {
        None => None,
        Some(value) => Some(parse_date(value, "start date")?),
    };
    let due_date = match &options.due_date {
        None => None,
        Some(value) => Some(parse_date(value, "due date")?),
    };

    // Version selection is the only network step and happens after every
    // local validation passed, so malformed values can never trigger a write
    // (or even a lookup). Redmine-only.
    let fixed_version_id = match &options.fixed_version {
        None => None,
        Some(value) => {
            let redmine = redmine_provider(provider)?;
            let versions = redmine.list_versions()?;
            Some(RedmineProvider::select_version(&versions, value)?.id)
        }
    };

    Ok(IssuePlanning {
        parent_issue_id,
        fixed_version_id,
        start_date,
        due_date,
        estimated_hours,
        done_ratio,
    })
}

/// Create an issue with optional tracker plus native planning fields.
/// Forgejo keeps its original plain path when no provider-specific
/// flag is set. GitLab supports the tracker-only path (which becomes
/// a `type::bug` / `type::feature` label) and accepts
/// `--estimated-hours` (forwarded through the native `time_estimate`
/// endpoint) but rejects every other Redmine planning flag with a
/// structured not-supported error.
pub(crate) fn create_issue(
    provider: &ProviderDispatcher,
    title: &str,
    body: &str,
    tracker: Option<&str>,
    planning_options: &PlanningOptions,
) -> Result<IssueSummary, ForgejoError> {
    let planning = resolve_planning(provider, planning_options)?;
    let needs_provider_specific = tracker.is_some() || !planning.is_empty();
    if !needs_provider_specific {
        return provider.create_issue(title, body);
    }
    match provider {
        ProviderDispatcher::Redmine(redmine) => {
            let tracker_id = match tracker {
                None => None,
                Some(value) => {
                    let trackers = redmine.list_trackers()?;
                    Some(RedmineProvider::select_tracker(&trackers, value)?.id)
                }
            };
            if planning.is_empty() {
                match tracker_id {
                    Some(tracker_id) => redmine.create_issue_with_tracker(title, body, tracker_id),
                    None => redmine.create_issue(title, body),
                }
            } else {
                redmine.create_issue_with_planning(title, body, tracker_id, &planning)
            }
        }
        ProviderDispatcher::Gitlab(gitlab) => {
            // GitLab accepts the tracker label and the
            // `--estimated-hours` flag (forwarded through the native
            // `time_estimate` endpoint). The body PUT path above only
            // needs the label; the estimate is applied in a separate
            // request right after the create call so the shared label
            // path stays untouched.
            let labels = match tracker {
                None => Vec::new(),
                Some(value) => gitlab.tracker_label_list(value)?,
            };
            let summary = gitlab.create_issue_with_labels(title, body, &labels)?;
            if let Some(hours) = planning.estimated_hours {
                let seconds = (hours * 3600.0).round() as i64;
                if seconds > 0 {
                    gitlab.set_time_estimate(summary.number, seconds)?;
                }
            }
            Ok(summary)
        }
        ProviderDispatcher::Forgejo(_) => Err(ForgejoError::not_supported(
            "forgejo",
            "issue tracker / planning fields",
        )),
    }
}

/// Update an issue body with optional tracker re-target plus native
/// planning fields in one atomic PUT. Forgejo keeps its original
/// plain path when no provider-specific flag is set. GitLab
/// supports the tracker-only path (which becomes a `type::bug` /
/// `type::feature` label add) and accepts `--estimated-hours`
/// (forwarded through the native `time_estimate` endpoint) but
/// rejects every other Redmine planning flag with a structured
/// not-supported error.
pub(crate) fn update_body(
    provider: &ProviderDispatcher,
    number: u64,
    body: &str,
    tracker: Option<&str>,
    planning_options: &PlanningOptions,
) -> Result<IssueSummary, ForgejoError> {
    let planning = resolve_planning(provider, planning_options)?;
    let needs_provider_specific = tracker.is_some() || !planning.is_empty();
    if !needs_provider_specific {
        return provider.update_body(number, body);
    }
    match provider {
        ProviderDispatcher::Redmine(redmine) => {
            let tracker_id = match tracker {
                None => None,
                Some(value) => {
                    let trackers = redmine.list_trackers()?;
                    Some(RedmineProvider::select_tracker(&trackers, value)?.id)
                }
            };
            if planning.is_empty() {
                match tracker_id {
                    Some(tracker_id) => redmine.update_body_with_tracker(number, body, tracker_id),
                    None => redmine.update_body(number, body),
                }
            } else {
                redmine.update_body_with_planning(number, body, tracker_id, &planning)
            }
        }
        ProviderDispatcher::Gitlab(gitlab) => {
            let labels = match tracker {
                None => Vec::new(),
                Some(value) => gitlab.tracker_label_list(value)?,
            };
            let summary = gitlab.update_body_with_labels(number, body, &labels)?;
            if let Some(hours) = planning.estimated_hours {
                let seconds = (hours * 3600.0).round() as i64;
                if seconds > 0 {
                    gitlab.set_time_estimate(number, seconds)?;
                }
            }
            Ok(summary)
        }
        ProviderDispatcher::Forgejo(_) => Err(ForgejoError::not_supported(
            "forgejo",
            "issue tracker / planning fields",
        )),
    }
}

/// Extract the concrete Redmine provider from the dispatcher for
/// Redmine-only operations that are not part of the shared issue trait.
fn redmine_provider(provider: &ProviderDispatcher) -> Result<&RedmineProvider, ForgejoError> {
    match provider {
        ProviderDispatcher::Redmine(redmine) => Ok(redmine),
        ProviderDispatcher::Forgejo(_) => Err(ForgejoError::not_supported(
            "forgejo",
            "issue planning fields",
        )),
        // Phase-4 GitLab provider: the GitLab planning surface is
        // narrower than Redmine's (only `--tracker` and
        // `--estimated-hours` are supported), and the dispatch above
        // already validated every other flag before reaching this
        // helper. Reaching this branch means the caller asked for a
        // Redmine-only field; surface a structured not-supported error
        // so the failure mode stays symmetric with the old behaviour.
        ProviderDispatcher::Gitlab(_) => Err(ForgejoError::not_supported(
            "gitlab",
            "issue planning fields",
        )),
    }
}

fn parse_positive(value: &str, field: &'static str) -> Result<u64, ForgejoError> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| ForgejoError::config(format!("Redmine {field} must be a positive integer")))?;
    if parsed == 0 {
        return Err(ForgejoError::config(format!(
            "Redmine {field} must be greater than zero"
        )));
    }
    Ok(parsed)
}

fn parse_estimated_hours(value: &str) -> Result<f64, ForgejoError> {
    let parsed = value.parse::<f64>().map_err(|_| {
        ForgejoError::config("Redmine estimated hours must be a non-negative number")
    })?;
    if !parsed.is_finite() || parsed < 0.0 {
        return Err(ForgejoError::config(
            "Redmine estimated hours must be a non-negative number",
        ));
    }
    Ok(parsed)
}

fn parse_done_ratio(value: &str) -> Result<u64, ForgejoError> {
    // done_ratio is a 0-100 percentage; 0% is a valid default state, so
    // only values above 100 are rejected.
    let parsed = value
        .parse::<u64>()
        .map_err(|_| ForgejoError::config("Redmine done ratio must be between 0 and 100"))?;
    if parsed > 100 {
        return Err(ForgejoError::config(
            "Redmine done ratio must be between 0 and 100",
        ));
    }
    Ok(parsed)
}

/// Validate the strict zero-padded `YYYY-MM-DD` shape Redmine expects for
/// date fields, including real calendar rules (month lengths and leap
/// years) so impossible dates are rejected locally before any write.
fn parse_date(value: &str, field: &'static str) -> Result<String, ForgejoError> {
    let invalid =
        || ForgejoError::config(format!("Redmine {field} must use the YYYY-MM-DD format"));
    let parts = value.split('-').collect::<Vec<_>>();
    let [year, month, day] = parts.as_slice() else {
        return Err(invalid());
    };
    // Strict shape: four-digit year plus two-digit month/day. Non-padded
    // forms like `2026-1-1` must not reach the server.
    if year.len() != 4
        || !year.bytes().all(|byte| byte.is_ascii_digit())
        || month.len() != 2
        || !month.bytes().all(|byte| byte.is_ascii_digit())
        || day.len() != 2
        || !day.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid());
    }
    let year: u32 = year.parse().map_err(|_| invalid())?;
    let month: u32 = month.parse().map_err(|_| invalid())?;
    let day: u32 = day.parse().map_err(|_| invalid())?;
    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return Err(invalid());
    }
    Ok(value.to_owned())
}

fn is_leap_year(year: u32) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

fn days_in_month(year: u32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}
