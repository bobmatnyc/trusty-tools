//! UTC instants at microsecond precision: the part of Python's `datetime`
//! that `usage.py` uses.
//!
//! Why: usage alert text and fingerprints carry times computed by `datetime`
//! arithmetic (`t + timedelta(hours=x)`, `.total_seconds()`), so the port
//! must round exactly where CPython rounds (#9801). The crate takes no date
//! dependency (#9171 A1), so the arithmetic is written here.
//! What: [`UtcTime`] holds microseconds since the epoch. [`UtcTime::from_unix`]
//! is `datetime.fromtimestamp(t, UTC)`; [`UtcTime::plus_hours`] is
//! `+ timedelta(hours=h)` (CPython's `accum` rounding); [`UtcTime::parse_iso`]
//! is `fromisoformat(s.replace("Z", "+00:00")).astimezone(UTC)` for the
//! extended forms; [`UtcTime::iso`] and [`UtcTime::ny`] are the two
//! `strftime` renderings `usage.py` prints. New York time uses the US rule in
//! force since 2007 (second Sunday of March to first Sunday of November).
//! Test: `utc_times_round_like_cpython`, `parse_iso_reads_the_extended_forms`,
//! `ny_follows_the_dst_rule` (below), and the goldens in `tests/parity_usage.rs`.

use crate::classify::alerts::{civil_from_days, days_from_civil, iso};
use crate::error::UsageError;

const US_PER_S: i64 = 1_000_000;
/// `datetime(1, 1, 1)` and `datetime.max`, as microseconds since the epoch.
const MIN_US: i64 = -62_135_596_800 * US_PER_S;
const MAX_US: i64 = 253_402_300_799 * US_PER_S + 999_999;
/// `timedelta.max.days`.
const MAX_DAYS: i128 = 999_999_999;

/// One instant, UTC, microsecond precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UtcTime(i64);

impl UtcTime {
    /// The instant `us` microseconds after the epoch, inside `datetime`'s range.
    ///
    /// # Errors
    /// [`UsageError::OutOfRange`] outside years 1-9999.
    pub fn from_micros(us: i64) -> Result<Self, UsageError> {
        if (MIN_US..=MAX_US).contains(&us) {
            Ok(Self(us))
        } else {
            Err(UsageError::OutOfRange)
        }
    }

    /// Microseconds since the epoch.
    pub fn micros(self) -> i64 {
        self.0
    }

    /// `datetime.fromtimestamp(t, timezone.utc)`: the fraction rounds to the
    /// microsecond, half to even.
    ///
    /// # Errors
    /// [`UsageError::OutOfRange`] for NaN, an infinity or a time outside
    /// years 1-9999.
    pub fn from_unix(t: f64) -> Result<Self, UsageError> {
        if !t.is_finite() || t.abs() > 1e15 {
            return Err(UsageError::OutOfRange);
        }
        let mut secs = t.trunc();
        let mut us = ((t - secs) * 1e6).round_ties_even();
        if us >= 1e6 {
            us -= 1e6;
            secs += 1.0;
        } else if us < 0.0 {
            us += 1e6;
            secs -= 1.0;
        }
        // #9801: both are integral and inside i64, but their product is not
        // (a time above about 9.2e12 s overflowed and panicked in debug).
        let total = (secs as i64)
            .checked_mul(US_PER_S)
            .and_then(|whole| whole.checked_add(us as i64))
            .ok_or(UsageError::OutOfRange)?;
        Self::from_micros(total)
    }

    /// `.timestamp()`: seconds since the epoch.
    pub fn timestamp(self) -> f64 {
        self.0 as f64 / 1e6
    }

    /// `(self - earlier).total_seconds()`.
    pub fn seconds_since(self, earlier: Self) -> f64 {
        (self.0 - earlier.0) as f64 / 1e6
    }

    /// `self + timedelta(hours=h)`, rounded as CPython's `timedelta`
    /// constructor rounds a float: the whole hours exactly, the fraction
    /// times 3.6e9 split again, and the last fraction half to even.
    ///
    /// # Errors
    /// [`UsageError::OutOfRange`] for a non-finite `h`, a span past
    /// `timedelta.max`, or a result outside years 1-9999.
    pub fn plus_hours(self, h: f64) -> Result<Self, UsageError> {
        if !h.is_finite() || h.abs() > 1e14 {
            return Err(UsageError::OutOfRange);
        }
        let whole = h.trunc();
        let scaled = (h - whole) * 3.6e9;
        let int2 = scaled.trunc();
        let leftover = scaled - int2;
        // Both are integral and bounded above, so the casts are exact.
        let mut us = whole as i128 * 3_600_000_000 + int2 as i128;
        let mut round = leftover.round();
        if (round - leftover).abs() == 0.5 {
            let odd = (us & 1) as f64;
            round = 2.0 * ((leftover + odd) * 0.5).round() - odd;
        }
        us += round as i128;
        if us.abs() / (86_400 * i128::from(US_PER_S)) > MAX_DAYS {
            return Err(UsageError::OutOfRange);
        }
        let sum = i128::from(self.0) + us;
        i64::try_from(sum)
            .map_err(|_| UsageError::OutOfRange)
            .and_then(Self::from_micros)
    }

    /// `strftime("%Y-%m-%dT%H:%M:%SZ")`: the microseconds are dropped.
    pub fn iso(self) -> String {
        iso(self.0.div_euclid(US_PER_S) as f64)
    }

    /// `astimezone(ZoneInfo("America/New_York")).strftime("%a %m-%d %H:%M")`.
    pub fn ny(self) -> String {
        const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
        let utc = self.0.div_euclid(US_PER_S);
        let local = utc + ny_offset(utc);
        let (days, rem) = (local.div_euclid(86_400), local.rem_euclid(86_400));
        let (_, month, day) = civil_from_days(days);
        let weekday = DAYS[(days + 4).rem_euclid(7) as usize];
        format!(
            "{weekday} {month:02}-{day:02} {:02}:{:02}",
            rem / 3600,
            rem % 3600 / 60
        )
    }

    /// Parses an extended ISO 8601 time with a UTC offset, after the
    /// prototype's `replace("Z", "+00:00")`: `YYYY-MM-DD`, any one separator
    /// character, `HH[:MM[:SS[.f...]]]`, then `+HH[:MM[:SS[.f...]]]` or `-...`.
    /// Fractional seconds past six digits are dropped, as Python drops them.
    ///
    /// # Errors
    /// [`UsageError::BadTime`] for any other text, including a time with no
    /// offset; [`UsageError::OutOfRange`] outside years 1-9999.
    pub fn parse_iso(text: &str) -> Result<Self, UsageError> {
        let bad = || UsageError::BadTime(text.to_owned());
        let s = text.replace('Z', "+00:00");
        let chars: Vec<char> = s.chars().collect();
        let date: String = chars.iter().take(10).collect();
        let (y, m, d) = parse_date(&date).ok_or_else(bad)?;
        if chars.len() <= 11 {
            return Err(bad());
        }
        let rest: String = chars[11..].iter().collect();
        let split = rest.find(['+', '-']).ok_or_else(bad)?;
        let (time, offset) = rest.split_at(split);
        let local = parse_clock(time).ok_or_else(bad)?;
        let sign = if offset.starts_with('-') { -1 } else { 1 };
        let off = parse_clock(&offset[1..]).ok_or_else(bad)?;
        if off >= 86_400 * US_PER_S {
            return Err(bad());
        }
        if !(1..=9999).contains(&y) {
            return Err(UsageError::OutOfRange);
        }
        let us = days_from_civil(y, m, d) * 86_400 * US_PER_S + local - sign * off;
        Self::from_micros(us)
    }
}

/// `YYYY-MM-DD` with a real calendar day.
fn parse_date(s: &str) -> Option<(i64, i64, i64)> {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (y, m, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    if !(1..=12).contains(&m) || d < 1 {
        return None;
    }
    let next = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    let len = days_from_civil(next.0, next.1, 1) - days_from_civil(y, m, 1);
    (d <= len).then_some((y, m, d))
}

/// `HH[:MM[:SS[.f...]]]` as microseconds; digits past six are dropped.
fn parse_clock(s: &str) -> Option<i64> {
    let (hms, frac) = match s.split_once(['.', ',']) {
        Some((a, f)) => (a, Some(f)),
        None => (s, None),
    };
    let parts: Vec<&str> = hms.split(':').collect();
    if parts.is_empty() || parts.len() > 3 || frac.is_some() && parts.len() != 3 {
        return None;
    }
    let mut units = [0i64; 3];
    for (i, p) in parts.iter().enumerate() {
        if p.len() != 2 || !p.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        units[i] = p.parse().ok()?;
    }
    if units[0] > 23 || units[1] > 59 || units[2] > 59 {
        return None;
    }
    let mut us = ((units[0] * 60 + units[1]) * 60 + units[2]) * US_PER_S;
    if let Some(f) = frac {
        if f.is_empty() || !f.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let six: String = f.chars().chain(std::iter::repeat('0')).take(6).collect();
        us += six.parse::<i64>().ok()?;
    }
    Some(us)
}

/// New York's offset from UTC, in seconds, at `utc` seconds since the epoch.
fn ny_offset(utc: i64) -> i64 {
    let (year, _, _) = civil_from_days(utc.div_euclid(86_400));
    let sunday = |month: i64, nth: i64| {
        let first = days_from_civil(year, month, 1);
        first + (7 - (first + 4).rem_euclid(7)) % 7 + 7 * (nth - 1)
    };
    // 02:00 local: 07:00 UTC in March (EST), 06:00 UTC in November (EDT).
    let start = sunday(3, 2) * 86_400 + 7 * 3600;
    let end = sunday(11, 1) * 86_400 + 6 * 3600;
    if (start..end).contains(&utc) {
        -4 * 3600
    } else {
        -5 * 3600
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type R = Result<(), UsageError>;

    #[test]
    fn utc_times_round_like_cpython() -> R {
        // datetime.fromtimestamp(1791400000.9999996, UTC) rolls to the next second.
        assert_eq!(
            UtcTime::from_unix(1_791_400_000.999_999_6)?.iso(),
            "2026-10-07T19:06:41Z"
        );
        let t = UtcTime::parse_iso("2026-10-07T16:15:00Z")?;
        // timedelta(hours=73 / (26 / 3.1)) from the masa pair: 2026-10-08T00:57:13.846154.
        let at = t.plus_hours(73.0 / (26.0 / (186.0 / 60.0)))?;
        assert_eq!(at.micros() % US_PER_S, 846_154);
        assert!(t.plus_hours(f64::INFINITY).is_err());
        assert!(t.plus_hours(1e13).is_err(), "past year 9999");
        assert_eq!(
            t.seconds_since(UtcTime::parse_iso("2026-10-07T13:09:00Z")?),
            11_160.0
        );
        Ok(())
    }

    #[test]
    fn parse_iso_reads_the_extended_forms() -> R {
        let base = UtcTime::parse_iso("2026-10-07T16:10:00Z")?;
        assert_eq!(UtcTime::parse_iso("2026-10-07T12:10:00-04:00")?, base);
        assert_eq!(UtcTime::parse_iso("2026-10-07 16:10+00:00")?, base);
        assert_eq!(
            UtcTime::parse_iso("2026-10-07T16+00:00")?.iso(),
            "2026-10-07T16:00:00Z"
        );
        let frac = UtcTime::parse_iso("2026-10-07T16:10:00.2500009Z")?;
        assert_eq!(frac.micros() - base.micros(), 250_000);
        for bad in [
            "garbage",
            "2026-10-07",
            "2026-10-07T16:10:00",
            "2026-02-30T00:00:00Z",
            "2026-10-07T24:00:00Z",
            "2026-10-07T16:10:00.Z",
            "2026-10-07T16:10:00+24:00",
        ] {
            assert!(UtcTime::parse_iso(bad).is_err(), "{bad}");
        }
        Ok(())
    }

    #[test]
    fn ny_follows_the_dst_rule() -> R {
        let ny = |s: &str| UtcTime::parse_iso(s).map(UtcTime::ny);
        assert_eq!(ny("2027-01-15T15:01:44Z")?, "Fri 01-15 10:01");
        assert_eq!(ny("2027-03-14T06:59:59Z")?, "Sun 03-14 01:59");
        assert_eq!(ny("2027-03-14T07:00:00Z")?, "Sun 03-14 03:00");
        assert_eq!(ny("2026-11-01T05:59:00Z")?, "Sun 11-01 01:59");
        assert_eq!(ny("2026-11-01T06:00:00Z")?, "Sun 11-01 01:00");
        assert_eq!(ny("2026-10-12T00:00:00Z")?, "Sun 10-11 20:00");
        Ok(())
    }
}
