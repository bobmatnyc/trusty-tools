//! URL scheme-stripping utilities shared across the console crate.
//!
//! Why: Both the service-connector layer (`detect/helpers.rs`) and the
//! reverse-proxy layer (`proxy/routes.rs`) need to strip leading `http://` /
//! `https://` scheme prefixes from discovered service addresses.  Centralising
//! the core loop in one place prevents the logic from drifting between the two
//! sites.
//! What: `strip_schemes` is the single implementation; callers wrap it with
//! their domain-specific formatting or normalization.
//! Test: `test_strip_schemes_*` below; callers add domain-specific tests on
//! top.

/// Strip all leading `http://` and `https://` scheme prefixes from a string.
///
/// Why: Discovery addr files should contain bare `host:port` (e.g.
/// `127.0.0.1:7788`), but a misconfigured or round-tripped value may include
/// one or more scheme prefixes.  Removing all prefixes before composing a URL
/// guarantees exactly one scheme at the call site and prevents double-scheme
/// URLs like `http://http://127.0.0.1:7788`.
/// What: Loops stripping `http://` or `https://` from the front of `s` until
/// neither prefix is present; returns the remaining string slice (borrowed from
/// `s`).  Allocation-free and idempotent.
/// Test: `test_strip_schemes_*` below.
pub(crate) fn strip_schemes(s: &str) -> &str {
    let mut rest = s;
    loop {
        if let Some(t) = rest.strip_prefix("http://") {
            rest = t;
        } else if let Some(t) = rest.strip_prefix("https://") {
            rest = t;
        } else {
            break;
        }
    }
    rest
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: a bare addr must pass through unchanged — no allocations, no
    /// modification.
    /// What: calls strip_schemes("127.0.0.1:7788"); asserts identity.
    /// Test: this test itself.
    #[test]
    fn test_strip_schemes_bare_unchanged() {
        assert_eq!(strip_schemes("127.0.0.1:7788"), "127.0.0.1:7788");
    }

    /// Why: a single http:// prefix must be stripped.
    /// What: calls strip_schemes("http://127.0.0.1:7788"); asserts bare addr.
    /// Test: this test itself.
    #[test]
    fn test_strip_schemes_strips_http() {
        assert_eq!(strip_schemes("http://127.0.0.1:7788"), "127.0.0.1:7788");
    }

    /// Why: a single https:// prefix must be stripped.
    /// What: calls strip_schemes("https://127.0.0.1:7788"); asserts bare addr.
    /// Test: this test itself.
    #[test]
    fn test_strip_schemes_strips_https() {
        assert_eq!(strip_schemes("https://127.0.0.1:7788"), "127.0.0.1:7788");
    }

    /// Why: a double http:// (produced when detect_service prepends http:// to
    /// an addr file that already contains http://) must be fully stripped.
    /// What: calls strip_schemes("http://http://127.0.0.1:7788"); asserts bare addr.
    /// Test: this test itself (double-scheme regression guard for #1849 Phase 2).
    #[test]
    fn test_strip_schemes_strips_double_http() {
        assert_eq!(
            strip_schemes("http://http://127.0.0.1:7788"),
            "127.0.0.1:7788"
        );
    }

    /// Why: a mixed https+http stack (e.g. https:// over http://) must be fully
    /// stripped.
    /// What: calls strip_schemes("https://http://127.0.0.1:7788"); asserts bare addr.
    /// Test: this test itself.
    #[test]
    fn test_strip_schemes_strips_mixed_https_http() {
        assert_eq!(
            strip_schemes("https://http://127.0.0.1:7788"),
            "127.0.0.1:7788"
        );
    }
}
