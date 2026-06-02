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

pub mod file_filter;
pub mod hunk_classifier;
pub mod hunk_filter;
pub mod models;

pub use file_filter::{FileFilter, FilterConfig};
pub use hunk_classifier::HunkClassifier;
pub use hunk_filter::HunkFilter;
pub use models::{DroppedFile, FilteredDiff, FilteredFile, FilteredHunk};

use std::sync::Arc;

use tracing::{debug, info};

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
        let parsed = parse_diff_files(raw_diff);
        debug!(file_count = parsed.len(), "parsed diff into files");

        // Stage A: file-level filter.
        let file_filter = FileFilter::new(self.config.clone());
        let (mut kept_files, dropped_files) = file_filter.apply(&parsed);
        info!(
            kept = kept_files.len(),
            dropped = dropped_files.len(),
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

// ─── Diff parser ──────────────────────────────────────────────────────────────

/// Parse a unified diff string into `(path, status, patch)` triples.
///
/// Why: Stage A needs per-file structured data; the raw diff is a flat string.
/// What: scans for `diff --git` lines to split files; reads `+++`/`---` headers
/// for paths; treats the remainder as the patch string.
/// Test: `parse_diff_files_basic`, `parse_diff_files_new_file`.
pub fn parse_diff_files(diff: &str) -> Vec<(String, String, String)> {
    let mut files = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_status = "modified".to_string();
    let mut current_patch = String::new();

    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            // Flush previous file.
            if let Some(path) = current_path.take() {
                files.push((path, current_status.clone(), current_patch.clone()));
                current_patch.clear();
            }
            current_status = "modified".to_string();
        } else if let Some(rest) = line.strip_prefix("+++ b/") {
            let path = rest.trim().to_string();
            if path != "/dev/null" && !path.is_empty() {
                current_path = Some(path);
            }
        } else if line.starts_with("+++ /dev/null") {
            // Deleted file — path already captured from --- line; status = removed.
            current_status = "removed".to_string();
        } else if line.starts_with("--- /dev/null") || line.starts_with("new file mode") {
            current_status = "added".to_string();
        } else if line.starts_with("deleted file mode") {
            current_status = "removed".to_string();
        } else if line.starts_with("rename to ") {
            current_status = "renamed".to_string();
        } else if current_path.is_some() {
            current_patch.push_str(line);
            current_patch.push('\n');
        }
    }
    // Flush last file.
    if let Some(path) = current_path {
        files.push((path, current_status, current_patch));
    }
    files
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

    #[test]
    fn parse_diff_files_basic() {
        let files = parse_diff_files(SAMPLE_DIFF);
        assert_eq!(files.len(), 2);
        let paths: Vec<&str> = files.iter().map(|(p, _, _)| p.as_str()).collect();
        assert!(paths.contains(&"Cargo.lock"));
        assert!(paths.contains(&"src/auth.rs"));
    }

    #[test]
    fn parse_diff_files_new_file() {
        let diff = "diff --git a/new.rs b/new.rs\nnew file mode 100644\n--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1 @@\n+fn new() {}\n";
        let files = parse_diff_files(diff);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, "new.rs");
        assert_eq!(files[0].1, "added");
    }
}
