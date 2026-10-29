//! Python's `datetime.fromisoformat` (3.11+ grammar), as the derive rules call it.
//!
//! Why: `_ts` in `dashboard_derive.py` decides whether a park is due and
//! whether a task is stale; a parser that accepts a different set of strings
//! changes which events the Architect sees (#10403).
//! What: [`ts`] is `_ts(value)`: Python `str(value)`, every `Z` replaced by
//! `+00:00`, then `fromisoformat`; a naive result is read as UTC. The answer is
//! microseconds since the Unix epoch, or `None` where Python raises. The date
//! half follows `_pydatetime` (calendar, basic and week dates, any
//! one-character separator); the time half follows CPython's C parser, which
//! is what `datetime` runs and which accepts a few strings `_pydatetime`
//! refuses (`T10:Z`, `T104Z`).
//! Test: `isotime_matches_python_fromisoformat` (2,900 strings CPython 3.14 parsed).

use serde_json::Value;

use super::super::derive::{days_from_civil, days_in_month};
use super::super::ids::py_str;

/// `_ts(value)`: microseconds since the epoch, or `None` where Python's
/// `fromisoformat` raises.
pub(crate) fn ts(value: &Value) -> Option<i64> {
    from_iso(&py_str(value).replace('Z', "+00:00"))
}

/// `datetime.fromisoformat(s)` as UTC microseconds (naive read as UTC).
pub(crate) fn from_iso(s: &str) -> Option<i64> {
    let c: Vec<char> = s.chars().collect();
    if c.len() < 7 {
        return None;
    }
    let sep = separator(&c)?;
    let (dstr, tstr) = (&c[..sep.min(c.len())], c.get(sep + 1..).unwrap_or(&[]));
    let mut day = parse_date(dstr)?;
    // The C `fromisoformat` (the one Python runs) refuses a separator with
    // no time after it; `_pydatetime` accepts it.
    if tstr.is_empty() && sep < c.len() {
        return None;
    }
    if tstr.is_empty() {
        return Some(day * 86_400_000_000);
    }
    let (h, mi, sec, us, offset) = parse_time(tstr)?;
    let mut hour = h;
    if h == 24 {
        if (mi, sec, us) != (0, 0, 0) {
            return None;
        }
        hour = 0;
        day += 1;
        // 9999-12-31T24:00 leaves `datetime`'s range.
        if day >= days_from_civil(10_000, 1, 1) {
            return None;
        }
    }
    if hour > 23 || mi > 59 || sec > 59 {
        return None;
    }
    let local = (day * 86_400 + hour * 3600 + mi * 60 + sec) * 1_000_000 + us;
    Some(local - offset)
}

fn digit(c: char) -> bool {
    c.is_ascii_digit()
}

/// `_find_isoformat_datetime_separator`.
fn separator(c: &[char]) -> Option<usize> {
    let n = c.len();
    if n == 7 {
        return Some(7);
    }
    if c[4] == '-' {
        if c[5] != 'W' {
            return Some(10);
        }
        if n > 8 && c[8] == '-' {
            if n == 9 {
                return None;
            }
            return Some(if n > 10 && digit(c[10]) { 8 } else { 10 });
        }
        return Some(8);
    }
    if c[4] != 'W' {
        return Some(8);
    }
    let mut idx = 7;
    while idx < n && digit(c[idx]) {
        idx += 1;
    }
    Some(if idx < 9 {
        idx
    } else if idx % 2 == 0 {
        7
    } else {
        8
    })
}

/// `_read_isoformat_component`: exactly `n` ASCII digits.
fn comp(c: &[char], from: usize, n: usize) -> Option<i64> {
    let s = c.get(from..from + n)?;
    s.iter().all(|ch| digit(*ch)).then(|| {
        s.iter()
            .fold(0, |a, ch| a * 10 + i64::from(*ch as u8 - b'0'))
    })
}

/// `_parse_isoformat_date` plus the `datetime` range checks: epoch days.
fn parse_date(d: &[char]) -> Option<i64> {
    if ![7, 8, 10].contains(&d.len()) || !d.iter().all(char::is_ascii) {
        return None;
    }
    let year = comp(d, 0, 4)?;
    let has_sep = d[4] == '-';
    let mut pos = 4 + usize::from(has_sep);
    if d.get(pos) == Some(&'W') {
        pos += 1;
        let week = comp(d, pos, 2)?;
        pos += 2;
        let mut dayno = 1;
        if d.len() > pos {
            if (d.get(pos) == Some(&'-')) != has_sep {
                return None;
            }
            pos += usize::from(has_sep);
            dayno = comp(d, pos, 1)?;
        }
        return iso_week(year, week, dayno);
    }
    let month = comp(d, pos, 2)?;
    pos += 2;
    if (d.get(pos) == Some(&'-')) != has_sep {
        return None;
    }
    pos += usize::from(has_sep);
    let day = comp(d, pos, 2)?;
    let ok = (1..=9999).contains(&year) && (1..=12).contains(&month);
    (ok && day >= 1 && day <= days_in_month(year, month)).then(|| days_from_civil(year, month, day))
}

/// `_isoweek_to_gregorian`, as epoch days.
fn iso_week(year: i64, week: i64, day: i64) -> Option<i64> {
    if !(1..=9999).contains(&year) {
        return None;
    }
    let first = days_from_civil(year, 1, 1);
    // Monday = 0; 1970-01-01 was a Thursday.
    let fw = (first + 3).rem_euclid(7);
    if !(1..53).contains(&week) {
        let leap = days_in_month(year, 2) == 29;
        if !(week == 53 && (fw == 3 || (fw == 2 && leap))) {
            return None;
        }
    }
    if !(1..8).contains(&day) {
        return None;
    }
    let monday = first - fw + if fw > 3 { 7 } else { 0 };
    Some(monday + (week - 1) * 7 + (day - 1))
}

/// C's `parse_hh_mm_ss_ff` over `t[from..end]`: `([h, m, s, us], trailing)`.
/// `trailing` is C's return of 1: a character was left unread before `end`,
/// which is an error only when no time zone follows.
fn hh_mm_ss_ff(t: &[char], from: usize, end: usize) -> Option<([i64; 4], bool)> {
    let mut out = [0i64; 4];
    let mut p = from;
    let mut has_sep = true;
    for i in 0..3 {
        out[i] = comp(t, p, 2)?;
        p += 2;
        // `c = *(p++)`; `None` is the string's NUL.
        let c = t.get(p).copied();
        p += 1;
        if i == 0 {
            has_sep = c == Some(':');
        }
        if matches!(c, Some('.' | ',')) {
            if i < 2 {
                return None;
            }
            break;
        }
        if p >= end {
            return Some((out, c.is_some()));
        }
        match c {
            Some(':') if has_sep => {
                if i == 2 {
                    return None;
                }
            }
            _ if !has_sep => p -= 1,
            _ => return None,
        }
    }
    // After `HHMMSS` with no mark C reads digits as the fraction too.
    let take = end.saturating_sub(p).min(6);
    if take == 0 {
        return None;
    }
    out[3] = comp(t, p, take)? * 10_i64.pow(6 - take as u32);
    p += take;
    while t.get(p).is_some_and(|c| digit(*c)) {
        p += 1;
    }
    Some((out, p < t.len()))
}

/// C's `parse_isoformat_time`: `(h, m, s, us, offset_us)`.
fn parse_time(t: &[char]) -> Option<(i64, i64, i64, i64, i64)> {
    if t.len() < 2 || !t.iter().all(char::is_ascii) {
        return None;
    }
    // The first `Z`, `+` or `-` starts the zone.
    let tz = t
        .iter()
        .position(|c| matches!(c, 'Z' | '+' | '-'))
        .unwrap_or(t.len());
    let ([h, mi, sec, us], trailing) = hh_mm_ss_ff(t, 0, tz)?;
    if tz == t.len() {
        return (!trailing).then_some((h, mi, sec, us, 0));
    }
    if t[tz] == 'Z' {
        return (tz + 1 == t.len()).then_some((h, mi, sec, us, 0));
    }
    let ([oh, om, os, ous], trailing) = hh_mm_ss_ff(t, tz + 1, t.len())?;
    if trailing {
        return None;
    }
    let size = ((oh * 60 + om) * 60 + os) * 1_000_000 + ous;
    // `timezone()` needs a magnitude under 24 hours.
    if size >= 86_400_000_000 {
        return None;
    }
    let offset = if t[tz] == '-' { -size } else { size };
    Some((h, mi, sec, us, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(input, Python's microseconds or None)`, from
    /// `tests/fixtures/dashboard/pending/isotime.json`.
    #[test]
    fn isotime_matches_python_fromisoformat() {
        let text = include_str!("../../../tests/fixtures/dashboard/pending/isotime.json");
        let rows: Vec<(String, Option<i64>)> = serde_json::from_str(text).unwrap_or_default();
        assert!(rows.len() > 40, "corpus missing");
        let bad: Vec<_> = rows
            .iter()
            .filter(|(input, want)| from_iso(input) != *want)
            .map(|(input, want)| (input, want, from_iso(input)))
            .collect();
        assert!(bad.is_empty(), "(input, python, rust): {bad:?}");
    }
}
