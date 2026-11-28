//! Python 3.14 `datetime.strptime` for the fixed formats the task checkers use (#12028 P1d).
//!
//! Why: `stalled_tasks.py` falls back to `strptime(s, "%Y-%m-%dT%H:%M%z")` and
//! `il_timeline.py` reads ledger times with three `strptime` formats; both
//! accept strings `fromisoformat` refuses (a one-digit month or hour, a
//! lower-case `t` or `z`, Unicode digits), so a port must accept the same set.
//! What: [`strptime`] compiles the format the way `_strptime` does (each
//! directive is its regex alternation, every literal matches without case)
//! and matches it with the regex engine's backtracking order: the first path
//! through the whole format wins, and the string must end where it ends.
//! The match then builds the `datetime`: year 1-9999, the day within its
//! month, a second under 60, an offset strictly inside 24 hours, and a
//! consistent use of `:` in `%z`. Supported directives: `%Y %m %d %H %M %S %f
//! %z`. Backtracking is bounded by the format (at most eight directives).
//! Test: `strptime_matches_python`, and the `stalled_tasks` and
//! `il_timeline` recordings.

use crate::classify::alerts::days_from_civil;
use crate::dashboard::derive::days_in_month;
use crate::pystr::digit_value;

/// One parsed time: wall-clock microseconds since the epoch and the `%z`
/// offset in microseconds (`None`: a naive result).
pub(crate) type Parsed = (i64, Option<i64>);

#[derive(Default)]
struct Fields {
    y: i64,
    m: i64,
    d: i64,
    h: i64,
    mi: i64,
    s: i64,
    us: i64,
    z: Option<(usize, usize)>,
}

fn dig(c: Option<&char>) -> Option<i64> {
    c.and_then(|c| digit_value(*c)).map(i64::from)
}

fn ascii_in(c: Option<&char>, lo: char, hi: char) -> bool {
    c.is_some_and(|c| (lo..=hi).contains(c))
}

/// The end positions of a directive's alternatives at `i`, in regex order.
fn alternatives(dir: char, s: &[char], i: usize) -> Vec<usize> {
    let at = |k: usize| s.get(i + k);
    let d = |k: usize| dig(at(k)).is_some();
    let mut out = Vec::new();
    let mut alt = |ok: bool, len: usize| {
        if ok {
            out.push(i + len);
        }
    };
    match dir {
        'Y' => alt((0..4).all(d), 4),
        'm' => {
            alt(at(0) == Some(&'1') && ascii_in(at(1), '0', '2'), 2);
            alt(at(0) == Some(&'0') && ascii_in(at(1), '1', '9'), 2);
            alt(ascii_in(at(0), '1', '9'), 1);
        }
        'd' => {
            alt(at(0) == Some(&'3') && ascii_in(at(1), '0', '1'), 2);
            alt(ascii_in(at(0), '1', '2') && d(1), 2);
            alt(at(0) == Some(&'0') && ascii_in(at(1), '1', '9'), 2);
            alt(ascii_in(at(0), '1', '9'), 1);
            alt(at(0) == Some(&' ') && ascii_in(at(1), '1', '9'), 2);
        }
        'H' => {
            alt(at(0) == Some(&'2') && ascii_in(at(1), '0', '3'), 2);
            alt(ascii_in(at(0), '0', '1') && d(1), 2);
            alt(d(0), 1);
            alt(at(0) == Some(&' ') && d(1), 2);
        }
        'M' => {
            alt(ascii_in(at(0), '0', '5') && d(1), 2);
            alt(d(0), 1);
        }
        'S' => {
            alt(at(0) == Some(&'6') && ascii_in(at(1), '0', '1'), 2);
            alt(ascii_in(at(0), '0', '5') && d(1), 2);
            alt(d(0), 1);
        }
        'f' => {
            let run = (0..6).take_while(|k| ascii_in(at(*k), '0', '9')).count();
            for len in (1..=run).rev() {
                out.push(i + len);
            }
        }
        'z' => zone_ends(s, i, &mut out),
        _ => {}
    }
    out
}

/// `[+-]\d\d:?[0-5]\d(:?[0-5]\d(\.\d{1,6})?)?|(?-i:Z)`, greedy, in order.
fn zone_ends(s: &[char], i: usize, out: &mut Vec<usize>) {
    if s.get(i) == Some(&'Z') {
        out.push(i + 1);
        return;
    }
    if !matches!(s.get(i), Some('+' | '-'))
        || dig(s.get(i + 1)).is_none()
        || dig(s.get(i + 2)).is_none()
    {
        return;
    }
    let colons = |p: usize| -> Vec<usize> {
        if s.get(p) == Some(&':') {
            vec![p + 1, p]
        } else {
            vec![p]
        }
    };
    let two = |p: usize| ascii_in(s.get(p), '0', '5') && dig(s.get(p + 1)).is_some();
    for p in colons(i + 3) {
        if !two(p) {
            continue;
        }
        let after_mm = p + 2;
        for q in colons(after_mm) {
            if !two(q) {
                continue;
            }
            let after_ss = q + 2;
            if s.get(after_ss) == Some(&'.') {
                let run = (1..=6)
                    .take_while(|k| dig(s.get(after_ss + k)).is_some())
                    .count();
                for len in (1..=run).rev() {
                    out.push(after_ss + 1 + len);
                }
            }
            out.push(after_ss);
        }
        out.push(after_mm);
    }
}

/// `re.IGNORECASE` equality of a text character and an ASCII format literal.
fn literal_eq(c: char, lit: char) -> bool {
    c == lit || (lit.is_ascii_alphabetic() && c.eq_ignore_ascii_case(&lit))
}

/// The first end position of `fmt[k..]` matched at `s[i..]`, recording each
/// directive's span; `None` when no path through the format matches.
fn walk(
    fmt: &[(bool, char)],
    k: usize,
    s: &[char],
    i: usize,
    spans: &mut Vec<(char, usize, usize)>,
) -> Option<usize> {
    let Some(&(is_dir, ch)) = fmt.get(k) else {
        return Some(i);
    };
    if !is_dir {
        return match s.get(i) {
            Some(c) if literal_eq(*c, ch) => walk(fmt, k + 1, s, i + 1, spans),
            _ => None,
        };
    }
    for end in alternatives(ch, s, i) {
        spans.push((ch, i, end));
        if let Some(done) = walk(fmt, k + 1, s, end, spans) {
            return Some(done);
        }
        spans.pop();
    }
    None
}

fn number(s: &[char], from: usize, to: usize) -> i64 {
    s[from..to]
        .iter()
        .filter_map(|c| digit_value(*c))
        .fold(0, |acc, d| acc * 10 + i64::from(d))
}

/// The `%z` offset in microseconds, or `None` where `_strptime` raises.
fn zone(z: &[char]) -> Option<i64> {
    if z == ['Z'] {
        return Some(0);
    }
    let mut z: Vec<char> = z.to_vec();
    if z.get(3) == Some(&':') {
        z.remove(3);
        if z.len() > 5 {
            if z[5] != ':' {
                return None;
            }
            z.remove(5);
        }
    }
    // `int(z[5:7] or 0)` fails on a `:` left in place.
    let field = |a: usize, b: usize| -> Option<i64> {
        let part = z.get(a..b.min(z.len()))?;
        part.iter()
            .all(|c| digit_value(*c).is_some())
            .then(|| number(part, 0, part.len()))
    };
    let hours = field(1, 3)?;
    let minutes = field(3, 5)?;
    let seconds = if z.len() > 5 { field(5, 7)? } else { 0 };
    let frac: String = z.get(8..).unwrap_or(&[]).iter().collect();
    let micros = format!("{frac:0<6}")
        .chars()
        .filter_map(digit_value)
        .fold(0i64, |a, d| a * 10 + i64::from(d));
    let mut off = ((hours * 60 + minutes) * 60 + seconds) * 1_000_000 + micros;
    if z[0] == '-' {
        off = -off;
    }
    (off.abs() < 86_400_000_000).then_some(off)
}

/// `datetime.strptime(s, fmt)`; `None` where Python raises `ValueError`.
pub(crate) fn strptime(s: &str, fmt: &str) -> Option<Parsed> {
    let mut compiled = Vec::new();
    let mut it = fmt.chars();
    while let Some(c) = it.next() {
        match c {
            '%' => compiled.push((true, it.next()?)),
            c => compiled.push((false, c)),
        }
    }
    let s: Vec<char> = s.chars().collect();
    let mut spans = Vec::new();
    if walk(&compiled, 0, &s, 0, &mut spans)? != s.len() {
        return None;
    }
    let mut f = Fields {
        m: 1,
        d: 1,
        ..Fields::default()
    };
    for (dir, a, b) in spans {
        let n = number(&s, a, b);
        match dir {
            'Y' => f.y = n,
            'm' => f.m = n,
            'd' => f.d = n,
            'H' => f.h = n,
            'M' => f.mi = n,
            'S' => f.s = n,
            'f' => {
                f.us = number(
                    &format!("{:0<6}", s[a..b].iter().collect::<String>())
                        .chars()
                        .collect::<Vec<_>>(),
                    0,
                    6,
                )
            }
            'z' => f.z = Some((a, b)),
            _ => {}
        }
    }
    if !(1..=9999).contains(&f.y) || f.d > days_in_month(f.y, f.m) || f.s > 59 {
        return None;
    }
    let offset = match f.z {
        Some((a, b)) => Some(zone(&s[a..b])?),
        None => None,
    };
    let day = days_from_civil(f.y, f.m, f.d);
    let local = (((day * 24 + f.h) * 60 + f.mi) * 60 + f.s) * 1_000_000 + f.us;
    Some((local, offset))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISO_Z: &str = "%Y-%m-%dT%H:%M%z";

    /// UTC microseconds of an aware result, as `.timestamp() * 1e6`.
    fn utc(s: &str, fmt: &str) -> Option<i64> {
        strptime(s, fmt).map(|(l, o)| l - o.unwrap_or(0))
    }

    // Expected values from CPython 3.14.8 `datetime.strptime(...).timestamp()`.
    #[test]
    fn strptime_matches_python() {
        assert_eq!(
            utc("2026-11-27T1:5+0000", ISO_Z),
            Some(1_795_741_500_000_000)
        );
        assert_eq!(
            utc("2026-11-27t10:00+05:30", ISO_Z),
            Some(1_795_753_800_000_000)
        );
        assert_eq!(utc("2026-11-27T10:00+05:3000", ISO_Z), None);
        assert_eq!(utc("2026-11-27T10:00+0530:00", ISO_Z), None);
        assert_eq!(utc("2026-11-27T10:00+24:00", ISO_Z), None);
        assert_eq!(utc("2026-11-27T10:00Z", ISO_Z), Some(1_795_773_600_000_000));
        assert_eq!(utc("2026-11-27T10:00z", ISO_Z), None);
        assert_eq!(utc("2026-02-30T10:00Z", ISO_Z), None);
        assert_eq!(
            utc("\u{661}\u{669}\u{667}\u{660}-1-1T0:0+0000", ISO_Z),
            Some(0)
        );
        let f = "%Y-%m-%dT%H:%M:%S.%fZ";
        assert_eq!(
            strptime("2026-11-20T09:07:00.5Z", f),
            Some((1_795_165_620_500_000, None))
        );
        assert_eq!(strptime("2026-11-20T09:07:60.5Z", f), None);
        assert_eq!(strptime("2026-11-20T09:07:00.1234567Z", f), None);
        assert_eq!(
            strptime("2026-11-20t9:10z", "%Y-%m-%dT%H:%MZ"),
            Some((1_795_165_800_000_000, None))
        );
        assert_eq!(strptime("0000-11-20T09:10Z", "%Y-%m-%dT%H:%MZ"), None);
    }
}
