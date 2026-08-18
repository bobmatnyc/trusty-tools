//! Whitelist-based synonym normalization for the raw synthesis JSON response
//! (#6009 shape 3).
//!
//! Why: three consecutive live calls to `anthropic/claude-opus-4.8` via
//! OpenRouter, all with `response_format: json_schema` and `strict: true`,
//! produced three different shapes for `top_risks`: `description`/`apps`
//! (canonical), `risk`/`applications` (shape 2), and
//! `risk`/`cost_effort_framing`/`affected_applications` (shape 3, captured at
//! `synthesis-unparseable-response.txt`). `response_format` is best-effort for
//! Anthropic-family models — Anthropic's own API has no native strict-JSON
//! mode, so OpenRouter forwards the request but cannot enforce the shape, and
//! per-shape `#[serde(alias)]` additions can never converge on a model that
//! renames fields differently on every call. Renaming known synonyms onto
//! their canonical name BEFORE serde ever sees the JSON — one table of
//! accepted synonyms per field, instead of one alias attribute per drifted
//! shape — is the class fix. The table is whitelist-based: a key this table
//! has never seen is DROPPED (never guessed onto the nearest-looking
//! canonical name), and the drop is recorded so a reader can see exactly what
//! was lost.
//! What: [`normalize_raw_synthesis`] walks the top-level object and the
//! `top_risks`/`findings` arrays, renaming any recognised synonym key onto
//! its canonical name and removing (with a note) any key that is neither
//! canonical nor a recognised synonym.
//! Test: `synthesize_tests.rs::{parse_raw_recovers_shape3_field_name_drift,
//! parse_raw_recovers_shape2_field_name_drift,
//! parse_raw_rejects_a_wholly_unrecognized_shape,
//! normalize_drops_unrecognized_field_with_note,
//! normalize_prefers_canonical_over_synonym_when_both_present}`.

use std::collections::HashSet;

use serde_json::{Map, Value};

/// One canonical field name plus every accepted synonym for it.
type FieldTable = &'static [(&'static str, &'static [&'static str])];

/// Top-level `RawSynthesis` fields. No drift has been observed here across
/// all three #6009 shapes — listed anyway so an unrecognised top-level key is
/// dropped (and noted) rather than silently retained by serde's default
/// unknown-field tolerance.
const TOP_LEVEL_FIELDS: FieldTable = &[
    ("executive_summary", &[]),
    ("top_risks", &[]),
    ("findings", &[]),
];

/// `top_risks` row fields. Synonyms are the exact drifted names observed in
/// the three live #6009 captures: shape 2 used `risk`/`applications`; shape 3
/// used `risk`/`cost_effort_framing`/`affected_applications`.
const RISK_ROW_FIELDS: FieldTable = &[
    ("description", &["risk"]),
    ("severity", &[]),
    ("cost", &["cost_effort_framing"]),
    ("apps", &["applications", "affected_applications"]),
];

/// `findings` row fields. No drift observed yet — listed so the mechanism is
/// symmetric and ready the moment a shape drifts here too.
const FINDING_ROW_FIELDS: FieldTable = &[
    ("app_slug", &[]),
    ("title", &[]),
    ("severity", &[]),
    ("description", &[]),
    ("evidence", &[]),
    ("component", &[]),
    ("business_impact", &[]),
    ("remediation", &[]),
    ("cost_effort", &[]),
];

/// Normalize the raw synthesis JSON value in place: rename synonyms onto
/// canonical field names, drop anything else, and record every drop.
///
/// Why: runs BEFORE `serde_json::from_value` so a drifted-but-recognised
/// shape parses cleanly through the ordinary typed path, and a wholly
/// unrecognised shape is left with nothing to deserialize into content —
/// still rejected downstream by the numeric guardrail's
/// `NoVerifiableContent`, never silently accepted.
/// What: normalizes the top-level object against [`TOP_LEVEL_FIELDS`], then
/// each `top_risks`/`findings` array item against [`RISK_ROW_FIELDS`] /
/// [`FINDING_ROW_FIELDS`]. A no-op when `value` is not a JSON object.
/// Test: `synthesize_tests.rs::parse_raw_recovers_shape3_field_name_drift`.
pub(super) fn normalize_raw_synthesis(value: &mut Value, notes: &mut Vec<String>) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    normalize_object(obj, TOP_LEVEL_FIELDS, "top level", notes);
    normalize_rows(obj, "top_risks", RISK_ROW_FIELDS, notes);
    normalize_rows(obj, "findings", FINDING_ROW_FIELDS, notes);
}

/// Normalize every object item of the array at `key`, if present.
fn normalize_rows(
    obj: &mut Map<String, Value>,
    key: &str,
    fields: FieldTable,
    notes: &mut Vec<String>,
) {
    let Some(items) = obj.get_mut(key).and_then(Value::as_array_mut) else {
        return;
    };
    for (i, item) in items.iter_mut().enumerate() {
        if let Some(row) = item.as_object_mut() {
            normalize_object(row, fields, &format!("{key} row {}", i + 1), notes);
        }
    }
}

/// Rename recognised synonyms onto their canonical key, then drop (with a
/// note) any key still not in `fields`' canonical set.
///
/// Why: whitelist-based — a key survives ONLY as the canonical name or a
/// synonym this table already lists. This is what keeps normalization from
/// ever guessing: a field name none of the three live shapes produced is
/// dropped, not mapped onto the closest-looking canonical field.
/// What: for each canonical name absent from `obj`, takes the value from the
/// first present synonym (if any). Afterward, any key not in the canonical
/// set — including a synonym left over because the canonical name was
/// ALREADY present — is removed and appended to `notes`.
fn normalize_object(
    obj: &mut Map<String, Value>,
    fields: FieldTable,
    context: &str,
    notes: &mut Vec<String>,
) {
    for (canonical, synonyms) in fields {
        if obj.contains_key(*canonical) {
            continue;
        }
        for syn in *synonyms {
            if let Some(v) = obj.remove(*syn) {
                obj.insert((*canonical).to_string(), v);
                break;
            }
        }
    }

    let canonical_names: HashSet<&str> = fields.iter().map(|(c, _)| *c).collect();
    let unknown: Vec<String> = obj
        .keys()
        .filter(|k| !canonical_names.contains(k.as_str()))
        .cloned()
        .collect();
    for k in unknown {
        obj.remove(&k);
        notes.push(format!(
            "synthesis: dropped unrecognized field '{k}' in {context}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: a synonym must rename onto the canonical key so the typed parse
    /// downstream sees the field it expects.
    /// What: `risk` → `description`, `affected_applications` → `apps`.
    /// Test: this test itself.
    #[test]
    fn normalize_renames_recognised_synonyms() {
        let mut value = serde_json::json!({
            "executive_summary": "x",
            "top_risks": [{"risk": "r1", "affected_applications": "a1"}],
            "findings": []
        });
        let mut notes = Vec::new();
        normalize_raw_synthesis(&mut value, &mut notes);
        assert_eq!(value["top_risks"][0]["description"], "r1");
        assert_eq!(value["top_risks"][0]["apps"], "a1");
        assert!(value["top_risks"][0].get("risk").is_none());
        assert!(value["top_risks"][0].get("affected_applications").is_none());
        assert!(notes.is_empty(), "recognised synonyms record no notes");
    }

    /// Why: a field name none of the three live shapes ever produced must be
    /// dropped, never guessed onto a canonical field — the whitelist
    /// discipline the class fix depends on.
    /// What: an invented key `confidence_score` is removed and noted.
    /// Test: this test itself.
    #[test]
    fn normalize_drops_unrecognized_field_with_note() {
        let mut value = serde_json::json!({
            "executive_summary": "x",
            "top_risks": [{"description": "r1", "apps": "a1", "confidence_score": 0.9}],
            "findings": []
        });
        let mut notes = Vec::new();
        normalize_raw_synthesis(&mut value, &mut notes);
        assert!(value["top_risks"][0].get("confidence_score").is_none());
        assert!(
            notes
                .iter()
                .any(|n| n.contains("confidence_score") && n.contains("top_risks row 1")),
            "unrecognized field must be dropped with a note: {notes:?}"
        );
    }

    /// Why: when both the canonical name and a synonym are present, the
    /// canonical value must win and the stray synonym must be dropped rather
    /// than silently overwriting it.
    /// What: `description` present alongside `risk`; asserts `description`
    /// keeps its own value and `risk` is dropped with a note.
    /// Test: this test itself.
    #[test]
    fn normalize_prefers_canonical_over_synonym_when_both_present() {
        let mut value = serde_json::json!({
            "executive_summary": "x",
            "top_risks": [{"description": "canonical", "risk": "synonym", "apps": "a1"}],
            "findings": []
        });
        let mut notes = Vec::new();
        normalize_raw_synthesis(&mut value, &mut notes);
        assert_eq!(value["top_risks"][0]["description"], "canonical");
        assert!(value["top_risks"][0].get("risk").is_none());
        assert!(notes.iter().any(|n| n.contains("risk")));
    }
}
