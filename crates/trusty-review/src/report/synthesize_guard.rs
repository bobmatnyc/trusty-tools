//! Numeric guardrail for LLM-synthesized report prose (M2, #2314).
//!
//! Why: synthesis is the one place an LLM touches a due-diligence report, and the
//! single failure mode that would silently corrupt a deal analysis is a
//! fabricated figure ("42% of the code is untested") that never appeared in the
//! source data.  This module is the report-side analogue of the pipeline's
//! verdict-integrity posture: after the model injects prose, every digit
//! sequence in that prose MUST trace back to a number already present in the
//! deterministic [`ReportModel`](crate::report::ReportModel).  A violation rejects the field (fail-closed) —
//! it is never partially trusted.
//! What: [`allowed_numbers`] builds the canonical set of numbers the source model
//! actually contains (walking the serialized model); [`verify_prose`] checks that
//! every number token in a candidate string is in that set, tolerating formatting
//! variants (thousands separators, `$`, `%`, trailing-zero decimals) and
//! conventional roundings of a measured value (#6030 — see [`rounded_forms`]).
//! Test: `synthesize_tests.rs` covers extraction, canonicalisation, the
//! formatting-variant equivalences, the rounding equivalences, and the reject
//! path.

use std::collections::HashSet;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

/// Compiled once: a digit sequence with optional thousands separators and an
/// optional decimal fraction.  Leading `$` / trailing `%` are intentionally
/// excluded from the capture so they are stripped for free.
fn number_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[0-9][0-9,]*(?:\.[0-9]+)?").expect("valid number regex"))
}

/// Canonicalise one raw number token to a comparison key.
///
/// Why: the same magnitude appears in prose and source in different skins —
/// `8,200`, `8200`, `8200.0`, `$8200` — and the guardrail must treat them as
/// equal or it would reject faithful restatements.
/// What: strips thousands-separator commas, then, for a decimal, trims trailing
/// zeros and a bare trailing dot (`3.50` → `3.5`, `8200.0` → `8200`).  The
/// surrounding `$`/`%` are already absent because the capture regex omits them.
/// Test: `canonicalize_number_variants`.
fn canonicalize(raw: &str) -> String {
    let no_commas = raw.replace(',', "");
    if let Some(dot) = no_commas.find('.') {
        let (int_part, frac_with_dot) = no_commas.split_at(dot);
        let frac = frac_with_dot[1..].trim_end_matches('0');
        if frac.is_empty() {
            int_part.to_string()
        } else {
            format!("{int_part}.{frac}")
        }
    } else {
        no_commas
    }
}

/// Extract the canonical number keys present in an arbitrary string.
///
/// Why: both the allowed-set construction and the prose check need the same
/// tokeniser so their notions of "a number" are identical.
/// What: runs the number regex over `text` and canonicalises each match.
/// Test: `numbers_in_extracts_and_canonicalises`.
pub fn numbers_in(text: &str) -> Vec<String> {
    number_re()
        .find_iter(text)
        .map(|m| canonicalize(m.as_str()))
        .collect()
}

/// Increment a bare digit string by one, carrying left.
///
/// `"9"` → `"10"`, `"099"` → `"100"`.  A full carry grows the string by one
/// digit, which the caller detects by comparing lengths.  Input is always a
/// digit run taken from an already-canonicalised key, so no parse can fail.
fn increment_digits(digits: &str) -> String {
    let mut out = digits.as_bytes().to_vec();
    for i in (0..out.len()).rev() {
        if out[i] == b'9' {
            out[i] = b'0';
        } else {
            out[i] += 1;
            return String::from_utf8_lossy(&out).into_owned();
        }
    }
    format!("1{}", String::from_utf8_lossy(&out))
}

/// Re-join an integer part and a (possibly empty) fraction into a canonical key.
fn join_parts(int_part: &str, frac: &str) -> String {
    if frac.is_empty() {
        canonicalize(int_part)
    } else {
        canonicalize(&format!("{int_part}.{frac}"))
    }
}

/// Every conventional rounding of a canonical key at a coarser decimal precision.
///
/// Why: #6030 — a narrative that writes a measured 85.19 as "85%" or "85.2%" is
/// restating the source figure, not inventing one, and rejecting it dropped the
/// entire LLM narrative.  The guardrail compares canonical STRINGS, so the only
/// way a coarser spelling can verify is for the allowed set to carry it.
/// What: for a key with a fraction, emits both the truncation and the
/// round-half-up form at every precision coarser than the key's own — `85.19`
/// yields `85.1`, `85.2`, and `85`.  Carries propagate (`9.99` yields `10`).  An
/// integer key yields nothing: this widens decimal precision only, so a measured
/// 8234 never admits "8,000".
/// Test: `allowed_numbers_admits_rounded_forms`, `verify_prose_accepts_rounding`.
fn rounded_forms(canonical: &str) -> Vec<String> {
    let Some(dot) = canonical.find('.') else {
        return Vec::new();
    };
    let (int_part, frac) = (&canonical[..dot], &canonical[dot + 1..]);
    let mut out = Vec::new();
    for k in 0..frac.len() {
        out.push(join_parts(int_part, &frac[..k]));
        if frac.as_bytes()[k] >= b'5' {
            let kept = format!("{int_part}{}", &frac[..k]);
            let bumped = increment_digits(&kept);
            let int_len = int_part.len() + (bumped.len() - kept.len());
            let (bumped_int, bumped_frac) = bumped.split_at(int_len);
            out.push(join_parts(bumped_int, bumped_frac));
        }
    }
    out
}

/// Build the set of numbers the deterministic source model legitimately contains.
///
/// Why: the guardrail's ground truth is "what figures does the report's own data
/// present?" — computed from the machine-readable twin so it captures every
/// metric, count, LoC total, git SHA digit-run, and date the report is derived
/// from, with no hand-maintained field list to drift.
/// What: walks the serialized [`Value`], adding canonical keys for every JSON
/// number node and every number token found inside every string node, plus each
/// key's conventional roundings ([`rounded_forms`], #6030).
/// Test: `allowed_numbers_covers_metrics`, `allowed_numbers_admits_rounded_forms`.
pub fn allowed_numbers(model: &Value) -> HashSet<String> {
    let mut set = HashSet::new();
    collect(model, &mut set);
    set
}

/// Recursively collect canonical number keys from a JSON value tree.
fn collect(value: &Value, set: &mut HashSet<String>) {
    match value {
        Value::Number(n) => insert_key(set, canonicalize(&n.to_string())),
        Value::String(s) => numbers_in(s).into_iter().for_each(|k| insert_key(set, k)),
        Value::Array(items) => items.iter().for_each(|v| collect(v, set)),
        Value::Object(map) => map.values().for_each(|v| collect(v, set)),
        Value::Bool(_) | Value::Null => {}
    }
}

/// Add a canonical key and every coarser rounding of it to the allowed set.
fn insert_key(set: &mut HashSet<String>, key: String) {
    set.extend(rounded_forms(&key));
    set.insert(key);
}

/// Verify that every number in `prose` traces back to the allowed set.
///
/// Why: this is the fail-closed gate — a single unverifiable figure disqualifies
/// the whole synthesized field, which is then dropped in favour of the
/// deterministic honesty marker.
/// What: returns `Ok(())` when every canonical number token in `prose` is a
/// member of `allowed`; otherwise `Err(first_offending_token)`.  Prose with no
/// numbers always passes.
///
/// The acceptance rule is membership in a set that [`allowed_numbers`] has
/// already widened by rounding (#6030), so a token verifies when it equals a
/// measured figure under formatting normalisation (commas, `$`, `%`,
/// trailing-zero decimals) OR equals that figure truncated or rounded half-up to
/// any coarser decimal precision — measured 85.19 admits `85.1`, `85.2`, and
/// `85`.  A token matching no measured figure under any of those readings still
/// fails, and the caller still drops the whole field.
/// Test: `verify_prose_passes_and_rejects`, `verify_prose_accepts_rounding`.
pub fn verify_prose(prose: &str, allowed: &HashSet<String>) -> Result<(), String> {
    for token in numbers_in(prose) {
        if !allowed.contains(&token) {
            return Err(token);
        }
    }
    Ok(())
}
