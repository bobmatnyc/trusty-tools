//! Data models for the DiffAnalyzer pipeline (spec REV-200–262, §4).
//!
//! Why: typed output lets each stage produce structured metadata rather than
//! opaque strings, enabling per-hunk telemetry, stage isolation in tests,
//! and a clean audit trail of what was dropped and why (lesson §12.12).
//! What: `FilteredDiff` is the top-level result; `render_for_prompt` produces
//! the noise-filtered diff text bounded to `max_chars`.  The manifest is
//! telemetry-only — never injected into the LLM prompt (spec REV-209).
//! Test: `filtered_diff_render_for_prompt_contains_surviving_content`,
//! `filtered_diff_render_respects_max_chars`,
//! `filtered_diff_drop_summary_emitted`.

use std::collections::HashMap;

// ─── Disposition / drop reason enums ─────────────────────────────────────────

/// Stage A file-level filtering outcome (spec REV-201).
///
/// Why: structured enum prevents the "KEPT vs kept" string mismatch bugs that
/// plagued the Python predecessor's early iterations.
/// What: three variants covering the three Stage A outcomes.
/// Test: used directly in `FileFilter` and `DiffAnalyzer` tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileDisposition {
    /// File survives in full; hunks are passed to Stage B.
    Kept,
    /// File is a noise class (lockfile, snapshot, generated); excluded entirely.
    Dropped,
    /// File is a fixture/i18n artefact; content collapsed to one summary line.
    SummaryOnly,
}

/// Reason a hunk was dropped in Stage B or Stage C (spec REV-203, REV-206).
///
/// Why: explicit reason enum enables per-reason telemetry counters and lets the
/// noise-summary line tell the reviewer exactly what was filtered.
/// What: four variants — three deterministic (Stage B) and one LLM (Stage C).
/// Test: used in `HunkFilter` and `HunkClassifier` tests.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum HunkDropReason {
    /// Hunk only changes whitespace (spaces/tabs/blank lines).
    WhitespaceOnly,
    /// Hunk only changes import/use statements.
    ImportOnly,
    /// Hunk only changes comments.
    CommentOnly,
    /// Stage C Haiku classifier marked this hunk `mechanical` with high confidence.
    MechanicalHaiku,
}

impl HunkDropReason {
    /// Human-readable label used in the noise summary injected into the prompt.
    ///
    /// Why: the summary must be legible to humans and the LLM; snake_case keys
    /// are not user-friendly.
    /// What: returns a static English phrase for each variant.
    /// Test: `hunk_drop_reason_label`.
    pub fn label(&self) -> &'static str {
        match self {
            HunkDropReason::WhitespaceOnly => "whitespace-only",
            HunkDropReason::ImportOnly => "import-only",
            HunkDropReason::CommentOnly => "comment-only",
            HunkDropReason::MechanicalHaiku => "mechanical (Haiku)",
        }
    }
}

// ─── Hunk types ───────────────────────────────────────────────────────────────

/// A single hunk that survived Stage B / Stage C filtering (spec §4).
///
/// Why: retains header and lines so `render_for_prompt` can reconstruct a valid
/// unified diff section verbatim.
/// What: `header` is the `@@` line; `lines` are the raw diff lines (including
/// `+`, `-`, and context lines); `substantive_confidence` is 1.0 for
/// deterministic survivors and the Haiku score for Stage C survivors.
/// Test: used in `HunkFilter` and integration tests.
#[derive(Debug, Clone)]
pub struct FilteredHunk {
    /// The `@@ -a,b +c,d @@ context` header line.
    pub header: String,
    /// Raw diff lines (context, `+`, `-`).
    pub lines: Vec<String>,
    /// Haiku-assigned substantive confidence (default 1.0 for det. survivors).
    pub substantive_confidence: f32,
    /// Human-readable reason this hunk was kept.
    pub reason_kept: String,
}

impl FilteredHunk {
    /// Reconstruct this hunk as a unified diff string segment.
    ///
    /// Why: `render_for_prompt` needs to reconstruct the diff body from
    /// individual hunks without re-parsing the original text.
    /// What: joins header and lines with `\n`.
    /// Test: `filtered_hunk_render_roundtrip`.
    pub fn render(&self) -> String {
        let mut out = self.header.clone();
        for line in &self.lines {
            out.push('\n');
            out.push_str(line);
        }
        out
    }
}

/// A hunk that was dropped in Stage B or Stage C (spec §4).
///
/// Why: preserves drop metadata for the noise-summary and telemetry manifest.
/// What: `reason` is the `HunkDropReason`; `lines_count` is the raw diff-line
/// count of the dropped hunk; `header` is the `@@` line for reference.
/// Test: used in `HunkFilter` tests.
#[derive(Debug, Clone)]
pub struct DroppedHunk {
    /// Why this hunk was dropped.
    pub reason: HunkDropReason,
    /// Number of diff lines in the dropped hunk (for telemetry).
    pub lines_count: usize,
    /// The `@@` header line of the dropped hunk.
    pub header: String,
}

// ─── File types ───────────────────────────────────────────────────────────────

/// A file that survived Stage A, with its Stage B-filtered hunk list (spec §4).
///
/// Why: per-file structure lets the pipeline track per-file drop counts and
/// re-render individual files for the prompt without rebuilding from the raw diff.
/// What: `disposition` is always `Kept` or `SummaryOnly` for files in this list
/// (`Dropped` files go to `DroppedFile`); `summary_line` is set for `SummaryOnly`
/// files; `hunks` contains the surviving Stage B hunks.
/// Test: used in `FileFilter`, `HunkFilter`, and integration tests.
#[derive(Debug, Clone)]
pub struct FilteredFile {
    /// File path (from the `+++ b/` header).
    pub filename: String,
    /// Git status: `"added"`, `"modified"`, `"renamed"`, `"removed"`.
    pub status: String,
    /// Stage A outcome (`Kept` or `SummaryOnly`; never `Dropped` here).
    pub disposition: FileDisposition,
    /// Stage B survivors. Empty for `SummaryOnly` files.
    pub hunks: Vec<FilteredHunk>,
    /// Stage B drops (retained for telemetry only; not rendered).
    pub dropped_hunks: Vec<DroppedHunk>,
    /// One-line summary for `SummaryOnly` files.
    pub summary_line: Option<String>,
}

/// A file that was dropped entirely in Stage A (spec §4).
///
/// Why: retained separately from `FilteredFile` so the noise summary can say
/// "N files dropped: lockfiles, snapshots, …" without scanning all files.
/// What: `path` is the file name; `reason` is a human label for the drop rule
/// that fired.
/// Test: used in `FileFilter` tests.
#[derive(Debug, Clone)]
pub struct DroppedFile {
    /// File path.
    pub path: String,
    /// Human-readable drop reason (e.g. `"lockfile"`, `"snapshot"`).
    pub reason: String,
}

// ─── FilteredDiff — top-level result ─────────────────────────────────────────

/// Top-level result from `DiffAnalyzer::analyze` (spec REV-200, §4).
///
/// Why: encapsulates the full analysis result — surviving files/hunks plus all
/// telemetry — so the pipeline can call `render_for_prompt` without knowing the
/// internals of the filter stages.
/// What: `files` are the surviving files (with their filtered hunks);
/// `dropped_files` are Stage A exclusions; `drop_hunk_counts` aggregates Stage B
/// drops by reason; `render_for_prompt` produces the bounded diff text.
/// Test: `filtered_diff_render_for_prompt_contains_surviving_content`,
/// `filtered_diff_render_respects_max_chars`.
#[derive(Debug, Clone)]
pub struct FilteredDiff {
    /// Files that survived Stage A (disposition Kept or SummaryOnly).
    pub files: Vec<FilteredFile>,
    /// Files dropped entirely in Stage A.
    pub dropped_files: Vec<DroppedFile>,
    /// Aggregate hunk-drop counts by reason (Stage B + Stage C).
    pub drop_hunk_counts: HashMap<HunkDropReason, u32>,
    /// Character length of the raw diff before filtering.
    pub original_byte_size: usize,
    /// Character length of the filtered diff text (after render).
    pub filtered_byte_size: usize,
}

impl FilteredDiff {
    /// Render the filtered diff as a bounded string for the LLM prompt.
    ///
    /// Why: the prompt builder needs a diff string bounded to `max_chars`; this
    /// method encapsulates the rendering logic so the pipeline has one call site.
    /// What: iterates `files`, renders each surviving hunk, appends the noise
    /// summary, and stops before exceeding `max_chars`.  Does NOT inject a
    /// manifest header (spec REV-209 — framing-regression guard).
    /// Test: `filtered_diff_render_for_prompt_contains_surviving_content`,
    /// `filtered_diff_render_respects_max_chars`.
    pub fn render_for_prompt(&self, max_chars: usize) -> String {
        let mut out = String::with_capacity(max_chars.min(64 * 1024));
        let suffix = self.build_noise_summary();

        for file in &self.files {
            match file.disposition {
                FileDisposition::SummaryOnly => {
                    if let Some(ref summary) = file.summary_line {
                        let line = format!("# {}: {}\n", file.filename, summary);
                        if out.len() + line.len() + suffix.len() > max_chars {
                            break;
                        }
                        out.push_str(&line);
                    }
                }
                FileDisposition::Kept => {
                    // Build the file header.
                    let file_header = format!("--- a/{0}\n+++ b/{0}\n", file.filename);
                    if out.len() + file_header.len() + suffix.len() > max_chars {
                        break;
                    }
                    out.push_str(&file_header);

                    for hunk in &file.hunks {
                        let rendered = hunk.render();
                        if out.len() + rendered.len() + suffix.len() + 1 > max_chars {
                            break;
                        }
                        out.push_str(&rendered);
                        out.push('\n');
                    }
                }
                FileDisposition::Dropped => {
                    // Dropped files are never rendered in the prompt.
                }
            }
        }

        if !suffix.is_empty() {
            out.push_str(&suffix);
        }

        out
    }

    /// Build the noise-summary line appended to the prompt (spec REV-209).
    ///
    /// Why: the reviewer model must know that filtering happened so it does not
    /// assume it is seeing the complete diff (spec REV-209).
    /// What: produces a one-line summary of what was omitted, or an empty string
    /// if nothing was dropped.  Never exceeds one paragraph.
    /// Test: `filtered_diff_drop_summary_emitted`.
    pub fn build_noise_summary(&self) -> String {
        let dropped_files = self.dropped_files.len();
        let dropped_hunks: u32 = self.drop_hunk_counts.values().sum();

        if dropped_files == 0 && dropped_hunks == 0 {
            return String::new();
        }

        let mut parts: Vec<String> = Vec::new();
        if dropped_files > 0 {
            // Collect up to 3 unique drop reasons for the file summary.
            let mut reasons: Vec<String> = self
                .dropped_files
                .iter()
                .map(|f| f.reason.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .take(3)
                .collect();
            reasons.sort();
            let reason_str = if reasons.is_empty() {
                String::new()
            } else {
                format!(" ({})", reasons.join(", "))
            };
            parts.push(format!("{dropped_files} file(s) omitted{reason_str}"));
        }
        if dropped_hunks > 0 {
            let mut labels: Vec<&str> = self
                .drop_hunk_counts
                .keys()
                .map(|r| r.label())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            labels.sort();
            let label_str = if labels.is_empty() {
                String::new()
            } else {
                format!(" ({})", labels.join(", "))
            };
            parts.push(format!("{dropped_hunks} hunk(s) omitted{label_str}"));
        }

        format!(
            "\n[DiffAnalyzer filtered {} — noise removed before review]\n",
            parts.join("; ")
        )
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_kept_file(name: &str, hunk_content: &str) -> FilteredFile {
        FilteredFile {
            filename: name.to_string(),
            status: "modified".to_string(),
            disposition: FileDisposition::Kept,
            hunks: vec![FilteredHunk {
                header: "@@ -1,3 +1,3 @@".to_string(),
                lines: vec![hunk_content.to_string()],
                substantive_confidence: 1.0,
                reason_kept: "deterministic-pass".to_string(),
            }],
            dropped_hunks: vec![],
            summary_line: None,
        }
    }

    #[test]
    fn filtered_hunk_render_roundtrip() {
        let h = FilteredHunk {
            header: "@@ -1,2 +1,2 @@".to_string(),
            lines: vec!["-old line".to_string(), "+new line".to_string()],
            substantive_confidence: 1.0,
            reason_kept: "test".to_string(),
        };
        let rendered = h.render();
        assert!(rendered.contains("@@ -1,2 +1,2 @@"));
        assert!(rendered.contains("-old line"));
        assert!(rendered.contains("+new line"));
    }

    #[test]
    fn hunk_drop_reason_label() {
        assert_eq!(HunkDropReason::WhitespaceOnly.label(), "whitespace-only");
        assert_eq!(HunkDropReason::ImportOnly.label(), "import-only");
        assert_eq!(HunkDropReason::CommentOnly.label(), "comment-only");
        assert_eq!(
            HunkDropReason::MechanicalHaiku.label(),
            "mechanical (Haiku)"
        );
    }

    #[test]
    fn filtered_diff_render_for_prompt_contains_surviving_content() {
        let diff = FilteredDiff {
            files: vec![make_kept_file("src/auth.rs", "+pub fn authenticate() {}")],
            dropped_files: vec![],
            drop_hunk_counts: HashMap::new(),
            original_byte_size: 500,
            filtered_byte_size: 100,
        };
        let rendered = diff.render_for_prompt(10_000);
        assert!(rendered.contains("src/auth.rs"), "file path must appear");
        assert!(
            rendered.contains("authenticate"),
            "hunk content must appear"
        );
    }

    #[test]
    fn filtered_diff_render_respects_max_chars() {
        // Create a large number of files — rendering should stop before max_chars.
        let files: Vec<FilteredFile> = (0..100)
            .map(|i| make_kept_file(&format!("src/file{i}.rs"), &"+fn foo() {}".repeat(50)))
            .collect();
        let diff = FilteredDiff {
            files,
            dropped_files: vec![],
            drop_hunk_counts: HashMap::new(),
            original_byte_size: 100_000,
            filtered_byte_size: 50_000,
        };
        let rendered = diff.render_for_prompt(2_000);
        assert!(
            rendered.len() <= 2_000 + 200,
            "rendered output must not greatly exceed max_chars: len={}",
            rendered.len()
        );
    }

    #[test]
    fn filtered_diff_drop_summary_emitted() {
        let mut drop_counts = HashMap::new();
        drop_counts.insert(HunkDropReason::ImportOnly, 3u32);
        drop_counts.insert(HunkDropReason::WhitespaceOnly, 1u32);

        let diff = FilteredDiff {
            files: vec![make_kept_file("src/main.rs", "+fn main() {}")],
            dropped_files: vec![DroppedFile {
                path: "Cargo.lock".to_string(),
                reason: "lockfile".to_string(),
            }],
            drop_hunk_counts: drop_counts,
            original_byte_size: 5_000,
            filtered_byte_size: 200,
        };

        let rendered = diff.render_for_prompt(100_000);
        assert!(
            rendered.contains("DiffAnalyzer filtered"),
            "noise summary must appear: {rendered}"
        );
        assert!(
            rendered.contains("file(s) omitted"),
            "file drop count must appear: {rendered}"
        );
        assert!(
            rendered.contains("hunk(s) omitted"),
            "hunk drop count must appear: {rendered}"
        );
    }

    #[test]
    fn no_summary_when_nothing_dropped() {
        let diff = FilteredDiff {
            files: vec![make_kept_file("src/lib.rs", "+pub fn new() {}")],
            dropped_files: vec![],
            drop_hunk_counts: HashMap::new(),
            original_byte_size: 100,
            filtered_byte_size: 100,
        };
        let summary = diff.build_noise_summary();
        assert!(summary.is_empty(), "empty summary when nothing was dropped");
    }
}
