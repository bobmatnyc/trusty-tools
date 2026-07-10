//! Deterministic file selection + relevance ranking for the investigation pass
//! (wave 3, #2357).
//!
//! Why: an LLM cannot read a whole repository, and sending files blindly would
//! blow the token budget and bury the signal.  The investigation must choose,
//! deterministically and within hard caps, the files most likely to carry
//! due-diligence evidence — steered by the analyst brief and by standard DD
//! dimensions (auth/secrets, dependencies, state, error handling, scaling,
//! tests) via path/name heuristics.  Determinism (a stable sort) means the same
//! repo always yields the same selection, so a report is reproducible.
//! What: [`Budget`] holds the file/byte caps; [`select_files`] ranks the tracked
//! file list, greedily fills the budget, reads (and truncates) content, and
//! records coverage — which DD dimensions were actually reached and which were
//! not.  No LLM, no network.
//! Test: `select_tests.rs` builds a fixture repo with planted auth/store/package
//! files and asserts ranking, budget caps, truncation, and coverage.

use std::path::{Path, PathBuf};

use serde::Serialize;

/// The per-file content ceiling before truncation (~24 KiB).
///
/// Why: a single vendored or generated file must not consume the whole byte
/// budget; 24 KiB is enough to carry the evidence-bearing head of any
/// hand-authored source file.
/// What: individual files longer than this are truncated with a visible marker.
const MAX_FILE_BYTES: usize = 24 * 1024;

/// The marker appended to a file whose content was truncated to the cap.
pub const TRUNCATION_MARKER: &str = "\n… [content truncated for investigation budget]\n";

/// The default maximum number of files selected for one investigation run.
pub const DEFAULT_MAX_FILES: usize = 40;
/// The default maximum total content bytes sent in one investigation run.
pub const DEFAULT_MAX_BYTES: usize = 400 * 1024;

/// Hard caps on how much of a repository one investigation run may read.
///
/// Why: budgets keep token spend and latency bounded and make coverage honest —
/// when a repo exceeds the cap the report says so rather than pretending the
/// whole codebase was inspected.
/// What: `max_files` caps the count; `max_bytes` caps the summed content sent.
/// Both are configurable (manifest keys / CLI overrides) with sane defaults.
/// Test: `select_tests::budget_caps_file_count`, `budget_caps_total_bytes`.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Budget {
    /// Maximum number of files to select.
    pub max_files: usize,
    /// Maximum total content bytes across all selected files.
    pub max_bytes: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            max_files: DEFAULT_MAX_FILES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// One file chosen for investigation, with its (possibly truncated) content.
///
/// Why: the analyze step sends the path + content verbatim and the verify step
/// substring-matches evidence against the same content, so both must read from
/// one authoritative copy captured at selection time.
/// What: `path` is repo-relative; `content` is the UTF-8 text actually sent
/// (truncated with [`TRUNCATION_MARKER`] when over the per-file cap); `truncated`
/// records whether that happened; `dimensions` are the DD dimensions it matched.
/// Test: `select_tests::truncates_oversize_file`.
#[derive(Debug, Clone, Serialize)]
pub struct SelectedFile {
    /// Repository-relative path.
    pub path: String,
    /// The UTF-8 content sent to the LLM (possibly truncated).
    pub content: String,
    /// True when the content was truncated to the per-file cap.
    pub truncated: bool,
    /// The DD dimensions this file matched by path/name heuristics.
    pub dimensions: Vec<String>,
}

/// The deterministic outcome of file selection, including coverage data.
///
/// Why: coverage honesty (#2357) requires the report to state exactly what was
/// examined vs skipped and which dimensions were reached — this struct is the
/// single record of that.
/// What: the chosen `files`, the repo's `total_files`, how many were `skipped`
/// (not selected), the `bytes_sent`, and the dimension coverage split.
/// Test: `select_tests::coverage_reports_dimensions`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Selection {
    /// The files chosen for investigation, in ranked order.
    pub files: Vec<SelectedFile>,
    /// Total tracked files in the repository (the denominator).
    pub total_files: usize,
    /// Files not selected (budget exhausted or ranked out).
    pub skipped: usize,
    /// Total content bytes across `files`.
    pub bytes_sent: usize,
    /// DD dimensions at least one selected file (or a present test dir) covers.
    pub dimensions_covered: Vec<String>,
    /// DD dimensions no selected file reached.
    pub dimensions_absent: Vec<String>,
}

impl Selection {
    /// True when no files were selected (empty/unreadable repo, or all skipped).
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// The canonical DD dimension display names, in report order.
///
/// Why: the coverage section and the prompt checklist enumerate a fixed set so a
/// reader always sees every dimension marked covered or not.
/// What: the six standard technical-DD dimensions.
/// Test: `select_tests::coverage_reports_dimensions`.
pub const DIMENSIONS: &[&str] = &[
    "authentication & secrets",
    "dependencies",
    "state management",
    "error handling",
    "scalability",
    "test coverage",
];

/// The DD dimensions a file matches by path/name heuristics (excluding tests,
/// which are presence-only and handled separately).
///
/// Why: steering selection toward evidence-bearing files needs a cheap,
/// deterministic classifier that needs no file contents — the path alone.
/// What: lower-cases the path and matches the documented substring heuristics per
/// dimension; returns every dimension the path satisfies.
/// Test: `select_tests::dimension_heuristics_classify`.
pub fn dimensions_for(path: &str) -> Vec<String> {
    let p = path.to_ascii_lowercase();
    let mut out = Vec::new();
    let base = p.rsplit('/').next().unwrap_or(&p);
    let segs: Vec<&str> = p.split('/').collect();

    // auth / secrets: auth*, *token*, *secret*, config, .env.example, middleware
    // (matched on any path segment so an `auth/` DIRECTORY counts, not just files)
    if segs.iter().any(|s| s.starts_with("auth"))
        || p.contains("token")
        || p.contains("secret")
        || p.contains("passwd")
        || p.contains("password")
        || p.contains("credential")
        || segs.iter().any(|s| s.contains("config"))
        || base == ".env.example"
        || base.starts_with(".env")
        || p.contains("middleware")
    {
        out.push(DIMENSIONS[0].to_string());
    }
    // dependencies: package/lockfiles, Cargo.toml/lock, pyproject, go.mod/sum
    if matches!(
        base,
        "package.json"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "cargo.toml"
            | "cargo.lock"
            | "pyproject.toml"
            | "poetry.lock"
            | "requirements.txt"
            | "go.mod"
            | "go.sum"
    ) {
        out.push(DIMENSIONS[1].to_string());
    }
    // state management: store*, */state/*, reducer*, context*, atoms*
    // (segment-based so a `store/` directory or a `user_store.ts` file counts)
    if segs
        .iter()
        .any(|s| s.starts_with("store") || s.starts_with("reducer") || s.starts_with("atom"))
        || p.contains("/state/")
        || p.contains("_store")
        || base.starts_with("context")
    {
        out.push(DIMENSIONS[2].to_string());
    }
    // error handling: error*, exception*
    if base.starts_with("error") || base.starts_with("exception") || base.contains("_error") {
        out.push(DIMENSIONS[3].to_string());
    }
    // scalability: queue*, cache*, db/pool config
    if base.starts_with("queue")
        || base.starts_with("cache")
        || p.contains("worker")
        || p.contains("pool")
        || p.contains("/db/")
        || base.contains("database")
    {
        out.push(DIMENSIONS[4].to_string());
    }
    out
}

/// True when a path looks like a test file/dir (presence-only coverage signal).
fn is_test_path(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.contains("/test/")
        || p.contains("/tests/")
        || p.contains("/__tests__/")
        || p.contains("/spec/")
        || p.ends_with("_test.rs")
        || p.ends_with("_tests.rs")
        || p.ends_with(".test.ts")
        || p.ends_with(".test.js")
        || p.ends_with(".spec.ts")
        || p.ends_with("_test.go")
        || p.ends_with("_test.py")
        || p.starts_with("test_")
}

/// True when the path has a recognised source-code extension (a relevance boost
/// so code outranks data/asset files when nothing else distinguishes them).
fn is_code_file(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "rs" | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "py"
            | "go"
            | "java"
            | "kt"
            | "rb"
            | "php"
            | "c"
            | "h"
            | "cc"
            | "cpp"
            | "cs"
            | "swift"
            | "scala"
            | "ex"
            | "exs"
            | "svelte"
            | "vue"
    )
}

/// Extract distinct lower-cased keyword tokens (length ≥ 4) from the analyst
/// brief, capped so a long brief cannot dominate ranking.
///
/// Why: the analyst focus areas should pull matching files up the ranking; long
/// stop-words and one/two-letter tokens add noise, so a length floor + a cap keep
/// the signal tight and deterministic.
/// What: splits on non-alphanumeric, lower-cases, keeps tokens ≥ 4 chars, dedupes
/// preserving first-seen order, and truncates to 40 tokens.
/// Test: `select_tests::instruction_keywords_extracted`.
pub fn instruction_keywords(instructions: Option<&str>) -> Vec<String> {
    let Some(text) = instructions else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for raw in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if raw.len() < 4 {
            continue;
        }
        let tok = raw.to_ascii_lowercase();
        if !out.contains(&tok) {
            out.push(tok);
        }
        if out.len() >= 40 {
            break;
        }
    }
    out
}

/// Compute a file's relevance score against instruction keywords + DD dimensions.
///
/// Higher is more relevant: instruction-keyword hits weigh most (×3), each DD
/// dimension match ×2, and a recognised source file gets +1 so code outranks
/// data when nothing else separates them.
fn score(path: &str, keywords: &[String], dims: &[String]) -> u32 {
    let p = path.to_ascii_lowercase();
    let instr_hits = keywords.iter().filter(|k| p.contains(k.as_str())).count() as u32;
    let dim_hits = dims.len() as u32;
    let code = u32::from(is_code_file(path));
    3 * instr_hits + 2 * dim_hits + code
}

/// Rank the repository's files and greedily fill the budget, capturing content
/// and coverage — the deterministic heart of the investigation pass.
///
/// Why: this is the single place that turns a raw tracked-file list into the
/// bounded, relevance-ranked, coverage-annotated set the LLM will inspect; its
/// determinism (a stable score-then-path sort) makes every report reproducible.
/// What: scores each file (instruction keywords + DD dimensions + code boost),
/// sorts by score desc then path asc, then walks the ranking reading content
/// (per-file truncated to [`MAX_FILE_BYTES`], and to the remaining byte budget)
/// until `max_files` or `max_bytes` is reached.  Records total/skipped counts,
/// bytes sent, and the covered/absent DD dimensions (tests count as covered when
/// any test path is present, even if unread).  Unreadable/binary files are
/// skipped without consuming budget.
/// Test: `select_tests::{ranks_relevant_first, budget_caps_file_count,
/// budget_caps_total_bytes, coverage_reports_dimensions}`.
pub fn select_files(
    root: &Path,
    files: &[PathBuf],
    instructions: Option<&str>,
    budget: Budget,
) -> Selection {
    let keywords = instruction_keywords(instructions);
    let total_files = files.len();

    // Score every candidate (skip obvious noise the scanner already excludes is
    // handled upstream; here we rank whatever the tracked list contains).
    let mut ranked: Vec<(u32, String, Vec<String>)> = files
        .iter()
        .map(|rel| {
            let path = rel.to_string_lossy().replace('\\', "/");
            let dims = dimensions_for(&path);
            let s = score(&path, &keywords, &dims);
            (s, path, dims)
        })
        .collect();
    // Deterministic: score desc, then path asc.
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

    let mut selected: Vec<SelectedFile> = Vec::new();
    let mut bytes_sent = 0usize;
    for (_, path, dims) in &ranked {
        if selected.len() >= budget.max_files {
            break;
        }
        let remaining = budget.max_bytes.saturating_sub(bytes_sent);
        if remaining == 0 {
            break;
        }
        let Some((content, truncated)) = read_capped(&root.join(path), remaining) else {
            continue; // unreadable / binary / empty — no budget consumed
        };
        bytes_sent += content.len();
        selected.push(SelectedFile {
            path: path.clone(),
            content,
            truncated,
            dimensions: dims.clone(),
        });
    }

    let (dimensions_covered, dimensions_absent) = coverage_split(&selected, files);
    let skipped = total_files.saturating_sub(selected.len());
    Selection {
        files: selected,
        total_files,
        skipped,
        bytes_sent,
        dimensions_covered,
        dimensions_absent,
    }
}

/// Read a file as UTF-8, truncating to `min(MAX_FILE_BYTES, remaining)` on a char
/// boundary; returns `(content, truncated)` or `None` for unreadable/binary/empty.
fn read_capped(abs: &Path, remaining: usize) -> Option<(String, bool)> {
    let text = std::fs::read_to_string(abs).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    let cap = MAX_FILE_BYTES.min(remaining);
    if text.len() <= cap {
        return Some((text, false));
    }
    // Reserve room for the marker so the returned content never exceeds `cap`
    // (the byte budget bounds what is actually sent, marker included).
    let room = cap.saturating_sub(TRUNCATION_MARKER.len());
    if room == 0 {
        return None; // not enough remaining budget to send any content
    }
    let mut end = room;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    if end == 0 {
        return None;
    }
    let mut out = text[..end].to_string();
    out.push_str(TRUNCATION_MARKER);
    Some((out, true))
}

/// Split the canonical dimension list into covered vs absent for this selection.
///
/// A dimension is covered when a selected file matched it; "test coverage" is
/// additionally covered when ANY tracked path (selected or not) looks like a test
/// file, because test presence is a repo-level fact independent of the byte
/// budget.
fn coverage_split(selected: &[SelectedFile], all_files: &[PathBuf]) -> (Vec<String>, Vec<String>) {
    let has_tests = all_files
        .iter()
        .any(|f| is_test_path(&f.to_string_lossy().replace('\\', "/")));

    let mut covered = Vec::new();
    let mut absent = Vec::new();
    for &dim in DIMENSIONS {
        let hit = if dim == "test coverage" {
            has_tests
        } else {
            selected
                .iter()
                .any(|f| f.dimensions.iter().any(|d| d == dim))
        };
        if hit {
            covered.push(dim.to_string());
        } else {
            absent.push(dim.to_string());
        }
    }
    (covered, absent)
}

#[cfg(test)]
#[path = "select_tests.rs"]
mod tests;
