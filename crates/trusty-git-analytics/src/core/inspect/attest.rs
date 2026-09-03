//! The data-handling attestation `tga inspect attest` prints (#5218).
//!
//! Why: the question a counterparty asks before granting repository access is
//! what the tool kept. The answer has to be checkable, so every part of it here
//! is either a live read of the operator's own database or a claim a test
//! re-checks on every run — never a sentence someone wrote once after reading
//! the migration files.
//! What: [`NO_CONTENT_CLAIM`] and [`NOT_A_NO_CODE_CLAIM`] are the exact wording
//! DOC-67 §10 quotes. [`attest`] pairs them with a scan for content-bearing
//! columns, a runtime read of every free-text column, and the pinned inventory
//! of [`DIFF_TEXT_CONSUMERS`].
//! Test: `core::inspect::tests`.

use rusqlite::Connection;
use serde::Serialize;

use crate::core::errors::{Result, TgaError};

use super::schema::{ObjectKind, SchemaSnapshot};
use super::text_columns::TextClass;

/// The claim tga's schema supports, in the wording a report may quote verbatim.
///
/// Why: #5218 — every looser paraphrase that has been tried ("contains no
/// code") is false, because a commit message can carry a pasted snippet. This
/// sentence is a claim about what the schema has columns FOR, which is exactly
/// what the scan below can verify.
/// Test: `core::inspect::tests::claim_never_says_no_code`.
pub const NO_CONTENT_CLAIM: &str =
    "tga's database stores no file content, diffs, patches, hunks, or blobs.";

/// The caveat that must travel with [`NO_CONTENT_CLAIM`].
///
/// Why: quoting the claim alone recreates the overreach it exists to prevent.
/// Test: `core::inspect::tests::claim_never_says_no_code`.
pub const NOT_A_NO_CODE_CLAIM: &str =
    "This is not a claim that the database contains no code. Free-text columns hold text a \
     human or an upstream system typed, so a pasted snippet in a commit message, a ticket \
     title, or an override note is stored verbatim. Those columns are named below and scanned \
     in this database, not assumed clean from the schema.";

/// Column-name substrings that would indicate a content-bearing column.
///
/// A hit is a finding, not proof: the reviewer sees the column name and decides.
const CONTENT_NAME_TOKENS: &[&str] = &[
    "blob",
    "diff",
    "patch",
    "hunk",
    "content",
    "payload",
    "snippet",
    "file_text",
    "source_code",
];

/// A caller of `collect::git::diff::diff_for_commit`, and what it does with the
/// diff text.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct DiffConsumer {
    /// Path to the calling file, relative to the crate root.
    pub source_path: &'static str,
    /// What that caller does with the returned diff.
    pub disposition: &'static str,
}

/// Every non-test caller of `diff_for_commit`, pinned.
///
/// Why: #5218 asked for an enforced check that this function has no non-test
/// caller, because a diff reaching a persistence path would break the claim
/// above silently. #5465 then wired the profile diff sampler onto it, so the
/// enforceable property is no longer "zero callers" — it is "these callers and
/// no others". `tests::diff_for_commit_callers_match_the_attestation` re-derives
/// the set from the source tree on every `cargo test -p tga` and fails on a
/// caller nobody added here.
/// Test: `core::inspect::tests::diff_for_commit_callers_match_the_attestation`.
pub const DIFF_TEXT_CONSUMERS: &[DiffConsumer] = &[DiffConsumer {
    source_path: "src/profile/diff_sampler/sampler.rs",
    disposition: "holds the diff in memory for the profile period-review prompt \
                  (`profile::batch_reviewer`) and never binds it to a SQL statement",
}];

/// The files that define and re-export `diff_for_commit`, which the caller
/// check must not count as callers.
pub const DIFF_API_SITES: &[&str] = &["src/collect/git/diff.rs", "src/collect/git/mod.rs"];

/// A column whose name or declared type suggests it carries file content.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ContentColumnFinding {
    /// Owning table.
    pub table: String,
    /// Column name.
    pub column: String,
    /// Declared type.
    pub declared_type: String,
    /// Why the scan flagged it.
    pub reason: String,
}

/// What one scanned column actually holds in this database.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct ColumnContentScan {
    /// Owning table.
    pub table: String,
    /// Column name.
    pub column: String,
    /// Why the column is scanned rather than trusted.
    pub class: TextClass,
    /// Rows in the table.
    pub rows: i64,
    /// Rows whose value is not NULL and not empty.
    pub populated: i64,
    /// Longest value in bytes, or 0 when the column is empty everywhere.
    pub max_len: i64,
    /// Rows carrying a unified-diff marker (`diff --git`, a hunk header, or a
    /// `---`/`+++` file header). Non-zero is a finding a reviewer must see.
    pub diff_shaped_rows: i64,
}

/// Whether the scan found anything a reviewer must look at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// No content-bearing column and no diff-shaped value in any scanned column.
    Consistent,
    /// At least one finding — the claim above does not hold unreviewed.
    Findings,
}

/// The full attestation for one database.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct Attestation {
    /// [`NO_CONTENT_CLAIM`].
    pub claim: &'static str,
    /// [`NOT_A_NO_CODE_CLAIM`].
    pub caveat: &'static str,
    /// Highest applied migration version, or `None` when absent.
    pub schema_version: Option<i64>,
    /// Tables inspected (views excluded — they project the same columns).
    pub tables_scanned: usize,
    /// Columns whose name or type suggests file content. Empty is the pass.
    pub content_columns: Vec<ContentColumnFinding>,
    /// Live reading of every free-text, embedded-payload, and unclassified
    /// column.
    pub scanned_columns: Vec<ColumnContentScan>,
    /// The pinned non-test callers of `diff_for_commit`.
    pub diff_text_consumers: &'static [DiffConsumer],
    /// Overall result.
    pub verdict: Verdict,
}

/// Build the attestation for an open, read-only connection.
///
/// Why: see the module docs — the value of this over reading `src/core/db/sql/`
/// is that it reports the operator's rows, not the binary's intentions.
/// What: takes the already-read `snapshot`, flags any column whose name or
/// declared type is content-shaped, then reads counts, maximum length, and a
/// unified-diff marker probe out of every column [`TextClass::is_scanned`]
/// selects.
/// Test: `core::inspect::tests::attest_on_a_fresh_database_is_consistent`,
/// `core::inspect::tests::attest_flags_a_diff_pasted_into_a_commit_message`.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if a scan query fails.
pub fn attest(conn: &Connection, snapshot: &SchemaSnapshot) -> Result<Attestation> {
    let mut content_columns = Vec::new();
    let mut scanned_columns = Vec::new();
    let mut tables_scanned = 0_usize;

    for table in snapshot
        .objects
        .iter()
        .filter(|o| o.kind == ObjectKind::Table)
    {
        tables_scanned += 1;
        for column in &table.columns {
            if let Some(reason) = content_reason(&column.name, &column.declared_type) {
                content_columns.push(ContentColumnFinding {
                    table: table.name.clone(),
                    column: column.name.clone(),
                    declared_type: column.declared_type.clone(),
                    reason,
                });
            }
            let Some(class) = column.text_class else {
                continue;
            };
            if !class.is_scanned() {
                continue;
            }
            scanned_columns.push(scan_column(
                conn,
                &table.name,
                &column.name,
                class,
                table.row_count.unwrap_or(0),
            )?);
        }
    }

    let verdict =
        if content_columns.is_empty() && scanned_columns.iter().all(|s| s.diff_shaped_rows == 0) {
            Verdict::Consistent
        } else {
            Verdict::Findings
        };

    Ok(Attestation {
        claim: NO_CONTENT_CLAIM,
        caveat: NOT_A_NO_CODE_CLAIM,
        schema_version: snapshot.schema_version,
        tables_scanned,
        content_columns,
        scanned_columns,
        diff_text_consumers: DIFF_TEXT_CONSUMERS,
        verdict,
    })
}

/// Why a column looks content-bearing, or `None` when it does not.
fn content_reason(name: &str, declared_type: &str) -> Option<String> {
    if declared_type.eq_ignore_ascii_case("BLOB") {
        return Some("declared BLOB".to_string());
    }
    let lower = name.to_ascii_lowercase();
    CONTENT_NAME_TOKENS
        .iter()
        .find(|token| lower.contains(*token))
        .map(|token| format!("column name contains \"{token}\""))
}

/// Markers that identify a unified diff pasted into a text column.
///
/// Each is anchored so an ordinary sentence cannot match: a hunk header and the
/// two file headers must start a line, and `diff --git` is distinctive on its
/// own. `char(10)` is the newline SQLite has no escape for.
const DIFF_MARKER_PREDICATES: &[&str] = &[
    "LIKE '%diff --git %'",
    "LIKE '%' || char(10) || '@@ %'",
    "LIKE '@@ %'",
    "LIKE '%' || char(10) || '--- a/%'",
    "LIKE '%' || char(10) || '+++ b/%'",
];

/// Read one column's live counts and diff-marker hits.
fn scan_column(
    conn: &Connection,
    table: &str,
    column: &str,
    class: TextClass,
    rows: i64,
) -> Result<ColumnContentScan> {
    // Both identifiers came from `sqlite_master` / `PRAGMA table_info` on this
    // same connection, never from caller input; quoting keeps an exotic
    // identifier from changing the statement's shape all the same.
    let t = table.replace('"', "\"\"");
    let c = column.replace('"', "\"\"");

    let (populated, max_len): (i64, i64) = conn
        .query_row(
            &format!(
                "SELECT COUNT(\"{c}\"), COALESCE(MAX(LENGTH(\"{c}\")), 0) \
                 FROM \"{t}\" WHERE \"{c}\" IS NOT NULL AND \"{c}\" <> ''"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(TgaError::from)?;

    let predicate = DIFF_MARKER_PREDICATES
        .iter()
        .map(|p| format!("\"{c}\" {p}"))
        .collect::<Vec<_>>()
        .join(" OR ");
    let diff_shaped_rows: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM \"{t}\" WHERE {predicate}"),
            [],
            |row| row.get(0),
        )
        .map_err(TgaError::from)?;

    Ok(ColumnContentScan {
        table: table.to_string(),
        column: column.to_string(),
        class,
        rows,
        populated,
        max_len,
        diff_shaped_rows,
    })
}
