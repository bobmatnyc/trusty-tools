//! JSON and Google OAuth client files for `tm secrets import`.
//! See G0b secrets JSON import.
//!
//! Why: owner request G0b — bulk-load a JSON credential file, with a named
//! path for the Google OAuth client JSON that Google Cloud hands out.
//! What: [`detect`] picks the parser; [`parse_json`]
//! flattens an object to `KEY_PATH` entries; [`parse_google_oauth`] takes
//! the three client fields. Both return the dotenv parser's `EnvEntry`
//! list, so `verbs::import` stores them through the one `secrets.set` loop.
//! No error text carries a value or a line of the file: only key names,
//! field names and a line/column position.
//! Test: `import_json_tests.rs` beside this file, and the `import_json_*`
//! and `import_google_*` tests in `tests.rs`.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{anyhow, bail};
use serde_json::{Map, Value};
use trusty_secrets::store::EnvEntry;

use crate::cli::ImportFormat;

/// The group the Google path stores under when the caller names none.
pub(super) const GOOGLE_GROUP: &str = "google-oauth";

/// The three Google client fields stored, in output order.
const GOOGLE_FIELDS: [&str; 3] = ["client_id", "client_secret", "project_id"];

/// The format to parse `text` as.
///
/// What: an explicit `google-oauth` or `dotenv` wins. Otherwise `--format
/// json` or a `.json` extension means JSON, and JSON whose top level holds
/// an `installed` or `web` object is the Google path. Anything else is
/// dotenv.
/// Test: `detect_picks_google_for_a_client_json_and_json_by_extension`.
pub(super) fn detect(explicit: Option<ImportFormat>, path: &Path, text: &str) -> ImportFormat {
    let json = match explicit {
        Some(ImportFormat::Json) => true,
        Some(other) => return other,
        None => path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("json")),
    };
    if !json {
        return ImportFormat::Dotenv;
    }
    let google = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.as_object().map(google_client).map(|c| c.is_some()))
        .unwrap_or(false);
    if google {
        ImportFormat::GoogleOauth
    } else {
        ImportFormat::Json
    }
}

/// The `installed` or `web` object of a Google client JSON, if any.
fn google_client(top: &Map<String, Value>) -> Option<&Map<String, Value>> {
    ["installed", "web"]
        .into_iter()
        .find_map(|k| top.get(k).and_then(Value::as_object))
}

/// Parse `text` as JSON with an error that names a position, never text.
fn parse_object(text: &str) -> anyhow::Result<Map<String, Value>> {
    // See G0b secrets JSON import: `serde_json::Error`'s Display is not
    // trusted to omit input; only its category and position are shown.
    let value: Value = serde_json::from_str(text).map_err(|e| {
        anyhow!(
            "is not valid JSON ({:?} error at line {} column {})",
            e.classify(),
            e.line(),
            e.column()
        )
    })?;
    match value {
        Value::Object(map) => Ok(map),
        _ => bail!("must hold one JSON object at the top level"),
    }
}

/// Flatten a JSON object to dotenv-style entries.
///
/// Why: G0b — a JSON credential file holds the same `NAME=value` pairs as a
/// `.env`, nested.
/// What: each nested key path is joined with `_` and upper-cased, `-`
/// becoming `_` (`{"db": {"pass-word": ..}}` is `DB_PASS_WORD`). A string
/// is stored as is; a number or boolean as its JSON text. An array, a null,
/// a key segment outside `[A-Za-z0-9_-]`, a name not starting with a letter
/// or `_`, and two paths that flatten to one name are refused, naming the
/// key only. The whole file is checked before any entry is returned.
/// Test: `json_flattens_nested_keys_to_upper_snake_names`,
/// `json_refuses_an_array_naming_the_key_only`,
/// `json_refuses_malformed_text_without_echoing_it`.
pub(super) fn parse_json(text: &str) -> anyhow::Result<Vec<EnvEntry>> {
    let top = parse_object(text)?;
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    flatten(&top, "", &mut entries, &mut seen)?;
    Ok(entries)
}

fn flatten(
    map: &Map<String, Value>,
    prefix: &str,
    entries: &mut Vec<EnvEntry>,
    seen: &mut BTreeSet<String>,
) -> anyhow::Result<()> {
    for (segment, value) in map {
        let name = join(prefix, segment)?;
        let raw = match value {
            Value::Object(inner) => {
                flatten(inner, &name, entries, seen)?;
                continue;
            }
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            // See G0b secrets JSON import: refused by name, never by value.
            Value::Array(_) => bail!("{name} is an array; only strings, numbers and booleans"),
            Value::Null => bail!("{name} is null; only strings, numbers and booleans"),
        };
        if !seen.insert(name.clone()) {
            bail!("{name} is defined twice after flattening");
        }
        entries.push(EnvEntry::new(name, raw));
    }
    Ok(())
}

/// `prefix_SEGMENT`, upper-cased, as a POSIX env name.
fn join(prefix: &str, segment: &str) -> anyhow::Result<String> {
    let valid = !segment.is_empty()
        && segment
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'));
    if !valid {
        // A key name is not a value; only its position is shown, since a
        // malformed segment might be a value pasted into the key slot.
        let at = if prefix.is_empty() {
            "top level"
        } else {
            prefix
        };
        bail!("a key under {at} may hold only [A-Za-z0-9_-]");
    }
    let segment = segment.to_ascii_uppercase().replace('-', "_");
    let name = if prefix.is_empty() {
        segment
    } else {
        format!("{prefix}_{segment}")
    };
    if !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        bail!("{name} must start with a letter or `_`");
    }
    Ok(name)
}

/// Take the client fields of a Google OAuth client JSON.
///
/// Why: G0b — the client JSON Google Cloud downloads is the common case and
/// should land as one named group.
/// What: reads the `installed` object, else `web`. Each of `client_id`,
/// `client_secret` and `project_id` must be a non-empty string; a missing
/// one is refused naming the field. `project_id` must be 1 to 30 of
/// `[a-z0-9-]`. Returns entries named `<project_id>.CLIENT_ID`,
/// `<project_id>.CLIENT_SECRET` and `<project_id>.PROJECT_ID`; `import`
/// prefixes the group, giving `google-oauth.<project_id>.CLIENT_ID` — the
/// store's `<group>.<KEY>` form of `google-oauth/<project_id>`.
/// Test: `google_installed_and_web_shapes_give_three_grouped_entries`,
/// `google_missing_field_is_refused_naming_the_field_only`.
pub(super) fn parse_google_oauth(text: &str) -> anyhow::Result<Vec<EnvEntry>> {
    let top = parse_object(text)?;
    let Some(client) = google_client(&top) else {
        bail!("is not a Google OAuth client JSON (no `installed` or `web` object)");
    };
    let mut values = Vec::with_capacity(GOOGLE_FIELDS.len());
    for field in GOOGLE_FIELDS {
        match client.get(field).and_then(Value::as_str) {
            Some(v) if !v.is_empty() => values.push(v),
            // See G0b secrets JSON import: the field name only.
            _ => bail!("Google OAuth client JSON lacks a string `{field}`"),
        }
    }
    let project = values[2];
    let valid = project.len() <= 30
        && project
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid {
        bail!("`project_id` is not a Google project id (1 to 30 of [a-z0-9-])");
    }
    Ok(GOOGLE_FIELDS
        .iter()
        .zip(values)
        .map(|(field, v)| EnvEntry::new(format!("{project}.{}", field.to_ascii_uppercase()), v))
        .collect())
}
