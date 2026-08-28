use crate::providers::api::ForgejoError;
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::{
    RedmineTimeEntry, RedmineTimeEntryActivity, RedmineTimeEntryActivityCollection,
    RedmineTimeEntryCollection, RedmineTimeEntryResponse,
};

impl RedmineProvider {
    /// Resolve a time-entry activity without ever guessing. The preferred
    /// names are exact and case-sensitive; only a single default activity is
    /// acceptable. Duplicate preferred names and multiple defaults are
    /// configuration errors so hours cannot be silently misclassified.
    pub fn select_time_entry_activity(
        activities: &[RedmineTimeEntryActivity],
    ) -> Result<&RedmineTimeEntryActivity, ForgejoError> {
        for name in ["AI automation", "Development"] {
            let matches = activities
                .iter()
                .filter(|activity| activity.name == name)
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [] => {}
                [activity] => {
                    if activity.id == 0 {
                        return Err(ForgejoError::config(format!(
                            "Redmine time-entry activity name '{name}' has id zero"
                        )));
                    }
                    return Ok(activity);
                }
                _ => {
                    return Err(ForgejoError::config(format!(
                        "Redmine time-entry activity name '{name}' is ambiguous"
                    )));
                }
            }
        }

        let defaults = activities
            .iter()
            .filter(|activity| activity.is_default)
            .collect::<Vec<_>>();
        match defaults.as_slice() {
            [activity] if activity.id > 0 => Ok(activity),
            [] => Err(ForgejoError::config(
                "Redmine has no exact AI automation or Development activity and no default time-entry activity",
            )),
            _ => Err(ForgejoError::config(
                "Redmine has multiple default time-entry activities; set an exact AI automation or Development activity",
            )),
        }
    }

    /// List time-entry activities from Redmine's enumeration endpoint.
    pub fn list_time_entry_activities(
        &self,
    ) -> Result<Vec<RedmineTimeEntryActivity>, ForgejoError> {
        let response: RedmineTimeEntryActivityCollection = self.http.get(
            "enumerations/time_entry_activities.json",
            &[],
            "time entry activity list",
        )?;
        Ok(response.time_entry_activities)
    }

    /// Create one issue Time Entry. A 201 response may contain the created
    /// object; 204 or an empty 201 response is represented by `Ok(None)`.
    pub fn create_time_entry(
        &self,
        issue_id: u64,
        hours: f64,
        spent_on: &str,
        activity_id: u64,
        comments: &str,
    ) -> Result<Option<RedmineTimeEntry>, ForgejoError> {
        if issue_id == 0 {
            return Err(ForgejoError::config(
                "Redmine time entry issue id must be greater than zero",
            ));
        }
        if activity_id == 0 {
            return Err(ForgejoError::config(
                "Redmine time entry activity id must be greater than zero",
            ));
        }
        if !hours.is_finite() || hours <= 0.0 {
            return Err(ForgejoError::config(
                "Redmine time entry hours must be positive",
            ));
        }
        if spent_on.trim().is_empty() {
            return Err(ForgejoError::config(
                "Redmine time entry spent_on date cannot be empty",
            ));
        }
        if comments.trim().is_empty() {
            return Err(ForgejoError::config(
                "Redmine time entry comments cannot be empty",
            ));
        }
        let payload = crate::providers::redmine::model::RedmineNewTimeEntry {
            time_entry: crate::providers::redmine::model::RedmineNewTimeEntryFields {
                issue_id,
                hours,
                spent_on,
                activity_id,
                comments,
            },
        };
        let response: Option<RedmineTimeEntryResponse> =
            self.http
                .post_optional("time_entries.json", &payload, "time entry create")?;
        Ok(response
            .and_then(|response| response.time_entry)
            .filter(|entry| entry.id != 0))
    }

    /// List Time Entries visible to the current API key, optionally bounded
    /// by issue and date. Pagination uses the bounded safeguards shared by
    /// the other Redmine list operations.
    pub fn list_time_entries(
        &self,
        issue_id: u64,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Result<Vec<RedmineTimeEntry>, ForgejoError> {
        if issue_id == 0 {
            return Err(ForgejoError::config(
                "Redmine time entry issue id must be greater than zero",
            ));
        }
        let from = from.map(str::to_owned);
        let to = to.map(str::to_owned);
        self.http.paginate("time entry list", |http, offset| {
            let mut params = vec![
                ("issue_id", issue_id.to_string()),
                ("limit", super::PAGE_SIZE.to_string()),
                ("offset", offset.to_string()),
            ];
            if let Some(from) = &from {
                params.push(("from", from.clone()));
            }
            if let Some(to) = &to {
                params.push(("to", to.clone()));
            }
            let page: RedmineTimeEntryCollection =
                http.get("time_entries.json", &params, "time entry list")?;
            let signature = page
                .time_entries
                .iter()
                .map(|entry| entry.id.to_string())
                .collect::<Vec<_>>()
                .join(",");
            Ok((page.time_entries, page.total_count, page.limit, signature))
        })
    }

    /// Find a marker-matching Time Entry before retrying a projection. This
    /// closes the 204/empty-response window and also recovers a request that
    /// succeeded remotely but lost its response locally.
    pub fn find_time_entry_by_comments(
        &self,
        issue_id: u64,
        spent_on: &str,
        comments: &str,
    ) -> Result<Option<RedmineTimeEntry>, ForgejoError> {
        if comments.trim().is_empty() {
            return Err(ForgejoError::config(
                "Redmine time entry marker cannot be empty",
            ));
        }
        let entries = self.list_time_entries(issue_id, Some(spent_on), Some(spent_on))?;
        let mut matches = entries
            .into_iter()
            .filter(|entry| {
                entry.comments.as_deref() == Some(comments)
                    && entry
                        .issue
                        .as_ref()
                        .is_none_or(|issue| issue.id == issue_id)
                    && entry
                        .spent_on
                        .as_deref()
                        .is_none_or(|entry_date| entry_date == spent_on)
            })
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return Err(ForgejoError::config(format!(
                "multiple Redmine Time Entries match run marker '{}'",
                comments
            )));
        }
        Ok(matches.pop())
    }
}
