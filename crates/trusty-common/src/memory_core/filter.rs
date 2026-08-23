//! Signal/noise filtering for `memory_remember` ingest (issue #61).
//!
//! Why: Auto-capture hooks fired every tool-use event, raw prompt, and commit
//! message into palace storage. Palaces accumulated 6,650+ low-value drawers
//! that drowned curated knowledge in recall results. This module rejects
//! obvious noise before it is stored, classifies what does get through, and
//! gives operators a single configuration surface to tune the policy.
//! What: Defines `FilterConfig` (token threshold + reject regexes),
//! `FilterReject` (rejection reason — carried as an error), `classify`
//! (heuristic content-type detection), and `apply` (run the gate).
//! Test: Unit tests in this module cover token counting, every reject
//! pattern, and classifier outcomes.

use crate::memory_core::palace::DrawerType;
use regex::Regex;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};
use thiserror::Error;

// #6199: the credential detector lives in a submodule so this gate module stays
// under the 500-SLOC production cap. `check_secret`/`find_secret_token` are the
// public surface; the predicate internals are re-exported only for the test
// suite (`filter_tests.rs`, `use super::*`).
mod secret;
pub use secret::{check_secret, find_secret_token};
// The detector's predicate internals are re-exported only for the test suite
// (`filter_tests.rs`, `use super::*`), which reaches into them directly.
#[cfg(test)]
pub(crate) use secret::*;

/// Library-default minimum token count. Conservative (3) so direct library
/// users (CLI tools, tests, embedded callers) aren't blocked on
/// borderline-short content. The MCP `memory_remember` tool overrides this
/// with the stricter `MCP_MIN_TOKENS` (8) to match the issue #61 policy
/// applied to auto-capture hooks.
pub const DEFAULT_MIN_TOKENS: u8 = 3;

/// Stricter threshold applied at the MCP boundary where auto-capture hooks
/// fire. Matches the issue #61 spec — content shorter than this should be
/// stored via `memory_note` or `kg_assert` instead.
pub const MCP_MIN_TOKENS: u8 = 8;

/// Default reject patterns covering the common auto-capture noise sources.
///
/// Why: These were the dominant categories observed in the 6,650-drawer
/// audit referenced by issue #61. Centralising them as data keeps the
/// rejection logic in one place and makes new patterns one-line additions.
/// What: Each entry is a case-insensitive regex compiled at first use.
/// Test: `default_patterns_match_known_noise`.
const DEFAULT_REJECT_PATTERNS: &[&str] = &[
    // Tool use/result framing emitted by hook capture.
    r"(?i)^tool use:",
    r"(?i)^tool result:",
    // Conventional commit message.
    r"(?i)^(feat|fix|chore|refactor|test|docs|perf|build|ci|style|revert)(\([^)]*\))?:",
    // Progress logs ("Running cargo test...").
    r"^Running .*\.\.\.$",
    // File path only.
    r"^[/~][^\s]*\.(rs|py|ts|js|tsx|jsx|toml|json|md|yaml|yml)$",
];

// Issue #1481: the bare-40-hex git-SHA reject pattern (`^[0-9a-f]{40}$`) was
// removed from `DEFAULT_REJECT_PATTERNS` above. A standalone git commit SHA is
// a legitimate engineering memory ("the regression landed in
// 0fda534e0fda534e0fda534e0fda534e0fda534e"), not noise. Git-SHA-shaped tokens
// are now explicitly allowlisted by [`is_git_sha_like`] across every gate so
// they never trip the noise, secret, or non-alphabetic heuristics.

/// Lower bound on git-SHA-shaped hex token length recognised by
/// [`is_git_sha_like`].
///
/// Why (issue #1481): git lets you abbreviate a commit to as few as 7 hex
/// characters and still resolve it unambiguously in most repos, so engineering
/// prose routinely references `0fda534`-style short SHAs. Treating 7 as the
/// floor matches git's own default abbreviation length.
/// What: inclusive minimum hex-digit count for a token to count as a SHA.
/// Test: `git_sha_like_recognises_short_and_full`.
pub const GIT_SHA_MIN_LEN: usize = 7;

/// Upper bound on git-SHA-shaped hex token length recognised by
/// [`is_git_sha_like`].
///
/// Why (issue #1484): SHA-1 object ids are 40 hex chars; SHA-256 object ids
/// (git's newer `--object-format=sha256`, supported since git 2.29) are 64.
/// Raising the cap to 64 keeps engineering prose that references SHA-256
/// commits (e.g. in repos converted to SHA-256) from being incorrectly
/// classified as raw code or a secret by the non-alpha-ratio heuristic. A
/// pure-lowercase-hex run of 41–64 characters is overwhelmingly likely to be
/// a SHA-256 commit id, not a credential (credentials never appear as a pure
/// lowercase-hex string of exactly SHA length). Arbitrarily longer hex blobs
/// (≥ 65 chars) remain subject to the normal secret/non-alpha heuristics.
///
/// Knowns limitation: `is_git_sha_like` accepts pure-digit tokens in the
/// 7–64 char range (since ASCII digits `0-9` are valid hex). This means
/// numeric IDs such as account numbers of that length are also allowlisted.
/// The risk is low — such tokens rarely appear in prose — but callers should
/// be aware the allowlist is broader than strictly "git SHA". See issue #1484.
/// What: inclusive maximum hex-digit count for a token to count as a SHA.
/// Test: `git_sha_like_recognises_sha256`, `git_sha_like_rejects_overlong_and_nonhex`.
pub const GIT_SHA_MAX_LEN: usize = 64;

/// Substring patterns whose presence at the START of drawer content marks it
/// as low-value auto-capture noise (Claude Code tool-use captures, session
/// lifecycle events).
///
/// Why (issue #220, tightened #2442): auto-capture hooks always emit these
/// patterns as the literal frame/prefix of the entry, never buried inside
/// prose. The original write-path gate (`trusty-memory`) and the retroactive
/// dream-cycle prune pass each carried their own copy of this list, checked
/// via `str::contains` — a substring-anywhere match that fired on any
/// legitimate memory that merely QUOTED one of these phrases (e.g. a coding
/// agent's turn recapping `"Tool use: Bash"` from its own transcript),
/// silently thinning the recall surface. Issue #2442 consolidates both call
/// sites onto this single list plus [`blocklist_match`], which anchors the
/// check to the start of the (whitespace-trimmed) content instead.
/// What: substring patterns (not regexes), matched case-sensitively because
/// the auto-capture hooks always emit the exact English prefix.
/// Test: `blocklist_match_blocks_known_prefixes`,
/// `blocklist_match_ignores_quoted_mid_text`.
pub const BLOCKLIST_PATTERNS: &[&str] = &[
    "Tool use: ",          // Claude Code tool-use captures
    "Claude Code session", // Session lifecycle events
];

/// Blocklist gate: returns the matched pattern when `content` is FRAMED
/// (starts with, after leading-whitespace trim) by a known low-value
/// auto-capture pattern.
///
/// Why (issue #2442): centralises the single source of truth for the
/// write-path gate (`trusty-memory::tools::helpers::blocklist_gate`) and the
/// dream-cycle retroactive prune pass
/// (`memory_core::dream::helpers::is_low_quality_content`), which previously
/// carried independently-drifting copies of the same list. Anchoring to
/// `starts_with` (rather than `contains`) is the "structural detection"
/// fix requested by the issue: auto-capture noise always begins with the
/// pattern, so prose that merely quotes the phrase mid-text no longer trips
/// the gate.
/// What: returns `Some(pat)` for the first pattern in [`BLOCKLIST_PATTERNS`]
/// where `content.trim_start().starts_with(pat)`, else `None`.
/// Test: `blocklist_match_blocks_known_prefixes`,
/// `blocklist_match_ignores_quoted_mid_text`.
pub fn blocklist_match(content: &str) -> Option<&'static str> {
    let trimmed = content.trim_start();
    BLOCKLIST_PATTERNS
        .iter()
        .copied()
        .find(|pat| trimmed.starts_with(pat))
}

/// Rejection reasons surfaced to the caller.
///
/// Why: Each branch carries enough context for the MCP tool to produce a
/// helpful, actionable error message rather than a generic "rejected".
/// What: A `thiserror`-derived enum so handlers can pattern-match for
/// metrics while still bubbling through `anyhow`.
/// Test: `reject_messages_are_actionable`.
#[derive(Debug, Error, PartialEq)]
pub enum FilterReject {
    /// Content has fewer meaningful tokens than the configured minimum.
    #[error(
        "Content too short to be worth storing ({tokens} tokens). Use memory_note for brief \
         facts or kg_assert for structured triples."
    )]
    TooShort { tokens: usize },
    /// Content matched one of the reject patterns.
    #[error("Content rejected as low-signal noise (matched pattern: {pattern})")]
    NoisePattern { pattern: String },
    /// Content is mostly non-alphabetic (code/JSON heuristic).
    #[error(
        "Content rejected: appears to be raw code or JSON ({ratio:.0}% non-alphabetic). Store \
         a human-readable summary instead, or pass force=true to override."
    )]
    NonAlphabetic { ratio: f32 },
    /// Content contains a token that looks like a high-entropy secret
    /// (API key, access token, long base64 blob) rather than a git SHA.
    ///
    /// Why (issue #1481): the content gate must keep blocking genuine
    /// credentials so they never land in palace storage, while NOT blocking
    /// the safe case of git-SHA-shaped hex tokens. This variant names the
    /// offending token (truncated) so the caller can identify and remediate
    /// exactly what tripped the gate instead of seeing an opaque "blocked
    /// pattern".
    /// What: carries a short, redacted preview of the triggering token.
    /// Test: `reject_messages_are_actionable`, `secret_token_is_blocked`.
    #[error(
        "Content rejected: contains a likely secret/credential token \
         (`{token}`). Remove or redact the secret before storing; git commit \
         SHAs are allowed and will not trigger this."
    )]
    PotentialSecret { token: String },
}

/// Tunable gate configuration.
///
/// Why: Different deployments may want stricter or looser thresholds; making
/// the policy data-driven lets callers swap the defaults without forking the
/// dispatcher.
/// What: Holds the minimum token count and the list of compiled-on-demand
/// reject patterns. `reject_patterns` accepts plain strings so the struct
/// stays `Clone`-friendly and serializable; the compiled `Regex` set is
/// memoised process-wide (keyed by the patterns) via `compiled_patterns`.
/// Test: `filter_config_default_blocks_known_noise`,
/// `filter_config_force_bypasses_all`.
#[derive(Debug, Clone)]
pub struct FilterConfig {
    /// Minimum meaningful tokens required for `memory_remember`.
    pub min_tokens: u8,
    /// String form of each reject regex (compiled lazily).
    pub reject_patterns: Vec<String>,
    /// Maximum allowed ratio of non-alphabetic chars before treating the
    /// content as raw code/JSON. Range `[0.0, 1.0]`. Default `0.80`.
    pub max_non_alpha_ratio: f32,
}

impl Default for FilterConfig {
    fn default() -> Self {
        Self {
            min_tokens: DEFAULT_MIN_TOKENS,
            reject_patterns: DEFAULT_REJECT_PATTERNS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            max_non_alpha_ratio: 0.80,
        }
    }
}

impl FilterConfig {
    /// Compile and cache the configured reject patterns.
    ///
    /// Why: Regex compilation is expensive, so it is memoised. The default set
    /// shares one process-wide cache. A custom set (#6199) is memoised in a
    /// process-wide cache keyed by the pattern strings, so a given set compiles
    /// exactly once — where the previous code recompiled AND `Box::leak`-ed a
    /// fresh slice on every call, a permanent per-call leak.
    /// What: returns the compiled set, borrowing the shared default cache or an
    /// owned clone of the memoised custom set (`Regex` clones are cheap — they
    /// share the compiled program behind an `Arc`). Patterns that fail to parse
    /// are logged and skipped so one bad entry can't break the gate.
    /// Test: `custom_patterns_compile_once` (memoised, single compilation, no
    /// leak); indirectly, every other test in this module exercises this path.
    fn compiled_patterns(&self) -> Cow<'static, [Regex]> {
        // Fast path: the default set shares a process-wide cache across every
        // default config so the daemon does not recompile per call.
        static GLOBAL_CACHE: OnceLock<Vec<Regex>> = OnceLock::new();
        if self.is_default_patterns() {
            return Cow::Borrowed(
                GLOBAL_CACHE.get_or_init(|| {
                    compile_reject_patterns(DEFAULT_REJECT_PATTERNS.iter().copied())
                }),
            );
        }
        Cow::Owned(compile_custom_patterns(&self.reject_patterns))
    }

    /// True when `reject_patterns` is byte-for-byte the default set, so the
    /// shared process-wide cache applies.
    fn is_default_patterns(&self) -> bool {
        self.reject_patterns.len() == DEFAULT_REJECT_PATTERNS.len()
            && self
                .reject_patterns
                .iter()
                .zip(DEFAULT_REJECT_PATTERNS.iter())
                .all(|(a, b)| a == *b)
    }

    /// Run the gate against `content`.
    ///
    /// Why: Single entry point used by both `memory_remember` and
    /// `memory_note` (which bypasses the token threshold but keeps the noise
    /// patterns).
    /// What: Counts meaningful tokens, optionally enforces `min_tokens`,
    /// then walks the compiled reject patterns and the non-alphabetic
    /// heuristic. Returns `Ok(())` on accept, `Err(FilterReject)` on reject.
    /// Test: `filter_config_default_blocks_known_noise`,
    /// `note_mode_allows_short_content`.
    pub fn apply(&self, content: &str, enforce_min_tokens: bool) -> Result<(), FilterReject> {
        let trimmed = content.trim();
        // Noise patterns fire first so callers see the most specific
        // diagnosis (e.g. "Tool use: x" is flagged as a known noise source
        // rather than just "too short").
        let patterns = self.compiled_patterns();
        for re in patterns.iter() {
            if re.is_match(trimmed) {
                return Err(FilterReject::NoisePattern {
                    pattern: re.as_str().to_string(),
                });
            }
        }
        // Issue #1481: block genuine high-entropy secrets (API keys, access
        // tokens, long base64) before the token/non-alpha heuristics so the
        // caller gets the most specific, actionable diagnosis. Git-SHA-shaped
        // hex tokens are explicitly allowlisted inside `find_secret_token` and
        // never reach this branch. Issue #2520: extracted to the standalone
        // [`check_secret`] so the two-tier `force` design in
        // `PalaceHandle::remember_with_options` can run this exact check on
        // its own, independent of the quality gates below.
        check_secret(trimmed)?;
        let tokens = count_meaningful_tokens(content);
        if enforce_min_tokens && tokens < self.min_tokens as usize {
            return Err(FilterReject::TooShort { tokens });
        }
        // Issue #1481: compute the non-alpha ratio over a SHA-masked view so a
        // legitimate engineering memory that quotes one or more git commit
        // SHAs ("merge 4c536992 -> 0fda534e") is not misclassified as raw
        // code/JSON purely because hex digits and arrows push the raw ratio up.
        let ratio = non_alphabetic_ratio(&mask_git_shas(trimmed));
        if ratio > self.max_non_alpha_ratio {
            return Err(FilterReject::NonAlphabetic {
                ratio: ratio * 100.0,
            });
        }
        Ok(())
    }
}

/// Compile a sequence of reject patterns into a `Regex` set.
///
/// Why (#6199): the default and custom compile paths were byte-identical
/// `filter_map` blocks; sharing one helper removes the duplication and keeps a
/// single skip-on-parse-error policy.
/// What: compiles each pattern; a pattern that fails to parse is logged at warn
/// and skipped so one bad entry can't break the gate.
/// Test: covered by `default_patterns_match_known_noise` and
/// `custom_patterns_compile_once`.
fn compile_reject_patterns<'a>(patterns: impl Iterator<Item = &'a str>) -> Vec<Regex> {
    patterns
        .filter_map(|p| match Regex::new(p) {
            Ok(r) => Some(r),
            Err(e) => {
                tracing::warn!(pattern = %p, "skip invalid reject regex: {e}");
                None
            }
        })
        .collect()
}

/// Process-wide memo of compiled non-default reject-pattern sets, keyed by the
/// pattern strings. Bounded by the number of DISTINCT custom sets ever seen —
/// today there are no custom-pattern callers, so it stays empty in practice.
static CUSTOM_PATTERN_CACHE: OnceLock<Mutex<HashMap<Vec<String>, Vec<Regex>>>> = OnceLock::new();

fn custom_pattern_cache() -> &'static Mutex<HashMap<Vec<String>, Vec<Regex>>> {
    CUSTOM_PATTERN_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Compile a non-default reject-pattern set, memoised process-wide.
///
/// Why (#6199): the previous code recompiled the patterns and `Box::leak`-ed a
/// fresh slice on EVERY `apply` call for a custom config — a permanent per-call
/// leak. Keying the cache on the pattern strings compiles each distinct set
/// exactly once; callers get a cheap clone (`Regex` shares its compiled program
/// behind an `Arc`), so nothing leaks and nothing recompiles.
/// What: returns the cached set for `patterns`, compiling and inserting it on
/// first sight. The lock is taken only on this custom path — the default set is
/// served from `compiled_patterns`' own `OnceLock` and never reaches here. A
/// poisoned lock is recovered rather than propagated as a panic.
/// Test: `custom_patterns_compile_once`.
fn compile_custom_patterns(patterns: &[String]) -> Vec<Regex> {
    let mut guard = custom_pattern_cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(compiled) = guard.get(patterns) {
        return compiled.clone();
    }
    let compiled = compile_reject_patterns(patterns.iter().map(String::as_str));
    guard.insert(patterns.to_vec(), compiled.clone());
    compiled
}

/// #6199 test probe: is `patterns` already memoised in the custom cache? Lets
/// `custom_patterns_compile_once` prove a set is compiled exactly once by
/// observing the not-cached -> cached transition on a key unique to that test.
#[cfg(test)]
pub(crate) fn custom_patterns_cached(patterns: &[String]) -> bool {
    custom_pattern_cache()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains_key(patterns)
}

/// Count tokens that carry signal — whitespace-split tokens that contain at
/// least one alphanumeric character.
///
/// Why: Pure-punctuation tokens (`---`, `==>`, `{`) shouldn't count toward
/// the minimum-length requirement.
/// What: Splits on Unicode whitespace, keeps tokens with any alphanumeric.
/// Test: `meaningful_tokens_ignore_pure_punctuation`.
pub fn count_meaningful_tokens(s: &str) -> usize {
    s.split_whitespace()
        .filter(|t| t.chars().any(|c| c.is_alphanumeric()))
        .count()
}

/// Ratio of non-alphabetic characters (ignoring whitespace) in `s`.
///
/// Why: A high ratio is a strong signal that the content is raw code/JSON
/// rather than prose.
/// What: `non_alpha / total` over non-whitespace characters. Returns `0.0`
/// for empty input.
/// Test: `non_alpha_ratio_detects_json`.
pub fn non_alphabetic_ratio(s: &str) -> f32 {
    let mut total = 0usize;
    let mut non_alpha = 0usize;
    for c in s.chars() {
        if c.is_whitespace() {
            continue;
        }
        total += 1;
        if !c.is_alphabetic() {
            non_alpha += 1;
        }
    }
    if total == 0 {
        return 0.0;
    }
    non_alpha as f32 / total as f32
}

/// True when `token` is shaped like an (abbreviated or full) git commit SHA:
/// a pure-hex run of [`GIT_SHA_MIN_LEN`]..=[`GIT_SHA_MAX_LEN`] characters.
///
/// Why (issue #1481): a git SHA is the canonical safe high-entropy token —
/// engineering memories constantly reference commits, PRs, and merges by SHA.
/// Treating it as a secret silently dropped legitimate knowledge. Recognising
/// the shape lets every gate allowlist it while still blocking real
/// credentials (which are never pure lowercase/uppercase hex of SHA length).
/// What: returns `true` iff every char is an ASCII hex digit and the length is
/// within the git-SHA band. Case-insensitive (`0FDA534E` and `0fda534e` both
/// match) so quoted SHAs from different tools are all allowlisted.
/// Test: `git_sha_like_recognises_short_and_full`,
/// `git_sha_like_rejects_overlong_and_nonhex`.
pub fn is_git_sha_like(token: &str) -> bool {
    let len = token.len();
    (GIT_SHA_MIN_LEN..=GIT_SHA_MAX_LEN).contains(&len)
        && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Replace every git-SHA-shaped whitespace token in `s` with a fixed
/// alphabetic placeholder so downstream ratio heuristics ignore them.
///
/// Why (issue #1481): the non-alphabetic ratio gate would otherwise count the
/// hex digits (and the surrounding `->`, `#`, `,` punctuation common in commit
/// references) against an otherwise-prose memory, occasionally tipping it over
/// the `max_non_alpha_ratio` and rejecting it as "raw code". Masking the SHAs
/// (the intended-safe tokens) keeps prose-with-SHAs on the prose side of the
/// line while leaving genuinely code-shaped content untouched.
/// What: splits on whitespace and joins back with single spaces, swapping any
/// [`is_git_sha_like`] token for the word "sha". Non-SHA tokens (including
/// real secrets, which are caught earlier) pass through unchanged.
///
/// **Note (issue #1484):** the `split_whitespace().join(" ")` implementation
/// collapses multi-space runs, tabs, and newlines to single spaces before the
/// non-alpha ratio is computed. For whitespace-heavy inputs (code blocks with
/// indentation, markdown tables) this slightly reduces the computed ratio
/// relative to the raw input — making the gate marginally more permissive on
/// whitespace-rich content. This is a nit; the effect is small because
/// whitespace is excluded from the ratio computation in
/// `non_alphabetic_ratio`. The behaviour is intentional: normalising
/// whitespace avoids penalising prose that happens to have extra spacing.
/// Test: exercised via `git_sha_prose_is_accepted` and
/// `non_alpha_masking_ignores_shas`.
fn mask_git_shas(s: &str) -> String {
    s.split_whitespace()
        .map(|tok| {
            // Strip common trailing/leading punctuation so `4c536992,` and
            // `(0fda534e)` still register as SHAs for masking purposes.
            let stripped = tok.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            if is_git_sha_like(stripped) {
                "sha"
            } else {
                tok
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Classify drawer content into a `DrawerType` using cheap heuristics.
///
/// Why: Issue #61 — when the dispatcher accepts a write, it should tag the
/// drawer so downstream code (recall ranking, TTL sweep, UIs) can treat
/// auto-captured noise differently from curated facts even when the filter
/// chose to let it through (e.g. `force = true`).
/// What: Returns `Commit` for commit-shaped content, `SessionEvent` for
/// tool-use framing or progress logs, otherwise the supplied `fallback`.
/// The classifier is intentionally conservative — it never returns
/// `UserFact` on its own; that label is reserved for the explicit
/// `memory_note` tool path.
/// Test: `classify_detects_commit_and_tool_use`.
pub fn classify(content: &str, fallback: DrawerType) -> DrawerType {
    let trimmed = content.trim();
    if is_commit_like(trimmed) {
        return DrawerType::Commit;
    }
    if is_session_event_like(trimmed) {
        return DrawerType::SessionEvent;
    }
    fallback
}

fn is_commit_like(s: &str) -> bool {
    // 40-hex SHA or a conventional commit prefix.
    static SHA: OnceLock<Regex> = OnceLock::new();
    static CONV: OnceLock<Regex> = OnceLock::new();
    let sha = SHA.get_or_init(|| Regex::new(r"^[0-9a-f]{40}$").expect("sha regex"));
    let conv = CONV.get_or_init(|| {
        Regex::new(
            r"(?i)^(feat|fix|chore|refactor|test|docs|perf|build|ci|style|revert)(\([^)]*\))?:",
        )
        .expect("conventional commit regex")
    });
    sha.is_match(s) || conv.is_match(s)
}

fn is_session_event_like(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.starts_with("tool use:")
        || lower.starts_with("tool result:")
        || (lower.starts_with("running ") && lower.ends_with("..."))
}

/// Unit tests live in a sibling file so the production module stays under the
/// 500-SLOC cap (the test file is classified as a test target, 1500-SLOC cap).
///
/// Why: `filter.rs` grew past the production cap as secret-detection coverage
/// expanded (issue #1481); extracting the suite keeps the gate logic and its
/// tests co-located without tripping the line-cap ratchet.
/// What: pulls in `filter_tests.rs` as the `tests` module under `cfg(test)`.
/// Test: the referenced file is itself the test suite.
#[cfg(test)]
#[path = "filter_tests.rs"]
mod tests;
