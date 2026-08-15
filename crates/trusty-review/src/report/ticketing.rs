//! The ticketing artifact tga produces, and the line the report states (#5405).
//!
//! Why: `tga audit` synced board data into `work_items` and joined it to the
//! commits it walked, and the due-diligence report read none of it — a run
//! against a fully configured JIRA/Linear/ADO board rendered exactly like a run
//! against a repository with no tracker at all. This module is the receiving
//! half of that seam.
//!
//! What: [`TicketingSummary`], the v0 schema tga writes; [`load_ticketing`],
//! which parses it from a path the manifest declared; and
//! [`TicketingSummary::coverage_line`], the one sentence the report renders.
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

/// One audit run's commit ↔ board-item correlation figures.
///
/// Why: this is the cross-process contract with tga's
/// `report::ticketing::TicketingSummary`, which has no Cargo edge to this crate
/// — the JSON file is the entire seam, exactly as `manifest.toml` is for the
/// manifest. Every field defaults so an artifact written by an older or newer
/// tga still parses rather than failing the whole report.
/// What: the commit and work-item totals, plus the boards the links came from.
/// Test: `super::ticketing_tests::parses_the_artifact_tga_writes`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TicketingSummary {
    /// Artifact schema tag (informational; `v0` at time of writing).
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

/// Load and parse a ticketing artifact from disk.
///
/// Why: mirrors [`super::metrics::load_metrics`] — a declared file that cannot
/// be read is a typed, named failure rather than a silently absent section.
/// What: reads `path` and parses it against [`TicketingSummary`].
/// Test: `super::ticketing_tests::{parses_the_artifact_tga_writes,
/// a_malformed_artifact_is_a_named_error}`.
///
/// # Errors
///
/// [`ReportError::Io`] when the file cannot be read and
/// [`ReportError::Ticketing`] when it does not parse.
pub fn load_ticketing(path: &Path) -> std::result::Result<TicketingSummary, ReportError> {
    let text = std::fs::read_to_string(path).map_err(|source| ReportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| ReportError::Ticketing {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
#[path = "ticketing_tests.rs"]
mod ticketing_tests;
