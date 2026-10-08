//! What a 1Password item is to this backend: the list rows it reads, the
//! template it writes, and the `op://` reference it reads through.
//!
//! Why: #7519 A7 — only validated names may reach `op`'s argv or a secret
//! reference. A trusty vault (`trusty/<owner>/<repo>`) holds `/`, which an
//! `op://` reference cannot carry, so a read names the vault and the item by
//! the ids `op item list` returned, each checked here before use. The key
//! never reaches argv or a reference: it is the item's title, compared with
//! list rows and written inside the stdin or file template.
//! What: [`Listed`] and [`matching`] (a listing to the rows titled `key`),
//! [`template`] (the JSON item, value inside, as a [`SecretValue`]),
//! [`op_reference`] and [`checked_id`].
//! Test: `onepassword_item_path_builder_refuses_hostile_segments`,
//! `onepassword_hostile_list_ids_never_reach_argv`.

use serde::{Deserialize, Serialize};

use crate::api::{SecretKey, SecretValue};

/// The category of every item this backend writes.
pub(super) const CATEGORY: &str = "PASSWORD";

/// The field that holds the value.
pub(super) const FIELD: &str = "password";

/// Longest id accepted from `op` output; 1Password ids are 26 characters.
const MAX_ID_LEN: usize = 64;

/// One `op item list --format json` row: ids, title and category only.
///
/// What: serde skips every other field, including
/// `additional_information`, so nothing else of the item is kept.
#[derive(Debug, Deserialize)]
pub(super) struct Listed {
    pub(super) id: String,
    pub(super) title: String,
    #[serde(default)]
    pub(super) category: String,
    pub(super) vault: ListedVault,
}

/// The vault a listed item sits in.
#[derive(Debug, Deserialize)]
pub(super) struct ListedVault {
    pub(super) id: String,
}

/// The rows of a listing whose title is exactly `key`.
///
/// What: empty or whitespace output is an empty vault. Output that does not
/// parse is `Err` with a fixed reason; the parser's message, which could
/// quote the output, is dropped.
/// Test: `onepassword_set_creates_with_the_value_on_stdin_only`.
pub(super) fn matching(stdout: &str, key: &SecretKey) -> Result<Vec<Listed>, &'static str> {
    if stdout.trim().is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<Listed> =
        serde_json::from_str(stdout).map_err(|_| "the CLI's item list did not parse")?;
    Ok(rows
        .into_iter()
        .filter(|row| row.title == key.as_str())
        .collect())
}

#[derive(Serialize)]
struct Template<'a> {
    title: &'a str,
    category: &'static str,
    fields: [Field<'a>; 1],
}

#[derive(Serialize)]
struct Field<'a> {
    id: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
    purpose: &'static str,
    label: &'static str,
    value: &'a str,
}

/// The JSON item template for `key` holding `value`.
///
/// Why: owner ruling 2026-10-07 — `op item create -` reads this on stdin,
/// and `op item edit --template` from a 0600 file; a `field=value`
/// assignment would put the value in argv (DOC-74 §8.2 correction).
/// What: a `PASSWORD` item titled `key` with one concealed `password`
/// field, serialised straight into a [`SecretValue`].
/// Test: `onepassword_set_creates_with_the_value_on_stdin_only`.
pub(super) fn template(key: &SecretKey, value: &SecretValue) -> Result<SecretValue, &'static str> {
    let item = Template {
        title: key.as_str(),
        category: CATEGORY,
        fields: [Field {
            id: FIELD,
            kind: "CONCEALED",
            purpose: "PASSWORD",
            label: FIELD,
            value: value.expose(),
        }],
    };
    serde_json::to_string(&item)
        .map(SecretValue::new)
        .map_err(|_| "the item template could not be built")
}

/// `op://<vault id>/<item id>/password`.
///
/// Why: A7 — the reference is the one argv word naming what `op read`
/// returns; a segment holding `/`, `..`, `op://` or a newline would name
/// something else.
/// What: both ids pass [`checked_id`] or the reference is refused.
/// Test: `onepassword_item_path_builder_refuses_hostile_segments`.
pub(super) fn op_reference(vault_id: &str, item_id: &str) -> Result<String, &'static str> {
    Ok(format!(
        "op://{}/{}/{FIELD}",
        checked_id(vault_id)?,
        checked_id(item_id)?
    ))
}

/// `raw`, when it is safe as an `op://` segment and as an argv word.
///
/// What: 1–64 characters of `[A-Za-z0-9_.-]`, not `.` or `..`, not
/// starting with `-` (which `op` would read as a flag).
/// Test: `onepassword_item_path_builder_refuses_hostile_segments`.
pub(super) fn checked_id(raw: &str) -> Result<&str, &'static str> {
    let safe = !raw.is_empty()
        && raw.len() <= MAX_ID_LEN
        && raw != "."
        && raw != ".."
        && !raw.starts_with('-')
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if safe {
        Ok(raw)
    } else {
        Err("the CLI returned an id outside op's reference grammar")
    }
}
