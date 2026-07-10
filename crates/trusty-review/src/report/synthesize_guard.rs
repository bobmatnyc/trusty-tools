//! Numeric guardrail for LLM-synthesized report prose (M2, #2314).
//!
//! Why: synthesis is the one place an LLM touches a due-diligence report, and the
//! single failure mode that would silently corrupt a deal analysis is a
//! fabricated figure ("42% of the code is untested") that never appeared in the
//! source data.  This module is the report-side analogue of the pipeline's
//! verdict-integrity posture: after the model injects prose, every digit
//! sequence in that prose MUST trace back to a number already present in the
//! deterministic [`ReportModel`].  A violation rejects the field (fail-closed) —
//! it is never partially trusted.
//! What: [`allowed_numbers`] builds the canonical set of numbers the source model
//! actually contains (walking the serialized model); [`verify_prose`] checks that
//! every number token in a candidate string is in that set, tolerating formatting
//! variants (thousands separators, `$`, `%`, trailing-zero decimals).
//! Test: `synthesize_tests.rs` covers extraction, canonicalisation, the
//! formatting-variant equivalences, and the reject path.

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

/// Build the set of numbers the deterministic source model legitimately contains.
///
/// Why: the guardrail's ground truth is "what figures does the report's own data
/// present?" — computed from the machine-readable twin so it captures every
/// metric, count, LoC total, git SHA digit-run, and date the report is derived
/// from, with no hand-maintained field list to drift.
/// What: walks the serialized [`Value`], adding canonical keys for every JSON
/// number node and every number token found inside every string node.
/// Test: `allowed_numbers_covers_metrics`.
pub fn allowed_numbers(model: &Value) -> HashSet<String> {
    let mut set = HashSet::new();
    collect(model, &mut set);
    set
}

/// Recursively collect canonical number keys from a JSON value tree.
fn collect(value: &Value, set: &mut HashSet<String>) {
    match value {
        Value::Number(n) => {
            set.insert(canonicalize(&n.to_string()));
        }
        Value::String(s) => {
            for k in numbers_in(s) {
                set.insert(k);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect(v, set)),
        Value::Object(map) => map.values().for_each(|v| collect(v, set)),
        Value::Bool(_) | Value::Null => {}
    }
}

/// Verify that every number in `prose` traces back to the allowed set.
///
/// Why: this is the fail-closed gate — a single unverifiable figure disqualifies
/// the whole synthesized field, which is then dropped in favour of the
/// deterministic honesty marker.
/// What: returns `Ok(())` when every canonical number token in `prose` is a
/// member of `allowed`; otherwise `Err(first_offending_token)`.  Prose with no
/// numbers always passes.
/// Test: `verify_prose_passes_faithful`, `verify_prose_rejects_fabricated`.
pub fn verify_prose(prose: &str, allowed: &HashSet<String>) -> Result<(), String> {
    for token in numbers_in(prose) {
        if !allowed.contains(&token) {
            return Err(token);
        }
    }
    Ok(())
}
