//! The idle screen fingerprint: `fleet-poll.py`'s `screen_hash` and its
//! `VOLATILE` pattern.
//!
//! Why: Bob D9t (2026-11-21): an idle session alerts once, and again only when
//! its screen changes. The deployed poller keys the `idle` fingerprint on this
//! hash and gives `idle` no re-emit interval (MB1595). A changed screen on
//! an already-alerted epoch alerts at once (#11826, as `fleet-poll.py`).
//! What: [`screen_hash`] takes the rows above the input box (all rows when
//! there is no box), blanks the [`strip_volatile`] runs (timers, clock times,
//! token counts, spinner glyphs), collapses whitespace per row, joins the rows
//! with `\n`, strips the result and returns its 12-hex SHA-1.
//! Test: `test_idle_same_screen_no_repeat`, `test_idle_changed_screen_alerts`,
//! `test_idle_new_stretch_alerts` (tests/shadow_parity_gaps.rs),
//! `parse_pane_matches_python_goldens`.

use super::alerts::fingerprint;
use crate::pystr::{collapse_space, is_digit, is_space, is_word, strip};

/// `screen_hash(lines, top)`.
pub fn screen_hash(lines: &[String], top: Option<usize>) -> String {
    let end = top.unwrap_or(lines.len()).min(lines.len());
    let body = lines[..end]
        .iter()
        .map(|l| collapse_space(&strip_volatile(l)))
        .collect::<Vec<_>>()
        .join("\n");
    fingerprint(strip(&body))
}

/// The spinner and bullet glyphs `VOLATILE`'s last branch removes.
fn is_glyph(c: char) -> bool {
    matches!(c, '✻' | '✶' | '✳' | '✢' | '·' | '*' | '⏺' | '◯' | '●' | '○')
        || ('\u{2800}'..='\u{28ff}').contains(&c)
}

/// `VOLATILE.sub("", line)`:
/// `\d+(?:\.\d+)?[kKmM]?\s?(?:tokens?|[hms])\b`
/// `|\b\d{1,2}:\d{2}(?::\d{2})?\b(?:\s?[AP]M)?`
/// `|[✻✶✳✢·*⏺◯●○⠀-⣿]`, with `re`'s leftmost, first-alternative semantics.
pub fn strip_volatile(line: &str) -> String {
    let c: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < c.len() {
        let end = amount(&c, i)
            .or_else(|| clock(&c, i))
            .or_else(|| is_glyph(c[i]).then_some(i + 1));
        match end {
            Some(e) if e > i => i = e,
            _ => {
                out.push(c[i]);
                i += 1;
            }
        }
    }
    out
}

/// `\b` at position `i` of `c`.
fn boundary(c: &[char], i: usize) -> bool {
    let before = i > 0 && is_word(c[i - 1]);
    let after = c.get(i).is_some_and(|x| is_word(*x));
    before != after
}

/// End of the longest run of digits starting at `i` (`i` when none).
fn digits(c: &[char], i: usize) -> usize {
    let mut j = i;
    while c.get(j).is_some_and(|x| is_digit(*x)) {
        j += 1;
    }
    j
}

/// The first branch: a number with a unit (`52.9k tokens`, `6m`, `34s`).
fn amount(c: &[char], i: usize) -> Option<usize> {
    let n = digits(c, i);
    if n == i {
        return None;
    }
    // Backtracking into `\d+` cannot help: every later element rejects a digit.
    let mut dots = Vec::with_capacity(2);
    if c.get(n) == Some(&'.') {
        let d = digits(c, n + 1);
        if d > n + 1 {
            dots.push(d);
        }
    }
    dots.push(n);
    for p in dots {
        let ks = optional(c, p, |x| matches!(x, 'k' | 'K' | 'm' | 'M'));
        for k in ks {
            for s in optional(c, k, is_space) {
                for unit in ["tokens", "token"] {
                    let u: Vec<char> = unit.chars().collect();
                    if c.get(s..s + u.len()) == Some(&u[..]) && boundary(c, s + u.len()) {
                        return Some(s + u.len());
                    }
                }
                if c.get(s).is_some_and(|x| matches!(x, 'h' | 'm' | 's')) && boundary(c, s + 1) {
                    return Some(s + 1);
                }
            }
        }
    }
    None
}

/// `x?` at `i`: the positions to try, greedy first.
fn optional(c: &[char], i: usize, f: impl Fn(char) -> bool) -> Vec<usize> {
    if c.get(i).is_some_and(|x| f(*x)) {
        vec![i + 1, i]
    } else {
        vec![i]
    }
}

/// The second branch: a clock time (`12:01`, `9:05:33 PM`).
fn clock(c: &[char], i: usize) -> Option<usize> {
    if !boundary(c, i) {
        return None;
    }
    let two = |j: usize| {
        c.get(j..j + 2)
            .is_some_and(|s| s.iter().all(|x| is_digit(*x)))
    };
    let one = |j: usize| c.get(j).is_some_and(|x| is_digit(*x));
    for h in [2, 1] {
        if !(0..h).all(|k| one(i + k)) {
            continue;
        }
        let m = i + h;
        if c.get(m) != Some(&':') || !two(m + 1) {
            continue;
        }
        let mut ends = Vec::with_capacity(2);
        if c.get(m + 3) == Some(&':') && two(m + 4) {
            ends.push(m + 6);
        }
        ends.push(m + 3);
        for e in ends {
            if !boundary(c, e) {
                continue;
            }
            for s in optional(c, e, is_space) {
                if c.get(s).is_some_and(|x| matches!(x, 'A' | 'P')) && c.get(s + 1) == Some(&'M') {
                    return Some(s + 2);
                }
            }
            return Some(e);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_volatile_blanks_timers_clocks_tokens_and_glyphs() {
        // Expected values are Python's `VOLATILE.sub("", line)`.
        let cases = [
            ("✻ Brewed for 6m 34s · ↓ 52.9k tokens", " Brewed for    ↓ "),
            ("  ⏵⏵ bypass 12:01 PM", "  ⏵⏵ bypass "),
            ("at 9:05:33x", "at :33x"),
            ("at 12:34:56", "at "),
            ("123x 4h", "123x "),
            ("version 1.2", "version 1.2"),
            ("1 token", ""),
            ("⠋ spin", " spin"),
            ("5 ms", "5 ms"),
            ("2.5m tokensx", " tokensx"),
            ("10:30AM", "10:30AM"),
        ];
        for (line, want) in cases {
            assert_eq!(strip_volatile(line), want, "{line:?}");
        }
    }
}
