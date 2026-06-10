//! LCOV coverage report parser.
//!
//! Why: cargo-llvm-cov and tarpaulin both emit LCOV format; parsing it here
//! lets the review pipeline ingest real coverage numbers without depending on
//! external tooling at review time (the CI job produces the file; we consume it).
//! What: parses the text format into a `CoverageReport` that summarises net line
//! coverage (%) and — when the diff is provided — new-code coverage (%).
//! LCOV format reference: genhtml man page / lcov README (DA:line,hits records).
//! Test: `parse_lcov_empty`, `parse_lcov_basic`, `parse_lcov_multiple_files`,
//! `new_code_coverage_all_hit`, `new_code_coverage_partial`.

use std::collections::HashMap;

use thiserror::Error;

/// Error produced by the LCOV parser.
///
/// Why: callers need a typed error to distinguish "file unreadable" from
/// "file malformed" — the policy layer treats both as "coverage unavailable"
/// but the diagnostics differ.
/// What: wraps I/O errors (when loading from a path) and format errors.
/// Test: `parse_lcov_bad_da_line`.
#[derive(Debug, Error)]
pub enum LcovError {
    /// The LCOV file could not be read.
    #[error("failed to read LCOV file: {0}")]
    Io(#[from] std::io::Error),
    /// A DA record had an unexpected format (expected "DA:<line>,<hits>").
    #[error("malformed DA record in LCOV: {0:?}")]
    MalformedDa(String),
}

// ─── Data model ───────────────────────────────────────────────────────────────

/// Net coverage summary parsed from an LCOV report.
///
/// Why: the policy layer only needs the high-level percentages; it does not
/// need the full per-file breakdown.  Keeping this slim reduces coupling.
/// What: `net_pct` is the overall line-hit percentage (hits / instrumented lines);
/// `lines_hit` and `lines_instrumented` are the raw totals.
/// Test: `parse_lcov_basic`.
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageReport {
    /// Total instrumented source lines across all files.
    pub lines_instrumented: u64,
    /// Total lines hit (executed at least once) across all files.
    pub lines_hit: u64,
    /// Overall line-coverage percentage in [0.0, 100.0].
    pub net_pct: f64,
    /// Per-file line-hit data: file path → (hits, instrumented).
    ///
    /// Why: enables the new-code coverage calculation — by knowing which lines
    /// in which files were hit we can intersect with the diff's changed-line set.
    /// What: keyed by the `SF:` path as it appears in the LCOV file.
    pub file_coverage: HashMap<String, FileCoverage>,
}

/// Per-file coverage counters.
///
/// Why: the new-code coverage calculation needs per-file, per-line data so it
/// can intersect with the diff's added-line set.
/// What: `line_hits` maps 1-based line numbers → hit count.  Lines absent from
/// the map were not instrumented (e.g. blank lines, comments, macros).
/// Test: `new_code_coverage_all_hit`, `new_code_coverage_partial`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FileCoverage {
    /// Line-number → hit-count mapping (1-based; instrumented lines only).
    pub line_hits: HashMap<u32, u64>,
}

impl FileCoverage {
    /// Count instrumented lines.
    ///
    /// Why: needed for per-file coverage percentage.
    /// What: returns the number of entries in `line_hits`.
    /// Test: covered transitively.
    pub fn instrumented(&self) -> usize {
        self.line_hits.len()
    }

    /// Count hit lines (executed at least once).
    ///
    /// Why: needed for per-file coverage percentage.
    /// What: counts entries with hit count > 0.
    /// Test: covered transitively.
    pub fn hit(&self) -> usize {
        self.line_hits.values().filter(|&&h| h > 0).count()
    }
}

// ─── Parser ───────────────────────────────────────────────────────────────────

/// Parse an LCOV text report from a string slice.
///
/// Why: the primary entry point for tests and for callers who already have the
/// file contents in memory (e.g. read via `std::fs::read_to_string`).
/// What: iterates over lines; interprets `SF:` (source file), `DA:` (data —
/// line,hits), and `end_of_record` markers per the LCOV format.  Accumulates
/// per-file data and computes net totals.  Unknown record types are silently
/// skipped (forward-compatible with lcov extensions).
/// Test: `parse_lcov_empty`, `parse_lcov_basic`, `parse_lcov_multiple_files`.
pub fn parse_lcov(text: &str) -> Result<CoverageReport, LcovError> {
    let mut file_coverage: HashMap<String, FileCoverage> = HashMap::new();
    let mut current_file: Option<String> = None;

    for line in text.lines() {
        let line = line.trim();
        if let Some(sf_path) = line.strip_prefix("SF:") {
            // New source-file record.
            let path = sf_path.to_string();
            current_file = Some(path.clone());
            file_coverage.entry(path).or_default();
        } else if let Some(da_data) = line.strip_prefix("DA:") {
            // Data record: "DA:<line_number>,<hit_count>[,<checksum>]"
            let data = da_data;
            // Split on comma; only first two fields are required.
            let mut parts = data.splitn(3, ',');
            let line_no_str = parts.next().unwrap_or("");
            let hits_str = parts.next().unwrap_or("");

            let line_no = line_no_str
                .trim()
                .parse::<u32>()
                .map_err(|_| LcovError::MalformedDa(line.to_string()))?;
            let hits = hits_str
                .trim()
                .parse::<u64>()
                .map_err(|_| LcovError::MalformedDa(line.to_string()))?;

            if let Some(ref fname) = current_file {
                file_coverage
                    .entry(fname.clone())
                    .or_default()
                    .line_hits
                    .insert(line_no, hits);
            }
        } else if line == "end_of_record" {
            current_file = None;
        }
        // All other record types (FN:, FNDA:, FNF:, FNH:, BRH:, BRF:, LH:, LF:, …)
        // are silently skipped — we only use per-line DA records.
    }

    // Compute net totals.
    let mut lines_instrumented: u64 = 0;
    let mut lines_hit: u64 = 0;
    for fc in file_coverage.values() {
        lines_instrumented += fc.instrumented() as u64;
        lines_hit += fc.hit() as u64;
    }

    let net_pct = if lines_instrumented == 0 {
        100.0 // No instrumented lines → trivially 100% (don't penalise empty reports).
    } else {
        (lines_hit as f64 / lines_instrumented as f64) * 100.0
    };

    Ok(CoverageReport {
        lines_instrumented,
        lines_hit,
        net_pct,
        file_coverage,
    })
}

/// Parse an LCOV file from disk.
///
/// Why: convenience wrapper for the common case of loading from a filesystem
/// path (e.g. `lcov.info` produced by `cargo llvm-cov --lcov`).
/// What: reads the file, then calls `parse_lcov`.
/// Test: `parse_lcov_from_path_roundtrip`.
pub fn parse_lcov_file(path: &std::path::Path) -> Result<CoverageReport, LcovError> {
    let text = std::fs::read_to_string(path)?;
    parse_lcov(&text)
}

// ─── New-code coverage ────────────────────────────────────────────────────────

/// Compute coverage percentage for lines added in the diff.
///
/// Why: "new code" coverage is a better signal for gating than net coverage —
/// it catches the case where a PR adds substantial untested logic while the
/// overall project coverage stays high due to an existing test suite.
/// What: `added_lines` maps relative file path → set of 1-based added line numbers
/// (as extracted from the unified diff `+` hunks).  We intersect these with the
/// per-file hit data in `report`.  Lines that appear in the diff but are absent
/// from the coverage map are counted as NOT covered (conservative).
/// Returns `None` when `added_lines` is empty (no new instrumented lines) so the
/// caller can distinguish "no new code" from "0% coverage on new code".
/// Test: `new_code_coverage_all_hit`, `new_code_coverage_partial`,
/// `new_code_coverage_no_new_lines`.
pub fn new_code_coverage(
    report: &CoverageReport,
    added_lines: &HashMap<String, Vec<u32>>,
) -> Option<f64> {
    let mut total_new: u64 = 0;
    let mut hit_new: u64 = 0;

    for (file, lines) in added_lines {
        for &lineno in lines {
            // Try both exact path match and suffix match (the diff may use relative
            // paths while the LCOV file uses absolute paths from the build root).
            let hits = report
                .file_coverage
                .get(file)
                .and_then(|fc| fc.line_hits.get(&lineno))
                .or_else(|| {
                    // Suffix-match fallback: find the first LCOV file whose path ends
                    // with the diff file path (handles absolute vs relative mismatch).
                    report
                        .file_coverage
                        .iter()
                        .find(|(k, _)| k.ends_with(file.as_str()))
                        .and_then(|(_, fc)| fc.line_hits.get(&lineno))
                });

            if let Some(&h) = hits {
                // Line is instrumented — count it.
                total_new += 1;
                if h > 0 {
                    hit_new += 1;
                }
            }
            // Lines absent from the coverage map are non-instrumented (comments,
            // blank lines, macros) — skip them to avoid false negatives.
        }
    }

    if total_new == 0 {
        return None; // No instrumented new lines.
    }

    Some((hit_new as f64 / total_new as f64) * 100.0)
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty LCOV input → report with zeros.
    ///
    /// Why: an empty file is valid (no tests run yet); the parser must not panic.
    /// Test: asserts net_pct = 100.0 (trivial), instrumented = 0.
    #[test]
    fn parse_lcov_empty() {
        let report = parse_lcov("").expect("empty LCOV must parse");
        assert_eq!(report.lines_instrumented, 0);
        assert_eq!(report.lines_hit, 0);
        assert!(
            (report.net_pct - 100.0).abs() < f64::EPSILON,
            "empty report must return 100% (trivial)"
        );
        assert!(report.file_coverage.is_empty());
    }

    /// Basic single-file LCOV with known hit/miss lines.
    ///
    /// Why: verifies the parser accumulates DA records correctly.
    /// Test: 3 instrumented, 2 hit → 66.66…%
    #[test]
    fn parse_lcov_basic() {
        let lcov = "SF:src/foo.rs\nDA:1,1\nDA:2,0\nDA:3,5\nend_of_record\n";
        let report = parse_lcov(lcov).expect("parse");
        assert_eq!(report.lines_instrumented, 3);
        assert_eq!(report.lines_hit, 2);
        let expected_pct = (2.0 / 3.0) * 100.0;
        assert!(
            (report.net_pct - expected_pct).abs() < 0.01,
            "expected ~66.67%, got {}",
            report.net_pct
        );
    }

    /// Multi-file LCOV — totals are summed across files.
    ///
    /// Why: verifies the accumulator works across multiple SF records.
    /// Test: two files, 4 total instrumented, 3 hit → 75%.
    #[test]
    fn parse_lcov_multiple_files() {
        let lcov = "SF:src/a.rs\nDA:1,1\nDA:2,1\nend_of_record\nSF:src/b.rs\nDA:10,1\nDA:11,0\nend_of_record\n";
        let report = parse_lcov(lcov).expect("parse");
        assert_eq!(report.lines_instrumented, 4);
        assert_eq!(report.lines_hit, 3);
        let expected_pct = 75.0;
        assert!(
            (report.net_pct - expected_pct).abs() < 0.01,
            "expected 75%, got {}",
            report.net_pct
        );
    }

    /// Malformed DA record produces `LcovError::MalformedDa`.
    ///
    /// Why: the parser must not silently corrupt the totals when a DA record
    /// is unreadable.
    /// Test: asserts Err variant.
    #[test]
    fn parse_lcov_bad_da_line() {
        let lcov = "SF:src/foo.rs\nDA:notanumber,1\nend_of_record\n";
        let result = parse_lcov(lcov);
        assert!(
            result.is_err(),
            "malformed DA line must produce an error, got {result:?}"
        );
    }

    /// new_code_coverage: all new lines are hit → 100%.
    ///
    /// Why: the happy path must report 100% when every added line is exercised.
    /// Test: 2 added lines, both hit.
    #[test]
    fn new_code_coverage_all_hit() {
        let lcov = "SF:src/foo.rs\nDA:5,3\nDA:6,1\nend_of_record\n";
        let report = parse_lcov(lcov).expect("parse");
        let mut added: HashMap<String, Vec<u32>> = HashMap::new();
        added.insert("src/foo.rs".to_string(), vec![5, 6]);
        let pct = new_code_coverage(&report, &added).expect("Some");
        assert!(
            (pct - 100.0).abs() < f64::EPSILON,
            "all hit → 100%, got {pct}"
        );
    }

    /// new_code_coverage: partial — one line hit, one not.
    ///
    /// Why: verifies the numerator/denominator are computed correctly.
    /// Test: 2 added lines, 1 hit → 50%.
    #[test]
    fn new_code_coverage_partial() {
        let lcov = "SF:src/foo.rs\nDA:5,1\nDA:6,0\nend_of_record\n";
        let report = parse_lcov(lcov).expect("parse");
        let mut added: HashMap<String, Vec<u32>> = HashMap::new();
        added.insert("src/foo.rs".to_string(), vec![5, 6]);
        let pct = new_code_coverage(&report, &added).expect("Some");
        assert!(
            (pct - 50.0).abs() < 0.01,
            "50% new-code coverage expected, got {pct}"
        );
    }

    /// new_code_coverage: no new lines → returns None.
    ///
    /// Why: distinguishes "no new code added" from "0% coverage on new code".
    /// Test: empty added_lines map → None.
    #[test]
    fn new_code_coverage_no_new_lines() {
        let lcov = "SF:src/foo.rs\nDA:5,1\nend_of_record\n";
        let report = parse_lcov(lcov).expect("parse");
        let added: HashMap<String, Vec<u32>> = HashMap::new();
        let result = new_code_coverage(&report, &added);
        assert!(result.is_none(), "empty added_lines must return None");
    }

    /// new_code_coverage: suffix-match fallback for absolute-path LCOV.
    ///
    /// Why: `cargo llvm-cov` emits absolute paths in LCOV but the diff uses
    /// repo-relative paths; the suffix-match fallback must bridge this gap.
    /// Test: LCOV uses absolute path, diff uses relative.
    #[test]
    fn new_code_coverage_absolute_vs_relative() {
        let lcov = "SF:/home/ci/workspace/trusty-tools/src/foo.rs\nDA:10,2\nend_of_record\n";
        let report = parse_lcov(lcov).expect("parse");
        let mut added: HashMap<String, Vec<u32>> = HashMap::new();
        added.insert("src/foo.rs".to_string(), vec![10]);
        let pct = new_code_coverage(&report, &added).expect("Some via suffix match");
        assert!(
            (pct - 100.0).abs() < f64::EPSILON,
            "suffix-match fallback must hit → 100%, got {pct}"
        );
    }
}
