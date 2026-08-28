//! GitLab human-duration formatter.

/// Format a positive second count as a GitLab human duration string.
/// GitLab's documented format is the concatenation of any subset of
/// `Nd`, `Nh`, `Nm`, `Ns` (for example `1h30m`, `45m`, `2d4h`). The
/// helper emits only the non-zero parts and rounds sub-second values
/// up so every duration represents at least one second.
pub(crate) fn format_gitlab_duration(seconds: i64) -> String {
    let mut total = seconds;
    if total < 0 {
        return "0s".to_owned();
    }
    if total == 0 {
        return "1s".to_owned();
    }
    let mut parts: Vec<String> = Vec::new();
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    let days = total / DAY;
    if days > 0 {
        parts.push(format!("{days}d"));
        total -= days * DAY;
    }
    let hours = total / HOUR;
    if hours > 0 {
        parts.push(format!("{hours}h"));
        total -= hours * HOUR;
    }
    let minutes = total / MINUTE;
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
        total -= minutes * MINUTE;
    }
    if total > 0 || parts.is_empty() {
        parts.push(format!("{total}s"));
    }
    parts.join("")
}

#[cfg(test)]
mod tests {
    use super::format_gitlab_duration;

    #[test]
    fn format_gitlab_duration_handles_every_part_with_zero_padding() {
        assert_eq!(format_gitlab_duration(0), "1s");
        assert_eq!(format_gitlab_duration(1), "1s");
        assert_eq!(format_gitlab_duration(60), "1m");
        assert_eq!(format_gitlab_duration(61), "1m1s");
        assert_eq!(format_gitlab_duration(3_600), "1h");
        assert_eq!(format_gitlab_duration(3_661), "1h1m1s");
        assert_eq!(format_gitlab_duration(5_400), "1h30m");
        assert_eq!(format_gitlab_duration(86_400), "1d");
        assert_eq!(format_gitlab_duration(86_400 + 5_400), "1d1h30m");
        assert_eq!(format_gitlab_duration(-1), "0s");
    }
}
