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
//! variants (thousands separators, `$`, `%`, trailing-zero decimals),
//! conventional roundings of a measured value (#6030 — see [`rounded_forms`]),
//! and scaled-unit restatements of a large integer (#6137 — see
//! [`scaled_forms`]).  [`allowed_numbers_with`] additionally admits the figures
//! the report computes at render time and prints in its own sections.
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
/// integer key yields nothing here — scaled-unit restatements of a large integer
/// are [`scaled_forms`]' job, not this function's.
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

/// The magnitudes at which a large integer is conventionally restated.
///
/// Thousand, million, billion, trillion — the decimal-point shift each implies.
const SCALE_EXPONENTS: [usize; 4] = [3, 6, 9, 12];

/// How many significant digits a scaled restatement must keep to be admitted.
///
/// Why: one significant digit is not a restatement, it is a different number.
/// Measured 1,553,771 admits `1.55` and `1.6` million; it must not admit a bare
/// `2`, which reads as a count in prose and would let a fabricated small integer
/// through.
const MIN_SCALED_SIGNIFICANT_DIGITS: usize = 2;

/// Every scaled-unit restatement of a large integer key.
///
/// Why: #6137 — [`rounded_forms`] widens decimal precision only, so an integer
/// key admitted nothing coarser than itself. A narrative that writes a measured
/// 1,553,771 lines as "1.55 million lines" is restating the source figure, and
/// rejecting it vetoed five LLM-written fields of one report (the executive
/// summary, the code-quality, security and authorship paragraphs, and a
/// top-risk row). The guardrail compares canonical STRINGS and the prose
/// tokeniser never sees the unit word, so the only way `1.55` can verify is for
/// the allowed set to carry it.
/// What: for each magnitude in [`SCALE_EXPONENTS`] at which the value is still
/// at least 1, shifts the decimal point and emits that scaled value plus every
/// [`rounded_forms`] spelling of it — 1553771 yields `1.553771`, `1.55`, `1.6`,
/// `1553.771`, `1554`, and the rest. Forms below
/// [`MIN_SCALED_SIGNIFICANT_DIGITS`] are dropped. A key that is not a plain
/// integer of at least four digits yields nothing.
/// Test: `allowed_numbers_admits_scaled_unit_forms`,
/// `verify_prose_accepts_a_million_scale_restatement`,
/// `verify_prose_still_rejects_a_fabricated_figure`.
fn scaled_forms(canonical: &str) -> Vec<String> {
    if canonical.len() < 4 || !canonical.bytes().all(|b| b.is_ascii_digit()) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for exp in SCALE_EXPONENTS {
        if canonical.len() <= exp {
            break;
        }
        let (int_part, frac) = canonical.split_at(canonical.len() - exp);
        let scaled = canonicalize(&format!("{int_part}.{frac}"));
        for form in rounded_forms(&scaled).into_iter().chain([scaled]) {
            if significant_digits(&form) >= MIN_SCALED_SIGNIFICANT_DIGITS {
                out.push(form);
            }
        }
    }
    out
}

/// Count a canonical key's significant digits — every digit after the leading
/// zeros, decimal point ignored (`0.5` → 1, `1.55` → 3, `1553` → 4).
fn significant_digits(key: &str) -> usize {
    key.chars()
        .filter(char::is_ascii_digit)
        .skip_while(|c| *c == '0')
        .count()
}

/// Build the set of numbers the deterministic source model legitimately contains.
///
/// Why: the guardrail's ground truth is "what figures does the report's own data
/// present?" — computed from the machine-readable twin so it captures every
/// metric, count, LoC total, git SHA digit-run, and date the report is derived
/// from, with no hand-maintained field list to drift.
/// What: walks the serialized [`Value`], adding canonical keys for every JSON
/// number node and every number token found inside every string node, plus each
/// key's conventional roundings ([`rounded_forms`], #6030) and scaled-unit
/// restatements ([`scaled_forms`], #6137).
/// Test: `allowed_numbers_covers_metrics`, `allowed_numbers_admits_rounded_forms`.
pub fn allowed_numbers(model: &Value) -> HashSet<String> {
    allowed_numbers_with(model, &[])
}

/// The same set, widened by the figures the report itself prints.
///
/// Why: #6137 — some figures a section renders are computed at RENDER time and
/// exist nowhere in the serialized model: the investigation coverage
/// percentage (`73 of 6664 tracked (1.1% coverage…)`) and the authorship
/// trajectory average (`avg 7213.5 commit(s)/mo`) are both derived from counts
/// the model does carry. The synthesis prompt quotes the coverage percentage
/// verbatim, so the model was being asked to cite a figure the guardrail would
/// then reject — which is what dropped the security and authorship paragraphs
/// of one report. A figure the report prints in its own deterministic sections
/// is in-model by definition.
/// What: [`allowed_numbers`], plus every number token found in `printed` (the
/// deterministic report text, from
/// [`figures::printed_figures`](super::figures::printed_figures)), each widened
/// the same way.
/// Test: `allowed_numbers_admits_a_printed_derived_figure`.
pub fn allowed_numbers_with(model: &Value, printed: &[String]) -> HashSet<String> {
    let mut set = HashSet::new();
    collect(model, &mut set);
    for text in printed {
        for key in numbers_in(text) {
            insert_key(&mut set, key);
        }
    }
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

/// Add a canonical key, every coarser rounding of it, and every scaled-unit
/// restatement of it to the allowed set.
fn insert_key(set: &mut HashSet<String>, key: String) {
    set.extend(rounded_forms(&key));
    set.extend(scaled_forms(&key));
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
