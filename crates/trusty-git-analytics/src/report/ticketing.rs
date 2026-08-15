//! The tga → trusty-review ticketing artifact (#5405).
//!
//! Why: the sweep synced board data into `work_items` and joined it to
//! `commits` (the [`crate::collect::correlate`] pass), and the due-diligence
//! report read none of it — `src/report/` referenced no board table at all, so
//! a run against a fully configured JIRA/Linear/ADO board produced a report
//! indistinguishable from one against a repository with no tracker. This module
//! is the read side: it reduces the correlation tables to the handful of counts
//! the report states, and serializes them beside the manifest.
//!
//! What: [`TicketingSummary`], [`build_ticketing_summary`] which reads it from
//! an open database, and [`TicketingSummary::to_json`]. Like
//! [`crate::report::dd_manifest`], the builder is pure apart from the database
//! read — the caller writes the file, so the field mapping is provable from
//! unit tests rather than only from a live audit.
//!
//! The file is a sidecar rather than a section of `manifest.toml` because
//! trusty-review resolves it the way it resolves a metrics JSON: a path
//! declared in the manifest, loaded relative to the manifest's directory. It
//! deliberately does NOT travel through `RepositoryEntry.metrics`, whose
//! "declared metrics always win" precedence would block the live `--analyze`
//! fetch for any repository carrying one.
//!
//! Scope: the counts are database-wide, not per repository. tga keeps one
//! SQLite database per engagement and the correlation pass joins across all of
//! it, so a per-repository split would have to key on `commits.repository`
//! matching a manifest entry's name — a match that silently yields zeros when
//! it fails. The figures are stated at the scope they are computed at.
//!
//! Test: `super::ticketing_tests`.

use rusqlite::Connection;
use serde::Serialize;

use crate::core::db::correlation::{correlation_counts, linked_work_item_sources};
use crate::core::errors::Result;

/// Schema tag written into the artifact.
///
/// Why: trusty-review reads this file across an independent release boundary —
/// the two binaries are installed separately — so the document says what shape
/// it is rather than leaving the reader to infer it from which keys parsed.
/// What: `"v0"`, matching the metrics artifact's own versioning convention.
pub const TICKETING_SCHEMA_VERSION: &str = "v0";

/// The board-correlation figures one audit run produced.
///
/// Why: this is the whole tga→trusty-review ticketing seam, and it is
/// deliberately four counts and a source list. A linkage-quality metric or a
/// grade is a product decision the board-selection axis has not landed yet, and
/// inventing one here would put a number in an acquirer's report that nothing
/// calibrated.
/// What: the commit and work-item totals from [`correlation_counts`], plus the
/// boards those links came from. Every field is a count the database can
/// produce; none is derived, weighted, or scored.
/// Test: `super::ticketing_tests::summary_counts_match_the_database`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct TicketingSummary {
    /// Artifact schema tag; always [`TICKETING_SCHEMA_VERSION`].
    pub schema_version: String,
    /// Rows in `commits`.
    pub commits: u64,
    /// Commits carrying at least one board-item link.
    pub commits_linked: u64,
    /// Rows in `work_items` — the board items the sweep synced.
    pub work_items: u64,
    /// Work items referenced by at least one commit.
    pub work_items_linked: u64,
    /// Boards contributing at least one linked item, sorted (e.g. `jira`).
    pub sources: Vec<String>,
}

impl TicketingSummary {
    /// Serialize to the JSON text trusty-review's loader reads.
    ///
    /// Why: keeping serialization here means the artifact's shape is testable
    /// without touching disk, exactly as [`crate::report::dd_manifest`] does.
    /// What: `serde_json::to_string_pretty` over the declared field order.
    /// Test: `super::ticketing_tests::round_trips_through_the_review_schema`.
    ///
    /// # Errors
    ///
    /// [`crate::core::errors::TgaError`] when serialization fails.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// Whether the run correlated nothing at all.
    ///
    /// Why: zero links is a real, reportable outcome — a corpus whose commits
    /// cite no tracked ticket, or a board that was never configured — and it
    /// must be distinguishable from "this artifact was never produced". The
    /// report states the zero rather than dropping the section, so the caller
    /// needs to ask.
    /// What: true when no commit carries a link.
    /// Test: `super::ticketing_tests::an_empty_database_still_produces_a_summary`.
    pub fn is_empty(&self) -> bool {
        self.commits_linked == 0 && self.work_items_linked == 0
    }
}

/// Read one run's ticketing figures out of the database.
///
/// Why: the closure condition of #5405 — the report must read a board table.
/// This is the only function that does, and it runs after the sweep's
/// correlation stage so `commit_work_items` is populated.
/// What: [`correlation_counts`] for the totals and [`linked_work_item_sources`]
/// for the board list. Read-only; no clock, no environment, no network, so two
/// calls against an unchanged database return equal values.
/// Test: `super::ticketing_tests::summary_counts_match_the_database`.
///
/// # Errors
///
/// Propagates [`crate::core::errors::TgaError::DbError`] from either query.
pub fn build_ticketing_summary(conn: &Connection) -> Result<TicketingSummary> {
    let counts = correlation_counts(conn)?;
    Ok(TicketingSummary {
        schema_version: TICKETING_SCHEMA_VERSION.to_string(),
        commits: counts.commits,
        commits_linked: counts.linked,
        work_items: counts.work_items,
        work_items_linked: counts.work_items_linked,
        sources: linked_work_item_sources(conn)?,
    })
}

#[cfg(test)]
#[path = "ticketing_tests.rs"]
mod ticketing_tests;
