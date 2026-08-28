//! Spent-time / time-estimate DTOs and confirmation logic.

use serde::{Deserialize, Serialize};

/// Request payload for `POST /projects/:id/issues/:iid/add_spent_time`.
/// GitLab's documented `summary` is a free-text label the user can use
/// to disambiguate individual entries; we use it as the durable
/// run-marker anchor so retries can be reconciled without inventing a
/// fake remote id.
#[derive(Debug, Serialize)]
pub(crate) struct NewSpentTime<'a> {
    pub duration: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<&'a str>,
}

/// Request payload for `POST /projects/:id/issues/:iid/time_estimate`.
/// GitLab stores a numeric second-precision estimate; the human-format
/// `duration` is a string so callers do not have to multiply hours by
/// 3600 themselves.
#[derive(Debug, Serialize)]
pub(crate) struct NewTimeEstimate<'a> {
    pub duration: &'a str,
}

/// Outcome of a single `add_spent_time` or `set_time_estimate`
/// request. GitLab REST v4 returns the updated issue totals so the
/// caller can render the new state without a follow-up GET, but it
/// does NOT return a per-entry identifier that the orchestrator
/// could persist as a remote id and reconcile against on retry; the
/// local SQLite ledger is therefore the sole idempotency marker for
/// the timer path, and `time_entry_id` is intentionally left `None`
/// after a successful GitLab projection.
///
/// Three response shapes are accepted by the decoder:
///
/// 1. The flat documented shape returned by older GitLab releases:
///    `{ "seconds": …, "human_readable": …, "total_seconds": …,
///    "total_human_readable": … }`.
/// 2. The issue-shaped body returned by GitLab 19.x on the live
///    `https://gitlab.example.com/19.2` instance for some
///    endpoints: the response echoes the full `ApiIssue` payload
///    with the running totals wrapped under a nested `time_stats`
///    block (`{ "time_stats": { "total_time_spent",
///    "time_estimate", "human_total_time_spent",
///    "human_time_estimate", ... } }`).
/// 3. The top-level time-stats body returned by GitLab 19.x for
///    `POST /projects/:id/issues/:iid/add_spent_time` and
///    `POST /projects/:id/issues/:iid/time_estimate`: the response
///    is a flat object whose top-level fields are the time-stats
///    totals (`{ "time_estimate", "total_time_spent",
///    "human_time_estimate", "human_total_time_spent" }`). This
///    shape was confirmed live against project 3 issue 5 on the
///    `https://gitlab.example.com/19.2` instance.
///
/// Every variant is parsed without inventing a remote id, and the
/// documented flat serialised projection stays a 4-key object via
/// `skip_serializing` (no condition) on every wire-compatibility
/// field plus `skip_serializing_if = "Option::is_none"` on the
/// original 4 flat keys; the round-trip contract test in
/// `gitlab_contract_tests.rs` keeps pinning the documented shape.
/// Use [`Self::is_confirmed`] to decide whether the response
/// confirms a successful write regardless of the shape GitLab
/// happened to return.
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ApiSpentTimeSummary {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_readable: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_seconds: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_human_readable: Option<String>,
    #[serde(default, skip_serializing)]
    pub time_estimate: Option<i64>,
    #[serde(default, skip_serializing)]
    pub total_time_spent: Option<i64>,
    #[serde(default, skip_serializing)]
    pub human_time_estimate: Option<String>,
    #[serde(default, skip_serializing)]
    pub human_total_time_spent: Option<String>,
    /// Issue-shaped body returned by GitLab 19.x on the live
    /// `https://gitlab.example.com/19.2` instance. The flat
    /// fields above stay `None` when GitLab returns the wrapped
    /// shape; callers MUST go through [`Self::is_confirmed`] so the
    /// projection handles either form. The field is deserialised
    /// only; `skip_serializing` (no condition) keeps it out of the
    /// serialised projection so the documented 4-key round-trip
    /// contract stays stable regardless of the input shape.
    #[serde(default, skip_serializing)]
    pub time_stats: Option<ApiIssueTimeStats>,
}

/// Subset of the `time_stats` block carried by the GitLab issue
/// payload. Only the four fields the projection uses for confirmation
/// are decoded; any additional server-side fields are silently
/// ignored so a future GitLab release can extend the payload without
/// breaking the client. `Serialize` is derived so the parent
/// `ApiSpentTimeSummary` (which derives both `Deserialize` and
/// `Serialize` for round-trip contract coverage) keeps compiling;
/// the inner field is skipped during serialisation by the parent,
/// so `Serialize` is never observed in the wire format.
#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ApiIssueTimeStats {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_estimate: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_time_spent: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_time_estimate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub human_total_time_spent: Option<String>,
}

impl ApiSpentTimeSummary {
    /// True when the response confirms a successful spent-time or
    /// time-estimate write. Accepts every response shape the live
    /// GitLab 19.x instance has been observed to return:
    ///
    /// * The flat documented shape: `seconds` or `total_seconds`
    ///   populated.
    /// * The GitLab 19.x issue-shaped body: `time_stats` carries a
    ///   non-null `total_time_spent` (for `add_spent_time`) or
    ///   `time_estimate` (for `set_time_estimate`).
    /// * The top-level time-stats body: `total_time_spent`
    ///   populated (for `add_spent_time`) or `time_estimate`
    ///   populated (for `set_time_estimate`).
    ///
    /// A fully empty / unknown-shape response stays `false` so the
    /// retry path keeps its structured `unconfirmed` semantics for
    /// genuinely ambiguous results; failure and already-synced
    /// ordering are handled by the caller (see
    /// [`crate::time_tracking_cli::project_run_with_gitlab_provider`]).
    pub(crate) fn is_confirmed(&self) -> bool {
        self.seconds.is_some()
            || self.total_seconds.is_some()
            || self.time_estimate.is_some()
            || self.total_time_spent.is_some()
            || self
                .time_stats
                .as_ref()
                .and_then(|stats| stats.total_time_spent)
                .is_some()
            || self
                .time_stats
                .as_ref()
                .and_then(|stats| stats.time_estimate)
                .is_some()
            || self
                .human_time_estimate
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            || self
                .human_total_time_spent
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            || self
                .time_stats
                .as_ref()
                .and_then(|stats| stats.human_time_estimate.as_deref())
                .is_some_and(|value| !value.trim().is_empty())
            || self
                .time_stats
                .as_ref()
                .and_then(|stats| stats.human_total_time_spent.as_deref())
                .is_some_and(|value| !value.trim().is_empty())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn spent_time_summary_is_confirmed_accepts_flat_nested_and_top_level_shapes() {
        use super::ApiSpentTimeSummary;

        // Fully empty body (GitLab 204 or unknown-shape failure):
        // not confirmed; the projection must keep its unconfirmed
        // semantics so the retry path stays safe.
        let empty: ApiSpentTimeSummary = serde_json::from_str("{}").unwrap();
        assert!(!empty.is_confirmed());

        // Documented flat shape: confirmed via total_seconds.
        let flat: ApiSpentTimeSummary = serde_json::from_str(
            r#"{"seconds":3600,"human_readable":"1h","total_seconds":3600,"total_human_readable":"1h"}"#,
        )
        .unwrap();
        assert!(flat.is_confirmed());

        // Live GitLab 19.x issue shape for spent time: confirmed via
        // time_stats.total_time_spent.
        let live_spent: ApiSpentTimeSummary = serde_json::from_str(
            r#"{
                "id": 7,
                "iid": 2,
                "state": "opened",
                "time_stats": {
                    "time_estimate": 0,
                    "total_time_spent": 2,
                    "human_time_estimate": null,
                    "human_total_time_spent": "2s"
                }
            }"#,
        )
        .unwrap();
        assert!(live_spent.is_confirmed());
        let stats = live_spent
            .time_stats
            .as_ref()
            .expect("live shape must decode nested time_stats");
        assert_eq!(stats.total_time_spent, Some(2));
        assert_eq!(stats.human_total_time_spent.as_deref(), Some("2s"));
        assert_eq!(stats.time_estimate, Some(0));
        // Flat totals stay None because the live response carries
        // them only under time_stats; callers must not mistake a
        // nested response for the flat shape.
        assert!(live_spent.seconds.is_none());
        assert!(live_spent.total_seconds.is_none());
        // The top-level time-stats fields also stay None for the
        // nested issue shape: serde looks for them at the JSON
        // root, not under time_stats.
        assert!(live_spent.total_time_spent.is_none());
        assert!(live_spent.time_estimate.is_none());

        // Live issue shape for time estimate: confirmed via
        // time_stats.time_estimate.
        let live_estimate: ApiSpentTimeSummary = serde_json::from_str(
            r#"{
                "id": 7,
                "iid": 2,
                "state": "opened",
                "time_stats": {
                    "time_estimate": 1800,
                    "total_time_spent": 0,
                    "human_time_estimate": "30m",
                    "human_total_time_spent": null
                }
            }"#,
        )
        .unwrap();
        assert!(live_estimate.is_confirmed());
        let stats = live_estimate.time_stats.as_ref().unwrap();
        assert_eq!(stats.time_estimate, Some(1_800));
        assert_eq!(stats.human_time_estimate.as_deref(), Some("30m"));

        // Live GitLab 19.x top-level time-stats response for
        // add_spent_time (confirmed against project 3 issue 5):
        // totals land at the JSON root and the nested time_stats
        // block stays None. is_confirmed must return true via the
        // top-level total_time_spent so the projection advances
        // sync_status to synced.
        let top_level_spent: ApiSpentTimeSummary = serde_json::from_str(
            r#"{
                "time_estimate": 0,
                "total_time_spent": 6,
                "human_time_estimate": null,
                "human_total_time_spent": "6s"
            }"#,
        )
        .unwrap();
        assert!(top_level_spent.is_confirmed());
        assert_eq!(top_level_spent.total_time_spent, Some(6));
        assert_eq!(
            top_level_spent.human_total_time_spent.as_deref(),
            Some("6s"),
        );
        assert_eq!(top_level_spent.time_estimate, Some(0));
        assert!(top_level_spent.human_time_estimate.is_none());
        // Nested block stays None because the response has no
        // wrapping issue body; legacy flat totals also stay None.
        assert!(top_level_spent.time_stats.is_none());
        assert!(top_level_spent.seconds.is_none());
        assert!(top_level_spent.total_seconds.is_none());

        // Live top-level time-stats response for set_time_estimate:
        // confirmed via top-level time_estimate.
        let top_level_estimate: ApiSpentTimeSummary = serde_json::from_str(
            r#"{
                "time_estimate": 1800,
                "total_time_spent": 0,
                "human_time_estimate": "30m",
                "human_total_time_spent": null
            }"#,
        )
        .unwrap();
        assert!(top_level_estimate.is_confirmed());
        assert_eq!(top_level_estimate.time_estimate, Some(1_800));
        assert_eq!(
            top_level_estimate.human_time_estimate.as_deref(),
            Some("30m"),
        );
        assert!(top_level_estimate.time_stats.is_none());
    }

    #[test]
    fn spent_time_summary_serialization_keeps_documented_four_key_shape() {
        use super::ApiSpentTimeSummary;

        // The round-trip contract test in gitlab_contract_tests.rs
        // pins the documented 4-key shape. After adding the nested
        // time_stats field, the serialised projection must stay a
        // 4-key object: the field is skipped when None and the
        // decoder never invents a remote id.
        let flat: ApiSpentTimeSummary = serde_json::from_str(
            r#"{"seconds":3600,"human_readable":"1h","total_seconds":3600,"total_human_readable":"1h"}"#,
        )
        .unwrap();
        let value = serde_json::to_value(&flat).unwrap();
        let mut keys = value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        keys.sort();
        assert_eq!(
            keys,
            vec![
                "human_readable".to_owned(),
                "seconds".to_owned(),
                "total_human_readable".to_owned(),
                "total_seconds".to_owned(),
            ],
            "ApiSpentTimeSummary must not invent an id field beyond the documented totals",
        );

        // Top-level time-stats shape: the four wire-compatibility
        // fields stay None on the legacy keys (the response has no
        // seconds/total_seconds), and the four new top-level
        // fields plus time_stats are deserialised-only. The
        // serialised projection therefore carries no key at all,
        // which is consistent with the documented contract that
        // only the original 4 flat fields may appear.
        let top_level: ApiSpentTimeSummary = serde_json::from_str(
            r#"{
                "time_estimate": 0,
                "total_time_spent": 6,
                "human_time_estimate": null,
                "human_total_time_spent": "6s"
            }"#,
        )
        .unwrap();
        let value = serde_json::to_value(&top_level).unwrap();
        let keys = value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        assert!(
            keys.is_empty(),
            "top-level shape must not serialise any of the wire-compatibility fields: {keys:?}",
        );
    }
}
