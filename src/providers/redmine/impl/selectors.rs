use crate::providers::api::ProviderError;
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::{RedmineIssueStatus, RedmineTracker, RedmineVersion};

impl RedmineProvider {
    /// Resolve an issue status by validated numeric id or exact name.
    /// Numeric zero is rejected; ambiguous names fail with an actionable
    /// error because a status update must land on exactly one status.
    pub fn select_status_by_value<'a>(
        statuses: &'a [RedmineIssueStatus],
        value: &str,
    ) -> Result<&'a RedmineIssueStatus, ProviderError> {
        if let Ok(id) = value.parse::<u64>() {
            if id == 0 {
                return Err(ProviderError::config(
                    "Redmine status id must be greater than zero",
                ));
            }
            return statuses
                .iter()
                .find(|status| status.id == id)
                .ok_or_else(|| {
                    ProviderError::config(format!("Redmine status id {id} was not found"))
                });
        }
        select_by_name(statuses.iter(), value, "status")
    }

    /// Resolve a tracker by validated numeric id or exact name using the
    /// same rules as status resolution so callers cannot silently target
    /// an ambiguous or unknown tracker.
    pub fn select_tracker<'a>(
        trackers: &'a [RedmineTracker],
        value: &str,
    ) -> Result<&'a RedmineTracker, ProviderError> {
        if let Ok(id) = value.parse::<u64>() {
            if id == 0 {
                return Err(ProviderError::config(
                    "Redmine tracker id must be greater than zero",
                ));
            }
            return trackers
                .iter()
                .find(|tracker| tracker.id == id)
                .ok_or_else(|| {
                    ProviderError::config(format!("Redmine tracker id {id} was not found"))
                });
        }
        select_by_name(trackers.iter(), value, "tracker")
    }

    /// Resolve a project version by validated numeric id or exact name so
    /// `--fixed-version` lands on exactly one version of the configured
    /// project. Zero ids are rejected like every other selector.
    pub fn select_version<'a>(
        versions: &'a [RedmineVersion],
        value: &str,
    ) -> Result<&'a RedmineVersion, ProviderError> {
        if let Ok(id) = value.parse::<u64>() {
            if id == 0 {
                return Err(ProviderError::config(
                    "Redmine version id must be greater than zero",
                ));
            }
            return versions
                .iter()
                .find(|version| version.id == id)
                .ok_or_else(|| {
                    ProviderError::config(format!("Redmine version id {id} was not found"))
                });
        }
        select_by_name(versions.iter(), value, "version")
    }
}

fn select_by_name<'a, I, T>(items: I, value: &str, kind: &str) -> Result<&'a T, ProviderError>
where
    I: IntoIterator<Item = &'a T>,
    T: NamedRecord,
{
    let matches = items
        .into_iter()
        .filter(|item| item.name() == value)
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [item] => Ok(item),
        [] => Err(ProviderError::config(format!(
            "Redmine {kind} name '{value}' was not found"
        ))),
        _ => Err(ProviderError::config(format!(
            "Redmine {kind} name '{value}' is ambiguous"
        ))),
    }
}

trait NamedRecord {
    fn name(&self) -> &str;
}

impl NamedRecord for RedmineIssueStatus {
    fn name(&self) -> &str {
        &self.name
    }
}

impl NamedRecord for RedmineTracker {
    fn name(&self) -> &str {
        &self.name
    }
}

impl NamedRecord for RedmineVersion {
    fn name(&self) -> &str {
        &self.name
    }
}
