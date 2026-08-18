//! The tga → trusty-review authorship artifact, receiving half (#5453, #6004).
//!
//! Why: tga's `report::authorship` module (the derivation, per #5468's ruling
//! that contributor-profiling derivation lives in tga, never here) writes one
//! JSON artifact per repository; this module is the read side, mirroring
//! [`super::ticketing`]'s and [`super::metrics`]'s loader shape.
//! What: [`AuthorshipSummary`], [`load_authorship`] which parses it from a
//! path the manifest declares and refuses a schema major this build does not
//! read, plus rendering helpers the reporter uses for the Authorship &
//! Key-Person Risk section's deterministic tables.
//! Test: `super::authorship_tests`.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::error::ReportError;

/// The artifact schema major this build reads — pairs with tga's
/// `report::authorship::AUTHORSHIP_SCHEMA_VERSION`.
const SUPPORTED_SCHEMA_MAJOR: u32 = 0;

/// One repository's ownership/bus-factor/trajectory figures, as tga writes
/// them.
///
/// Why: the cross-process contract — no Cargo edge to tga (#5468) — so every
/// field defaults, matching [`super::ticketing::TicketingSummary`]'s shape.
/// What: see tga's `report::authorship::AuthorshipSummary` for the derivation;
/// this is its deserialization mirror.
/// Test: `super::authorship_tests::parses_the_artifact_tga_writes`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[non_exhaustive]
pub struct AuthorshipSummary {
    /// Artifact schema tag, e.g. `v0`; empty when the artifact declared none.
    #[serde(default)]
    pub schema_version: String,
    /// The repository these figures describe.
    #[serde(default)]
    pub repository: String,
    /// Distinct non-bot authors with at least one non-merge commit.
    #[serde(default)]
    pub distinct_authors: u64,
    /// Smallest number of top-touching authors whose combined file-touch
    /// share reaches 50%.
    #[serde(default)]
    pub bus_factor: u64,
    /// The single largest author's share of all file-touches, `0.0..=100.0`.
    #[serde(default)]
    pub top_author_share_pct: f64,
    /// Top-level path segments touched by exactly one non-bot author.
    #[serde(default)]
    pub single_author_subsystems: Vec<String>,
    /// One entry per active month in the trailing 12 months, oldest first.
    #[serde(default)]
    pub monthly_trajectory: Vec<MonthlyActivity>,
    /// Data-trap limitations the derivation did NOT correct for (issue
    /// #5453) — rendered verbatim in the section's caption.
    #[serde(default)]
    pub caveats: Vec<String>,
}

/// One month's active-author/commit-volume figures.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct MonthlyActivity {
    /// `YYYY-MM`.
    #[serde(default)]
    pub month: String,
    /// Distinct non-bot authors with a non-merge commit in this month.
    #[serde(default)]
    pub active_authors: u64,
    /// Non-merge, non-bot commits in this month.
    #[serde(default)]
    pub commits: u64,
}

impl AuthorshipSummary {
    /// A one-line, high-level trend description of [`Self::monthly_trajectory`].
    ///
    /// Why: the Key Facts block and the deterministic table both want a
    /// compact "12-month trajectory" statement rather than requiring a reader
    /// to eyeball a 12-row table.
    /// What: compares the average commits/month across the first half of the
    /// window against the second half and labels the trend increasing,
    /// stable (within 10%), or decreasing; `None` when there is fewer than
    /// one active month of data.
    /// Test: `super::authorship_tests::{trajectory_summary_labels_increasing,
    /// trajectory_summary_none_when_empty}`.
    pub fn trajectory_summary(&self) -> Option<String> {
        if self.monthly_trajectory.is_empty() {
            return None;
        }
        let n = self.monthly_trajectory.len();
        let mid = n.div_ceil(2);
        let avg = |slice: &[MonthlyActivity]| -> f64 {
            if slice.is_empty() {
                0.0
            } else {
                slice.iter().map(|m| m.commits as f64).sum::<f64>() / slice.len() as f64
            }
        };
        let first_half = avg(&self.monthly_trajectory[..mid]);
        let second_half = avg(&self.monthly_trajectory[mid..]);
        let trend = if first_half == 0.0 && second_half == 0.0 {
            "flat"
        } else if second_half > first_half * 1.1 {
            "increasing"
        } else if second_half < first_half * 0.9 {
            "decreasing"
        } else {
            "stable"
        };
        let avg_authors = self
            .monthly_trajectory
            .iter()
            .map(|m| m.active_authors as f64)
            .sum::<f64>()
            / n as f64;
        let avg_commits = self
            .monthly_trajectory
            .iter()
            .map(|m| m.commits as f64)
            .sum::<f64>()
            / n as f64;
        Some(format!(
            "{trend} over the trailing {n} month(s): avg {avg_commits:.1} commit(s)/mo across \
             avg {avg_authors:.1} active author(s)/mo"
        ))
    }
}

/// Load and parse an authorship artifact from disk.
///
/// Why: mirrors [`super::ticketing::load_ticketing`] — a declared file that
/// cannot be read is a typed, named failure the CALLER decides how to handle.
/// Unlike ticketing (engagement-wide, hard-fails the whole build), a failed
/// authorship load is fail-open at the model layer (#5453/#6004): one
/// repository's artifact failing must produce a named gap for that
/// repository, never abort a report that has data for every other one.
/// What: reads `path`, parses it against [`AuthorshipSummary`], then refuses
/// any [`super::schema::major`] other than [`SUPPORTED_SCHEMA_MAJOR`].
/// Test: `super::authorship_tests::{parses_the_artifact_tga_writes,
/// a_malformed_artifact_is_a_named_error,
/// an_artifact_from_an_unknown_schema_major_is_a_named_error}`.
///
/// # Errors
///
/// [`ReportError::Io`] when the file cannot be read,
/// [`ReportError::Authorship`] when it does not parse, and
/// [`ReportError::AuthorshipSchema`] when it declares an unreadable major.
pub fn load_authorship(path: &Path) -> std::result::Result<AuthorshipSummary, ReportError> {
    let text = std::fs::read_to_string(path).map_err(|source| ReportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let summary: AuthorshipSummary =
        serde_json::from_str(&text).map_err(|source| ReportError::Authorship {
            path: path.to_path_buf(),
            source,
        })?;

    if super::schema::major(&summary.schema_version) != Some(SUPPORTED_SCHEMA_MAJOR) {
        return Err(ReportError::AuthorshipSchema {
            path: path.to_path_buf(),
            found: summary.schema_version,
            supported: SUPPORTED_SCHEMA_MAJOR,
        });
    }
    Ok(summary)
}

#[cfg(test)]
#[path = "authorship_tests.rs"]
mod authorship_tests;
