//! Folding a freshly-built [`DdManifest`] into a `manifest.toml` that already
//! exists (#6190).
//!
//! Why: `manifest.toml` has two writers. tga writes it at the end of every
//! `tga audit`, and trusty-audit's grounding pass edits the same file
//! afterwards, adding `inspect_priority`, `crate_topology`, and the
//! `investigate_*` budget keys. Neither side knew about the other, and tga's
//! write was a whole-file replacement — so running `tga audit --output <dir>`
//! a second time into a live engagement threw away everything the grounding
//! pass had put there. Observed 2026-08-23: an investigation collapsed from 31
//! batches / 226 findings to 5 batches / 31 findings, with no error anywhere.
//!
//! What: [`merge_into`] — tga rewrites the keys it produces and leaves every
//! other key, table, and repository entry in the document exactly as it found
//! them. It is pure: text in, text out, so the preservation property is
//! provable without a live audit.
//!
//! ## The one key this deliberately replaces
//!
//! `[report].gaps` is rewritten, not appended to. A gap line states what THIS
//! run could not assess; carrying a previous run's lines forward would assert
//! unassessed dimensions that this run may well have covered, and that
//! assertion ships to the client inside the report. A grounding pass that runs
//! after this one re-appends its own lines (`grounding::priority::write_into`
//! dedupes as it goes), so the lines that are still true come back.

use toml_edit::{Array, DocumentMut, Item, Table, Value};

use super::dd_manifest::{DdManifest, DdManifestError, DdRepositoryEntry};

/// Rewrite tga's keys inside `existing`, preserving everything else.
///
/// Why: see the module docs — this is the whole of #6190's fix. The rule is one
/// sentence on purpose: tga updates the keys it produces and touches nothing
/// else. Stated that way it covers the three keys the incident named and every
/// key either tool grows later, which an explicit preserve-list would not.
///
/// What: on `[report]`, sets `title` and `gaps`; sets `analyst`, `client`, and
/// `ticketing` only when this run supplies one, so an absent flag never deletes
/// a value the file already carries. Each `[[repositories]]` entry is matched to
/// an existing entry by `path`, else by `name`, and only `name` / `path` /
/// `authorship` are written on it. A fresh entry that matches nothing is
/// appended; an existing entry this run does not name is left alone.
///
/// Test: `super::dd_manifest_merge_tests`.
///
/// # Errors
///
/// [`DdManifestError::MergeSource`] when `existing` is not readable as TOML, or
/// when `report` / `repositories` are present with the wrong shape. Refusing is
/// the point: the caller must not fall back to a replacing write, which is the
/// defect this exists to remove.
pub fn merge_into(manifest: &DdManifest, existing: &str) -> Result<String, DdManifestError> {
    let mut doc: DocumentMut = existing
        .parse()
        .map_err(|e: toml_edit::TomlError| DdManifestError::MergeSource(e.to_string()))?;

    merge_report(manifest, &mut doc)?;
    merge_repositories(manifest, &mut doc)?;

    Ok(doc.to_string())
}

/// Fold the `[report]` section in, leaving keys this run has no value for.
fn merge_report(manifest: &DdManifest, doc: &mut DocumentMut) -> Result<(), DdManifestError> {
    let report = doc
        .entry("report")
        .or_insert_with(|| Item::Table(Table::new()));
    let table = report
        .as_table_like_mut()
        .ok_or_else(|| DdManifestError::MergeSource("`report` is not a table".to_string()))?;

    table.insert("title", string(&manifest.report.title));
    set_or_keep(table, "analyst", manifest.report.analyst.as_deref());
    set_or_keep(table, "client", manifest.report.client.as_deref());
    set_or_keep(
        table,
        "ticketing",
        manifest
            .report
            .ticketing
            .as_ref()
            .map(|p| p.display().to_string())
            .as_deref(),
    );

    // See the module docs: this run's gaps replace the file's, because a stale
    // gap line is a false statement in the delivered report.
    if manifest.report.gaps.is_empty() {
        table.remove("gaps");
    } else {
        table.insert("gaps", array(&manifest.report.gaps));
    }
    Ok(())
}

/// Fold each `[[repositories]]` entry in, by `path` then by `name`.
fn merge_repositories(manifest: &DdManifest, doc: &mut DocumentMut) -> Result<(), DdManifestError> {
    let item = doc
        .entry("repositories")
        .or_insert_with(|| Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
    let entries = item.as_array_of_tables_mut().ok_or_else(|| {
        DdManifestError::MergeSource("`repositories` is not an array of tables".to_string())
    })?;

    for fresh in &manifest.repositories {
        // Bound in its own statement: `iter()` hands back a boxed iterator whose
        // borrow would otherwise outlive the `match` scrutinee.
        let at = entries.iter().position(|e| matches_entry(e, fresh));
        match at {
            Some(at) => {
                let existing = entries
                    .get_mut(at)
                    .expect("the index came from this array a statement ago");
                write_repository(existing, fresh);
            }
            None => {
                let mut table = Table::new();
                write_repository(&mut table, fresh);
                entries.push(table);
            }
        }
    }
    Ok(())
}

/// Whether `entry` is the file's record of the repository `fresh` describes.
///
/// `path` first, because that is the key trusty-audit's grounding pass matches
/// on (`grounding::priority::names_checkout`) — agreeing with it is what keeps
/// the ranking attached to the entry tga rewrites. `name` is the fallback for a
/// checkout that moved between runs. Purely textual: this module performs no
/// I/O, so it never canonicalizes.
fn matches_entry(entry: &Table, fresh: &DdRepositoryEntry) -> bool {
    let declared = entry.get("path").and_then(Item::as_str);
    if let Some(declared) = declared {
        return declared == fresh.path.display().to_string();
    }
    entry.get("name").and_then(Item::as_str) == Some(fresh.name.as_str())
}

/// Write only the three keys tga owns on one repository entry.
fn write_repository(entry: &mut Table, fresh: &DdRepositoryEntry) {
    entry.insert("name", string(&fresh.name));
    entry.insert("path", string(&fresh.path.display().to_string()));
    set_or_keep(
        entry,
        "authorship",
        fresh
            .authorship
            .as_ref()
            .map(|p| p.display().to_string())
            .as_deref(),
    );
}

/// Set `key` when this run has a value, and leave the file's alone when it does
/// not — the difference between "tga has nothing to say here" and "delete it".
fn set_or_keep(table: &mut dyn toml_edit::TableLike, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        table.insert(key, string(value));
    }
}

/// A TOML string item.
fn string(value: &str) -> Item {
    Item::Value(Value::from(value))
}

/// A TOML array-of-strings item.
fn array(values: &[String]) -> Item {
    Item::Value(Value::Array(
        values.iter().map(String::as_str).collect::<Array>(),
    ))
}

#[cfg(test)]
#[path = "dd_manifest_merge_tests.rs"]
mod dd_manifest_merge_tests;
