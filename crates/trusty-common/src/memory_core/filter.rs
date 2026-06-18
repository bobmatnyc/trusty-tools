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
/// Why (issue #1481): SHA-1 object ids are 40 hex chars and SHA-256 object ids
/// (git's newer object format) are 64. We cap at 40 per the issue spec — a
/// pure-hex run of exactly git-SHA length is the safe case to allowlist, while
/// arbitrarily long hex blobs stay subject to the secret/non-alpha heuristics.
/// What: inclusive maximum hex-digit count for a token to count as a SHA.
/// Test: `git_sha_like_rejects_overlong_and_nonhex`.
pub const GIT_SHA_MAX_LEN: usize = 40;

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
        // never reach this branch.
        if let Some(token) = find_secret_token(trimmed) {
            return Err(FilterReject::PotentialSecret { token });
        }
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
/// What: splits on whitespace and joins back, swapping any [`is_git_sha_like`]
/// token for the word "sha". Non-SHA tokens (including real secrets, which are
/// caught earlier) pass through unchanged.
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

/// Known credential prefixes that should always be treated as secrets.
///
/// Why (issue #1481): provider-issued keys (OpenAI `sk-`, GitHub `ghp_`/`gho_`,
/// AWS `AKIA`, Slack `xoxb-`) have distinctive prefixes that make them
/// unambiguously secret regardless of their entropy profile. Matching the
/// prefix is cheaper and more precise than entropy alone.
/// What: lowercased prefix list checked case-insensitively in
/// [`looks_like_secret`].
/// Test: `known_key_prefixes_are_blocked`.
const SECRET_PREFIXES: &[&str] = &[
    "sk-",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
];

/// Heuristic: is `token` a likely secret/credential (and not a git SHA)?
///
/// Why (issue #1481): see [`find_secret_token`]. This is the core decision —
/// it must say "no" to git SHAs and "yes" to credentials.
/// What: returns `false` immediately for [`is_git_sha_like`] tokens (the
/// allowlist), then returns `true` when the token (a) carries a known
/// credential prefix, or (b) is long (≥ 20 chars) AND mixes character classes
/// in a way SHAs cannot — i.e. contains both letters and digits with at least
/// one uppercase letter, or contains a base64/url-safe symbol (`+ / =`). Pure
/// lowercase-hex (SHA-shaped or longer all-hex) and ordinary words are not
/// secrets.
/// Test: `secret_token_is_blocked`, `base64_blob_is_blocked`,
/// `git_sha_like_is_not_secret`, `ordinary_words_are_not_secret`.
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
    let has_lower = token.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = token.chars().any(|c| c.is_ascii_uppercase());
    let has_digit = token.chars().any(|c| c.is_ascii_digit());
    let has_b64_sym = token.chars().any(|c| matches!(c, '+' | '/' | '='));
    // base64/url-safe blob: long and carries a base64-only symbol.
    if has_b64_sym && (has_lower || has_upper || has_digit) {
        return true;
    }
    // Mixed-case alphanumeric of credential length: SHAs are single-case hex,
    // so requiring BOTH cases plus a digit excludes them while catching the
    // typical `AKIA…`, `AbCd12…`, JWT-segment shapes.
    has_lower && has_upper && has_digit
}

/// Produce a short, non-reversible preview of a flagged secret token.
///
/// Why (issue #1481): the rejection message must name *which* token tripped the
/// gate (so the caller can find and remove it) without echoing the full secret
/// back into logs or responses.
/// What: returns the first up-to-4 characters followed by `…` and the token
/// length, e.g. `sk-A…(48 chars)`. Short tokens (≤ 4 chars never reach here
/// because the secret heuristic requires length) are returned verbatim.
/// Test: `redact_token_masks_tail`.
fn redact_token(token: &str) -> String {
    let head: String = token.chars().take(4).collect();
    format!("{head}…({} chars)", token.len())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meaningful_tokens_ignore_pure_punctuation() {
        assert_eq!(count_meaningful_tokens("--- === >>>"), 0);
        assert_eq!(count_meaningful_tokens("one two three"), 3);
        assert_eq!(count_meaningful_tokens("foo --- bar"), 2);
    }

    #[test]
    fn non_alpha_ratio_detects_json() {
        let json = r#"{"a":1,"b":[2,3,4]}"#;
        let ratio = non_alphabetic_ratio(json);
        assert!(
            ratio > 0.5,
            "expected JSON to register as mostly non-alphabetic; got {ratio}"
        );
        let prose = "The quick brown fox jumps over the lazy dog";
        assert!(non_alphabetic_ratio(prose) < 0.2);
    }

    #[test]
    fn default_patterns_match_known_noise() {
        let cfg = FilterConfig::default();
        let cases = [
            "Tool use: search_files",
            "Tool result: ok",
            // NOTE (issue #1481): a bare 40-hex git SHA is NO LONGER rejected —
            // it is a legitimate engineering memory. See
            // `bare_git_sha_is_no_longer_rejected` below.
            "feat(memory): add filter",
            "fix: handle nulls",
            "Running cargo test...",
            "/Users/x/foo.rs",
            "~/notes.md",
        ];
        for c in cases {
            assert!(cfg.apply(c, false).is_err(), "expected reject for: {c}");
        }
    }

    /// Why (issue #1481): regression lock — the previous behaviour rejected a
    /// bare 40-hex git SHA as noise, silently dropping a valid memory. The
    /// allowlist must keep it accepted.
    /// What: asserts `apply` accepts a bare full SHA.
    /// Test: itself.
    #[test]
    fn bare_git_sha_is_no_longer_rejected() {
        let cfg = FilterConfig::default();
        // pragma: allowlist secret (test fixture: a bare git SHA, not a credential)
        assert!(
            cfg.apply("abcdef0123456789abcdef0123456789abcdef01", false) // pragma: allowlist secret
                .is_ok()
        );
    }

    #[test]
    fn filter_config_default_blocks_known_noise() {
        let cfg = FilterConfig::default();
        let res = cfg.apply("Tool use: read_file", true);
        assert!(matches!(res, Err(FilterReject::NoisePattern { .. })));
    }

    #[test]
    fn filter_config_too_short_triggers_token_error() {
        // Use a config with the stricter MCP threshold (8) so the assertion
        // is independent of the lower library default (3).
        let cfg = FilterConfig {
            min_tokens: MCP_MIN_TOKENS,
            ..FilterConfig::default()
        };
        let res = cfg.apply("only four tokens here", true);
        match res {
            Err(FilterReject::TooShort { tokens }) => assert_eq!(tokens, 4),
            other => panic!("expected TooShort, got {other:?}"),
        }
    }

    #[test]
    fn note_mode_allows_short_content() {
        // Use the stricter MCP threshold so the assertion documents the
        // contract independently of the library default.
        let cfg = FilterConfig {
            min_tokens: MCP_MIN_TOKENS,
            ..FilterConfig::default()
        };
        // 3 tokens, would fail with enforce_min=true, must pass with false.
        assert!(cfg.apply("User prefers snake_case", false).is_ok());
        assert!(cfg.apply("User prefers snake_case", true).is_err());
    }

    #[test]
    fn filter_accepts_real_content() {
        let cfg = FilterConfig::default();
        assert!(
            cfg.apply(
                "When refactoring search indices, prefer postcard over JSON for redb \
                 values because of size and decode speed.",
                true,
            )
            .is_ok()
        );
    }

    #[test]
    fn filter_rejects_high_non_alpha_content() {
        let cfg = FilterConfig::default();
        let json = r#"{"id":1,"items":[2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20]}"#;
        let res = cfg.apply(json, false);
        assert!(matches!(res, Err(FilterReject::NonAlphabetic { .. })));
    }

    #[test]
    fn classify_detects_commit_and_tool_use() {
        assert_eq!(
            classify("feat(memory): add filter", DrawerType::Unknown),
            DrawerType::Commit
        );
        assert_eq!(
            classify(
                "abcdef0123456789abcdef0123456789abcdef01", // pragma: allowlist secret
                DrawerType::Unknown
            ),
            DrawerType::Commit
        );
        assert_eq!(
            classify("Tool use: search_code", DrawerType::Unknown),
            DrawerType::SessionEvent
        );
        assert_eq!(
            classify("Running cargo test...", DrawerType::Unknown),
            DrawerType::SessionEvent
        );
        // Prose falls through to the supplied fallback.
        assert_eq!(
            classify(
                "A regular curated knowledge fragment.",
                DrawerType::AgentNote
            ),
            DrawerType::AgentNote
        );
    }

    #[test]
    fn reject_messages_are_actionable() {
        let too_short = FilterReject::TooShort { tokens: 3 };
        assert!(too_short.to_string().contains("memory_note"));
        let noise = FilterReject::NoisePattern {
            pattern: "x".to_string(),
        };
        assert!(noise.to_string().contains("low-signal"));
        let na = FilterReject::NonAlphabetic { ratio: 85.0 };
        assert!(na.to_string().contains("force=true"));
        // Issue #1481: the secret rejection must name the offending token.
        let secret = FilterReject::PotentialSecret {
            token: "sk-A…(48 chars)".to_string(),
        };
        let msg = secret.to_string();
        assert!(msg.contains("sk-A"), "secret reject must name token: {msg}");
        assert!(
            msg.contains("secret"),
            "secret reject must say 'secret': {msg}"
        );
    }

    // ---- Issue #1481: git-SHA allowlist + secret detection ----

    #[test]
    fn git_sha_like_recognises_short_and_full() {
        // 7-char abbreviation (git's default) through full 40-char SHA-1.
        assert!(is_git_sha_like("0fda534"));
        assert!(is_git_sha_like("4c536992"));
        assert!(is_git_sha_like("0fda534e0fda534e0fda534e0fda534e0fda534e"));
        // Case-insensitive: uppercase hex is still a SHA.
        assert!(is_git_sha_like("0FDA534E"));
    }

    #[test]
    fn git_sha_like_rejects_overlong_and_nonhex() {
        // 6 chars is below git's abbreviation floor.
        assert!(!is_git_sha_like("0fda53"));
        // 41 chars exceeds SHA-1 length.
        assert!(!is_git_sha_like(
            "0fda534e0fda534e0fda534e0fda534e0fda534e0"
        ));
        // Non-hex characters.
        assert!(!is_git_sha_like("0fda534g"));
        assert!(!is_git_sha_like("hello123"));
    }

    #[test]
    fn git_sha_prose_is_accepted() {
        // The exact repro from issue #1481 must be stored, not skipped.
        let cfg = FilterConfig::default();
        let repro = "Shipped via PR #1466 squash 0fda534e -> merge 4c536992, CI green.";
        assert!(
            cfg.apply(repro, true).is_ok(),
            "git-SHA prose must pass the gate; got {:?}",
            cfg.apply(repro, true)
        );
        // A bare full SHA on its own is also legitimate now.
        assert!(
            cfg.apply("0fda534e0fda534e0fda534e0fda534e0fda534e", false)
                .is_ok()
        );
    }

    #[test]
    fn git_sha_like_is_not_secret() {
        assert!(find_secret_token("merge 4c536992 into main").is_none());
        assert!(find_secret_token("0fda534e0fda534e0fda534e0fda534e0fda534e").is_none());
    }

    #[test]
    fn secret_token_is_blocked() {
        // A real high-entropy, mixed-case+digit credential token must still
        // be rejected, and the reject must name the (redacted) trigger.
        let cfg = FilterConfig::default();
        let content = "Use this token AbCd1234EfGh5678IjKl9012 to authenticate the deploy webhook"; // pragma: allowlist secret
        match cfg.apply(content, true) {
            Err(FilterReject::PotentialSecret { token }) => {
                assert!(token.contains("AbCd"), "should name token: {token}");
                assert!(token.contains("chars"), "should redact tail: {token}");
            }
            other => panic!("expected PotentialSecret, got {other:?}"),
        }
    }

    #[test]
    fn base64_blob_is_blocked() {
        let content =
            "config blob: aGVsbG8rd29ybGQvZm9vK2Jhcj09bG9uZ2Jhc2U2NA== embedded in the note"; // pragma: allowlist secret
        assert!(matches!(
            FilterConfig::default().apply(content, false),
            Err(FilterReject::PotentialSecret { .. })
        ));
    }

    #[test]
    fn known_key_prefixes_are_blocked() {
        for tok in [
            "sk-abcdef0123456789abcdef01",              // pragma: allowlist secret
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789", // pragma: allowlist secret
            "xoxb-1234-5678-abcdEFGH",                  // pragma: allowlist secret
        ] {
            assert!(
                looks_like_secret(tok),
                "prefix token should be a secret: {tok}"
            );
        }
    }

    #[test]
    fn ordinary_words_are_not_secret() {
        for tok in [
            "authentication",
            "configuration",
            "refactoring",
            "snake_case",
        ] {
            assert!(!looks_like_secret(tok), "ordinary word flagged: {tok}");
        }
    }

    #[test]
    fn non_alpha_masking_ignores_shas() {
        // Masking turns SHAs into "sha" so the ratio reflects the prose only.
        let masked = mask_git_shas("merge 4c536992 -> 0fda534e done");
        assert!(masked.contains("sha"));
        assert!(!masked.contains("4c536992"));
    }

    #[test]
    fn redact_token_masks_tail() {
        let r = redact_token("AbCd1234EfGh5678IjKl9012"); // pragma: allowlist secret
        assert!(r.starts_with("AbCd"));
        assert!(r.contains("…"));
        assert!(r.contains("chars"));
        assert!(!r.contains("9012"), "tail must be masked: {r}");
    }

    #[test]
    fn filter_config_force_bypasses_all() {
        // The `force` semantics live in the caller (we model it by skipping
        // `apply` entirely); this test exists so the contract is documented:
        // there is no way *inside* `apply` to bypass — the caller must not
        // call it at all to force-store.
        let cfg = FilterConfig::default();
        assert!(cfg.apply("Tool use: x", true).is_err());
    }
}
