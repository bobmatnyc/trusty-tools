//! Terminal rendering for the schema snapshot and the attestation.
//!
//! Why: #5218's first closure condition is that a reviewer can SEE every table
//! and column, with the free-text ones called out. A JSON dump satisfies a
//! downstream tool; a person reading it in a terminal needs the layout.
//! What: [`schema_report`] and [`attestation_report`] each return one `String`
//! the command prints. Neither writes to stdout itself, so both are directly
//! assertable.
//! Test: `core::inspect::tests::schema_report_marks_free_text_columns`,
//! `core::inspect::tests::attestation_report_states_the_claim_and_the_caveat`.

use std::fmt::Write as _;

use super::attest::{Attestation, Verdict};
use super::schema::{ObjectKind, SchemaSnapshot};
use super::text_columns::TextClass;

/// Short marker printed beside a `TEXT` column's name.
fn class_marker(class: Option<TextClass>) -> &'static str {
    match class {
        Some(TextClass::FreeText) => "  ← FREE TEXT",
        Some(TextClass::EmbeddedPayload) => "  ← EMBEDDED PAYLOAD",
        Some(TextClass::Unclassified) => "  ← UNCLASSIFIED TEXT",
        Some(TextClass::Constrained) | None => "",
    }
}

/// Render the whole live schema: every table, every column, every row count.
///
/// Why: this is the artefact a reviewer reads; the free-text markers are what
/// stop it being a wall of columns with the risky ones buried.
/// What: tables first with their counts, then views, then a trailing summary of
/// the free-text and embedded-payload columns so the reviewer does not have to
/// scan back through the table listing to collect them.
/// Test: `core::inspect::tests::schema_report_marks_free_text_columns`.
pub fn schema_report(snapshot: &SchemaSnapshot) -> String {
    let mut out = String::new();
    let version = snapshot.schema_version.map_or_else(
        || "unknown (no schema_migrations table)".to_string(),
        |v| v.to_string(),
    );
    let _ = writeln!(out, "tga database schema — migration version {version}");
    let _ = writeln!(out);

    for object in &snapshot.objects {
        match object.kind {
            ObjectKind::Table => {
                let rows = object.row_count.unwrap_or(0);
                let _ = writeln!(
                    out,
                    "TABLE {} ({} column(s), {rows} row(s))",
                    object.name,
                    object.columns.len()
                );
            }
            ObjectKind::View => {
                let _ = writeln!(
                    out,
                    "VIEW  {} ({} column(s))",
                    object.name,
                    object.columns.len()
                );
            }
        }
        for column in &object.columns {
            let declared = if column.declared_type.is_empty() {
                "(untyped)"
            } else {
                &column.declared_type
            };
            let pk = if column.pk_position > 0 { " PK" } else { "" };
            let not_null = if column.not_null { " NOT NULL" } else { "" };
            let _ = writeln!(
                out,
                "    {:<28} {declared}{pk}{not_null}{}",
                column.name,
                class_marker(column.text_class)
            );
        }
        let _ = writeln!(out);
    }

    let flagged: Vec<_> = snapshot
        .text_columns()
        .into_iter()
        .filter(|(_, c)| c.text_class.is_some_and(TextClass::is_scanned))
        .collect();
    let _ = writeln!(out, "Free-text and payload columns ({}):", flagged.len());
    for (table, column) in flagged {
        let _ = writeln!(
            out,
            "    {}.{}{}",
            table.name,
            column.name,
            class_marker(column.text_class)
        );
    }
    out
}

/// Render the attestation, claim and caveat first.
///
/// Why: the claim is the sentence a report quotes, so it leads; the evidence
/// under it is what makes quoting it defensible.
/// What: claim, caveat, the content-column scan result, the per-column live
/// reading, the pinned `diff_for_commit` consumers, then the verdict.
/// Test: `core::inspect::tests::attestation_report_states_the_claim_and_the_caveat`.
pub fn attestation_report(attestation: &Attestation) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", attestation.claim);
    let _ = writeln!(out);
    let _ = writeln!(out, "{}", attestation.caveat);
    let _ = writeln!(out);

    let version = attestation
        .schema_version
        .map_or_else(|| "unknown".to_string(), |v| v.to_string());
    let _ = writeln!(
        out,
        "Scanned {} table(s) at migration version {version}.",
        attestation.tables_scanned
    );
    let _ = writeln!(out);

    let _ = writeln!(out, "Content-bearing columns");
    if attestation.content_columns.is_empty() {
        let _ = writeln!(
            out,
            "    none — no BLOB column, and no column named for a diff, patch, hunk, or file body."
        );
    } else {
        for finding in &attestation.content_columns {
            let _ = writeln!(
                out,
                "    {}.{} ({}) — {}",
                finding.table, finding.column, finding.declared_type, finding.reason
            );
        }
    }
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "Free-text columns, as they stand in this database\n    \
         {:<44} {:>10} {:>10} {:>9}  CLASS",
        "COLUMN", "POPULATED", "MAX BYTES", "DIFFS"
    );
    for scan in &attestation.scanned_columns {
        let class = match scan.class {
            TextClass::FreeText => "free text",
            TextClass::EmbeddedPayload => "embedded payload",
            TextClass::Unclassified => "UNCLASSIFIED",
            TextClass::Constrained => "constrained",
        };
        let _ = writeln!(
            out,
            "    {:<44} {:>10} {:>10} {:>9}  {class}",
            format!("{}.{}", scan.table, scan.column),
            scan.populated,
            scan.max_len,
            scan.diff_shaped_rows
        );
    }
    let _ = writeln!(
        out,
        "    DIFFS counts rows carrying a unified-diff marker. Any non-zero value \
         contradicts the claim above."
    );
    let _ = writeln!(out);

    let _ = writeln!(
        out,
        "Callers of collect::git::diff::diff_for_commit ({})",
        attestation.diff_text_consumers.len()
    );
    for consumer in attestation.diff_text_consumers {
        let _ = writeln!(
            out,
            "    {} — {}",
            consumer.source_path, consumer.disposition
        );
    }
    let _ = writeln!(
        out,
        "    This list is pinned in source and re-derived from the source tree by \
         `cargo test -p tga`, so a new caller fails the build rather than passing unnoticed."
    );
    let _ = writeln!(out);

    let _ = match attestation.verdict {
        Verdict::Consistent => writeln!(
            out,
            "VERDICT: consistent — the claim holds for this database."
        ),
        Verdict::Findings => writeln!(
            out,
            "VERDICT: findings — review the rows above before quoting the claim."
        ),
    };
    out
}
