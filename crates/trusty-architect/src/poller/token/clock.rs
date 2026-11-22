//! Hour keys and transcript timestamps: `hour_of`, `hour_fp`, `parse_ts`.
//!
//! Why: hour keys are part of the shared state and of fingerprints, so they
//! must be byte-identical to `token_watch.py`'s.
//! What: [`hour_of`] is `%Y-%m-%dT%H` in UTC, [`hour_fp`] the same without
//! `-` and `T`; [`parse_ts`] reads an ISO time with an optional fraction and
//! `Z` or `+00:00`, to fractional Unix seconds.
//! Test: `parse_ts_reads_fractions_and_offsets`.

use crate::classify::alerts::iso;
use crate::sched::slot::parse_iso;

/// `hour_of(ts)`: `YYYY-MM-DDTHH` in UTC.
pub fn hour_of(ts: f64) -> String {
    iso(ts.floor()).chars().take(13).collect()
}

/// `hour_fp(ts)`: `YYYYMMDDHH`.
pub fn hour_fp(ts: f64) -> String {
    compact(&hour_of(ts))
}

/// An hour key without `-` and `T`.
pub fn compact(hour: &str) -> String {
    hour.chars().filter(|c| *c != '-' && *c != 'T').collect()
}

/// `parse_ts(s)`: `None` for anything that is not a UTC ISO time.
pub fn parse_ts(s: &str) -> Option<f64> {
    let s = s
        .strip_suffix('Z')
        .or_else(|| s.strip_suffix("+00:00"))
        .unwrap_or(s);
    let (base, frac) = match s.split_once('.') {
        Some((b, f)) => (b, f),
        None => (s, ""),
    };
    if !frac.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let whole = parse_iso(base)? as f64;
    let frac = if frac.is_empty() {
        0.0
    } else {
        format!("0.{frac}").parse::<f64>().ok()?
    };
    Some(whole + frac)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ts_reads_fractions_and_offsets() {
        let t = parse_ts("2026-10-07T12:30:00.500Z").expect("ts");
        assert!((t - 1_791_376_200.5).abs() < 1e-6);
        assert_eq!(parse_ts("2026-10-07T12:30:00+00:00"), Some(1_791_376_200.0));
        assert_eq!(parse_ts("nope"), None);
        assert_eq!(hour_of(1_791_376_200.5), "2026-10-07T12");
        assert_eq!(hour_fp(1_791_376_200.5), "2026100712");
    }
}
