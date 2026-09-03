//! The one `[report].findings` writer every assurance collector shares (#6077).
//!
//! Why: #6075's CVE leg and #6076's license leg each carry a private
//! `write_into` that reads the manifest, walks to `[report].findings`, skips a
//! row already declared, appends the rest format-preserving, and writes the
//! document back. The two are identical apart from the type they iterate.
//! #6077's secrets leg needs the same thirty lines a third time, and three
//! copies of one TOML mutation drift the moment any of them grows a rule —
//! BASE-ENGINEER's duplicate-elimination bar (same domain, over 80% similar)
//! puts that at consolidate rather than copy.
//!
//! What: [`append`], which takes rows already built as `toml_edit` inline
//! tables and the key set that identifies one, plus [`first_line`], the
//! one-line diagnostic reducer all three legs reach for. Each collector still
//! owns its own row SHAPE and its own identity rule — what a duplicate row IS
//! differs per collector, and this module takes that as a parameter rather than
//! deciding it.
//!
//! Nothing here decides whether a row should exist; that stays with the
//! collector, which is also where the fail-open gap wording lives.
//!
//! Test: `super::cve::cve_tests::{the_advisories_land_in_the_manifest,
//! a_resumed_sweep_does_not_duplicate_a_row}`,
//! `super::license::license_tests::the_findings_land_in_the_manifest`,
//! `super::secrets::secrets_tests::the_leaks_land_in_the_manifest`.

use std::path::Path;

use toml_edit::{Array, DocumentMut, InlineTable, Item, Table, Value};

/// Append `rows` to `[report].findings` in the manifest at `path`.
///
/// Why: the manifest is the interface (owner ruling 2026-08-19). A scan a
/// collector performs and does not write reaches no renderer — not the sweep's,
/// and not the recipient's own re-render of the delivered package.
///
/// Why nothing is written for an EMPTY `rows`: the presence of the key is what
/// trusty-review renders the Assurance Scans section from, and an empty array
/// would put an empty table in the report. A clean scan states itself through
/// its `[report].gaps` scope line instead.
/// What: appends each row that is not already declared, comparing the values of
/// every key in `identity` — the caller's definition of "the same finding".
/// Written format-preserving with `toml_edit`, exactly as
/// [`super::priority::write_into`] writes its ranking, so the two other crates
/// that own this document keep their key order and their comments.
///
/// # Errors
/// One line, safe to show the recipient, when the manifest cannot be read,
/// parsed, or written back. The caller turns it into a gap of its own.
///
/// # Postconditions
/// On `Ok`, every row is declared exactly once under the `identity` key set and
/// nothing else in the document changed. An empty `rows` writes nothing, does
/// not open the file, and cannot fail.
///
/// Test: `super::cve::cve_tests::{the_advisories_land_in_the_manifest,
/// a_resumed_sweep_does_not_duplicate_a_row}`,
/// `super::secrets::secrets_tests::a_resumed_sweep_does_not_restate_a_leak`.
pub fn append(path: &Path, rows: &[InlineTable], identity: &[&str]) -> Result<(), String> {
    if rows.is_empty() {
        return Ok(());
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{} could not be read ({e})", path.display()))?;
    let mut doc: DocumentMut = text
        .parse()
        .map_err(|e| format!("{} is not readable as TOML ({e})", path.display()))?;

    let report = doc
        .entry("report")
        .or_insert_with(|| Item::Table(Table::new()));
    let table = report
        .as_table_like_mut()
        .ok_or_else(|| "the manifest's `report` is not a table".to_string())?;
    let item = table
        .entry("findings")
        .or_insert_with(|| Item::Value(Value::Array(Array::new())));
    let array = item
        .as_array_mut()
        .ok_or_else(|| "the manifest's `report.findings` is not an array".to_string())?;

    for row in rows {
        if array
            .iter()
            .any(|declared| same_row(declared, row, identity))
        {
            continue;
        }
        let mut value = Value::InlineTable(row.clone());
        value.decor_mut().set_prefix("\n    ");
        array.push_formatted(value);
    }
    array.set_trailing("\n");
    array.set_trailing_comma(true);

    std::fs::write(path, doc.to_string())
        .map_err(|e| format!("{} could not be written ({e})", path.display()))
}

/// Whether a row already declared in the manifest is this one.
///
/// A declared entry that is not an inline table is never a match: the array is
/// this crate's to shape, and anything else in it is another producer's data
/// that must be left alone rather than deduplicated against.
fn same_row(declared: &Value, row: &InlineTable, identity: &[&str]) -> bool {
    let Some(table) = declared.as_inline_table() else {
        return false;
    };
    identity
        .iter()
        .all(|key| table.get(key).and_then(Value::as_str) == row.get(key).and_then(Value::as_str))
}

/// The first non-empty line of a diagnostic stream, for a one-line gap.
///
/// // #6720: this is only ever the PARENTHETICAL of a gap line. A collector's
/// own diagnosis leads, because a child process's first stderr line is
/// routinely an unrelated update notice and reporting it as the cause discards
/// the real one.
///
/// Test: `super::secrets::secrets_tests::a_noisy_stderr_never_replaces_the_diagnosis`.
pub fn first_line(stderr: &str) -> &str {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no diagnostic")
}
