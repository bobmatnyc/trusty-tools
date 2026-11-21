//! `tm doctor` compress-rows-per-day probe (#11033).
//!
//! Why: a hook that stops firing, or a session on an old `tm`, shows first as
//! a drop in daily `compress` rows. Nothing flagged it, so compression could
//! go dark while the lifetime totals still looked healthy.
//! What: [`check_compress_rows`] counts `compress` rows per UTC day for the 14
//! complete days ending yesterday (today is partial and would read as a drop
//! every morning). `Warn` when yesterday is more than 80% below the window's
//! median; `Ok` otherwise. The message carries rows/day, so `tm doctor
//! --json` does too. Read-only; never `Fail`.
//! Test: the `tests` module below.

use std::path::Path;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::core::savings_status::compress_rows_per_day;

/// The check's name, as `tm doctor --json` reports it.
const NAME: &str = "compress_rows_per_day";
/// Days in the comparison window.
const WINDOW_DAYS: u32 = 14;

/// Probe the savings ledger under `framework_root` against today's UTC date.
///
/// Test: covered through `build_check` — `a_steady_series_is_quiet`,
/// `a_drop_over_80_percent_warns`.
pub(super) fn check_compress_rows(framework_root: &Path) -> DoctorCheck {
    let today = chrono::Utc::now().date_naive();
    let yesterday = today.pred_opt().unwrap_or(today);
    build_check(compress_rows_per_day(
        framework_root,
        yesterday,
        WINDOW_DAYS,
    ))
}

/// Median of the counts (mean of the middle two on an even length).
fn median(counts: &[u64]) -> f64 {
    let mut sorted = counts.to_vec();
    sorted.sort_unstable();
    match sorted.len() {
        0 => 0.0,
        n if n % 2 == 1 => sorted[n / 2] as f64,
        n => (sorted[n / 2 - 1] + sorted[n / 2]) as f64 / 2.0,
    }
}

/// Fold the series into a verdict (pure).
///
/// What: `Warn` on a read error; `Warn` when the last day is under 20% of the
/// median and the median is above zero; `Ok` otherwise. Every arm lists
/// rows/day.
/// Test: `a_steady_series_is_quiet`, `a_drop_over_80_percent_warns`,
/// `an_unreadable_ledger_warns`.
fn build_check(series: Result<Vec<(String, u64)>, String>) -> DoctorCheck {
    let series = match series {
        Ok(series) => series,
        Err(e) => {
            return DoctorCheck::new(
                NAME,
                CheckStatus::Warn,
                format!("the savings ledger could not be read: {e}"),
            );
        }
    };
    let counts: Vec<u64> = series.iter().map(|(_, n)| *n).collect();
    let med = median(&counts);
    let rows: Vec<String> = series.iter().map(|(d, n)| format!("{d}={n}")).collect();
    let rows = rows.join(" ");
    let Some((last_day, last)) = series.last() else {
        return DoctorCheck::new(NAME, CheckStatus::Ok, "no days in window");
    };
    if med > 0.0 && (*last as f64) < med * 0.2 {
        return DoctorCheck::new(
            NAME,
            CheckStatus::Warn,
            format!(
                "compress rows fell on {last_day}: {last} vs a {WINDOW_DAYS}-day median of \
                 {med} (over 80% below) — check the compress hook and the installed tm \
                 version; rows/day: {rows}"
            ),
        );
    }
    DoctorCheck::new(
        NAME,
        CheckStatus::Ok,
        format!(
            "{last} compress rows on {last_day} ({WINDOW_DAYS}-day median {med}); rows/day: {rows}"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(counts: &[u64]) -> Result<Vec<(String, u64)>, String> {
        Ok(counts
            .iter()
            .enumerate()
            .map(|(i, n)| (format!("2026-11-{:02}", i + 1), *n))
            .collect())
    }

    #[test]
    fn a_steady_series_is_quiet() {
        let check = build_check(series(&[
            40, 42, 38, 41, 39, 40, 44, 37, 40, 41, 43, 39, 40, 35,
        ]));
        assert_eq!(check.status, CheckStatus::Ok, "{}", check.message);
        assert!(check.message.contains("2026-11-14=35"), "{}", check.message);
    }

    #[test]
    fn a_drop_over_80_percent_warns() {
        let check = build_check(series(&[
            40, 42, 38, 41, 39, 40, 44, 37, 40, 41, 43, 39, 40, 7,
        ]));
        assert_eq!(check.status, CheckStatus::Warn, "{}", check.message);
        assert!(
            check.message.contains("over 80% below"),
            "{}",
            check.message
        );
        assert!(
            check.message.contains("rows/day: 2026-11-01=40"),
            "{}",
            check.message
        );
    }

    #[test]
    fn an_unreadable_ledger_warns() {
        let check = build_check(Err("permission denied".to_string()));
        assert_eq!(check.status, CheckStatus::Warn);
        assert!(check.message.contains("permission denied"));
    }
}
