//! `unicodedata.normalize("NFC", s)` on the frozen Python's tables (#12029 P2h).
//!
//! Why: `clean_name` normalises the upload name before it refuses or
//! replaces characters; NFC can merge a base letter and a combining mark
//! into one character, or map a singleton (the Kelvin sign) to ASCII, so the
//! cleaned name's length and letters depend on it.
//! What: [`nfc`]: full canonical decomposition (Hangul by formula), the
//! canonical reordering of combining marks, then the canonical composition
//! of UAX #15 over [`DECOMP`] and [`CCC`].
//! Test: `nfc_matches_python_on_composed_decomposed_and_singleton_text`.

use std::collections::HashMap;
use std::sync::OnceLock;

use super::nfc_table::{CCC, COMPOSES, DECOMP};

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

/// The canonical combining class of `c`.
fn ccc(c: u32) -> u8 {
    let i = CCC.partition_point(|r| r.0 <= c);
    match i.checked_sub(1).map(|i| CCC[i]) {
        Some((_, end, k)) if c <= end => k,
        _ => 0,
    }
}

fn decomp(c: u32) -> Option<(u32, u32)> {
    DECOMP
        .binary_search_by_key(&c, |d| d.0 & !COMPOSES)
        .ok()
        .map(|i| (DECOMP[i].1, DECOMP[i].2))
}

fn decompose(c: u32, out: &mut Vec<u32>) {
    if (S_BASE..S_BASE + S_COUNT).contains(&c) {
        let s = c - S_BASE;
        out.push(L_BASE + s / N_COUNT);
        out.push(V_BASE + (s % N_COUNT) / T_COUNT);
        if !s.is_multiple_of(T_COUNT) {
            out.push(T_BASE + s % T_COUNT);
        }
        return;
    }
    match decomp(c) {
        Some((a, b)) => {
            decompose(a, out);
            if b != 0 {
                decompose(b, out);
            }
        }
        None => out.push(c),
    }
}

fn pairs() -> &'static HashMap<(u32, u32), u32> {
    static P: OnceLock<HashMap<(u32, u32), u32>> = OnceLock::new();
    P.get_or_init(|| {
        DECOMP
            .iter()
            .filter(|d| d.0 & COMPOSES != 0)
            .map(|d| ((d.1, d.2), d.0 & !COMPOSES))
            .collect()
    })
}

fn compose(a: u32, b: u32) -> Option<u32> {
    if (L_BASE..L_BASE + L_COUNT).contains(&a) && (V_BASE..V_BASE + V_COUNT).contains(&b) {
        return Some(S_BASE + ((a - L_BASE) * V_COUNT + (b - V_BASE)) * T_COUNT);
    }
    let lv = (S_BASE..S_BASE + S_COUNT).contains(&a) && (a - S_BASE).is_multiple_of(T_COUNT);
    if lv && (T_BASE + 1..T_BASE + T_COUNT).contains(&b) {
        return Some(a + (b - T_BASE));
    }
    pairs().get(&(a, b)).copied()
}

/// Why: Python's `unicodedata.normalize("NFC", s)`.
/// What: see the module doc.
/// Test: `nfc_matches_python_on_composed_decomposed_and_singleton_text`.
pub(crate) fn nfc(s: &str) -> String {
    let mut buf = Vec::with_capacity(s.len());
    for c in s.chars() {
        decompose(u32::from(c), &mut buf);
    }
    // Canonical ordering: a stable sort of each run of non-starters.
    let mut i = 0;
    while i < buf.len() {
        if ccc(buf[i]) == 0 {
            i += 1;
            continue;
        }
        let start = i;
        while i < buf.len() && ccc(buf[i]) != 0 {
            i += 1;
        }
        buf[start..i].sort_by_key(|c| ccc(*c));
    }
    let mut out: Vec<u32> = Vec::with_capacity(buf.len());
    let mut starter: Option<usize> = None;
    let mut last: u16 = 0;
    for c in buf {
        let k = u16::from(ccc(c));
        if let Some(si) = starter {
            let adjacent = out.len() == si + 1;
            if (adjacent || (last != 0 && last < k))
                && let Some(x) = compose(out[si], c)
            {
                out[si] = x;
                continue;
            }
        }
        if k == 0 {
            starter = Some(out.len());
        }
        last = k;
        out.push(c);
    }
    out.into_iter().filter_map(char::from_u32).collect()
}
