//! Pull the two effort-only fields out of a `work_items.raw_json` payload
//! (issue #3915): story points, and the parent key that makes a ticket a
//! child of some epic.
//!
//! Why: [`crate::core::pm_work::extract`] already recovers reporter,
//! description and creation date, and the effort scorer reuses it verbatim.
//! What it does not recover is story points — the one field issue #3915
//! calls out as structurally unreliable — or the parent link that lets the
//! loader count an epic's children. Both are effort-tier concerns, so they
//! live here rather than widening the work-tier extractor.
//!
//! What: [`extract_fields`] is a best-effort parse. A missing or
//! unparseable field yields `None` rather than failing the row.
//!
//! # Story points across four custom-field IDs
//!
//! `JiraClient::get_story_point_field` (`collect/jira/client.rs`) discovers
//! the field at COLLECT time by asking `/rest/api/3/field` for a descriptor
//! whose NAME is "Story Points" or "Story point estimate", then caches the
//! single ID it found. That discovery is unavailable here: the backfill
//! reads payloads already on disk, with no descriptor list and no HTTP.
//!
//! This module reuses the same discovery SHAPE offline — match by field
//! name first, fall back to a known-ID list — because one global lookup is
//! exactly what migration v23 warns is insufficient. The source instance
//! spells the field as four different per-project custom-field IDs
//! (`customfield_10004` / `10016` / `13001` / `13737`), so a payload
//! collected from one project carries a key a payload from another never
//! will. [`super::thresholds::STORY_POINT_FIELD_NAMES`] and
//! [`super::thresholds::STORY_POINT_FIELD_IDS`] are the v1 lists, tried in
//! that order.
//!
//! Test: `tests` in `tests.rs`.

use serde_json::Value;

use super::{plausible_story_points, thresholds};

/// The effort-only fields recovered from one `raw_json` payload.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EffortFields {
    /// Story points, already range-checked by
    /// [`super::plausible_story_points`]. `None` when the payload carried
    /// no recognised field, an unparseable value, or an implausible one —
    /// all three are the same thing to the scorer, which simply drops the
    /// term.
    pub story_points: Option<f64>,
    /// The key of this ticket's parent, when it has one. The loader inverts
    /// these into per-parent child counts.
    pub parent_key: Option<String>,
}

/// Parse `raw_json` into the fields the effort scorer reads.
///
/// Why: see the module docs.
/// What: parses once and reads both fields off the same tree, so the
/// backfill pays one `serde_json` parse per ticket for the effort tier.
/// An unparseable payload yields [`EffortFields::default`].
/// Test: `extracts_story_points_from_a_named_field`,
/// `extracts_story_points_from_each_known_custom_field_id`,
/// `implausible_story_points_are_treated_as_absent`,
/// `extracts_a_jira_parent_key`, `unparseable_payload_yields_no_fields`.
#[must_use]
pub fn extract_fields(raw_json: Option<&str>) -> EffortFields {
    let Some(text) = raw_json else {
        return EffortFields::default();
    };
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return EffortFields::default();
    };
    let fields = v.get("fields");

    EffortFields {
        story_points: extract_story_points(&v, fields),
        parent_key: extract_parent_key(&v, fields),
    }
}

/// Story points, by field name first and known custom-field ID second.
///
/// The name pass is what keeps this working on an instance whose field ID
/// is not in the v1 list: JIRA payloads collected with `*all` carry the
/// custom-field keys, but Linear and Azure DevOps payloads name the field
/// outright. The ID pass is what keeps it working on the four spellings
/// issue #3915 measured.
fn extract_story_points(root: &Value, fields: Option<&Value>) -> Option<f64> {
    for object in [fields, Some(root)].into_iter().flatten() {
        let Some(map) = object.as_object() else {
            continue;
        };
        for (key, value) in map {
            if thresholds::STORY_POINT_FIELD_NAMES.contains(&normalize_key(key).as_str()) {
                if let Some(points) = numeric(value).and_then(plausible_story_points) {
                    return Some(points);
                }
            }
        }
        for id in thresholds::STORY_POINT_FIELD_IDS {
            if let Some(points) = map
                .get(*id)
                .and_then(numeric)
                .and_then(plausible_story_points)
            {
                return Some(points);
            }
        }
    }
    None
}

/// Lowercase `key` and drop every non-alphanumeric character, so
/// `"Story Points"`, `"story_points"` and `"storyPoints"` all compare equal.
fn normalize_key(key: &str) -> String {
    key.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// A number from either a JSON number or a numeric string.
///
/// Some JIRA custom fields serialize the estimate as a string; a value that
/// is neither is not an error, just not a story point.
fn numeric(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
}

/// Parent ticket key across the four providers.
///
/// JIRA nests it at `fields.parent.key`; Azure DevOps flattens the parent
/// work-item id into `fields["System.Parent"]`; Linear and GitHub expose a
/// top-level `parent` object. Migration v23 records that decomposition on
/// the source instance runs through `parent_key`, never `epic_key`, which is
/// 100% NULL there — so this is the only link that yields a child count.
fn extract_parent_key(root: &Value, fields: Option<&Value>) -> Option<String> {
    let candidates = [
        fields.and_then(|f| f.get("parent")),
        fields.and_then(|f| f.get("System.Parent")),
        root.get("parent"),
    ];
    candidates.into_iter().flatten().find_map(key_of)
}

/// A ticket key from either a bare scalar or a provider's parent object.
fn key_of(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => non_empty(s),
        Value::Number(n) => Some(n.to_string()),
        Value::Object(_) => ["key", "identifier", "id", "number"]
            .into_iter()
            .find_map(|k| value.get(k).and_then(key_of)),
        _ => None,
    }
}

/// `Some(trimmed)` when `s` has non-whitespace content.
fn non_empty(s: &str) -> Option<String> {
    let trimmed = s.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}
