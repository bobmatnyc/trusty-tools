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
use std::sync::OnceLock;
use thiserror::Error;

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
/// cached per-config via `compiled_patterns`.
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
    /// Compile and cache the configured patterns.
    ///
    /// Why: Regex compilation is amortised across calls — the cache is keyed
    /// to the config instance via a `OnceLock` so repeated `apply` calls on
    /// the same config don't re-parse the same strings.
    /// What: On first call, compiles each pattern. Patterns that fail to
    /// parse are logged and skipped so a bad entry can't break the gate.
    /// Test: Indirect — every other test in this module exercises this path.
    fn compiled_patterns(&self) -> &[Regex] {
        // We store the compiled set in a per-instance OnceLock so identical
        // configs reuse it. Because `FilterConfig` is `Clone`, the cache is
        // not shared across clones — that's fine for the tiny default set.
        static GLOBAL_CACHE: OnceLock<Vec<Regex>> = OnceLock::new();
        // Fast path: when the strings match the defaults exactly, share the
        // global cache so the daemon doesn't recompile per call.
        if self.reject_patterns.len() == DEFAULT_REJECT_PATTERNS.len()
            && self
                .reject_patterns
                .iter()
                .zip(DEFAULT_REJECT_PATTERNS.iter())
                .all(|(a, b)| a == *b)
        {
            return GLOBAL_CACHE.get_or_init(|| {
                DEFAULT_REJECT_PATTERNS
                    .iter()
                    .filter_map(|p| match Regex::new(p) {
                        Ok(r) => Some(r),
                        Err(e) => {
                            tracing::warn!(pattern = %p, "skip invalid reject regex: {e}");
                            None
                        }
                    })
                    .collect()
            });
        }
        // Custom config: compile inline. We leak a Box to keep the slice
        // borrow alive — acceptable because custom configs are rare and the
        // memory is bounded by the number of patterns.
        let compiled: Vec<Regex> = self
            .reject_patterns
            .iter()
            .filter_map(|p| match Regex::new(p) {
                Ok(r) => Some(r),
                Err(e) => {
                    tracing::warn!(pattern = %p, "skip invalid reject regex: {e}");
                    None
                }
            })
            .collect();
        Box::leak(compiled.into_boxed_slice())
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
        for re in self.compiled_patterns() {
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

/// Scan `content` for the first whitespace token that looks like a genuine
/// high-entropy secret (API key, access token, long base64/JWT-ish blob),
/// explicitly allowlisting git-SHA-shaped hex tokens.
///
/// Why (issue #1481): credentials must never be stored, but git SHAs (the most
/// common "high-entropy-looking" token in engineering prose) must be. A pure
/// detector keyed only on entropy/length would block both; this function adds
/// the SHA allowlist and keys "secret" on the character-class mix that real
/// credentials exhibit (mixed upper+lower+digit, or known credential prefixes,
/// or symbol-bearing base64) which a SHA never does.
/// What: returns `Some(<redacted preview>)` for the first secret-looking token,
/// else `None`. The preview shows the leading characters and masks the tail so
/// the secret itself is not echoed back verbatim. Tokens are stripped of
/// surrounding punctuation before classification.
/// Test: `secret_token_is_blocked`, `git_sha_prose_is_accepted`,
/// `base64_blob_is_blocked`, `known_key_prefixes_are_blocked`.
pub fn find_secret_token(content: &str) -> Option<String> {
    for raw in content.split_whitespace() {
        let tok =
            raw.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_')));
        if looks_like_secret(tok) {
            return Some(redact_token(tok));
        }
    }
    None
}

/// Standalone secret gate: `Ok(())` when `content` carries no
/// [`find_secret_token`] hit, `Err(FilterReject::PotentialSecret)` otherwise.
///
/// Why (issue #2520, two-tier `force` design): `RememberOptions::force`
/// bypasses the QUALITY gates (noise patterns, short-content, non-alphabetic
/// ratio) but must never bypass secret detection — otherwise an automated
/// writer that always passes `force: true` (e.g. trusty-code's per-turn
/// recorder) would persist raw credentials with zero screening. Extracting
/// this single-purpose check out of [`FilterConfig::apply`] lets
/// `PalaceHandle::remember_with_options` call it on its own when `force` is
/// set but the caller has NOT also set the explicit `allow_secret_like`
/// opt-in.
/// What: thin wrapper around [`find_secret_token`] that packages a hit into
/// the same [`FilterReject::PotentialSecret`] variant `apply` returns, so
/// both call sites produce an identical, actionable error message.
/// Test: exercised via `FilterConfig::apply`'s existing secret-detection
/// tests (this function is `apply`'s sole secret-check code path) and
/// directly by the two-tier `force` tests in
/// `trusty-common::memory_core::retrieval::handle`.
pub fn check_secret(content: &str) -> Result<(), FilterReject> {
    if let Some(token) = find_secret_token(content) {
        return Err(FilterReject::PotentialSecret { token });
    }
    Ok(())
}

/// Known credential prefixes that should always be treated as secrets.
///
/// Why (issue #1481): provider-issued keys (OpenAI `sk-`, GitHub `ghp_`/`gho_`,
/// AWS `AKIA`/`ASIA`, Slack `xoxb-`) have distinctive prefixes that make them
/// unambiguously secret regardless of their entropy profile. Matching the
/// prefix is cheaper and more precise than entropy alone — and it is the ONLY
/// thing that catches AWS access key IDs, which are all-uppercase base32 and so
/// never satisfy the mixed-case-plus-digit fallback in [`looks_like_secret`]
/// (FN-1, issue #1481).
/// What: lowercased prefix list checked case-insensitively in
/// [`looks_like_secret`] (which lowercases the token before comparing).
/// Test: `known_key_prefixes_are_blocked`, `aws_access_key_ids_are_blocked`.
const SECRET_PREFIXES: &[&str] = &[
    "sk-",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    // AWS long-term access key IDs (`AKIA…`) and STS temporary credential IDs
    // (`ASIA…`): 4-char prefix + 16 base32 chars `[A-Z2-7]`, ALL-UPPERCASE, so
    // the mixed-case heuristic below can never flag them. Compared lowercased.
    "akia",
    "asia",
];

/// True when `seg` is a "uniform-case word segment": non-empty, consists only
/// of ASCII alphanumeric chars (no punctuation), and every alphabetic char is
/// either all-lowercase or all-uppercase (no internal mixed case within the
/// segment). Digits count as case-neutral.
///
/// Why: this is the per-segment predicate used by [`is_segmented_identifier`]
/// to determine whether a hyphen-delimited token reads like a compound
/// human-readable identifier (`2-medium->REQUEST_CHANGES`) vs. a random
/// mixed-case credential blob (`AbCd1234-EfGh5678`).
/// What: strips leading/trailing non-alnum chars (from arrow punctuation like
/// `>`, `<`), then checks all-lowercase-or-digit or all-uppercase-or-digit.
/// Test: `structural_tokens_are_not_flagged`.
fn is_uniform_word_segment(seg: &str) -> bool {
    let s = seg.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if s.is_empty() {
        return false;
    }
    // All alphabetic chars in segment must share a single case.
    let all_lower = s
        .chars()
        .all(|c| !c.is_ascii_alphabetic() || c.is_ascii_lowercase());
    let all_upper = s
        .chars()
        .all(|c| !c.is_ascii_alphabetic() || c.is_ascii_uppercase());
    all_lower || all_upper
}

/// True when `token` looks like a compound identifier whose hyphen-separated
/// segments are each uniform-case words (e.g. `2-medium->REQUEST_CHANGES`,
/// `trusty-review-v0.6.0`, `snake-case-value`).
///
/// Why (issue #1667): the mixed-case-plus-digit heuristic in
/// [`looks_like_secret`] fires on `2-medium->REQUEST_CHANGES` because the
/// compound token contains lower (`medium`), upper (`REQUEST_CHANGES`), and
/// digit (`2`) when considered as a whole. But each segment is internally
/// uniform-case, which is the hallmark of a human-readable compound
/// identifier, not a random credential blob.
/// What: requires at least two non-empty segments after splitting on `-`,
/// with each segment passing [`is_uniform_word_segment`].
/// Test: `structural_tokens_are_not_flagged`.
fn is_segmented_identifier(token: &str) -> bool {
    if !token.contains('-') {
        return false;
    }
    let segments: Vec<&str> = token.split('-').collect();
    if segments.len() < 2 {
        return false;
    }
    segments.iter().all(|s| is_uniform_word_segment(s))
}

/// True when `token` is a structured path/slug/key=value/compound-identifier
/// that should NOT be treated as a base64 blob or mixed-case credential.
///
/// Why (issues #1667, #1676): `looks_like_secret`'s heuristics (b64-symbol
/// check and mixed-case+digit check) fire on legitimate technical tokens.
/// Examples: `verdict/grade/prose-summary` contains `/` (b64 sym) → false
/// positive; `>=2-medium->REQUEST_CHANGES` is lower+upper+digit → false
/// positive on the mixed-case branch; `reviewer_model=openrouter/openai/...`
/// was routed to the slash-path branch, where the first `/`-segment
/// `reviewer_model=openrouter` contains `=` and fails `is_word_segment`,
/// causing a false positive (#1676).
/// This function recognises those structural shapes and short-circuits the
/// two dangerous branches in `looks_like_secret`.
/// What: returns `true` for (a) `=`-containing tokens where the LHS is a
/// word segment and the RHS is itself structural (a word segment OR a
/// slash-path), checked before the slash-path branch so that tokens like
/// `key=path/to/value` are decomposed at `=` first; (b) slash-path tokens
/// where every `/`-segment is word-like; or (c) hyphen-segmented compound
/// identifiers where each segment is uniform-case. A token with `+` is
/// never structural (`+` never appears in identifiers).
/// Test: `structural_tokens_are_not_flagged`, `base64_blob_is_blocked`,
/// `key_equals_slashpath_not_flagged` (issue #1676 regression tests).
fn is_structural_token(token: &str) -> bool {
    // `+` is unambiguously base64 — no structural pattern uses it.
    if token.contains('+') {
        return false;
    }
    // A "word segment" for slash/equals splitting: non-empty, contains only
    // ASCII alphanumeric chars plus the minimal set of punctuation that
    // appears in legitimate structural tokens:
    //   `-`  — hyphen in slug/version segments (`prose-summary`, `v0.6.0`)
    //   `_`  — underscore in snake_case identifiers
    //   `.`  — dot in file extensions and semver (`synthesis.rs`, `v0.6.0`)
    //   `>`  — needed for the `>` in `>=2-medium->REQUEST_CHANGES` which
    //           arrives as the LHS of the `=` split (i.e. the lone `>` char).
    //   `:`  — Rust/module path separator (`::`) inside a slash-path segment,
    //           e.g. `client/http_client/error.rs::response_or_body_error`
    //           (issue #2442 real-world false positive: a source-location
    //           reference, not a credential). A lone or doubled `:` never
    //           appears in base64/credential alphabets, so admitting it does
    //           not widen the entropy-heuristic bypass meaningfully — a
    //           colon-bearing "path segment" still has to pass the slash-path
    //           structural shape to be exempted at all.
    // All other chars (`<`, `!`, `@`, `#`, `~`) are excluded — none
    // appear in paths, slugs, or key=value tokens we need to allow, and
    // admitting them would unnecessarily widen the bypass surface.
    fn is_word_segment(s: &str) -> bool {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '>' | ':'))
    }
    // True when every `/`-separated segment of `s` is a word segment.
    // Extracted as a named helper so the `=` branch can use it for the RHS
    // without re-entering `is_structural_token` (one-level bounded check).
    fn is_slash_path(s: &str) -> bool {
        s.contains('/') && s.split('/').all(is_word_segment)
    }
    // (a) key=value / semver-operator shape — checked BEFORE slash-path so
    // that tokens containing BOTH `=` and `/` (e.g.
    // `reviewer_model=openrouter/openai/gpt-5.4-mini-20260317`) are
    // decomposed at the `=` boundary first, letting the RHS be validated as
    // a slash-path rather than having the whole token routed to branch (b)
    // where the first `/`-segment (`reviewer_model=openrouter`) contains `=`
    // and fails `is_word_segment`.
    //
    // Compositional rule (issue #1676): LHS must be a word segment AND the
    // RHS must be EITHER a word segment OR a slash-path (all its `/`-separated
    // segments are word-like). A RHS that is a high-entropy opaque blob — no
    // slashes, not a simple word — is not structural, so the token falls
    // through to the entropy heuristics and is correctly flagged.
    // Exception: RHS that is pure `=` padding is non-structural (base64).
    if token.contains('=') {
        let parts: Vec<&str> = token.splitn(2, '=').collect();
        if parts.len() == 2 {
            let lhs = parts[0];
            let rhs = parts[1];
            if rhs.chars().all(|c| c == '=') {
                return false; // pure base64 padding, not key=value
            }
            if is_word_segment(lhs) && (is_word_segment(rhs) || is_slash_path(rhs)) {
                return true;
            }
            // Fall back to the original OR for semver-operator tokens like
            // `>=value` where the LHS may be a bare `>` and the RHS drives
            // the structural signal.
            return is_word_segment(lhs) || is_word_segment(rhs);
        }
    }
    // (b) Path/slug shape: all `/`-separated segments are word-like.
    // Covers `verdict/grade/prose-summary`, `org/repo`, file paths.
    if token.contains('/') {
        return token.split('/').all(is_word_segment);
    }
    // (c) Hyphen-segmented compound identifier: no b64 syms remain at this
    // point; check whether every `-`-separated segment is uniform-case.
    // Covers `2-medium->REQUEST_CHANGES`, `trusty-review-v0.6.0`.
    is_segmented_identifier(token)
}

/// Heuristic: is `token` a likely secret/credential (and not a git SHA)?
///
/// Why (issue #1481): see [`find_secret_token`]. This is the core decision —
/// it must say "no" to git SHAs and "yes" to credentials.
/// What: returns `false` immediately for [`is_git_sha_like`] tokens (the
/// allowlist), then returns `true` when the token (a) carries a known
/// [`SECRET_PREFIXES`] credential prefix (e.g. `sk-`, `ghp_`, AWS `AKIA`/`ASIA`),
/// or (b) is long (≥ 20 chars) AND is not a structural token (see
/// [`is_structural_token`]) AND mixes character classes in a way SHAs cannot
/// — i.e. contains BOTH a lowercase and an uppercase letter plus a digit, or
/// contains a base64-indicator symbol (`+`) or an `=`/`/` that is NOT part of
/// a structural path/slug/key=value token. Pure lowercase-hex (SHA-shaped or
/// longer all-hex), all-uppercase tokens without a known prefix, ordinary words,
/// path-like tokens, and compound identifiers are not flagged.
///
/// **Known limitation (FN-2, issue #1484):** mixed-case-but-no-digit tokens
/// ≥ 20 chars (e.g. base58 key segments like `xPubKeySegmentAbCdEf`) are not
/// flagged by the (b) fallback because `has_digit` is `false`. The prefix list
/// (a) remains the authoritative gate for well-known credential formats. If
/// your deployment stores content that may contain bare mixed-case-alphabetic
/// high-entropy blobs without a known prefix, add a custom prefix or extend
/// the pattern list in `FilterConfig`.
/// Test: `secret_token_is_blocked`, `base64_blob_is_blocked`,
/// `git_sha_like_is_not_secret`, `ordinary_words_are_not_secret`,
/// `aws_access_key_ids_are_blocked`, `mixed_case_no_digit_limitation`,
/// `structural_tokens_are_not_flagged`.
fn looks_like_secret(token: &str) -> bool {
    // Allowlist git SHAs first — the whole point of issue #1481.
    if is_git_sha_like(token) {
        return false;
    }
    let lower = token.to_ascii_lowercase();
    if SECRET_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return true;
    }
    // Below the length floor, entropy is too low to confidently flag.
    if token.len() < 20 {
        return false;
    }
    // Issue #1667: structural tokens (paths, slugs, key=value pairs, and
    // hyphen-segmented compound identifiers) must not be flagged as secrets
    // even when they contain `/`, `=`, or mixed case across segments.
    if is_structural_token(token) {
        return false;
    }
    let has_lower = token.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = token.chars().any(|c| c.is_ascii_uppercase());
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let has_b64_sym = token.chars().any(|c| matches!(c, '+' | '/' | '='));
    // base64/url-safe blob: long, not structural, and carries a base64 symbol.
    if has_b64_sym && (has_lower || has_upper || has_digit) {
        return true;
    }
    // Mixed-case alphanumeric of credential length: SHAs are single-case hex,
    // so requiring BOTH cases plus a digit excludes them while catching the
    // typical `AbCd12…` / JWT-segment shapes. NOTE (FN-1, issue #1481): this
    // does NOT catch AWS access key IDs — `AKIAIOSFODNN7EXAMPLE` is // pragma: allowlist secret
    // all-uppercase base32 (`has_lower == false`), so it relies entirely on the
    // `akia`/`asia` entries in SECRET_PREFIXES above.
    //
    // Issue #2442: this fallback used to fire on ANY ≥20-char token that
    // mixed case + digit, regardless of what other punctuation it carried.
    // A real-world false positive: an issue/PR/SHA "ledger" reference like
    // `#2486→PR#2491(e993c18a)` mixes digits, uppercase (`PR`), and lowercase
    // hex — but no credential format ever contains `#`, arrows, or parens.
    // Gate the fallback on [`is_plausible_credential_charset`] so it only
    // fires for tokens shaped like an actual bare credential (alphanumeric
    // plus the `-`/`_`/`.` separators used by JWTs and slug-style API keys).
    has_lower && has_upper && has_digit && is_plausible_credential_charset(token)
}

/// True when every character in `token` could plausibly appear in a bare
/// credential/API-key string: ASCII alphanumeric, or one of `-`, `_`, `.`,
/// `:` (hyphen/underscore-delimited key segments, JWT `.`-separated parts,
/// colon-delimited `user:secret` / `Bearer:token` shapes).
///
/// Why (issue #2442): the mixed-case-plus-digit fallback in
/// [`looks_like_secret`] must not fire on prose punctuation that a real
/// credential never contains — issue/PR ledger markers (`#`), arrows (`→`,
/// `->`), parentheses, etc. Restricting the fallback to tokens built only
/// from this charset keeps it precise without weakening the base64-symbol
/// branch (which already requires `+`/`/`/`=` and is unaffected by this gate)
/// or the known-prefix layer (checked earlier, unaffected).
///
/// Why `:` is included (issue #2520 review, BLOCKER regression fix): the
/// first cut of this function omitted `:`, which meant ANY colon in a token
/// — including a bare colon-delimited credential like
/// `token:aBc123XyZ987uvW456QrS` — flunked the `.all()` check and silently
/// disabled the fallback (`find_secret_token` returned `None` where
/// `origin/main`'s pre-#2442 code, which had no charset gate at all,
/// correctly returned `Some`). The `path::fn` false positive this module
/// fixes is handled entirely by `is_structural_token`'s slash-path branch
/// (checked earlier in [`looks_like_secret`] and gated on `/` being
/// present), so admitting `:` here does not reopen it — a bare
/// `path::fn`-shaped token with no digit/mixed-case never reaches the
/// fallback in the first place, and one that DOES contain a `/` short-
/// circuits via `is_structural_token` before this function is ever called.
/// What: returns `true` iff every char is ASCII alphanumeric or in
/// `{'-', '_', '.', ':'}`.
/// Test: `ledger_reference_token_not_flagged`, `secret_token_is_blocked`,
/// `real_secrets_still_blocked_after_1667_fix`,
/// `colon_bearing_credential_is_flagged`.
fn is_plausible_credential_charset(token: &str) -> bool {
    token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// Produce a short, non-reversible preview of a flagged secret token.
///
/// Why (issue #1481): the rejection message must name *which* token tripped the
/// gate (so the caller can find and remove it) without echoing the full secret
/// back into logs or responses. Issue #2401 consolidated the masking logic
/// itself into `inference::credentials::redact_secret` — the `config` clap
/// module (epic #2400) needs the identical shape, and duplicating it here
/// would violate the "one implementation per behaviour" rule. This function
/// is now a thin delegating wrapper kept for call-site stability and doc
/// continuity at this module's original issue (#1481).
/// What: returns the first up-to-4 characters followed by `…` and the token
/// length, e.g. `sk-A…(48 chars)`. Short tokens (≤ 4 chars never reach here
/// because the secret heuristic requires length) are returned verbatim.
/// Test: `redact_token_masks_tail` (format stability); the masking
/// implementation itself is tested in
/// `inference::credentials::redact::tests`.
fn redact_token(token: &str) -> String {
    crate::inference::credentials::redact_secret(token)
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
