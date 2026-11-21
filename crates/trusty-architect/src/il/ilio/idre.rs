//! Hand matchers for the id and reference patterns of `ilio.py`.
//!
//! Why: `ilio.py` finds ids (`ID`), look-alike ids (`LOOKALIKE`) and R references
//! with regexes; the crate takes no `regex` dependency. The matchers keep the
//! regex priority (optional `code-` first, optional `@stamp` first) and `\b`
//! semantics, so `check` rejects and accepts the same blocks.
//! What: [`ids_in`] (`ID.finditer`), [`id_full`] (`ID.fullmatch`),
//! [`lookalike_full`] (`LOOKALIKE.fullmatch`) and [`rrefs_in`]
//! (`\bR[0-9a-km-np-z]{2}-[a-z]\b`), all over `char` slices.
//! Test: `finds_ids_with_code_pre_and_stamp`, `fullmatch_needs_the_whole_token`,
//! `lookalike_flags_o_and_l`, `rrefs_are_whole_words`.

use crate::il::ids::dec;
use crate::pystr::{is_digit, is_word};

/// One `ID` match, as char offsets into the searched text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdMatch {
    /// Start of the whole match.
    pub start: usize,
    /// End of the whole match.
    pub end: usize,
    /// The `code-` prefix without the dash, when present.
    pub code: Option<String>,
    /// `G`, `D`, `A`, `T` or `R`.
    pub pre: char,
    /// The two base-34 digits.
    pub n: String,
    /// The `@MMDD` stamp, when present.
    pub stamp: Option<String>,
}

fn boundary(t: &[char], i: usize) -> bool {
    let before = i > 0 && is_word(t[i - 1]);
    let after = i < t.len() && is_word(t[i]);
    before != after
}

fn digit_pair(t: &[char], j: usize) -> Option<String> {
    let pair: String = t.get(j..j + 2)?.iter().collect();
    dec(&pair).map(|_| pair)
}

/// The matches at `i` after the optional code, in regex priority (stamp first), as `(pre, n, stamp, end)`.
fn tails(t: &[char], j: usize) -> Vec<(char, String, Option<String>, usize)> {
    let Some(&pre) = t.get(j).filter(|c| "GDATR".contains(**c)) else {
        return Vec::new();
    };
    let Some(n) = digit_pair(t, j + 1) else {
        return Vec::new();
    };
    let k = j + 3;
    let mut out = Vec::new();
    if t.get(k) == Some(&'@')
        && t.get(k + 1..k + 5)
            .is_some_and(|d| d.iter().all(|c| is_digit(*c)))
    {
        let stamp: String = t[k + 1..k + 5].iter().collect();
        if boundary(t, k + 5) {
            out.push((pre, n.clone(), Some(stamp), k + 5));
        }
    }
    if boundary(t, k) {
        out.push((pre, n, None, k));
    }
    out
}

/// Every way `ID` can match at `i`, best first.
fn candidates(t: &[char], i: usize) -> Vec<IdMatch> {
    if !boundary(t, i) {
        return Vec::new();
    }
    let mut out = Vec::new();
    let run = (i..t.len())
        .take_while(|&k| t[k].is_ascii_lowercase() || t[k].is_ascii_digit())
        .count();
    if t.get(i).is_some_and(char::is_ascii_lowercase)
        && (2..=8).contains(&run)
        && t.get(i + run) == Some(&'-')
    {
        let code: String = t[i..i + run].iter().collect();
        for (pre, n, stamp, end) in tails(t, i + run + 1) {
            out.push(IdMatch {
                start: i,
                end,
                code: Some(code.clone()),
                pre,
                n,
                stamp,
            });
        }
    }
    for (pre, n, stamp, end) in tails(t, i) {
        out.push(IdMatch {
            start: i,
            end,
            code: None,
            pre,
            n,
            stamp,
        });
    }
    out
}

/// `ID.finditer(text)`: non-overlapping matches, leftmost first.
pub fn ids_in(t: &[char]) -> Vec<IdMatch> {
    let (mut out, mut i) = (Vec::new(), 0);
    while i < t.len() {
        match candidates(t, i).into_iter().next() {
            Some(m) => {
                i = m.end;
                out.push(m);
            }
            None => i += 1,
        }
    }
    out
}

/// `ID.fullmatch(tok)`.
pub fn id_full(tok: &str) -> Option<IdMatch> {
    let t: Vec<char> = tok.chars().collect();
    candidates(&t, 0).into_iter().find(|m| m.end == t.len())
}

/// `LOOKALIKE.fullmatch(tok)`: an id-shaped token whose digits hold an `o` or an `l`.
pub fn lookalike_full(tok: &str) -> bool {
    let t: Vec<char> = tok.chars().collect();
    let shape = |j: usize| {
        let rest = &t[j.min(t.len())..];
        t.get(j).is_some_and(|c| "GDATR".contains(*c))
            && rest.len() > 1
            && rest[1..]
                .iter()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
            && rest[1..].iter().any(|c| matches!(c, 'o' | 'l'))
    };
    let run = t
        .iter()
        .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .count();
    let with_code = t.first().is_some_and(char::is_ascii_lowercase)
        && (2..=8).contains(&run)
        && t.get(run) == Some(&'-')
        && shape(run + 1);
    !t.is_empty() && is_word(t[0]) && (with_code || shape(0))
}

/// `re.findall(r"\bR[0-9a-km-np-z]{2}-[a-z]\b", text)`.
pub fn rrefs_in(text: &str) -> Vec<String> {
    let t: Vec<char> = text.chars().collect();
    let (mut out, mut i) = (Vec::new(), 0);
    while i + 5 <= t.len() {
        let ok = t[i] == 'R'
            && boundary(&t, i)
            && digit_pair(&t, i + 1).is_some()
            && t[i + 3] == '-'
            && t[i + 4].is_ascii_lowercase()
            && boundary(&t, i + 5);
        if ok {
            out.push(t[i..i + 5].iter().collect());
            i += 5;
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    #[test]
    fn finds_ids_with_code_pre_and_stamp() {
        let m = ids_in(&chars("close pm-T0f@1120, pm-D01 and T0g; R01"));
        let got: Vec<_> = m
            .iter()
            .map(|m| (m.code.clone(), m.pre, m.n.clone(), m.stamp.clone()))
            .collect();
        assert_eq!(
            got,
            [
                (Some("pm".into()), 'T', "0f".into(), Some("1120".into())),
                (Some("pm".into()), 'D', "01".into(), None),
                (None, 'T', "0g".into(), None),
                (None, 'R', "01".into(), None),
            ]
        );
        assert!(ids_in(&chars("xT0f")).is_empty(), "no boundary before T");
        assert!(
            ids_in(&chars("T0fx")).is_empty(),
            "no boundary after the digits"
        );
        assert_eq!(
            ids_in(&chars("T0f@12"))[0].stamp,
            None,
            "a short stamp backs off"
        );
    }

    #[test]
    fn fullmatch_needs_the_whole_token() {
        assert!(id_full("pm-T0f").is_some());
        assert!(id_full("pm-T0f@1120").is_some_and(|m| m.stamp.as_deref() == Some("1120")));
        assert!(id_full("pm-T0f,").is_none());
        assert!(id_full("T0f").is_some_and(|m| m.code.is_none()));
        assert!(id_full("pm-T0o").is_none());
        assert!(id_full("").is_none());
    }

    #[test]
    fn lookalike_flags_o_and_l() {
        for yes in ["pm-T0o", "pm-T1l", "T0l", "pm-Tol"] {
            assert!(lookalike_full(yes), "{yes}");
        }
        for no in ["pm-T0f", "pm-T", "free", "pm-X0o", ""] {
            assert!(!lookalike_full(no), "{no}");
        }
    }

    #[test]
    fn rrefs_are_whole_words() {
        assert_eq!(
            rrefs_in("see R01-a and R0z-b, not xR01-a or R01-ab"),
            ["R01-a", "R0z-b"]
        );
        assert!(rrefs_in("R0o-a").is_empty());
    }
}
