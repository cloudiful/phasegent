use super::Storage;
use rusqlite::params;

/// Row fields for [`Storage::record_notification`]. Groups the
/// notification-delivery columns so the recorder takes one parameter
/// instead of seven positional arguments.
pub struct NotificationRecord<'a> {
    pub event: &'a str,
    pub channel: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub issue_id: Option<u64>,
    pub status: &'a str,
    pub error: Option<&'a str>,
}

impl Storage {
    /// Persist a structured notification intent before delivery. The
    /// row is the durable intent: triggers insert with `pending` (or
    /// `skipped` when notifications are disabled) and then update to
    /// `delivered`/`failed` after the network attempt. Titles/bodies
    /// are already bounded by the caller.
    pub fn record_notification(&self, record: &NotificationRecord<'_>) -> Result<i64, String> {
        let created_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.connection
            .execute(
                "INSERT INTO notification_deliveries (created_at, event, channel, title, body, issue_id, status, error) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    created_at,
                    record.event,
                    record.channel,
                    record.title,
                    record.body,
                    record.issue_id.map(|v| v as i64),
                    record.status,
                    record.error
                ],
            )
            .map_err(|error| format!("could not record notification: {error}"))?;
        Ok(self.connection.last_insert_rowid())
    }

    /// Update the delivery outcome for a previously recorded intent.
    /// Best-effort: callers ignore the result so a failed update can
    /// never break the workflow operation.
    pub fn update_notification_result(
        &self,
        row_id: i64,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), String> {
        self.connection
            .execute(
                "UPDATE notification_deliveries SET status = ?1, error = ?2 WHERE id = ?3",
                params![status, error, row_id],
            )
            .map_err(|error| format!("could not update notification: {error}"))?;
        Ok(())
    }
}
