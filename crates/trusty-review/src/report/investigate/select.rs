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
//! Path heuristics are guesses, so #6078 lets better-informed rankers override
//! them through [`RiskSignals`] — the manifest's declared `inspect_priority`
//! (trusty-audit writes it; owner ruling 2026-08-19 makes it THE interface for
//! external selection intelligence) and the trusty-analyze findings already on
//! the model.  Absent both, the ranking is byte-identical to the pre-#6078 one.
//! What: [`Budget`] holds the file/byte caps; [`select_files`] ranks the tracked
//! file list, greedily fills the budget, reads (and truncates) content, and
//! records coverage — which DD dimensions were actually reached and which were
//! not.  No LLM, no network.
//! Test: `select_tests.rs` builds a fixture repo with planted auth/store/package
//! files and asserts ranking, budget caps, truncation, and coverage.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::report::manifest::InspectionPriority;
use crate::report::metrics::AnalyzeMetrics;

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
    /// The DD dimensions this file matched by path/name heuristics, plus the
    /// one the manifest declared it as evidence for (#6082).
    pub dimensions: Vec<String>,
    /// The manifest's one-line reason this file was ranked (#6082) — the query
    /// or measurement that found it. `None` for a path-heuristic selection.
    pub selected_by: Option<String>,
}

/// One dimension's share of the examined set (#6082).
///
/// Why: the coverage section states which dimensions were reached; a reader
/// weighing the findings also needs to know how many files each got and on what
/// basis, because "covered by one file a path name matched" and "covered by six
/// files a search query found" are not the same claim.
/// What: the dimension, how many examined files carry it, and one example file
/// with the reason it was read.
/// Test: `select_tests::per_dimension_coverage_names_why_a_file_was_read`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DimensionCoverage {
    /// The DD dimension.
    pub dimension: String,
    /// Examined files carrying it.
    pub files_examined: usize,
    /// One example: `path (reason it was read)`.
    pub example: Option<String>,
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
    /// Per-dimension coverage of the examined set (#6082).
    pub per_dimension: Vec<DimensionCoverage>,
    /// Examined files the manifest itself named, with a reason (#6082). Zero
    /// means the ranking was path names and complexity alone — which is what
    /// the coverage section must SAY rather than imply.
    pub attributed_files: usize,
    /// True when the manifest asked for declared paths only (#6082), so an
    /// examined set smaller than the budget is a stated shortfall rather than
    /// room the heuristics failed to fill.
    pub attributed_only: bool,
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

/// The relevance weight a file trusty-analyze already flagged receives (#6078).
///
/// Why: every other term guesses from the path name. A `MetricFinding` naming
/// this file is a tool's direct observation that something is wrong in it, so it
/// must weigh more than a DD-dimension name match (×2). It stays below two
/// analyst-brief keyword hits (×3 each) because the analyst's stated focus is
/// the one signal that outranks a machine's.
/// What: added once per flagged file however many findings name it, so a single
/// noisy linter rule cannot bury the rest of the ranking.
const FLAGGED_WEIGHT: u32 = 4;

/// External risk evidence that steers selection — every field optional (#6078).
///
/// Why: two independent rankers know more about a repository than any path-name
/// heuristic: the manifest's declared `inspect_priority` (written by
/// trusty-audit, the stable interface per the owner ruling of 2026-08-19) and
/// the trusty-analyze findings already on the model. Passing them as one struct
/// means a third signal later changes no call site.
/// What: `priorities` is the ranked manifest list; `metrics` are the analyze
/// findings. [`RiskSignals::default`] carries neither, which reproduces the
/// pre-#6078 ranking exactly — that is the contract the absent-input tests pin.
/// Test: `select_tests::{absent_signals_reproduce_baseline_order,
/// manifest_priority_outranks_heuristics, analyze_flagged_file_outranks_peer}`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RiskSignals<'a> {
    /// Manifest-declared ranked paths; empty means "no external ranking".
    pub priorities: &'a [InspectionPriority],
    /// Per-file findings from trusty-analyze; `None` when `--analyze` was off.
    pub metrics: Option<&'a AnalyzeMetrics>,
    /// Select only `priorities`, never padding the budget with heuristic-scored
    /// files (#6082). See `Manifest`'s `attributed_only` for the rationale.
    pub attributed_only: bool,
}

/// Compute a file's relevance score against instruction keywords, DD dimensions,
/// and whether a code-analysis tool already flagged it.
///
/// Higher is more relevant: instruction-keyword hits weigh most (×3), each DD
/// dimension match ×2, a tool-flagged file [`FLAGGED_WEIGHT`], and a recognised
/// source file gets +1 so code outranks data when nothing else separates them.
fn score(path: &str, keywords: &[String], dims: &[String], flagged: bool) -> u32 {
    let p = path.to_ascii_lowercase();
    let instr_hits = keywords.iter().filter(|k| p.contains(k.as_str())).count() as u32;
    let dim_hits = dims.len() as u32;
    let code = u32::from(is_code_file(path));
    3 * instr_hits + 2 * dim_hits + FLAGGED_WEIGHT * u32::from(flagged) + code
}

/// Normalise a path-ish string into the key both signal matchers compare on.
///
/// Why: a `MetricFinding.component` may be absolute or repo-relative and may
/// carry a `:<line>` suffix (that is the shape `apply_investigation` writes),
/// and a manifest author may use either separator. One normal form makes both
/// comparisons a plain string test.
/// What: backslashes to `/`, lower-cased, and a trailing `:<digits>` dropped.
/// Lower-casing only ever ranks a file higher, never excludes one.
/// Test: `select_tests::analyze_component_with_line_suffix_matches`.
fn path_key(raw: &str) -> String {
    let c = raw.replace('\\', "/").to_ascii_lowercase();
    match c.rsplit_once(':') {
        Some((file, line)) if !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit()) => {
            file.to_string()
        }
        _ => c,
    }
}

/// True when `key` names the same file as the already-normalised `path`.
///
/// Matches exactly, or when `key` is a longer path ending in `/{path}` — which
/// is how an absolute component from the analyze daemon names a repo-relative
/// file. A bare basename never matches, so one `login.rs` cannot flag every
/// other one.
fn names_same_file(path: &str, key: &str) -> bool {
    key == path
        || (key.len() > path.len()
            && key.ends_with(path)
            && key.as_bytes()[key.len() - path.len() - 1] == b'/')
}

/// The normalised paths trusty-analyze flagged, or empty when it supplied none.
fn flagged_keys(metrics: Option<&AnalyzeMetrics>) -> Vec<String> {
    let Some(m) = metrics else {
        return Vec::new();
    };
    let mut keys: Vec<String> = m
        .findings
        .iter()
        .filter(|f| !f.component.is_empty())
        .map(|f| path_key(&f.component))
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// One manifest priority, normalised for matching against the tracked list.
///
/// Why: #6082 gave a priority two more things to say — which dimension it is
/// evidence for, and which query found it — and both must survive the match, so
/// the normalised form carries them rather than only the weight.
/// Test: `select_tests::a_declared_dimension_counts_as_covered`.
struct Declared {
    key: String,
    weight: u32,
    dimension: Option<String>,
    reason: Option<String>,
}

/// The manifest priorities, normalised for matching.
fn declarations(priorities: &[InspectionPriority]) -> Vec<Declared> {
    priorities
        .iter()
        .filter(|p| !p.path.is_empty())
        .map(|p| Declared {
            key: path_key(&p.path),
            weight: p.weight,
            dimension: p.dimension.clone(),
            reason: p.reason.clone(),
        })
        .collect()
}

/// The highest-weight declaration naming `path`, when the manifest names it.
fn declaration_for<'a>(path: &str, declared: &'a [Declared]) -> Option<&'a Declared> {
    declared
        .iter()
        .filter(|d| names_same_file(path, &d.key))
        .max_by_key(|d| d.weight)
}

/// One tracked file, scored and attributed, before the budget is applied.
struct Candidate {
    weight: u32,
    score: u32,
    path: String,
    dimensions: Vec<String>,
    reason: Option<String>,
}

/// Score one tracked file and fold in what the manifest declared about it.
///
/// #6082: a declared dimension is what the file is evidence FOR, and it counts
/// toward coverage even when no path heuristic would have recognised it.
fn candidate(
    rel: &Path,
    keywords: &[String],
    flagged: &[String],
    declared: &[Declared],
) -> Candidate {
    let path = rel.to_string_lossy().replace('\\', "/");
    let key = path.to_ascii_lowercase();
    let mut dimensions = dimensions_for(&path);
    let is_flagged = flagged.iter().any(|f| names_same_file(&key, f));
    let score = score(&path, keywords, &dimensions, is_flagged);

    let declaration = declaration_for(&key, declared);
    let mut reason = None;
    if let Some(d) = declaration {
        if let Some(dim) = &d.dimension
            && !dimensions.contains(dim)
        {
            dimensions.push(dim.clone());
        }
        reason.clone_from(&d.reason);
    }
    Candidate {
        weight: declaration.map_or(0, |d| d.weight),
        score,
        path,
        dimensions,
        reason,
    }
}

/// Rank the repository's files and greedily fill the budget, capturing content
/// and coverage — the deterministic heart of the investigation pass.
///
/// Why: this is the single place that turns a raw tracked-file list into the
/// bounded, relevance-ranked, coverage-annotated set the LLM will inspect; its
/// determinism (a stable score-then-path sort) makes every report reproducible.
/// What: scores each file (instruction keywords + DD dimensions + a trusty-analyze
/// flag + code boost), sorts by manifest priority desc, then score desc, then path
/// asc, then walks the ranking reading content (per-file truncated to
/// [`MAX_FILE_BYTES`], and to the remaining byte budget) until `max_files` or
/// `max_bytes` is reached.  Records total/skipped counts, bytes sent, and the
/// covered/absent DD dimensions (tests count as covered when any test path is
/// present, even if unread).  Unreadable/binary files are skipped without
/// consuming budget.
///
/// `signals` carries the two external rankers (#6078); [`RiskSignals::default`]
/// carries neither and reproduces the pre-#6078 ranking exactly.  A manifest
/// priority is a SEPARATE, dominant sort key rather than another score term, so
/// a declared path is inspected ahead of every heuristic-only file whatever its
/// weight — the budget still bounds how many of them are actually read.
/// `signals.attributed_only` turns that dominance into a FILTER (#6082): only
/// declared paths are read, and a declared list shorter than the budget leaves
/// the remainder unspent instead of padding it with path-name guesses.
/// Test: `select_tests::{ranks_relevant_first, budget_caps_file_count,
/// budget_caps_total_bytes, coverage_reports_dimensions,
/// absent_signals_reproduce_baseline_order, manifest_priority_outranks_heuristics,
/// priorities_beyond_the_budget_truncate_in_rank_order}`.
pub fn select_files(
    root: &Path,
    files: &[PathBuf],
    instructions: Option<&str>,
    budget: Budget,
    signals: RiskSignals<'_>,
) -> Selection {
    let keywords = instruction_keywords(instructions);
    let total_files = files.len();
    // #6078: both external rankings are normalised once, not per candidate.
    let flagged = flagged_keys(signals.metrics);
    let declared = declarations(signals.priorities);

    // Score every candidate (skip obvious noise the scanner already excludes is
    // handled upstream; here we rank whatever the tracked list contains).
    let mut ranked: Vec<Candidate> = files
        .iter()
        .map(|rel| candidate(rel, &keywords, &flagged, &declared))
        .collect();
    // Deterministic: declared priority desc, then score desc, then path asc.
    ranked.sort_by(|a, b| {
        b.weight
            .cmp(&a.weight)
            .then_with(|| b.score.cmp(&a.score))
            .then_with(|| a.path.cmp(&b.path))
    });

    let mut selected: Vec<SelectedFile> = Vec::new();
    let mut bytes_sent = 0usize;
    for c in &ranked {
        if selected.len() >= budget.max_files {
            break;
        }
        // #6082: a weight of 0 means no manifest entry named this file, so it
        // reached the ranking on its path name alone. Under `attributed_only`
        // the remaining budget goes unspent rather than to a guess.
        if signals.attributed_only && c.weight == 0 {
            continue;
        }
        let remaining = budget.max_bytes.saturating_sub(bytes_sent);
        if remaining == 0 {
            break;
        }
        let Some((content, truncated)) = read_capped(&root.join(&c.path), remaining) else {
            continue; // unreadable / binary / empty — no budget consumed
        };
        bytes_sent += content.len();
        selected.push(SelectedFile {
            path: c.path.clone(),
            content,
            truncated,
            dimensions: c.dimensions.clone(),
            selected_by: c.reason.clone(),
        });
    }

    let (dimensions_covered, dimensions_absent) = coverage_split(&selected, files);
    let skipped = total_files.saturating_sub(selected.len());
    Selection {
        per_dimension: per_dimension(&selected, &dimensions_covered),
        attributed_files: selected.iter().filter(|f| f.selected_by.is_some()).count(),
        attributed_only: signals.attributed_only,
        files: selected,
        total_files,
        skipped,
        bytes_sent,
        dimensions_covered,
        dimensions_absent,
    }
}

/// Per-dimension coverage: how many examined files each covered dimension got,
/// and why the first of them was read (#6082).
///
/// Why: "dimensions covered: error handling, dependencies" says a dimension was
/// reached without saying how thinly, or on what basis. A reader weighing a DD
/// finding needs both, and the reason is the part no path-name ranking could
/// ever have supplied.
/// What: one row per covered dimension, in the canonical order, counting the
/// examined files that carry it. `example` names the first such file and its
/// selection reason — the manifest's when it declared one, the path-name
/// heuristic otherwise. "test coverage" can be covered by an unexamined test
/// directory, so a row with zero files and no example is a real state.
/// Test: `select_tests::per_dimension_coverage_names_why_a_file_was_read`.
fn per_dimension(selected: &[SelectedFile], covered: &[String]) -> Vec<DimensionCoverage> {
    covered
        .iter()
        .map(|dimension| {
            let hits: Vec<&SelectedFile> = selected
                .iter()
                .filter(|f| f.dimensions.iter().any(|d| d == dimension))
                .collect();
            DimensionCoverage {
                dimension: dimension.clone(),
                files_examined: hits.len(),
                example: hits.first().map(|f| {
                    let why = f
                        .selected_by
                        .clone()
                        .unwrap_or_else(|| "path-name heuristic".to_string());
                    format!("{} ({why})", f.path)
                }),
            }
        })
        .collect()
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
