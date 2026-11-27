//! Base-34 ledger ids: parse, encode, and allocation with recycling.
//!
//! Why: ids are `<code>-<G|D|A|T><2 base-34 digits>` (digits `0-9a-z` without `l`
//! and `o`), 1155 issuable per kind, and a closed id is reused ("ids cycle").
//! The port must pick the same next id as `datg.py` `_next_id` and `ids36.py`.
//! What: [`enc`] / [`dec`], [`parse_id`], [`is_code`], and [`next_id`], which
//! scans from the `next` pointer to `zz` then `01` up to it, takes the first id
//! not live, and moves the pointer to the id after it.
//! Test: `enc_dec_round_trip`, `next_id_skips_live_wraps_and_recycles`,
//! `explicit_id_rules`.

use super::error::{IlResult, invalid};
use super::json::Json;
use crate::pystr::py_repr;
use std::collections::HashSet;

// See #12029: the digit table and the decode are `crate::ids36`'s.
pub use crate::ids36::DIGITS;
/// Values per kind (`34 * 34`); `00` is never issued.
pub const SPACE: i64 = crate::ids36::SPACE as i64;

/// Two base-34 digits for `0 <= n < SPACE`.
pub fn enc(n: i64) -> String {
    let d = DIGITS.as_bytes();
    let (hi, lo) = (n.div_euclid(34) as usize % 34, n.rem_euclid(34) as usize);
    format!("{}{}", char::from(d[hi]), char::from(d[lo]))
}

/// The value of exactly two lowercase base-34 digits; `None` for anything
/// else. Stricter than [`crate::ids36::dec`], which also takes one final
/// newline as the script's `$` does: the ledger's callers port full matches.
pub fn dec(s: &str) -> Option<i64> {
    (s.len() == 2)
        .then(|| crate::ids36::dec(s).ok())
        .flatten()
        .map(|n| n as i64)
}

/// `^[a-z][a-z0-9]{1,7}$`.
pub fn is_code(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=8).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// `ID_RE`: `(code, kind, digits)` of `<code>-<G|D|A|T><2 digits>`.
pub fn parse_id(s: &str) -> Option<(&str, char, &str)> {
    let dash = s.find('-')?;
    let (code, rest) = (&s[..dash], &s[dash + 1..]);
    let mut it = rest.chars();
    let pre = it.next().filter(|c| "GDAT".contains(*c))?;
    let digits = &rest[1..];
    (is_code(code) && digits.len() == 2 && dec(digits).is_some()).then_some((code, pre, digits))
}

/// Every id in goals, items and tasks.
pub fn live_ids(st: &Json) -> HashSet<&str> {
    ["goals", "items", "tasks"]
        .iter()
        .flat_map(|k| st.arr(k).iter())
        .filter_map(|o| o.get("id").and_then(Json::as_str))
        .collect()
}

/// `_next_id`: an explicit id is validated, otherwise the first free id from the pointer.
///
/// # Errors
/// `Invalid` for a bad, `00`, colliding explicit id or an exhausted range.
pub fn next_id(st: &mut Json, pre: char, explicit: Option<&str>) -> IlResult<String> {
    let code = st.s("code").to_owned();
    let used = live_ids(st);
    if let Some(e) = explicit.filter(|e| !e.is_empty()) {
        let ok = parse_id(e).filter(|(c, p, _)| *c == code && *p == pre);
        let Some((_, _, digits)) = ok else {
            return invalid(format!("bad id {} for {pre}", py_repr(e)));
        };
        if digits == "00" {
            return invalid(format!("bad id {}: 00 is never issued", py_repr(e)));
        }
        if used.contains(e) {
            return invalid(format!("id collision: {e} exists"));
        }
        return Ok(e.to_owned());
    }
    let pointer = st
        .get("next")
        .and_then(|n| n.get(&pre.to_string()))
        .and_then(Json::as_str)
        .unwrap_or("01");
    let Some(parsed) = dec(pointer) else {
        return invalid(format!(
            "next.{pre} pointer {} is not two base-34 digits",
            py_repr(pointer)
        ));
    };
    let start = if parsed == 0 { 1 } else { parsed };
    let found = (start..SPACE)
        .chain(1..start)
        .find(|i| !used.contains(format!("{code}-{pre}{}", enc(*i)).as_str()));
    let Some(n) = found else {
        return invalid(format!("no free {pre} id (01-zz exhausted)"));
    };
    let next = enc(n % (SPACE - 1) + 1);
    if st.get("next").is_none() {
        st.set("next", Json::Obj(Vec::new()));
    }
    if let Some(m) = st.get_mut("next") {
        m.set(&pre.to_string(), next);
    }
    Ok(format!("{code}-{pre}{}", enc(n)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(ids: &[&str], next: &str) -> Json {
        let tasks = ids
            .iter()
            .map(|i| super::super::json::obj([("id", Json::from(*i))]))
            .collect();
        super::super::json::obj([
            ("code", "tt".into()),
            ("tasks", Json::Arr(tasks)),
            ("next", super::super::json::obj([("T", next.into())])),
        ])
    }

    #[test]
    fn enc_dec_round_trip() {
        assert_eq!(enc(1), "01");
        assert_eq!(enc(1155), "zz");
        assert_eq!(dec("zz"), Some(1155));
        assert_eq!(dec("0l"), None);
        assert_eq!(dec("0o"), None);
        assert_eq!(dec("0A"), None);
        assert_eq!(enc(33), "0z");
        assert_eq!(enc(34), "10");
    }

    #[test]
    fn next_id_skips_live_wraps_and_recycles() {
        let mut s = st(&["tt-T01", "tt-T02"], "01");
        assert_eq!(next_id(&mut s, 'T', None).unwrap(), "tt-T03");
        assert_eq!(s.get("next").unwrap().s("T"), "04");
        // The pointer sits on zz: take zz, then wrap to the first free id from 01.
        let mut s = st(&["tt-T01"], "zz");
        assert_eq!(next_id(&mut s, 'T', None).unwrap(), "tt-Tzz");
        assert_eq!(s.get("next").unwrap().s("T"), "01");
        assert_eq!(next_id(&mut s, 'T', None).unwrap(), "tt-T02");
        // A hex-era pointer reads as base 34, and 0k follows 0j (no l, no o).
        let mut s = st(&[], "0j");
        assert_eq!(next_id(&mut s, 'T', None).unwrap(), "tt-T0j");
        assert_eq!(s.get("next").unwrap().s("T"), "0k");
    }

    #[test]
    fn explicit_id_rules() {
        let mut s = st(&["tt-T05"], "01");
        assert!(next_id(&mut s, 'T', Some("tt-T00")).is_err());
        assert!(next_id(&mut s, 'T', Some("tt-T05")).is_err());
        assert!(next_id(&mut s, 'T', Some("tt-D05")).is_err());
        assert_eq!(next_id(&mut s, 'T', Some("tt-T06")).unwrap(), "tt-T06");
    }
}
