//! trusty-search library crate.
//!
//! Why: Exposes the previously separate `trusty-search-core`, `-service`, and
//! `-mcp` sub-crate modules under a single library target so integration tests
//! (and downstream consumers) can reach the internal APIs after the workspace
//! was consolidated into one crate.
//! What: Re-publishes `core`, `service`, and `mcp` as `pub mod`s. The `main`
//! binary uses these via `crate::core::...`; integration tests use
//! `trusty_search::core::...`.
//! Test: `cargo build --lib` succeeds; `cargo test` runs integration tests
//! that import `trusty_search::core::*`.

pub mod allowlist;
pub mod config;
pub mod core;
pub mod mcp;
pub mod service;

// Why: surface the unified `rpc.discover` service descriptor at the crate
// root so open-mpm and other host processes can `use trusty_search::SearchMcpService`
// without traversing into the internal module layout (closes #115).
pub use service::SearchMcpService;

/// Compute the tokio worker-thread count for this machine.
///
/// Why (issue #1006): raising the floor to 16 prevents accept-loop starvation
/// when embed-pool workers block on 30 s CoreML/CUDA sidecar calls; with only
/// `available_parallelism` workers (e.g. 8 on a 4-core box) and tasks blocking
/// on long embed calls, the axum accept loop starves.
/// What: returns `max(cpu_count, 16)`. The result is always `>= 16`.
/// Test: `worker_thread_count_at_least_16` in `tests_state.rs` — asserts the
/// floor with `cpu_count=1` (→ 16) and the pass-through with `cpu_count=32` (→ 32).
pub fn worker_thread_count(cpu_count: usize) -> usize {
    std::cmp::max(cpu_count, 16)
}

/// Truncate `s` to at most `max_bytes` bytes without splitting a multi-byte
/// UTF-8 character.
///
/// Why (issue #3685): the query-log truncation sites used `&s[..n.min(80)]`,
/// a raw byte-index slice. When byte 80 landed inside a multi-byte character
/// (e.g. an emoji or CJK query), that slice panicked with "byte index is not
/// a char boundary" and crashed the search request instead of just logging
/// it.
/// What: walks backward from `max_bytes` to the nearest char boundary (the
/// same backward-scan pattern already used in
/// `core::extract::extract_text`'s byte-cap truncation) and returns that
/// prefix. Returns `s` unchanged when it already fits within `max_bytes`
/// bytes.
/// Test: `truncate_at_char_boundary_*` in this module — covers an ASCII
/// string under/over the limit and a multibyte string whose raw byte 80 (or
/// other cap) falls mid-character.
pub fn truncate_at_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut idx = max_bytes;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    &s[..idx]
}

// Regression tests for persistence data-integrity fixes (#1088, #1089, #1090).
// Extracted from persistence.rs to keep that file under its line-cap budget.
#[cfg(test)]
#[path = "service/persistence_tests_1088.rs"]
mod persistence_tests_1088;

#[cfg(test)]
mod truncate_at_char_boundary_tests {
    use super::truncate_at_char_boundary;

    /// Regression test for issue #3685: a query string whose 80th byte lands
    /// mid-way through a multibyte emoji used to panic with the old
    /// `&s[..80]` byte-slice truncation. This constructs a string that is
    /// exactly 80 bytes long up through the middle of a 4-byte emoji, so a
    /// naive `[..80]` cut is guaranteed to split it.
    #[test]
    fn truncate_at_char_boundary_handles_multibyte_split() {
        // 79 ASCII bytes + a 4-byte emoji (🔥, U+1F525) pushes the emoji's
        // start to byte 79 and its end to byte 83 — byte index 80 sits
        // squarely inside the emoji's UTF-8 encoding.
        let s = format!("{}{}", "a".repeat(79), "🔥 rest of the query text");
        assert_eq!(
            s.as_bytes()[80].leading_ones(),
            1,
            "sanity: byte 80 must be a UTF-8 continuation byte, not a boundary"
        );

        // Old behavior `&s[..s.len().min(80)]` would panic here.
        let truncated = truncate_at_char_boundary(&s, 80);

        assert!(s.is_char_boundary(truncated.len()));
        assert!(truncated.len() <= 80);
        assert_eq!(
            truncated,
            &s[..79],
            "must back off to the boundary before the split emoji"
        );
    }

    #[test]
    fn truncate_at_char_boundary_leaves_short_ascii_unchanged() {
        let s = "short query";
        assert_eq!(truncate_at_char_boundary(s, 80), s);
    }

    #[test]
    fn truncate_at_char_boundary_cuts_long_ascii_at_exactly_max() {
        let s = "x".repeat(200);
        let truncated = truncate_at_char_boundary(&s, 80);
        assert_eq!(truncated.len(), 80);
        assert_eq!(truncated, &s[..80]);
    }

    #[test]
    fn truncate_at_char_boundary_handles_cjk_at_limit() {
        // Each CJK character below is 3 bytes in UTF-8; 27 repeats = 81 bytes,
        // so the 80-byte cap lands mid-character (80 is not a multiple of 3).
        let s = "検".repeat(27);
        assert_eq!(s.len(), 81);

        let truncated = truncate_at_char_boundary(&s, 80);

        assert!(s.is_char_boundary(truncated.len()));
        assert_eq!(
            truncated,
            &s[..78],
            "must back off to the last full 3-byte char before byte 80"
        );
    }
}
