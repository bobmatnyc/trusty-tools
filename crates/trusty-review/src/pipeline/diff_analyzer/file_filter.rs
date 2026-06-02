//! Stage A: file-level deterministic noise filter (spec REV-201–202).
//!
//! Why: lockfiles, snapshots, generated code, and pure renames consume large
//! portions of the LLM context budget without contributing review signal.
//! Dropping or summarising them before Stage B increases effective signal
//! density and protects the reviewer from the §12.12 fixture-churn problem.
//!
//! What: `FileFilter::apply` classifies each file from a parsed diff as
//! `Kept`, `Dropped`, or `SummaryOnly`.  Decisions are ordered:
//! 1. Preserve (security paths, credential filenames) — highest priority.
//! 2. Drop (lockfiles, snapshots, generated code).
//! 3. SummaryOnly (fixture/i18n files > `fixture_summary_min_lines`).
//! 4. Kept — default.
//!
//! Test: `lockfile_is_dropped`, `security_file_is_preserved`,
//! `fixture_file_is_summarised`, `plain_rust_file_is_kept`.

use super::models::{DroppedFile, FileDisposition, FilteredFile, FilteredHunk};

// ─── Built-in pattern lists ───────────────────────────────────────────────────

static LOCKFILE_SUFFIXES: &[&str] = &[
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "Cargo.lock",
    "uv.lock",
    "poetry.lock",
    "Pipfile.lock",
    "Gemfile.lock",
    "composer.lock",
    "go.sum",
];

static SNAPSHOT_SUFFIXES: &[&str] = &[".snap", ".snapshot."];

static SNAPSHOT_SEGMENTS: &[&str] = &["__snapshots__/", "__fixtures__/"];

static GENERATED_SUFFIXES: &[&str] = &[
    "_pb2.py",
    "_pb2_grpc.py",
    ".pb.go",
    ".generated.",
    ".gen.go",
];

static GENERATED_SEGMENTS: &[&str] = &["dist/", "build/", ".next/", "__generated__/"];

/// Pattern: `generated/<lang-ext>` file.
fn is_generated_lang(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.contains("generated/")
        && (lower.ends_with(".java") || lower.ends_with(".ts") || lower.ends_with(".js"))
}

static FIXTURE_SUFFIXES: &[&str] = &[".tsv", ".csv"];

static FIXTURE_SEGMENTS: &[&str] = &[
    "nls/",
    "messages/",
    "labels/",
    "fixtures/",
    "test_fixtures/",
    "__generated__/",
    "generated/",
];

fn is_fixture_json_xml_sql(path: &str) -> bool {
    let lower = path.to_lowercase();
    (lower.contains("fixture") || lower.contains("test_fixture"))
        && (lower.ends_with(".json") || lower.ends_with(".xml") || lower.ends_with(".sql"))
}

// ─── Preserve rules ───────────────────────────────────────────────────────────

static PRESERVE_PATH_SEGMENTS: &[&str] = &["security/", ".env", "secrets/"];

static CREDENTIAL_FILENAME_SUBSTRINGS: &[&str] =
    &["secret", "token", "password", "key", "dsn", "credential"];

/// Regex for credential URLs embedded in diff content.
static CREDENTIAL_URL_PATTERN: &str = r"https?://[^@ ]+@";

/// Returns `true` if the path or patch likely contains credentials (spec REV-207).
///
/// Why: force-preserving credential-bearing files prevents the filter from
/// accidentally stripping security-sensitive changes.
/// What: checks path segments, filename substrings, and content for credential
/// URL patterns.
/// Test: `credential_path_is_preserved`, `credential_url_in_patch_is_preserved`.
pub fn should_preserve(path: &str, patch: &str) -> bool {
    let lower = path.to_lowercase();

    for seg in PRESERVE_PATH_SEGMENTS {
        if lower.contains(seg) {
            return true;
        }
    }
    // Filename (last component) check for credential substrings.
    let filename = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    for sub in CREDENTIAL_FILENAME_SUBSTRINGS {
        if filename.contains(sub) {
            return true;
        }
    }
    // Content-level credential URL check.
    if !patch.is_empty()
        && patch.contains("://")
        && patch.contains('@')
        && regex::Regex::new(CREDENTIAL_URL_PATTERN)
            .map(|re| re.is_match(patch))
            .unwrap_or(false)
    {
        return true;
    }
    false
}

// ─── Classification helpers ───────────────────────────────────────────────────

fn is_lockfile(path: &str) -> bool {
    for suffix in LOCKFILE_SUFFIXES {
        if path == *suffix || path.ends_with(&format!("/{suffix}")) {
            return true;
        }
    }
    false
}

fn is_snapshot(path: &str) -> bool {
    for suffix in SNAPSHOT_SUFFIXES {
        if path.ends_with(suffix) {
            return true;
        }
    }
    for seg in SNAPSHOT_SEGMENTS {
        if path.contains(seg) {
            return true;
        }
    }
    false
}

fn is_generated(path: &str) -> bool {
    for suffix in GENERATED_SUFFIXES {
        if path.ends_with(suffix) {
            return true;
        }
    }
    for seg in GENERATED_SEGMENTS {
        if path.contains(seg) {
            return true;
        }
    }
    is_generated_lang(path)
}

fn is_fixture(path: &str) -> bool {
    let lower = path.to_lowercase();
    for suffix in FIXTURE_SUFFIXES {
        if lower.ends_with(suffix) {
            return true;
        }
    }
    for seg in FIXTURE_SEGMENTS {
        if lower.contains(seg) {
            return true;
        }
    }
    is_fixture_json_xml_sql(&lower)
}

fn count_patch_lines(patch: &str) -> usize {
    patch.lines().count()
}

// ─── Diff parsing helper ──────────────────────────────────────────────────────

/// Split a unified-diff patch string into (header, lines) pairs.
///
/// Why: Stage A needs per-hunk structure to populate `FilteredFile.hunks` for
/// Stage B.  If the patch has no `@@` headers (e.g., opaque blobs), the entire
/// patch is treated as a single hunk.
/// What: scans for `@@` lines; each found line starts a new hunk.
/// Test: `split_patch_into_hunks_basic`.
pub fn split_patch_into_hunks(patch: &str) -> Vec<(String, Vec<String>)> {
    let mut hunks: Vec<(String, Vec<String>)> = Vec::new();
    let mut current_header: Option<String> = None;
    let mut current_lines: Vec<String> = Vec::new();

    for line in patch.lines() {
        if line.starts_with("@@") {
            if let Some(h) = current_header.take() {
                hunks.push((h, std::mem::take(&mut current_lines)));
            }
            current_header = Some(line.to_string());
        } else if let Some(ref _h) = current_header {
            current_lines.push(line.to_string());
        }
    }
    if let Some(h) = current_header {
        hunks.push((h, current_lines));
    }

    // Opaque blob: no @@ headers found but patch has content.
    if hunks.is_empty() && !patch.trim().is_empty() {
        hunks.push((
            "@@ (opaque) @@".to_string(),
            patch.lines().map(|l| l.to_string()).collect(),
        ));
    }

    hunks
}

// ─── FileFilter ───────────────────────────────────────────────────────────────

/// Configures the Stage A file-level noise filter.
///
/// Why: all filter knobs are passed explicitly (spec REV-262 "no global state").
/// What: `preserve_patterns` are additional regex patterns that force-keep files;
/// `suppress_patterns` are additional drop patterns; `fixture_summary_min_lines`
/// controls when fixture files are summarised vs dropped.
/// Test: used directly by `FileFilter` and `DiffAnalyzer`.
#[derive(Debug, Clone)]
pub struct FilterConfig {
    /// Extra preserve patterns (beyond built-ins); regex strings.
    pub preserve_patterns: Vec<String>,
    /// Extra suppress/drop patterns; regex strings.
    pub suppress_patterns: Vec<String>,
    /// Min diff lines for a fixture/i18n file to be summarised (default 30).
    pub fixture_summary_min_lines: usize,
    /// When true, Stage B drops whitespace-only hunks.
    pub ignore_whitespace: bool,
    /// When true, Stage B drops import-only hunks.
    pub ignore_imports: bool,
    /// When true, Stage B drops comment-only hunks.
    pub ignore_comments: bool,
    /// When true, Stage C (LLM classifier) is disabled.
    pub disable_classifier: bool,
    /// Mechanical confidence threshold above which Stage C drops a hunk.
    pub drop_confidence_threshold: f32,
    /// Haiku batch size for Stage C.
    pub classifier_batch_size: usize,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            preserve_patterns: Vec::new(),
            suppress_patterns: Vec::new(),
            fixture_summary_min_lines: 30,
            ignore_whitespace: true,
            ignore_imports: true,
            ignore_comments: true,
            disable_classifier: true,
            drop_confidence_threshold: 0.7,
            classifier_batch_size: 10,
        }
    }
}

/// Stage A file-level noise filter (spec REV-201–202).
///
/// Why: fast, deterministic pre-filter that eliminates the most egregious
/// noise classes before hunk-level analysis.  Zero external dependencies.
/// What: `apply` iterates parsed diff files and classifies each; returns
/// surviving `FilteredFile` stubs (hunk content is raw text for Stage B to
/// re-classify) and a list of `DroppedFile` records.
/// Test: `lockfile_is_dropped`, `security_file_is_preserved`,
/// `fixture_file_is_summarised`, `plain_rust_file_is_kept`.
pub struct FileFilter {
    config: FilterConfig,
    user_preserve: Vec<regex::Regex>,
    user_suppress: Vec<regex::Regex>,
}

impl FileFilter {
    /// Build a `FileFilter` from the given `FilterConfig`.
    ///
    /// Why: pattern compilation is expensive; doing it once at construction
    /// avoids redundant work across many `apply` calls.
    /// What: compiles user-supplied preserve and suppress regex patterns;
    /// silently ignores patterns that fail to compile (non-fatal, logged).
    /// Test: `filefilter_user_preserve_keeps_file`.
    pub fn new(config: FilterConfig) -> Self {
        let user_preserve = config
            .preserve_patterns
            .iter()
            .filter_map(|p| regex::Regex::new(p).ok())
            .collect();
        let user_suppress = config
            .suppress_patterns
            .iter()
            .filter_map(|p| regex::Regex::new(p).ok())
            .collect();
        Self {
            config,
            user_preserve,
            user_suppress,
        }
    }

    /// Classify each file in `parsed_files` and return (kept, dropped).
    ///
    /// Why: centralises Stage A logic so tests can run it without the full
    /// pipeline; the `DiffAnalyzer` calls this once per review.
    /// What: for each `(path, status, patch)`, calls `classify` and builds the
    /// appropriate `FilteredFile` or `DroppedFile`.
    /// Test: `lockfile_is_dropped`, `plain_rust_file_is_kept`.
    pub fn apply(
        &self,
        parsed_files: &[(String, String, String)], // (path, status, patch)
    ) -> (Vec<FilteredFile>, Vec<DroppedFile>) {
        let mut kept = Vec::new();
        let mut dropped = Vec::new();

        for (path, status, patch) in parsed_files {
            let (disposition, reason) = self.classify(path, patch);
            match disposition {
                FileDisposition::Dropped => {
                    dropped.push(DroppedFile {
                        path: path.clone(),
                        reason: reason.unwrap_or_else(|| "noise".to_string()),
                    });
                }
                FileDisposition::SummaryOnly => {
                    kept.push(FilteredFile {
                        filename: path.clone(),
                        status: status.clone(),
                        disposition: FileDisposition::SummaryOnly,
                        hunks: Vec::new(),
                        dropped_hunks: Vec::new(),
                        summary_line: Some(self.summarize(path, patch)),
                    });
                }
                FileDisposition::Kept => {
                    let raw_hunks = split_patch_into_hunks(patch);
                    kept.push(FilteredFile {
                        filename: path.clone(),
                        status: status.clone(),
                        disposition: FileDisposition::Kept,
                        hunks: raw_hunks
                            .into_iter()
                            .map(|(header, lines)| FilteredHunk {
                                header,
                                lines,
                                substantive_confidence: 1.0,
                                reason_kept: "stage-a-pass".to_string(),
                            })
                            .collect(),
                        dropped_hunks: Vec::new(),
                        summary_line: None,
                    });
                }
            }
        }
        (kept, dropped)
    }

    /// Classify a single file as `Kept`, `Dropped`, or `SummaryOnly`.
    ///
    /// Why: isolated for per-file unit testing without constructing full input.
    /// What: applies rules in priority order (preserve > drop > summarize > keep).
    /// Returns `(disposition, Option<drop_reason>)`.
    /// Test: `classify_lockfile`, `classify_security_file`.
    pub fn classify(&self, path: &str, patch: &str) -> (FileDisposition, Option<String>) {
        // 1. Preserve (highest priority — overrides all drops).
        if should_preserve(path, patch) {
            return (FileDisposition::Kept, None);
        }
        for re in &self.user_preserve {
            if re.is_match(path) {
                return (FileDisposition::Kept, None);
            }
        }

        // 2. User suppress patterns (optional extra drops before built-ins).
        for re in &self.user_suppress {
            if re.is_match(path) {
                return (
                    FileDisposition::Dropped,
                    Some("suppress-pattern".to_string()),
                );
            }
        }

        // 3. Built-in drop patterns.
        if is_lockfile(path) {
            return (FileDisposition::Dropped, Some("lockfile".to_string()));
        }
        if is_snapshot(path) {
            return (FileDisposition::Dropped, Some("snapshot".to_string()));
        }
        if is_generated(path) {
            return (FileDisposition::Dropped, Some("generated".to_string()));
        }

        // 4. SummaryOnly: fixture/i18n with enough lines.
        if is_fixture(path) && count_patch_lines(patch) > self.config.fixture_summary_min_lines {
            return (FileDisposition::SummaryOnly, None);
        }

        // 5. Default: Kept.
        (FileDisposition::Kept, None)
    }

    fn summarize(&self, path: &str, patch: &str) -> String {
        let n_lines = count_patch_lines(patch);
        format!("[fixture/i18n: {n_lines} diff lines omitted — {path}]")
    }

    /// Expose the parsed config for use by the DiffAnalyzer / HunkFilter.
    pub fn config(&self) -> &FilterConfig {
        &self.config
    }
}

// ─── Unit tests (extracted to keep file under 500-line cap) ──────────────────

#[cfg(test)]
#[path = "file_filter_tests.rs"]
mod tests;
