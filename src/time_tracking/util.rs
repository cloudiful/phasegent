use crate::providers::forgejo::ForgejoError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_TIMER_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Round a positive exact-second duration up to a two-decimal hour. Redmine's
/// minimum granularity is 0.01 hours (36 seconds); one second therefore still
/// records as 0.01 hours, never as zero.
pub(crate) fn rounded_hours(elapsed_seconds: i64) -> f64 {
    if elapsed_seconds < 0 {
        return 0.0;
    }
    let hundredths = ((i128::from(elapsed_seconds) * 100 + 3599) / 3600).max(1);
    hundredths as f64 / 100.0
}

pub(crate) fn now_epoch_seconds() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
    .unwrap_or(i64::MAX)
}

pub(crate) fn format_unix_date(timestamp: i64) -> Result<String, ForgejoError> {
    let days = timestamp.div_euclid(86_400);
    // Howard Hinnant's civil_from_days algorithm, without adding a date
    // crate solely for this small projection.
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

pub(crate) fn bounded_error_message(message: &str) -> String {
    message.chars().take(512).collect()
}

pub(crate) fn generate_run_id() -> String {
    let timestamp = now_epoch_seconds();
    let counter = NEXT_TIMER_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("timer-{timestamp:x}-{}-{:x}", std::process::id(), counter)
}

pub(crate) fn generate_projection_token() -> String {
    let timestamp = now_epoch_seconds();
    let counter = NEXT_TIMER_COUNTER.fetch_add(1, Ordering::Relaxed);
    // Bounded, control-free, unique per caller invocation. The token is
    // persisted as the lease owner; only the holder may finalize the
    // projection. Hard-crash stale tokens are recoverable via the
    // explicit stale-reset path after the lease window expires.
    format!("proj-{timestamp:x}-{}-{:x}", std::process::id(), counter)
}
