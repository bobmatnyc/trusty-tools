//! DiffAnalyzer — three-stage diff noise filter (spec REV-200–262).
//!
//! Why: PR diffs frequently contain lockfiles, snapshots, whitespace-only hunks,
//! import reorderings, and comment-only changes that consume LLM context budget
//! without contributing review signal.  The DiffAnalyzer strips these before the
//! reviewer LLM sees the diff, maximising the fraction of the context window that
//! carries real signal (lesson §12.12 — the PR #9545 fixture-churn problem).
//!
//! What: implements the three-stage pipeline from spec REV-200:
//!  - Stage A (`file_filter`) — deterministic file-level classification.
//!  - Stage B (`hunk_filter`) — deterministic hunk-level classification.
//!  - Stage C (`hunk_classifier`) — optional Haiku LLM hunk classification.
//!
//! Standalone usage (spec REV-260): the module is usable without the review
//! pipeline.  Stages A+B are always deterministic (no LLM needed).  Stage C
//! requires an injected `LlmProvider` and is disabled by default
//! (`FilterConfig::disable_classifier = true`).
//!
//! Test: `diff_analyzer_stages_a_b_integration`, `diff_analyzer_drops_lockfile`.

pub mod diff_parser;
pub mod file_filter;
pub mod hunk_classifier;
pub mod hunk_filter;
pub mod models;

pub use diff_parser::{ParsedDiff, UnparsedSection, parse_diff_files, parse_diff_files_detailed};
pub use file_filter::{FileFilter, FilterConfig};
pub use hunk_classifier::HunkClassifier;
pub use hunk_filter::HunkFilter;
pub use models::{DroppedFile, FilteredDiff, FilteredFile, FilteredHunk};

use std::sync::Arc;

use tracing::{debug, info, warn};

use crate::llm::LlmProvider;

/// Top-level DiffAnalyzer — orchestrates Stages A, B, and optionally C.
///
/// Why: single entry point so the pipeline has a minimal, stable integration
/// surface (spec REV-260).  The orchestrator computes byte-size telemetry and
/// logs filter results without requiring the caller to know Stage internals.
/// What: `analyze` accepts a raw unified diff string plus an optional file-status
/// map, runs the three stages, and returns a `FilteredDiff`.
/// Test: `diff_analyzer_stages_a_b_integration`, `diff_analyzer_drops_lockfile`.
pub struct DiffAnalyzer {
    config: FilterConfig,
    classifier_provider: Option<Arc<dyn LlmProvider>>,
}

impl Default for DiffAnalyzer {
    /// Default DiffAnalyzer: default FilterConfig, no Stage C provider.
    ///
    /// Why: most callers want Stages A+B only (deterministic, no LLM required).
    /// What: equivalent to `DiffAnalyzer::new(FilterConfig::default(), None)`.
    /// Test: used in pipeline integration tests and runner.rs.
    fn default() -> Self {
        Self::new(FilterConfig::default(), None)
    }
}

impl DiffAnalyzer {
    /// Build a `DiffAnalyzer` with the given config and optional Stage C provider.
    ///
    /// Why: provider is injected for testability (spec REV-261); passing `None`
    /// runs Stages A+B only (fully deterministic, no LLM).
    /// What: stores config and provider; no I/O at construction.
    /// Test: `diff_analyzer_stages_a_b_integration`.
    pub fn new(config: FilterConfig, classifier_provider: Option<Arc<dyn LlmProvider>>) -> Self {
        Self {
            config,
            classifier_provider,
        }
    }

    /// Analyze a unified diff string; return a `FilteredDiff`.
    ///
    /// Why: wraps parse → Stage A → Stage B → Stage C → telemetry in one call
    /// so the pipeline just calls `analyze(&raw_diff).render_for_prompt(cap)`.
    /// What: parses the raw diff into `(path, status, patch)` triples, runs
    /// `FileFilter.apply`, then `HunkFilter.apply`, then (if enabled and a
    /// provider is available) `HunkClassifier.classify`.  Computes byte-size
    /// telemetry.  Returns a `FilteredDiff` ready for `render_for_prompt`.
    /// Test: `diff_analyzer_stages_a_b_integration`, `diff_analyzer_drops_lockfile`.
    pub async fn analyze(&self, raw_diff: &str) -> FilteredDiff {
        let original_byte_size = raw_diff.len();

        // Parse the raw diff into (path, status, patch) triples.
        let ParsedDiff {
            files: parsed,
            unparsed,
        } = parse_diff_files_detailed(raw_diff);
        debug!(file_count = parsed.len(), "parsed diff into files");
        // #4458: content the parser could not attribute to a file is reported
        // rather than dropped in silence — a shrinking file list is how the
        // collapse stayed invisible in production.
        if !unparsed.is_empty() {
            let unparsed_lines: usize = unparsed.iter().map(|s| s.line_count).sum();
            warn!(
                unparsed_sections = unparsed.len(),
                unparsed_lines,
                first = %unparsed[0].header,
                "diff content could not be attributed to a file"
            );
        }

        // Stage A: file-level filter.
        let file_filter = FileFilter::new(self.config.clone());
        let (mut kept_files, dropped_files) = file_filter.apply(&parsed);
        info!(
            parsed = parsed.len(),
            kept = kept_files.len(),
            dropped = dropped_files.len(),
            unparsed_sections = unparsed.len(),
            "Stage A complete"
        );

        // Stage B: hunk-level filter.
        let hunk_filter = HunkFilter::new(&self.config);
        let mut drop_hunk_counts = hunk_filter.apply(&mut kept_files);
        let stage_b_total: u32 = drop_hunk_counts.values().sum();
        info!(dropped_hunks = stage_b_total, "Stage B complete");

        // Stage C: LLM classifier (optional — disabled by default).
        if !self.config.disable_classifier
            && let Some(ref provider) = self.classifier_provider
        {
            use crate::pipeline::diff_analyzer::hunk_classifier::{
                DEFAULT_CLASSIFIER_MODEL, DROP_CONFIDENCE_THRESHOLD, HunkClassifier,
            };
            use models::{DroppedHunk, HunkDropReason};

            let classifier = HunkClassifier::new(
                Arc::clone(provider),
                DEFAULT_CLASSIFIER_MODEL,
                self.config.classifier_batch_size,
                DROP_CONFIDENCE_THRESHOLD,
            );
            for file in kept_files.iter_mut() {
                if file.disposition != models::FileDisposition::Kept {
                    continue;
                }
                let classifications = classifier.classify(&file.hunks).await;
                let mut surviving = Vec::new();
                for (hunk, cls) in file.hunks.drain(..).zip(classifications.iter()) {
                    if cls.should_drop() {
                        *drop_hunk_counts
                            .entry(HunkDropReason::MechanicalHaiku)
                            .or_insert(0) += 1;
                        file.dropped_hunks.push(DroppedHunk {
                            reason: cls.drop_reason(),
                            lines_count: hunk.lines.len(),
                            header: hunk.header.clone(),
                        });
                    } else {
                        surviving.push(hunk);
                    }
                }
                file.hunks = surviving;
            }
            let stage_c_total: u32 = drop_hunk_counts
                .get(&models::HunkDropReason::MechanicalHaiku)
                .copied()
                .unwrap_or(0);
            info!(dropped_hunks = stage_c_total, "Stage C complete");
        }

        // Compute filtered byte size (approximate; based on rendered content).
        let filtered_byte_size = kept_files
            .iter()
            .flat_map(|f| f.hunks.iter().flat_map(|h| h.lines.iter().map(|l| l.len())))
            .sum::<usize>();

        FilteredDiff {
            files: kept_files,
            dropped_files,
            drop_hunk_counts,
            original_byte_size,
            filtered_byte_size,
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_DIFF: &str = r#"diff --git a/Cargo.lock b/Cargo.lock
index abc..def 100644
--- a/Cargo.lock
+++ b/Cargo.lock
@@ -1,3 +1,3 @@
-serde = "1.0.100"
+serde = "1.0.200"
diff --git a/src/auth.rs b/src/auth.rs
index abc..def 100644
--- a/src/auth.rs
+++ b/src/auth.rs
@@ -1,3 +1,5 @@
-pub fn authenticate(user: &str) -> Result<Token, Error> {
+pub fn authenticate(user: &str, config: &Config) -> Result<Token, Error> {
+    validate(user)?;
     Ok(Token::new(user))
 }
"#;

    #[tokio::test]
    async fn diff_analyzer_drops_lockfile() {
        let analyzer = DiffAnalyzer::default();
        let result = analyzer.analyze(SAMPLE_DIFF).await;
        assert_eq!(result.dropped_files.len(), 1);
        assert_eq!(result.dropped_files[0].path, "Cargo.lock");
        assert_eq!(result.files.len(), 1);
        assert_eq!(result.files[0].filename, "src/auth.rs");
    }

    #[tokio::test]
    async fn diff_analyzer_stages_a_b_integration() {
        // A diff where one file is a lockfile (dropped) and another has an
        // import-only hunk alongside a logic hunk.
        let diff = "\
diff --git a/package-lock.json b/package-lock.json\n\
--- a/package-lock.json\n\
+++ b/package-lock.json\n\
@@ -1,1 +1,1 @@\n\
-\"version\": \"1\"\n\
+\"version\": \"2\"\n\
diff --git a/src/api.rs b/src/api.rs\n\
--- a/src/api.rs\n\
+++ b/src/api.rs\n\
@@ -1,1 +1,1 @@\n\
-use std::io;\n\
+use std::io::{Read, Write};\n\
@@ -10,3 +10,4 @@\n\
-pub fn handle(req: Request) -> Response {\n\
+pub fn handle(req: Request, cfg: &Config) -> Response {\n\
+    cfg.validate()?;\n\
     Ok(Response::ok())\n\
 }\n\
";
        let analyzer = DiffAnalyzer::default();
        let result = analyzer.analyze(diff).await;

        assert_eq!(result.dropped_files.len(), 1, "lockfile must be dropped");
        assert_eq!(result.files.len(), 1, "only src/api.rs should survive");

        let api_file = &result.files[0];
        // Stage B should have dropped the import-only hunk.
        assert!(
            !api_file.dropped_hunks.is_empty() || api_file.hunks.len() < 2,
            "import-only hunk should be dropped by Stage B"
        );

        let rendered = result.render_for_prompt(100_000);
        assert!(
            rendered.contains("handle"),
            "logic hunk must appear in rendered diff"
        );
    }

    /// #4458: a diff whose files are separated only by `---`/`+++` pairs must
    /// reach Stage A as N files, not as one file holding every file's hunks.
    #[tokio::test]
    async fn diff_analyzer_keeps_every_file_of_a_marker_less_diff() {
        let diff = "\
--- a/package-lock.json\n\
+++ b/package-lock.json\n\
@@ -1,1 +1,1 @@\n\
-\"version\": \"1\"\n\
+\"version\": \"2\"\n\
--- a/src/alpha.rs\n\
+++ b/src/alpha.rs\n\
@@ -1,1 +1,1 @@\n\
-fn alpha() {}\n\
+fn alpha(cfg: &Config) {}\n\
--- a/src/beta.rs\n\
+++ b/src/beta.rs\n\
@@ -1,1 +1,1 @@\n\
-fn beta() {}\n\
+fn beta(cfg: &Config) {}\n\
";
        let analyzer = DiffAnalyzer::default();
        let result = analyzer.analyze(diff).await;

        assert_eq!(
            result.files.len() + result.dropped_files.len(),
            3,
            "all three files must reach Stage A; kept={:?} dropped={:?}",
            result.files.iter().map(|f| &f.filename).collect::<Vec<_>>(),
            result
                .dropped_files
                .iter()
                .map(|f| &f.path)
                .collect::<Vec<_>>()
        );
        assert_eq!(result.dropped_files[0].path, "package-lock.json");
        let kept: Vec<&str> = result.files.iter().map(|f| f.filename.as_str()).collect();
        assert_eq!(kept, vec!["src/alpha.rs", "src/beta.rs"]);
    }
}
