//! Bootstrap result types.
//!
//! Why: shared data structures used by both the scanner and the async entry
//! point. Keeping them in a separate file prevents circular dependencies and
//! keeps each module under the 500-SLOC cap.
//! What: `BootstrapTriple`, `ScannedFile`, `BootstrapResult`, plus the
//! `KG_EMPTY_HINT` / `KG_SUBJECT_NOT_FOUND_HINT` constants, the `KgMiss`
//! classifier behind `kg_query`'s `graph_state` field, and `result_to_json`.
//! Test: types are covered by the scanner and integration tests in sibling
//! modules.

use anyhow::{anyhow, Result};
use serde::Serialize;

/// A single bootstrap discovery before it becomes a Triple.
///
/// Why: Keeping the scanner output as plain tuples (rather than full
/// `Triple`s) lets the unit tests verify the extraction logic without
/// constructing timestamps or worrying about confidence values. The async
/// caller converts these into `Triple`s with the live `chrono::Utc::now()`
/// timestamp right before assertion.
/// What: Carries subject, predicate, object, and the provenance tag that
/// identifies which scanner produced the fact.
/// Test: Each scanner test asserts the expected `BootstrapTriple`s land in
/// the result list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapTriple {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub provenance: String,
}

/// Per-file scan summary returned to the MCP caller.
///
/// Why: Operators want to know *which* files contributed to the bootstrap
/// (and which were absent) without re-running the tool with verbose logging.
/// What: Filename + count of triples it produced; emitted as JSON in the
/// MCP response.
/// Test: `bootstrap_palace_returns_per_file_counts`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ScannedFile {
    pub file: String,
    pub triples: usize,
}

/// Aggregate result of a bootstrap run.
///
/// Why: The MCP `kg_bootstrap` tool returns this verbatim so the model (or a
/// human operator) can see exactly what was asserted and which files were
/// scanned.
/// What: Total triple count + per-file summaries + the resolved project
/// subject. `Serialize` so it round-trips into the MCP JSON envelope.
/// Test: `bootstrap_palace_seeds_temporal_metadata_when_no_files`.
#[derive(Debug, Clone, Serialize)]
pub struct BootstrapResult {
    pub palace: String,
    pub project_subject: String,
    pub triples_asserted: usize,
    pub scanned_files: Vec<ScannedFile>,
}

/// Hint string returned by `kg_query` when the palace KG holds no triples.
///
/// Why: Issue #60 — when a user calls `kg_query` against a brand-new palace
/// they get an empty triples array with no indication that `kg_bootstrap` /
/// `kg_assert` even exist. A short hint embedded in the response solves
/// this with one line of code at the call site.
/// What: Static string, kept in this module so tests can pin it.
/// Test: `kg_query_reports_graph_empty_when_graph_has_no_triples`.
pub const KG_EMPTY_HINT: &str =
    "Knowledge graph is empty. Run kg_bootstrap to seed it from project files, \
     or use kg_assert to add triples manually.";

/// Hint string returned by `kg_query` when the graph has triples but not for
/// the queried subject.
///
/// Why (#4775): the caller guessed a subject the graph does not hold. Telling
/// them to seed the graph is wrong — it is already seeded — and it sends them
/// to `kg_assert` when what they need is the list of subjects that do exist.
/// What: names `kg_list_subjects` (shipped in #4776) so the recovery step is a
/// tool call, not a second guess.
/// Test: `kg_query_reports_subject_not_found_when_graph_has_other_subjects`.
pub const KG_SUBJECT_NOT_FOUND_HINT: &str =
    "No active triples for this subject. The knowledge graph is not empty — \
     call kg_list_subjects to see which subjects it holds.";

/// Which of the two distinct "no triples came back" outcomes a `kg_query` hit.
///
/// Why (#4775): both outcomes returned the same "Knowledge graph is empty"
/// hint, and for the common case — a graph with facts, queried for a subject
/// it does not hold — that hint states something the handler can prove false.
/// A caller that believes it wastes a `kg_bootstrap` on an already-seeded
/// graph instead of listing the subjects that are there.
/// What: the discriminator behind `kg_query`'s `graph_state` response field.
/// [`Self::classify`] decides which one applies; [`Self::wire_value`] and
/// [`Self::hint`] are the two strings that reach the caller. Marked
/// `#[non_exhaustive]` so a future third outcome is not a breaking change.
/// Test: `kg_miss_classify_distinguishes_empty_graph_from_missing_subject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum KgMiss {
    /// The graph holds active triples, just none for the queried subject.
    SubjectNotFound,
    /// The graph holds no active triples at all.
    GraphEmpty,
}

impl KgMiss {
    /// Classify a `kg_query` result; `None` means the subject matched.
    ///
    /// Why (#4775): emptiness is a whole-graph property, so it cannot be read
    /// off the per-subject result alone — that conflation is the defect. Both
    /// counts are required, and taking them as parameters keeps the decision
    /// pure and directly testable without a live palace.
    /// What: `None` when `subject_triples > 0`. Otherwise `GraphEmpty` iff the
    /// whole graph reports zero active triples, else `SubjectNotFound`.
    /// Test: `kg_miss_classify_distinguishes_empty_graph_from_missing_subject`.
    pub fn classify(subject_triples: usize, total_active_triples: usize) -> Option<Self> {
        if subject_triples > 0 {
            return None;
        }
        if total_active_triples == 0 {
            Some(Self::GraphEmpty)
        } else {
            Some(Self::SubjectNotFound)
        }
    }

    /// The string emitted as `kg_query`'s `graph_state` field.
    pub fn wire_value(self) -> &'static str {
        match self {
            Self::SubjectNotFound => "subject_not_found",
            Self::GraphEmpty => "graph_empty",
        }
    }

    /// The recovery hint that matches this outcome.
    pub fn hint(self) -> &'static str {
        match self {
            Self::SubjectNotFound => KG_SUBJECT_NOT_FOUND_HINT,
            Self::GraphEmpty => KG_EMPTY_HINT,
        }
    }
}

/// Helper: bubble up the bootstrap result as the MCP JSON envelope expects.
///
/// Why: `tools.rs` keeps the dispatcher branches small; converting the
/// `BootstrapResult` into a `serde_json::Value` here keeps the JSON shape
/// owned by this module and stable for tests.
/// What: Serialises the result via serde and wraps any failure in
/// `anyhow::Error` with context.
/// Test: round-tripped via the MCP dispatcher test.
pub fn result_to_json(r: &BootstrapResult) -> Result<serde_json::Value> {
    serde_json::to_value(r).map_err(|e| anyhow!("serialize BootstrapResult: {e}"))
}
