//! The ticketing artifact tga produces, and the line the report states (#5405).
//!
//! Why: `tga audit` synced board data into `work_items` and joined it to the
//! commits it walked, and the due-diligence report read none of it — a run
//! against a fully configured JIRA/Linear/ADO board rendered exactly like a run
//! against a repository with no tracker at all. This module is the receiving
//! half of that seam.
//!
//! What: [`TicketingSummary`], the v0 schema tga writes; [`load_ticketing`],
//! which parses it from a path the manifest declared and refuses a schema major
//! this build does not read; and [`TicketingSummary::coverage_line`], the one
//! sentence the report renders.
//!
//! ## What this deliberately does not do
//!
//! It states counts and names the boards they came from. It computes no linkage
//! score, grade, or quality band. A calibrated signal needs the board-selection
//! axis that does not exist yet, and a number nothing calibrated is worse than
//! no number in a document an acquirer prices a deal from.
//!
//! ## Scope
//!
//! The figures are engagement-wide, not per repository: tga keeps one database
//! per engagement and the correlation pass joins across all of it. That is why
//! the manifest key rides on `[report]` rather than on a `[[repositories]]`
//! entry — and why it is not `RepositoryEntry.metrics`, whose "declared metrics
//! always win" precedence would block the live `--analyze` fetch.
//!
//! Test: `super::ticketing_tests`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::ReportError;

/// The artifact schema major this build reads (#5405).
///
/// Why: the two binaries ship independently, so the loader must be able to say
/// "not mine" about a file it can nonetheless deserialize. Pairs with tga's
/// `report::ticketing::TICKETING_SCHEMA_VERSION`.
/// What: `0`, matching the `v0` tag tga writes.
const SUPPORTED_SCHEMA_MAJOR: u32 = 0;

/// One audit run's commit ↔ board-item correlation figures.
///
/// Why: this is the cross-process contract with tga's
/// `report::ticketing::TicketingSummary`, which has no Cargo edge to this crate
/// — the JSON file is the entire seam, exactly as `manifest.toml` is for the
/// manifest. Every field defaults so an added key within a major this build
/// reads does not fail the whole report; [`load_ticketing`] enforces the major
/// itself, because defaulting is only safe once the shape is known.
/// What: the commit and work-item totals, plus the boards the links came from.
/// Test: `super::ticketing_tests::parses_the_artifact_tga_writes`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct TicketingSummary {
    /// Artifact schema tag, e.g. `v0`; empty when the artifact declared none.
    #[serde(default)]
    pub schema_version: String,
    /// Commits the sweep walked.
    #[serde(default)]
    pub commits: u64,
    /// Commits carrying at least one board-item link.
    #[serde(default)]
    pub commits_linked: u64,
    /// Board items the sweep synced.
    #[serde(default)]
    pub work_items: u64,
    /// Board items referenced by at least one commit.
    #[serde(default)]
    pub work_items_linked: u64,
    /// Boards contributing at least one linked item, e.g. `jira`, `linear`.
    #[serde(default)]
    pub sources: Vec<String>,
}

impl TicketingSummary {
    /// The sentence the report states.
    ///
    /// Why: the section must read as a statement of fact whether the figures are
    /// large, small, or zero. A run that correlated nothing is a real finding
    /// about a codebase — it says commits do not cite tracked work — so it gets
    /// a stated sentence rather than an omitted section.
    /// What: counts and, when any board contributed, the board names. Never a
    /// ratio, score, or grade; see the module doc.
    /// Test: `super::ticketing_tests::{coverage_line_states_counts_and_sources,
    /// coverage_line_states_a_zero_run}`.
    pub fn coverage_line(&self) -> String {
        if self.commits_linked == 0 {
            return format!(
                "No commit referenced a tracked board item. {} commit(s) and {} synced board \
                 item(s) were examined.",
                self.commits, self.work_items
            );
        }
        let sources = if self.sources.is_empty() {
            String::new()
        } else {
            format!(" across {}", self.sources.join(", "))
        };
        format!(
            "{} of {} commit(s) reference {} of {} synced board item(s){}.",
            self.commits_linked, self.commits, self.work_items_linked, self.work_items, sources
        )
    }
}

/// The major component of a schema tag, if it has one.
///
/// Why: the tag is a document the producer wrote, not a number, so the loader
/// reads only what it needs to decide compatibility — a `v0.3` written by a
/// later tga must resolve to the same major as `v0`.
/// What: strips a leading `v`, takes everything up to the first separator, and
/// parses it. `None` for an empty, absent, or non-numeric tag.
/// Test: `super::ticketing_tests::{an_artifact_with_a_newer_minor_of_a_known_major_still_parses,
/// an_artifact_with_no_schema_version_is_a_named_error}`.
fn schema_major(tag: &str) -> Option<u32> {
    tag.strip_prefix('v')
        .unwrap_or(tag)
        .split(['.', '-', '+'])
        .next()?
        .parse()
        .ok()
}

/// Load and parse a ticketing artifact from disk.
///
/// Why: mirrors [`super::metrics::load_metrics`] — a declared file that cannot
/// be read is a typed, named failure rather than a silently absent section.
/// #5405 extends that to a file that reads but is not this schema: because
/// every field defaults, a renamed count in a future tga would otherwise render
/// as a stated zero. Version skew between the two binaries is the normal case,
/// not an edge case, so the check is on the load path rather than left to the
/// caller.
/// What: reads `path`, parses it against [`TicketingSummary`], then refuses any
/// [`schema_major`] other than [`SUPPORTED_SCHEMA_MAJOR`]. A newer MINOR of a
/// known major loads — that is the added-field case `#[serde(default)]` exists
/// for.
/// Test: `super::ticketing_tests::{parses_the_artifact_tga_writes,
/// a_malformed_artifact_is_a_named_error,
/// an_artifact_from_an_unknown_schema_major_is_a_named_error}`.
///
/// # Errors
///
/// [`ReportError::Io`] when the file cannot be read,
/// [`ReportError::Ticketing`] when it does not parse, and
/// [`ReportError::TicketingSchema`] when it declares an unreadable major.
pub fn load_ticketing(path: &Path) -> std::result::Result<TicketingSummary, ReportError> {
    let text = std::fs::read_to_string(path).map_err(|source| ReportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let summary: TicketingSummary =
        serde_json::from_str(&text).map_err(|source| ReportError::Ticketing {
            path: path.to_path_buf(),
            source,
        })?;

    if schema_major(&summary.schema_version) != Some(SUPPORTED_SCHEMA_MAJOR) {
        return Err(ReportError::TicketingSchema {
            path: path.to_path_buf(),
            found: summary.schema_version,
            supported: SUPPORTED_SCHEMA_MAJOR,
        });
    }
    Ok(summary)
}

#[cfg(test)]
#[path = "ticketing_tests.rs"]
mod ticketing_tests;
