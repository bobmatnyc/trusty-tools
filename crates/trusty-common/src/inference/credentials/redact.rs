//! The one credential-masking implementation (issue #2401).
//!
//! Why: `memory_core::filter` had its own private `redact_token` (issue
//! #1481) with the exact masking shape this ticket's `config` clap module
//! (Wave 2) also needs: "never echo the secret back verbatim, but show
//! enough of it that a human can identify *which* credential tripped a
//! check". Two independent implementations of the same shape is exactly the
//! duplication BASE-ENGINEER's consolidation rule targets — this module is
//! now the single implementation; `memory_core::filter::redact_token`
//! delegates to it.
//! What: [`redact_secret`] returns `{head}…({N} chars)` where `head` is the
//! first 4 characters and `N` is the input's byte length — the exact shape
//! `memory_core::filter`'s prior private implementation produced, verified
//! by the pre-existing `redact_token_masks_tail` test which now exercises
//! this function transitively.
//! Test: `redact_tests` (sibling file, this module) and
//! `memory_core::filter_tests::redact_token_masks_tail` (format stability).

/// Head length used by [`redact_secret`]'s default (4 characters), matching
/// the format `memory_core::filter`'s prior private `redact_token` produced.
const DEFAULT_HEAD_LEN: usize = 4;

/// Produce a short, non-reversible preview of `secret`.
///
/// Why: callers (rejection messages, `config list --show-status`, log lines)
/// need to name *which* credential they're referring to without ever
/// echoing the value back. See module docs for the format-stability
/// rationale.
/// What: returns the first [`DEFAULT_HEAD_LEN`] characters of `secret`
/// followed by `…` and `(<byte length> chars)`. Secrets shorter than the
/// head length are returned with their full (short) prefix — there is no
/// minimum-length guard here; callers gate on "is this actually secret-
/// shaped" before calling, same as `memory_core::filter::looks_like_secret`
/// did for its 20-char floor.
/// Test: `redact_tests::redact_secret_masks_tail`,
/// `redact_tests::redact_secret_handles_short_input`.
pub fn redact_secret(secret: &str) -> String {
    let head: String = secret.chars().take(DEFAULT_HEAD_LEN).collect();
    format!("{head}…({} chars)", secret.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: pins the exact format `memory_core::filter` depends on.
    /// Test: itself.
    #[test]
    fn redact_secret_masks_tail() {
        let r = redact_secret("AbCd1234EfGh5678IjKl9012"); // pragma: allowlist secret
        assert!(r.starts_with("AbCd"));
        assert!(r.contains('…'));
        assert!(r.contains("chars"));
        assert!(!r.contains("9012"), "tail must be masked: {r}");
    }

    /// Why: short inputs must not panic (no out-of-bounds slicing).
    /// Test: itself.
    #[test]
    fn redact_secret_handles_short_input() {
        assert_eq!(redact_secret(""), "…(0 chars)");
        assert_eq!(redact_secret("ab"), "ab…(2 chars)");
    }
}
