//! Stage B: hunk-level deterministic noise filter (spec REV-203–204).
//!
//! Why: even after Stage A removes noisy files, many surviving hunks contain
//! only whitespace changes, import reorderings, or comment-only edits that
//! consume LLM context without contributing actionable review signal.
//!
//! What: `HunkFilter::apply` iterates each `KEPT` `FilteredFile`, checks
//! every hunk against language-specific regex rules, and moves purely-noise
//! hunks to `dropped_hunks`.  Stage B is purely deterministic — no LLM.
//!
//! Test: `whitespace_only_hunk_dropped`, `import_only_hunk_dropped`,
//! `mixed_hunk_kept`, `comment_only_hunk_dropped`.

use std::collections::HashMap;

use super::file_filter::FilterConfig;
use super::models::{DroppedHunk, FileDisposition, FilteredFile, HunkDropReason};

// ─── Language detection ───────────────────────────────────────────────────────

fn ext(filename: &str) -> &str {
    filename.rfind('.').map(|i| &filename[i..]).unwrap_or("")
}

fn is_python(f: &str) -> bool {
    matches!(ext(f), ".py" | ".pyi")
}

fn is_js_ts(f: &str) -> bool {
    matches!(ext(f), ".js" | ".jsx" | ".ts" | ".tsx" | ".mjs" | ".cjs")
}

fn is_java(f: &str) -> bool {
    ext(f) == ".java"
}

fn is_go(f: &str) -> bool {
    ext(f) == ".go"
}

fn is_rust(f: &str) -> bool {
    ext(f) == ".rs"
}

fn is_c_like(f: &str) -> bool {
    matches!(ext(f), ".c" | ".cpp" | ".cc" | ".cxx" | ".h" | ".hpp")
}

// ─── Import patterns ─────────────────────────────────────────────────────────

/// Return the import-line regex for `filename`, or `None` for unknown languages.
///
/// Why: import detection is language-specific; returning `None` means "no
/// import filtering" rather than a silent miss.
/// What: returns a compiled `Regex` whose full-line match indicates an import
/// statement.  A hunk is import-only if ALL changed lines match.
/// Test: tested via `HunkFilter` import tests.
fn import_pattern_for(filename: &str) -> Option<regex::Regex> {
    let pat = if is_python(filename) {
        // Matches: "import x", "import x, y, z", "from x import y", "from x import (y, z)"
        r"^(import\s+[\w.,\s]+|from\s+[\w.]+\s+import\s+.+)$"
    } else if is_js_ts(filename) {
        r#"^(import\s+.*\s+from\s+['"].+['"]|import\s+['"].+['"]|require\s*\(|export\s+\{.*\}\s+from\s+['"].+['"]);?\s*$"#
    } else if is_java(filename) {
        r"^import\s+(static\s+)?[\w.]+(\.\*)?\s*;\s*$"
    } else if is_go(filename) {
        r#"^(import\s+("[\w./]+"|\()|"[\w./]+")\s*$"#
    } else if is_rust(filename) {
        // Matches: "use std::io;", "use std::io::{Read, Write};", "use x as y;"
        r"^(use\s+[\w::{},\s*]+(\s+as\s+\w+)?;|extern\s+crate\s+\w+;)\s*$"
    } else if is_c_like(filename) {
        r#"^#\s*include\s*[<"].+[>"]"#
    } else {
        return None;
    };
    regex::Regex::new(pat).ok()
}

// ─── Comment patterns ─────────────────────────────────────────────────────────

/// Return the comment-line regex for `filename`, or `None`.
///
/// Why: comment-only diffs add no logic signal and are safe to omit from the
/// reviewer context.
/// What: matches lines that are entirely a comment (no code on the same line).
/// Test: tested via `HunkFilter` comment tests.
fn comment_pattern_for(filename: &str) -> Option<regex::Regex> {
    let pat = if is_python(filename) || is_c_like(filename) {
        r"^(#.*|/\*.*\*/|//.*)$"
    } else if is_js_ts(filename) || is_java(filename) || is_go(filename) || is_rust(filename) {
        r"^(/\*.*\*/|//.*|\s*\*\s?.*)$"
    } else {
        return None;
    };
    regex::Regex::new(pat).ok()
}

// ─── Hunk analysis ────────────────────────────────────────────────────────────

/// Extract changed lines (lines starting with `+` or `-`, excluding `+++`/`---`).
///
/// Why: context lines must not influence classification — only actually-changed
/// lines determine whether a hunk is "import-only" or "comment-only".
/// What: strips the `+`/`-` prefix and trims; skips `+++`/`---` headers.
/// Test: covered transitively by hunk classification tests.
fn extract_changed_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| {
            (l.starts_with('+') || l.starts_with('-'))
                && !l.starts_with("+++")
                && !l.starts_with("---")
        })
        .map(|l| l[1..].trim().to_string())
        .collect()
}

fn is_whitespace_only(lines: &[String]) -> bool {
    lines.iter().all(|l| l.trim().is_empty())
}

fn all_match(lines: &[String], re: &regex::Regex) -> bool {
    !lines.is_empty() && lines.iter().all(|l| re.is_match(l))
}

// ─── HunkFilter ───────────────────────────────────────────────────────────────

/// Stage B: deterministic hunk-level noise filter (spec REV-203–204).
///
/// Why: applies three config-gated rules (whitespace, import, comment) to
/// each surviving hunk in `KEPT` files; moves noise hunks to `dropped_hunks`
/// for telemetry; leaves `SUMMARY_ONLY` files untouched.
/// What: `apply` mutates `FilteredFile.hunks` and `FilteredFile.dropped_hunks`
/// in place and returns aggregate `HunkDropReason` counts.
/// Test: `whitespace_only_hunk_dropped`, `import_only_hunk_dropped`,
/// `mixed_hunk_kept`, `comment_only_hunk_dropped`.
pub struct HunkFilter {
    ignore_whitespace: bool,
    ignore_imports: bool,
    ignore_comments: bool,
}

impl HunkFilter {
    /// Build a `HunkFilter` from the crate's shared `FilterConfig`.
    ///
    /// Why: all three gates come from the same config so the caller only needs
    /// one config object (spec REV-262 "no global state").
    /// What: copies the three `ignore_*` booleans.
    /// Test: `hunk_filter_gates_respect_config`.
    pub fn new(config: &FilterConfig) -> Self {
        Self {
            ignore_whitespace: config.ignore_whitespace,
            ignore_imports: config.ignore_imports,
            ignore_comments: config.ignore_comments,
        }
    }

    /// Apply Stage B to all `KEPT` files; return aggregate drop counts.
    ///
    /// Why: the `DiffAnalyzer` needs to merge Stage B counts into the
    /// `FilteredDiff.drop_hunk_counts` map.
    /// What: calls `filter_file` for each `KEPT` file; accumulates counts.
    /// Test: `apply_returns_aggregate_counts`.
    pub fn apply(&self, files: &mut [FilteredFile]) -> HashMap<HunkDropReason, u32> {
        let mut totals: HashMap<HunkDropReason, u32> = HashMap::new();
        for file in files.iter_mut() {
            if file.disposition != FileDisposition::Kept {
                continue;
            }
            let counts = self.filter_file(file);
            for (reason, n) in counts {
                *totals.entry(reason).or_insert(0) += n;
            }
        }
        totals
    }

    /// Apply Stage B to a single file; returns per-reason drop counts.
    ///
    /// Why: isolated for per-file unit testing without constructing a Vec.
    /// What: checks each hunk via `classify_hunk`; surviving hunks replace
    /// `file.hunks`; dropped hunks go to `file.dropped_hunks`.
    /// Test: used in all per-language hunk tests.
    pub fn filter_file(&self, file: &mut FilteredFile) -> HashMap<HunkDropReason, u32> {
        let import_rx = import_pattern_for(&file.filename);
        let comment_rx = comment_pattern_for(&file.filename);

        let mut surviving = Vec::new();
        let mut counts: HashMap<HunkDropReason, u32> = HashMap::new();

        let hunks = std::mem::take(&mut file.hunks);
        for hunk in hunks {
            match self.classify_hunk(&hunk.lines, &import_rx, &comment_rx) {
                Some(reason) => {
                    *counts.entry(reason.clone()).or_insert(0) += 1;
                    file.dropped_hunks.push(DroppedHunk {
                        reason,
                        lines_count: hunk.lines.len(),
                        header: hunk.header.clone(),
                    });
                }
                None => surviving.push(hunk),
            }
        }
        file.hunks = surviving;
        counts
    }

    /// Classify a single hunk's lines; returns the drop reason or `None` to keep.
    ///
    /// Why: isolated for direct unit testing of classification edge cases.
    /// What: extracts changed lines; applies whitespace/import/comment checks
    /// in priority order.  Returns the first matching drop reason or `None`.
    /// Test: `classify_whitespace_hunk`, `classify_import_hunk`.
    pub fn classify_hunk(
        &self,
        lines: &[String],
        import_rx: &Option<regex::Regex>,
        comment_rx: &Option<regex::Regex>,
    ) -> Option<HunkDropReason> {
        let changed = extract_changed_lines(lines);

        if changed.is_empty() {
            // Context-only hunk: keep if there is any non-empty content.
            let has_content = lines.iter().any(|l| !l.trim().is_empty());
            return if has_content {
                None
            } else {
                if self.ignore_whitespace {
                    Some(HunkDropReason::WhitespaceOnly)
                } else {
                    None
                }
            };
        }

        if self.ignore_whitespace && is_whitespace_only(&changed) {
            return Some(HunkDropReason::WhitespaceOnly);
        }
        if self.ignore_imports && import_rx.as_ref().is_some_and(|re| all_match(&changed, re)) {
            return Some(HunkDropReason::ImportOnly);
        }
        if self.ignore_comments
            && comment_rx
                .as_ref()
                .is_some_and(|re| all_match(&changed, re))
        {
            return Some(HunkDropReason::CommentOnly);
        }
        None
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
#[path = "hunk_filter_tests.rs"]
mod tests;
