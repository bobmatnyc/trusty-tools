//! Truncation detection for the reviewer's response (#1241, #1357).
//!
//! Why: split from `runner.rs` (#8905) to keep it under the 500-SLOC cap.
//! What: the token-ratio fallback and the `finish_reason` check.
//! Test: `is_truncated_*` and `truncation_ratio_*` in `runner_tests.rs`.

/// Default fraction of the output-token ceiling at/above which a response is
/// treated as truncated when no `finish_reason` is available (closes #1241).
///
/// Why: this is the FALLBACK heuristic.  Some providers stop generating exactly
/// at `max_tokens` without surfacing a `finish_reason`; when the completion lands
/// at ≥95 % of the ceiling the structured JSON is very likely cut off mid-object,
/// so trusting its parse risks a silent wrong-APPROVE.  95 % (not 100 %) leaves a
/// small margin for provider-side token-count rounding so a genuinely-complete
/// response that lands a few tokens under the ceiling is not mis-flagged.  As of
/// #1357 this ratio is only consulted when `finish_reason` is absent — a provider
/// that reports `finish_reason: "stop"` at 99 % of the ceiling is NOT flagged.
/// What: the default multiplier applied to `max_tokens`, overridable at runtime
/// via `truncation_token_ratio` (env seam) so operators can retune without a
/// rebuild.
/// Test: `is_truncated_ratio_fallback_*` unit tests in `runner_tests.rs`.
pub(super) const DEFAULT_TRUNCATION_TOKEN_RATIO: f64 = 0.95;

/// Environment variable that overrides [`DEFAULT_TRUNCATION_TOKEN_RATIO`].
///
/// Why: #1357 asked for the fallback ratio to be configurable.  A single env seam
/// (rather than threading a config field through every call site) keeps the change
/// small while still letting operators retune the fallback band without a rebuild.
/// What: parsed as `f64` in `truncation_token_ratio`; ignored when unset, empty,
/// unparseable, or outside `(0.0, 1.0]`.
pub(super) const TRUNCATION_TOKEN_RATIO_ENV: &str = "TRUSTY_REVIEW_TRUNCATION_TOKEN_RATIO";

/// Resolve the effective truncation token ratio (env override, else default).
///
/// Why: centralises the configurable-ratio seam (#1357) so both the runner and its
/// tests read the ratio through one place; an out-of-range or unparseable override
/// falls back to the default rather than silently disabling the safety check.
/// What: reads `TRUSTY_REVIEW_TRUNCATION_TOKEN_RATIO`; returns the parsed value when
/// it is a finite `f64` in `(0.0, 1.0]`, else `DEFAULT_TRUNCATION_TOKEN_RATIO`.
/// Test: `truncation_ratio_env_override_applies`, `truncation_ratio_env_invalid_falls_back`.
pub(super) fn truncation_token_ratio() -> f64 {
    match std::env::var(TRUNCATION_TOKEN_RATIO_ENV) {
        Ok(raw) => match raw.trim().parse::<f64>() {
            Ok(v) if v.is_finite() && v > 0.0 && v <= 1.0 => v,
            _ => DEFAULT_TRUNCATION_TOKEN_RATIO,
        },
        Err(_) => DEFAULT_TRUNCATION_TOKEN_RATIO,
    }
}

/// Return `true` when an LLM completion appears truncated at the token ceiling.
///
/// Why: a truncated reviewer response must fail CLOSED to UNKNOWN rather than be
/// parsed into a (likely wrong) APPROVE — the #1241 safety fix.  Before #1357 the
/// detection was purely arithmetic (token-ratio), which FALSE-POSITIVED on large
/// but complete responses that legitimately landed in the ≥95 % band.  The
/// provider's own `finish_reason` is the authoritative truncation signal, so #1357
/// makes it PRIMARY and keeps the token-ratio only as a fallback when the provider
/// did not surface a reason.
/// What:
///   1. PRIMARY — when `finish_reason` is present: return `true` iff it is a
///      length/truncation reason (`"length"` / `"max_tokens"` / `"max_token"`),
///      and `false` for any natural-stop reason (`"stop"`, `"end_turn"`, …).  The
///      token ratio is NOT consulted, so a complete response at 99 % of the ceiling
///      is not mis-flagged.
///   2. FALLBACK — when `finish_reason` is `None`: return `true` when `max_tokens > 0`
///      AND `output_tokens >= ceil(max_tokens * truncation_token_ratio())`.  A
///      `max_tokens` of 0 (unknown ceiling) disables the check (returns `false`).
///
/// `finish_reason` is matched case-insensitively (providers already lowercase it,
/// but we trim/lowercase defensively).
///
/// Test: `is_truncated_finish_reason_length_true`,
/// `is_truncated_finish_reason_stop_at_high_ratio_false`,
/// `is_truncated_ratio_fallback_at_ceiling_true`,
/// `is_truncated_ratio_fallback_well_under_false`,
/// `is_truncated_unset_ceiling_false`.
pub(super) fn is_truncated(
    finish_reason: Option<&str>,
    output_tokens: u32,
    max_tokens: u32,
) -> bool {
    // PRIMARY: trust the provider's explicit completion reason when present.
    if let Some(reason) = finish_reason {
        let r = reason.trim().to_ascii_lowercase();
        if !r.is_empty() {
            return matches!(r.as_str(), "length" | "max_tokens" | "max_token");
        }
    }

    // FALLBACK: no usable finish_reason — use the token-ratio heuristic.
    if max_tokens == 0 {
        return false;
    }
    let threshold = (f64::from(max_tokens) * truncation_token_ratio()).ceil() as u32;
    output_tokens >= threshold
}

// ─── Dedup claim gate (#5064) ────────────────────────────────────────────────
