//! UTC calendar arithmetic and `strptime` for the ledger's timestamp shapes.
//!
//! Why: the scripts format and parse `%Y-%m-%dT%H:%MZ` (and a seconds form) with
//! `datetime`; the crate has no date dependency, and the ledger is UTC only.
//! What: epoch seconds to the minute, second and day strings; a parser for the
//! date, minute and second shapes with Python's field widths and range checks.
//! Test: `format_and_parse_round_trip`, `strptime_matches_python_ranges`.

/// The three shapes `datetime.strptime` is asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// `%Y-%m-%d`.
    Date,
    /// `%Y-%m-%dT%H:%MZ`.
    Minute,
    /// `%Y-%m-%dT%H:%M:%SZ`.
    Second,
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

fn parts(epoch: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = epoch.div_euclid(86_400);
    let rem = epoch.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    (y, m, d, rem / 3600, rem % 3600 / 60, rem % 60)
}

/// `%Y-%m-%dT%H:%MZ` of `epoch` seconds.
pub fn fmt_min(epoch: i64) -> String {
    let (y, mo, d, h, mi, _) = parts(epoch);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}Z")
}

/// `%Y-%m-%dT%H:%M:%SZ` of `epoch` seconds.
pub fn fmt_sec(epoch: i64) -> String {
    let (y, mo, d, h, mi, s) = parts(epoch);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// `%Y%m%d` of `epoch` seconds (the rotation day).
pub fn fmt_day(epoch: i64) -> String {
    let (y, mo, d, ..) = parts(epoch);
    format!("{y:04}{mo:02}{d:02}")
}

struct Cur<'a>(&'a [u8]);

impl Cur<'_> {
    /// Python's `\d\d|\d` fields: two digits when the pair is in `min..=max`, else one.
    fn num(&mut self, min: i64, max: i64) -> Option<i64> {
        let digit = |i: usize| {
            self.0
                .get(i)
                .filter(|c| c.is_ascii_digit())
                .map(|c| i64::from(c - b'0'))
        };
        let a = digit(0)?;
        if let Some(b) = digit(1) {
            let two = a * 10 + b;
            if (min..=max).contains(&two) {
                self.0 = &self.0[2..];
                return Some(two);
            }
        }
        if a < min {
            return None;
        }
        self.0 = &self.0[1..];
        Some(a)
    }

    fn lit(&mut self, c: u8) -> Option<()> {
        (self.0.first() == Some(&c)).then(|| self.0 = &self.0[1..])
    }
}

/// `datetime.strptime(s, shape)` in UTC, as epoch seconds; `None` where Python raises `ValueError`.
pub fn strptime(s: &str, shape: Shape) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 5 || !b[..4].iter().all(u8::is_ascii_digit) {
        return None;
    }
    let y: i64 = s[..4].parse().ok()?;
    let mut c = Cur(&b[4..]);
    c.lit(b'-')?;
    let mo = c.num(1, 12)?;
    c.lit(b'-')?;
    let d = c.num(1, 31)?;
    let (mut h, mut mi, mut sec) = (0, 0, 0);
    if shape != Shape::Date {
        c.lit(b'T')?;
        h = c.num(0, 23)?;
        c.lit(b':')?;
        mi = c.num(0, 59)?;
        if shape == Shape::Second {
            c.lit(b':')?;
            sec = c.num(0, 59)?;
        }
        c.lit(b'Z')?;
    }
    if !c.0.is_empty() || y < 1 || d > month_len(y, mo) {
        return None;
    }
    Some(days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + sec)
}

fn month_len(y: i64, m: i64) -> i64 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `datg.parse_ts`: the minute form, else the second form.
pub fn parse_ts(s: &str) -> Option<i64> {
    strptime(s, Shape::Minute).or_else(|| strptime(s, Shape::Second))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_parse_round_trip() {
        let e = 1_795_000_000;
        assert_eq!(fmt_min(e), "2026-11-18T11:06Z");
        assert_eq!(strptime(&fmt_min(e), Shape::Minute), Some(e - e % 60));
        assert_eq!(strptime(&fmt_sec(e), Shape::Second), Some(e));
        assert_eq!(fmt_day(e), "20261118");
        assert_eq!(fmt_min(0), "1970-01-01T00:00Z");
        assert_eq!(fmt_min(951_782_400), "2000-02-29T00:00Z");
    }

    #[test]
    fn strptime_matches_python_ranges() {
        assert_eq!(strptime("2026-02-30", Shape::Date), None);
        assert_eq!(strptime("2026-13-01", Shape::Date), None);
        assert_eq!(
            strptime("2026-1-5T3:4Z", Shape::Minute),
            strptime("2026-01-05T03:04Z", Shape::Minute)
        );
        assert_eq!(strptime("2026-01-05T24:00Z", Shape::Minute), None);
        assert_eq!(strptime("2026-01-05T10:00:60Z", Shape::Second), None);
        assert_eq!(strptime("2026-01-05T10:00Z ", Shape::Minute), None);
        assert_eq!(strptime("0000-01-01", Shape::Date), None);
        assert_eq!(
            parse_ts("2026-11-18T11:06:30Z"),
            strptime("2026-11-18T11:06Z", Shape::Minute).map(|m| m + 30)
        );
    }
}
