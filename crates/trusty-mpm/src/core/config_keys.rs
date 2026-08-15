//! Unrecognised-key reporting for the HOST-level config files (#5207).
//!
//! Why: the owner ruling's complaint is that a misspelled config key currently
//! no-ops in silence. The obvious fix — `#[serde(deny_unknown_fields)]` on
//! [`crate::core::config::MpmConfig`] and
//! [`crate::core::trusty_tools_config::TrustyToolsConfig`] — is a REGRESSION on
//! those two, because both loaders answer a parse failure by returning
//! `Default`. Denying unknown fields there would upgrade "one key is ignored"
//! into "the entire file is ignored": a single typo would silently drop every
//! model override, every section, the whole config, behind one `warn` line. The
//! failure it introduces is strictly worse than the one it fixes.
//!
//! So strictness is applied where it is safe and total — the NEW committed
//! surface, [`crate::core::project_config::ProjectLevelConfig`], which has no
//! legacy corpus and is reviewed in a PR — and the host files get this instead:
//! the lenient parse they already had, plus a loud, precise report of every key
//! that parse threw away. The typo stops being silent; the other keys keep
//! working.
//!
//! What: [`unknown_key_paths`] diffs the raw document against a round-trip of
//! the parsed struct. Anything present in the input but absent from the
//! round-trip was dropped by serde, i.e. the schema does not define it. This
//! needs no hand-maintained key list, so it cannot drift as sections are added,
//! and it descends into nested tables. [`report_unknown_keys`] logs the result.
//! Test: `unknown_top_level_key_is_reported`, `unknown_nested_key_is_reported`,
//! `known_keys_are_not_reported`, `map_valued_sections_are_not_reported`,
//! `explicit_null_is_not_reported` in `config_keys_tests.rs`.
//!
//! [`unknown_key_paths`]: crate::core::config_keys::unknown_key_paths
//! [`report_unknown_keys`]: crate::core::config_keys::report_unknown_keys

use serde::Serialize;
use serde_json::Value;

/// Convert a TOML document to the common `serde_json::Value` tree.
///
/// Why: the two host config files are TOML and YAML; normalising both into one
/// tree type lets a single diff serve them. Returns `None` for input that does
/// not parse — the caller has already reported that as a malformed file.
/// Test: `unknown_top_level_key_is_reported`.
pub fn toml_document(raw: &str) -> Option<Value> {
    toml::from_str::<Value>(raw).ok()
}

/// Convert a YAML document to the common `serde_json::Value` tree.
///
/// Why: see [`toml_document`]. A YAML file whose root is not a mapping (empty
/// file, bare scalar) yields `None`, since there are no keys to check.
/// Test: `explicit_null_is_not_reported`.
pub fn yaml_document(raw: &str) -> Option<Value> {
    serde_yaml::from_str::<Value>(raw)
        .ok()
        .filter(Value::is_object)
}

/// Dotted paths of every key in `raw` that `parsed`'s schema does not define.
///
/// Why: this is the whole detection mechanism, and it is deliberately derived
/// rather than declared — round-tripping the PARSED value re-emits exactly the
/// keys serde understood, so the difference is exactly what serde discarded. A
/// hand-written list of valid keys would have to be updated every time a section
/// is added, and would silently stop catching typos the day someone forgot.
/// What: returns dotted paths (`"models.tiers.hiaku"`) in document order.
/// Descends into any table present on both sides. Two deliberate exclusions:
/// a key whose input value is `null` is skipped, because a field with
/// `skip_serializing_if = "Option::is_none"` legitimately round-trips to
/// nothing; and a table that round-trips as a table is compared key-by-key,
/// which means a `HashMap`-valued section (`[models.agents]`, whose keys are
/// arbitrary agent names) reports nothing, since every key survives.
/// Test: `unknown_top_level_key_is_reported`, `unknown_nested_key_is_reported`,
/// `known_keys_are_not_reported`, `map_valued_sections_are_not_reported`.
pub fn unknown_key_paths<T: Serialize>(raw: &Value, parsed: &T) -> Vec<String> {
    let Ok(known) = serde_json::to_value(parsed) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    diff_into(raw, &known, "", &mut out);
    out
}

/// Recursive worker for [`unknown_key_paths`].
///
/// What: walks `raw`'s object keys; a key missing from `known` is recorded, a
/// key present on both recurses. Non-object nodes terminate the walk.
/// Test: via [`unknown_key_paths`]'s tests.
fn diff_into(raw: &Value, known: &Value, prefix: &str, out: &mut Vec<String>) {
    let (Value::Object(raw_map), Value::Object(known_map)) = (raw, known) else {
        return;
    };
    for (key, raw_value) in raw_map {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match known_map.get(key) {
            Some(known_value) => diff_into(raw_value, known_value, &path, out),
            // An explicit `null` legitimately round-trips to nothing via
            // `skip_serializing_if = "Option::is_none"`, so it is not a typo.
            None if raw_value.is_null() => {}
            None => out.push(path),
        }
    }
}

/// Log every unrecognised key in a host config file.
///
/// Why: the whole point of the module — an ignored key must produce a signal an
/// operator can act on, naming the file and the exact path so the fix is
/// mechanical. `warn` rather than `error`: the file still applied, and every key
/// the schema does define took effect.
/// What: one warning listing all unrecognised paths; silent when there are none
/// (the overwhelmingly common case).
/// Test: `report_is_silent_for_a_clean_document`.
pub fn report_unknown_keys<T: Serialize>(file: &str, raw: &Value, parsed: &T) {
    let unknown = unknown_key_paths(raw, parsed);
    if unknown.is_empty() {
        return;
    }
    tracing::warn!(
        "{file}: ignoring {} unrecognised key(s): {}. \
         These had no effect — check for a typo against the documented schema.",
        unknown.len(),
        unknown.join(", ")
    );
}

#[cfg(test)]
#[path = "config_keys_tests.rs"]
mod tests;
