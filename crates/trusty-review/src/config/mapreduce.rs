//! Map-reduce review configuration and mode selector (Phase 1, #690 / #680).
//!
//! Why: the unified-diff review path silently drops files on large PRs because
//! `render_for_prompt` breaks at `MAX_DIFF_CHARS` (160 K chars, ~40 K tokens).
//! A per-file map-reduce path reviews every file independently; this module
//! defines the configuration knobs that control it and the pure selector
//! function that decides which path to take. Phase 1 introduces the config +
//! selector only — nothing in the live review runner consumes the selector yet
//! (wired in Phase 5, #680).
//!
//! What: `MapMode` enum, `MapReduceConfig` struct (9 env-var knobs), and
//! `select_review_mode` pure function.  No I/O; fully unit-testable.
//!
//! Test: `mapreduce_config_defaults`, `mapreduce_env_overrides`,
//! `mapreduce_malformed_env_fallback`, `select_review_mode_*` decision table.

use tracing::warn;

use crate::config::constants::MAX_DIFF_CHARS;

// ─── Env-var name constants ───────────────────────────────────────────────────

const ENV_MAP_MODE: &str = "TRUSTY_REVIEW_MAP_MODE";
const ENV_FILE_THRESHOLD: &str = "TRUSTY_REVIEW_MAP_FILE_THRESHOLD";
const ENV_PER_FILE_CHARS: &str = "TRUSTY_REVIEW_MAP_PER_FILE_CHARS";
const ENV_CONCURRENCY: &str = "TRUSTY_REVIEW_MAP_CONCURRENCY";
const ENV_TOTAL_CHAR_BUDGET: &str = "TRUSTY_REVIEW_MAP_TOTAL_CHAR_BUDGET";
const ENV_MAX_CALLS: &str = "TRUSTY_REVIEW_MAP_MAX_CALLS";
const ENV_MAX_FINDINGS: &str = "TRUSTY_REVIEW_MAP_MAX_FINDINGS";
const ENV_SYNTHESIS: &str = "TRUSTY_REVIEW_MAP_SYNTHESIS";
const ENV_PER_FILE_SEARCH: &str = "TRUSTY_REVIEW_MAP_PER_FILE_SEARCH";

// ─── Config defaults ──────────────────────────────────────────────────────────

/// Default file-count threshold above which `auto` mode selects map-reduce.
///
/// Why: PRs with more than this many changed files are expensive to review in a
/// single unified call even before the char budget is exhausted; map-reduce
/// distributes the cognitive load per-file.
/// What: matches `TRUSTY_REVIEW_MAP_FILE_THRESHOLD` default documented in #680.
pub const DEFAULT_MAP_FILE_THRESHOLD: usize = 12;

/// Default per-unit char budget for a single file's map call.
///
/// Why: each map call gets its own context window; capping per-file input keeps
/// individual map calls well within the model's context limit.
/// What: 120 000 chars ≈ 30 K tokens — comfortably below the 200 K-token window.
pub const DEFAULT_MAP_PER_FILE_CHARS: usize = 120_000;

/// Default concurrency for parallel map calls.
///
/// Why: bounded fan-out prevents rate-limit hammering; 4 mirrors the verifier's
/// `buffer_unordered(4)` pattern in `verify.rs:147`.
/// What: `TRUSTY_REVIEW_MAP_CONCURRENCY` default.
pub const DEFAULT_MAP_CONCURRENCY: usize = 4;

/// Default total character budget summed across all map inputs.
///
/// Why: a global budget caps the worst-case cost of a map-reduce review
/// independent of per-file caps.  1 M chars ≈ 250 K tokens.
/// What: `TRUSTY_REVIEW_MAP_TOTAL_CHAR_BUDGET` default.
pub const DEFAULT_MAP_TOTAL_CHAR_BUDGET: usize = 1_000_000;

/// Default hard ceiling on map LLM calls per review.
///
/// Why: prevents runaway cost on a PR with hundreds of files.
/// What: `TRUSTY_REVIEW_MAP_MAX_CALLS` default.
pub const DEFAULT_MAP_MAX_CALLS: usize = 40;

/// Default cap on findings surfaced in the reduce output.
///
/// Why: the PR comment must remain actionable; too many findings overwhelm
/// the author and bury the critical ones.
/// What: `TRUSTY_REVIEW_MAP_MAX_FINDINGS` default.
pub const DEFAULT_MAP_MAX_FINDINGS: usize = 50;

// ─── MapMode enum ─────────────────────────────────────────────────────────────

/// Controls when map-reduce review is used instead of unified-diff review.
///
/// Why: operators need an escape hatch (`never` = today's behaviour exactly)
/// and a forced path (`always`) for testing, while `auto` provides intelligent
/// switching based on diff size + file count.
/// What: parsed from `TRUSTY_REVIEW_MAP_MODE` env var (case-insensitive).
/// `auto` is the default; malformed values fall back to `auto`.
/// Test: `map_mode_parse_auto`, `map_mode_parse_always`, `map_mode_parse_never`,
/// `map_mode_parse_garbage_falls_back_to_auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MapMode {
    /// Automatically choose map-reduce when the unified render would truncate
    /// (diff chars > `MAX_DIFF_CHARS`) OR file count > `file_threshold`.
    #[default]
    Auto,
    /// Always use map-reduce, regardless of diff size or file count.
    Always,
    /// Never use map-reduce; always use the unified path (current behaviour).
    Never,
}

impl MapMode {
    /// Parse from a string (case-insensitive); returns `None` for unrecognised values.
    ///
    /// Why: centralises the string→enum mapping; callers handle the `None` case
    /// by falling back to default and logging a warning.
    /// What: recognises `"auto"`, `"always"`, `"never"` (case-insensitive).
    /// Test: covered by `map_mode_parse_*` unit tests.
    fn from_str_opt(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "auto" => Some(MapMode::Auto),
            "always" => Some(MapMode::Always),
            "never" => Some(MapMode::Never),
            _ => None,
        }
    }

    /// Load from `TRUSTY_REVIEW_MAP_MODE`; fall back to `Auto` on unset/malformed.
    ///
    /// Why: env-var loading is centralised here so `MapReduceConfig::from_env`
    /// does not scatter the parse logic.
    /// What: reads the env var, parses, warns on unrecognised values, falls back
    /// to `Auto`.
    /// Test: `map_mode_env_auto`, `map_mode_env_always`, `map_mode_env_garbage`.
    fn from_env() -> Self {
        match std::env::var(ENV_MAP_MODE) {
            Err(_) => MapMode::Auto,
            Ok(raw) => {
                let v = raw.trim().to_string();
                if v.is_empty() {
                    return MapMode::Auto;
                }
                MapMode::from_str_opt(&v).unwrap_or_else(|| {
                    warn!("unrecognised {ENV_MAP_MODE}={v:?} — falling back to 'auto'");
                    MapMode::Auto
                })
            }
        }
    }
}

impl std::fmt::Display for MapMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MapMode::Auto => write!(f, "auto"),
            MapMode::Always => write!(f, "always"),
            MapMode::Never => write!(f, "never"),
        }
    }
}

// ─── ReviewPath (selector output) ────────────────────────────────────────────

/// Decision output of `select_review_mode`.
///
/// Why: a typed enum prevents the caller from comparing strings or booleans;
/// pattern-matching on this enum will produce a compile error if new variants
/// are added without handling them.
/// What: `Unified` = current path; `MapReduce` = per-file fan-out path.
/// Test: returned by `select_review_mode`; asserted in decision-table tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewPath {
    /// Use the existing unified-diff single-LLM-call path.
    Unified,
    /// Use the per-file map-reduce path (Phase 2-5 of #680).
    MapReduce,
}

// ─── MapReduceConfig ──────────────────────────────────────────────────────────

/// Configuration for the map-reduce review path (Phase 1, #690).
///
/// Why: aggregates all 9 `TRUSTY_REVIEW_MAP_*` knobs into a single owned struct
/// so the pipeline has one place to read map-reduce configuration, matching the
/// pattern used by `VerificationConfig` (#583).
/// What: loaded via `MapReduceConfig::from_env`; all fields have documented
/// defaults; malformed env values fall back to default (never panic).
/// Test: `mapreduce_config_defaults`, `mapreduce_env_overrides`,
/// `mapreduce_malformed_env_fallback`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapReduceConfig {
    /// When to use map-reduce vs. unified (`TRUSTY_REVIEW_MAP_MODE`, default `auto`).
    pub mode: MapMode,

    /// File-count trigger for `auto` mode (`TRUSTY_REVIEW_MAP_FILE_THRESHOLD`,
    /// default `12`).  When the PR touches more than this many files and mode
    /// is `auto`, map-reduce is selected even if the diff fits within
    /// `MAX_DIFF_CHARS`.
    pub file_threshold: usize,

    /// Maximum chars per map unit / file (`TRUSTY_REVIEW_MAP_PER_FILE_CHARS`,
    /// default `120_000`).
    pub per_file_chars: usize,

    /// Maximum parallel map LLM calls (`TRUSTY_REVIEW_MAP_CONCURRENCY`,
    /// default `4`).
    pub concurrency: usize,

    /// Total char budget across all map inputs (`TRUSTY_REVIEW_MAP_TOTAL_CHAR_BUDGET`,
    /// default `1_000_000`).
    pub total_char_budget: usize,

    /// Hard ceiling on map LLM calls per review (`TRUSTY_REVIEW_MAP_MAX_CALLS`,
    /// default `40`).
    pub max_calls: usize,

    /// Maximum findings surfaced in reduce output (`TRUSTY_REVIEW_MAP_MAX_FINDINGS`,
    /// default `50`).
    pub max_findings: usize,

    /// Whether the optional reduce LLM synthesis pass is enabled
    /// (`TRUSTY_REVIEW_MAP_SYNTHESIS`, default `true`).
    pub synthesis: bool,

    /// Whether focused per-file search context is fetched for each map unit
    /// (`TRUSTY_REVIEW_MAP_PER_FILE_SEARCH`, default `false`).
    pub per_file_search: bool,
}

impl Default for MapReduceConfig {
    /// Why: provides the documented defaults from #680 so callers can construct
    /// a config without reading env vars (useful in tests and library consumers).
    /// What: all fields set to their documented defaults.
    /// Test: `mapreduce_config_defaults`.
    fn default() -> Self {
        Self {
            mode: MapMode::Auto,
            file_threshold: DEFAULT_MAP_FILE_THRESHOLD,
            per_file_chars: DEFAULT_MAP_PER_FILE_CHARS,
            concurrency: DEFAULT_MAP_CONCURRENCY,
            total_char_budget: DEFAULT_MAP_TOTAL_CHAR_BUDGET,
            max_calls: DEFAULT_MAP_MAX_CALLS,
            max_findings: DEFAULT_MAP_MAX_FINDINGS,
            synthesis: true,
            per_file_search: false,
        }
    }
}

impl MapReduceConfig {
    /// Load configuration from environment variables, falling back to defaults.
    ///
    /// Why: single, authoritative loading path so callers do not scatter env
    /// lookups; matches the `VerificationConfig::from_env_and_file` pattern.
    /// What: reads each `TRUSTY_REVIEW_MAP_*` var; malformed/empty values fall
    /// back to the documented default with a tracing warning (never panic).
    /// Test: `mapreduce_env_overrides`, `mapreduce_malformed_env_fallback`.
    pub fn from_env() -> Self {
        let mode = MapMode::from_env();
        let file_threshold = parse_usize_env(ENV_FILE_THRESHOLD, DEFAULT_MAP_FILE_THRESHOLD);
        let per_file_chars = parse_usize_env(ENV_PER_FILE_CHARS, DEFAULT_MAP_PER_FILE_CHARS);
        let concurrency = parse_usize_env(ENV_CONCURRENCY, DEFAULT_MAP_CONCURRENCY);
        let total_char_budget =
            parse_usize_env(ENV_TOTAL_CHAR_BUDGET, DEFAULT_MAP_TOTAL_CHAR_BUDGET);
        let max_calls = parse_usize_env(ENV_MAX_CALLS, DEFAULT_MAP_MAX_CALLS);
        let max_findings = parse_usize_env(ENV_MAX_FINDINGS, DEFAULT_MAP_MAX_FINDINGS);
        let synthesis = parse_bool_env_default(ENV_SYNTHESIS, true);
        let per_file_search = parse_bool_env_default(ENV_PER_FILE_SEARCH, false);

        Self {
            mode,
            file_threshold,
            per_file_chars,
            concurrency,
            total_char_budget,
            max_calls,
            max_findings,
            synthesis,
            per_file_search,
        }
    }
}

// ─── Mode selector ────────────────────────────────────────────────────────────

/// Diff statistics passed to `select_review_mode`.
///
/// Why: bundles the two inputs the selector needs so the signature stays clean
/// and the decision table in tests reads clearly.
/// What: `diff_chars` is the length of the rendered unified diff (before any
/// truncation); `file_count` is the number of surviving files in `FilteredDiff`.
/// Test: constructed inline in `select_review_mode_*` unit tests.
#[derive(Debug, Clone, Copy)]
pub struct DiffStats {
    /// Character length of the rendered unified diff (before truncation).
    ///
    /// The selector compares this against `MAX_DIFF_CHARS` to determine whether
    /// the unified path would truncate.
    pub diff_chars: usize,
    /// Number of files that survived the DiffAnalyzer's Stage A filter.
    ///
    /// Corresponds to `FilteredDiff::files.len()`.
    pub file_count: usize,
}

/// Select the review path (unified vs. map-reduce) from diff statistics + config.
///
/// Why: centralises the trigger heuristic documented in #680 so it is tested
/// independently of the runner.  Phase 1 only; nothing in the live runner
/// consumes the return value yet (wired in Phase 5).
/// What: implements the decision table:
///   - `Never`  → `Unified` (always, regardless of diff stats)
///   - `Always` → `MapReduce` (always, regardless of diff stats)
///   - `Auto`   → `MapReduce` if `diff_chars > MAX_DIFF_CHARS`
///     OR `file_count > config.file_threshold`
///   - `Auto`   → `Unified` otherwise
///
/// The "would truncate" predicate uses `diff_chars > MAX_DIFF_CHARS` (strict
/// greater-than) because `render_for_prompt` and `truncate_diff` both use
/// `<= MAX_DIFF_CHARS` as the "fits" condition (see `diff.rs:93`).
///
/// Test: `select_never_always_unified`, `select_always_forces_mapreduce`,
/// `select_auto_small_diff_is_unified`, `select_auto_truncates_triggers_mapreduce`,
/// `select_auto_many_files_triggers_mapreduce`,
/// `select_auto_boundary_at_exactly_max_chars`,
/// `select_auto_boundary_at_exactly_threshold`.
pub fn select_review_mode(stats: DiffStats, config: &MapReduceConfig) -> ReviewPath {
    match config.mode {
        MapMode::Never => ReviewPath::Unified,
        MapMode::Always => ReviewPath::MapReduce,
        MapMode::Auto => {
            let would_truncate = stats.diff_chars > MAX_DIFF_CHARS;
            let too_many_files = stats.file_count > config.file_threshold;
            if would_truncate || too_many_files {
                ReviewPath::MapReduce
            } else {
                ReviewPath::Unified
            }
        }
    }
}

// ─── Private helpers ──────────────────────────────────────────────────────────

/// Parse a `usize` env var, falling back to `default` on unset/malformed.
///
/// Why: all numeric knobs need the same parse-or-warn-and-fallback pattern;
/// extracting it here keeps `MapReduceConfig::from_env` readable.
/// What: reads `var`, trims whitespace, parses as `usize`.  Empty or unset
/// returns `default` silently; unparseable returns `default` with a warning.
/// Test: covered indirectly by `mapreduce_malformed_env_fallback`.
fn parse_usize_env(var: &str, default: usize) -> usize {
    match std::env::var(var) {
        Err(_) => default,
        Ok(raw) => {
            let v = raw.trim().to_string();
            if v.is_empty() {
                return default;
            }
            v.parse::<usize>().unwrap_or_else(|_| {
                warn!("unrecognised {var}={v:?} (expected usize) — using default {default}");
                default
            })
        }
    }
}

/// Parse a boolean env var with default, using lenient truthiness rules.
///
/// Why: boolean knobs (`synthesis`, `per_file_search`) need the same
/// true/false/1/0/yes/no/on/off parsing as `verification.rs` with an explicit
/// default when unset.
/// What: `Some(false)` for false/0/no/off, `Some(true)` for true/1/yes/on,
/// `None` (→ default) for unset/empty, `None` (with warning → default) for
/// unrecognised.
/// Test: covered by `mapreduce_synthesis_env_toggle`.
fn parse_bool_env_default(var: &str, default: bool) -> bool {
    let raw = match std::env::var(var) {
        Err(_) => return default,
        Ok(r) => r,
    };
    let v = raw.trim().to_lowercase();
    if v.is_empty() {
        return default;
    }
    match v.as_str() {
        "false" | "0" | "no" | "off" => false,
        "true" | "1" | "yes" | "on" => true,
        other => {
            warn!("unrecognised {var}={other:?} (expected bool) — using default {default}");
            default
        }
    }
}

// ─── Unit tests ───────────────────────────────────────────────────────────────
// Why: tests are kept in a separate file to honour the 500-line cap on this
// file (CLAUDE.md §"500-line file size hard cap").
// What: `mapreduce_tests.rs` contains all unit tests for this module.
// Test: run `cargo test -p trusty-review -- config::mapreduce` to execute.

#[cfg(test)]
#[path = "mapreduce_tests.rs"]
mod tests;
