//! Subprocess-based `AnalyzeClient` — invokes `trusty-analyze` on demand.
//!
//! Why: replaces the HTTP-daemon model with an on-demand executable runtime so
//! trusty-review no longer requires a long-running `trusty-analyze serve` process.
//! The binary is invoked as a subprocess, the diff is written to its stdin, and the
//! JSON `ReviewReport` written to stdout is parsed into the `ComplexityHotspot` and
//! `Smell` shapes the review pipeline consumes.  (closes #632)
//!
//! Architecture:
//!   trusty-review  →  spawn(`trusty-analyze review --index-id <id> -`)
//!                         writes diff → stdin
//!                         reads JSON  ← stdout
//!                     parse JSON → ComplexityHotspot + Smell
//!
//! What: `SubprocessAnalyzeClient` implements `AnalyzeClient`.  `health()` probes
//! trusty-search directly (same as the HTTP path) and verifies the binary is
//! resolvable on PATH (or the override path).  `has_analysis` verifies both.
//! `complexity_hotspots` and `smells` invoke the binary with a no-op diff and
//! return empty vecs — they are lightweight compared to `has_analysis`.  Real data
//! is produced by the review runner calling `analyze_diff` directly on the
//! subprocess; the pipeline only calls `complexity_hotspots` / `smells` for
//! supplementary hotspot annotations, which the subprocess model returns as empty
//! (see code comment).
//!
//! Binary discovery: `TRUSTY_ANALYZE_BIN` env var overrides the default
//! `trusty-analyze` (searched on PATH).
//!
//! Test: `subprocess_client_health_check_fails_gracefully`,
//! `map_report_to_hotspots_and_smells`, `subprocess_client_binary_not_found`.

use serde::Deserialize;

use crate::integrations::analyze_client::{ComplexityHotspot, Smell};

pub mod client;

#[cfg(test)]
mod tests;

pub use client::SubprocessAnalyzeClient;

/// Environment variable that overrides the `trusty-analyze` binary path.
///
/// Why: allows operators and CI environments to pin the exact binary used
/// without modifying PATH.
/// What: when set to a non-empty string, `SubprocessAnalyzeClient` uses this
/// path instead of looking up `trusty-analyze` on PATH.
/// Test: `subprocess_client_respects_bin_env`.
pub(super) const ENV_ANALYZE_BIN: &str = "TRUSTY_ANALYZE_BIN";

/// Default binary name searched on PATH.
pub(super) const DEFAULT_ANALYZE_BIN: &str = "trusty-analyze";

// ─── Wire-format shapes ───────────────────────────────────────────────────────

/// Minimal projection of `trusty-analyze review --format json` stdout.
///
/// Why: trusty-review does not depend on the trusty-analyze library crate, so
/// we inline the subset of `ReviewReport` that the pipeline needs.
/// What: deserialises `files` from the JSON output of `trusty-analyze review`.
/// Test: `map_report_to_hotspots_and_smells`.
#[derive(Debug, Deserialize)]
pub(super) struct SubprocessReviewReport {
    pub files: Vec<SubprocessFileReview>,
}

/// One file in the subprocess `ReviewReport`.
#[derive(Debug, Deserialize)]
pub(super) struct SubprocessFileReview {
    pub path: String,
    pub complexity: SubprocessComplexity,
    pub smells: Vec<SubprocessSmellHit>,
}

/// Complexity metrics for one file.
#[derive(Debug, Deserialize)]
pub(super) struct SubprocessComplexity {
    pub cyclomatic: u32,
    pub cognitive: u32,
}

/// One smell hit from the subprocess report.
#[derive(Debug, Deserialize)]
pub(super) struct SubprocessSmellHit {
    pub category: String,
    pub line: u32,
    pub severity: String,
}

// ─── Mapping helpers ──────────────────────────────────────────────────────────

/// Map a `SubprocessReviewReport` to a `(hotspots, smells)` pair.
///
/// Why: the pipeline consumes `Vec<ComplexityHotspot>` and `Vec<Smell>` typed
/// from the HTTP API; this mapping bridges the subprocess JSON to those types so
/// the rest of the pipeline does not need to know about the subprocess transport.
/// What: for each file review, emits one `ComplexityHotspot` (file path +
/// cyclomatic + cognitive) and one `Smell` per detected smell hit.
/// Test: `map_report_to_hotspots_and_smells`.
pub(super) fn map_report(report: &SubprocessReviewReport) -> (Vec<ComplexityHotspot>, Vec<Smell>) {
    let mut hotspots = Vec::new();
    let mut smells = Vec::new();

    for fr in &report.files {
        // Only emit a hotspot if the file has non-trivial complexity.
        if fr.complexity.cyclomatic > 0 || fr.complexity.cognitive > 0 {
            hotspots.push(ComplexityHotspot {
                file: fr.path.clone(),
                function_name: None,
                cyclomatic: fr.complexity.cyclomatic,
                cognitive: fr.complexity.cognitive,
            });
        }

        for sh in &fr.smells {
            smells.push(Smell {
                file: fr.path.clone(),
                category: sh.category.clone(),
                severity: sh.severity.clone(),
                line: Some(sh.line),
            });
        }
    }

    (hotspots, smells)
}
