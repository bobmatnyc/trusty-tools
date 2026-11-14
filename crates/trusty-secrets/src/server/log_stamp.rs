//! The UTC timestamp on the server's start and exit lines (#11215).
//!
//! Why: the on-demand server appends to one stderr log across every start,
//! and its lines had no time, so a log could not place a start next to the
//! audit rows of the calls that followed it.
//! What: [`log_stamp`] renders a `SystemTime` as RFC 3339 UTC to the second,
//! e.g. `2026-11-14T15:20:01Z`, with no date library.
//! Test: `log_stamp_renders_rfc3339_utc`.

use std::time::{SystemTime, UNIX_EPOCH};

/// `at` as `YYYY-MM-DDTHH:MM:SSZ`; a time before the epoch renders as the
/// epoch.
pub fn log_stamp(at: SystemTime) -> String {
    let secs = at.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Howard Hinnant's `civil_from_days` for days since 1970-01-01, which is
/// never negative here.
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn log_stamp_renders_rfc3339_utc() {
        let at = |secs| UNIX_EPOCH + Duration::from_secs(secs);
        assert_eq!(log_stamp(at(0)), "1970-01-01T00:00:00Z");
        // 2026-11-14T15:20:01Z.
        assert_eq!(log_stamp(at(1_794_669_601)), "2026-11-14T15:20:01Z");
        // A leap day.
        assert_eq!(log_stamp(at(951_782_400)), "2000-02-29T00:00:00Z");
        assert_eq!(
            log_stamp(UNIX_EPOCH - Duration::from_secs(5)),
            "1970-01-01T00:00:00Z"
        );
    }
}
