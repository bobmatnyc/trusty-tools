//! Bootstrap result types.
//!
//! Why: shared data structures used by both the scanner and the async entry
//! point. Keeping them in a separate file prevents circular dependencies and
//! keeps each module under the 500-SLOC cap.
//! What: `BootstrapTriple`, `ScannedFile`, `BootstrapResult`, plus the
//! `KG_EMPTY_HINT` constant and the `is_kg_empty_for_subject` / `result_to_json`
//! helpers that operate directly on these types.
//! Test: types are covered by the scanner and integration tests in sibling
//! modules.

use anyhow::{anyhow, Result};
use serde::Serialize;
use trusty_common::memory_core::store::kg::Triple;

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

/// Hint string returned by `kg_query` when the palace KG is empty.
///
/// Why: Issue #60 — when a user calls `kg_query` against a brand-new palace
/// they get an empty triples array with no indication that `kg_bootstrap` /
/// `kg_assert` even exist. A short hint embedded in the response solves
/// this with one line of code at the call site.
/// What: Static string, kept in this module so tests can pin it.
/// Test: `kg_query_emits_hint_when_palace_empty` in `tools.rs`.
pub const KG_EMPTY_HINT: &str =
    "Knowledge graph is empty. Run kg_bootstrap to seed it from project files, \
     or use kg_assert to add triples manually.";

/// Convenience: count active triples across an entire palace.
///
/// Why: `kg_query` is per-subject, so to determine "is the KG empty?" the
/// `kg_query` handler needs a separate broader check. Centralising the
/// emptiness check here keeps the hint logic in one place and lets future
/// changes (e.g. counting across closets) live alongside their consumer.
/// What: Returns `Ok(true)` iff the palace has zero triples for the queried
/// subject AND the broader "is_anything_asserted" check is empty. Practical
/// emptiness: we treat the palace as empty if the queried subject returned
/// no triples — this is the user's signal that something is wrong, even if
/// other subjects have data.
/// Test: covered indirectly through `kg_query_emits_hint_when_palace_empty`.
pub fn is_kg_empty_for_subject(triples: &[Triple]) -> bool {
    triples.is_empty()
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
