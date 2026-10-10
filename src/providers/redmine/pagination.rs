//! Shared Redmine pagination completion rules.
//!
//! Every Redmine list helper requests one page at a time and must agree on
//! when the collection is complete, so the page-size constants and the
//! completion decision live here instead of being copied per call site.

use crate::providers::api::PhasegentError;

pub(crate) const PAGE_SIZE: usize = 100;
pub(crate) const MAX_PAGES: usize = 10_000;

/// Decide whether a fetched page completes the collection.
///
/// Completion rules, shared by every Redmine list helper so they cannot drift
/// apart:
///
/// - a non-empty page is complete when the declared `total_count` was reached,
///   or when the server reported no `total_count` and the page was short;
/// - an empty page is complete only when there is no positive unmet
///   `total_count`: an absent `total_count`, or one whose count the current
///   offset already covered. A positive `total_count` the empty page did not
///   reach is an incomplete enumeration error, including on the first page, so
///   a truncated response can never be mistaken for a complete set.
pub(crate) fn page_is_complete(
    operation: &str,
    offset: usize,
    count: usize,
    total_count: Option<usize>,
    response_limit: usize,
) -> Result<bool, PhasegentError> {
    if count == 0 {
        return match total_count {
            Some(total) if total > offset => Err(PhasegentError::pagination(
                operation,
                "Redmine returned an empty page before the declared total_count was reached",
            )),
            _ => Ok(true),
        };
    }
    Ok(
        total_count.is_some_and(|total| offset.saturating_add(count) >= total)
            || (total_count.is_none() && count < response_limit),
    )
}

#[cfg(test)]
mod tests {
    use super::page_is_complete;

    #[test]
    fn non_empty_page_completes_on_reached_total_or_short_unknown_page() {
        assert!(page_is_complete("op", 0, 5, Some(5), 100).unwrap());
        assert!(!page_is_complete("op", 0, 5, Some(6), 100).unwrap());
        assert!(!page_is_complete("op", 0, 5, Some(6), 3).unwrap());
        assert!(page_is_complete("op", 0, 5, None, 100).unwrap());
        assert!(!page_is_complete("op", 0, 100, None, 100).unwrap());
    }

    #[test]
    fn empty_page_with_positive_unmet_total_is_an_error() {
        let error = page_is_complete("op", 0, 0, Some(3), 100).unwrap_err();
        assert_eq!(error.json()["kind"], "pagination");
    }

    #[test]
    fn empty_page_is_fine_for_total_zero_or_absent_total() {
        assert!(page_is_complete("op", 0, 0, Some(0), 100).unwrap());
        assert!(page_is_complete("op", 0, 0, None, 100).unwrap());
        assert!(page_is_complete("op", 3, 0, Some(3), 100).unwrap());
    }
}
