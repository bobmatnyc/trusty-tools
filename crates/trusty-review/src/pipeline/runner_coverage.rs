//! Coverage data loading for the review runner (issue #1014).
//!
//! Why: extracted from `runner.rs` to keep that file under the 500-line cap
//! (#610) after adding the coverage-gating pipeline.  All coverage-specific
//! runner logic lives here; the runner itself only calls `load_coverage_contrib`.
//!
//! What: `load_coverage_contrib` is the single entry point used by the runner
//! (step 5b).  `extract_added_lines_from_diff` is a pure helper that parses
//! the diff for added-line numbers so the LCOV intersection can be computed.
//!
//! Test: `extract_added_lines_basic`, `extract_added_lines_multi_file`,
//! `extract_added_lines_hunk_restart`.

use std::collections::HashMap;

use tracing::{info, warn};

use crate::{
    config::ReviewConfig,
    coverage::{CoverageVerdictContrib, evaluate_coverage, new_code_coverage, parse_lcov_file},
};

/// Load coverage data from the configured LCOV path and evaluate the policy.
///
/// Why: the runner calls this once per review; it is async (future-proofed for
/// fetching coverage from a remote URL) but currently only reads from disk.
/// The function is FAIL-OPEN: any error produces a tracing warning and `None`,
/// never an error that blocks the review.
/// What: when `config.coverage.enabled` is false, returns None immediately (no-op).
/// When enabled, reads `config.coverage.lcov_path` (if set), parses it, extracts
/// added-line data from the diff for new-code coverage, and calls `evaluate_coverage`.
/// Test: runner_tests.rs — `run_review_coverage_off_is_noop`,
/// `run_review_coverage_floors_approve`.
pub async fn load_coverage_contrib(
    config: &ReviewConfig,
    diff: &str,
) -> Option<CoverageVerdictContrib> {
    // OFF by default — strict no-op.
    if !config.coverage.enabled {
        return None;
    }

    let path = config.coverage.lcov_path.as_ref()?;

    let report = match parse_lcov_file(path) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                path = %path.display(),
                "coverage: failed to load LCOV file (proceeding without coverage): {e}"
            );
            return None;
        }
    };

    // Extract added lines from the diff for new-code coverage calculation.
    let added_lines = extract_added_lines_from_diff(diff);
    let new_code_pct = new_code_coverage(&report, &added_lines);

    let contrib = evaluate_coverage(&config.coverage, &report, new_code_pct, None);

    info!(
        net_pct = report.net_pct,
        new_code_pct = new_code_pct.unwrap_or(-1.0),
        floor = ?contrib.floor,
        "coverage evaluation complete"
    );

    Some(contrib)
}

/// Extract added lines ("+"-prefixed) from a unified diff, keyed by file path.
///
/// Why: the new-code coverage calculation needs to know which lines were added
/// so it can intersect with the per-file LCOV hit data.
/// What: iterates over diff lines; tracks the current file via "+++ b/" headers
/// and the current hunk start via "@@ … +<start>,<count> @@" markers.
/// Lines beginning with "+" (but not "+++") are counted as added at the inferred
/// line number.  Non-instrumented lines (blank lines, comments) will simply be
/// absent from the LCOV map and are ignored by `new_code_coverage`.
/// Test: `extract_added_lines_basic`, `extract_added_lines_multi_file`.
pub fn extract_added_lines_from_diff(diff: &str) -> HashMap<String, Vec<u32>> {
    let mut result: HashMap<String, Vec<u32>> = HashMap::new();
    let mut current_file: Option<String> = None;
    let mut current_line: u32 = 0;

    for line in diff.lines() {
        if let Some(stripped) = line.strip_prefix("+++ b/") {
            // New file in the diff.
            let path = stripped.to_string();
            current_file = Some(path);
            current_line = 0;
        } else if line.starts_with("--- ") || line.starts_with("+++ ") {
            // Other header line — ignore.
        } else if line.starts_with("@@ ") {
            // Hunk header: "@@ -<old_start>,<old_count> +<new_start>,<new_count> @@"
            // Extract the new-file start line number.
            if let Some(plus_pos) = line.find('+') {
                let rest = &line[plus_pos + 1..];
                let end = rest.find([',', ' ']).unwrap_or(rest.len());
                if let Ok(n) = rest[..end].parse::<u32>() {
                    // Hunk starts at line n; we'll increment as we see lines.
                    current_line = n.saturating_sub(1);
                }
            }
        } else if line.starts_with('+') {
            // Added line.
            current_line += 1;
            if let Some(ref file) = current_file {
                result.entry(file.clone()).or_default().push(current_line);
            }
        } else if line.starts_with('-') {
            // Removed line — does not advance the new-file line counter.
        } else {
            // Context line — advances the new-file line counter.
            current_line += 1;
        }
    }

    result
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// extract_added_lines_from_diff: single file, single hunk.
    ///
    /// Why: the basic case must map "+"-prefixed lines to the correct 1-based
    /// line numbers in the new file.
    /// Test: simple diff with one added line at line 3.
    #[test]
    fn extract_added_lines_basic() {
        let diff = "\
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -1,2 +1,3 @@
 fn foo() {
+    let x = 1;
 }
";
        let added = extract_added_lines_from_diff(diff);
        let lines = added.get("src/foo.rs").expect("src/foo.rs must be present");
        assert_eq!(lines, &[2], "added line must be at new-file line 2");
    }

    /// extract_added_lines_from_diff: multi-file diff.
    ///
    /// Why: verifies the file-tracking logic resets correctly across file boundaries.
    /// Test: two files; each with one added line.
    #[test]
    fn extract_added_lines_multi_file() {
        let diff = "\
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,1 +1,2 @@
 fn a() {}
+fn b() {}
--- a/src/b.rs
+++ b/src/b.rs
@@ -1,1 +1,2 @@
 fn c() {}
+fn d() {}
";
        let added = extract_added_lines_from_diff(diff);
        assert!(added.contains_key("src/a.rs"), "src/a.rs must be present");
        assert!(added.contains_key("src/b.rs"), "src/b.rs must be present");
        // Both have one added line at new-file line 2.
        assert_eq!(added["src/a.rs"], [2]);
        assert_eq!(added["src/b.rs"], [2]);
    }

    /// extract_added_lines_from_diff: hunk with explicit start line > 1.
    ///
    /// Why: verifies the @@ parser correctly reads the new-file start offset.
    /// Test: hunk starting at line 10 with one added line.
    #[test]
    fn extract_added_lines_hunk_restart() {
        let diff = "\
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -10,2 +10,3 @@
 existing_line();
+new_line();
 another_existing();
";
        let added = extract_added_lines_from_diff(diff);
        let lines = added.get("src/foo.rs").expect("must be present");
        // Context line at 10, added line at 11.
        assert_eq!(lines, &[11]);
    }

    /// extract_added_lines_from_diff: empty diff → empty map.
    ///
    /// Why: an empty diff must not panic or produce spurious entries.
    /// Test: empty string input.
    #[test]
    fn extract_added_lines_empty() {
        let added = extract_added_lines_from_diff("");
        assert!(added.is_empty(), "empty diff must produce empty map");
    }
}
