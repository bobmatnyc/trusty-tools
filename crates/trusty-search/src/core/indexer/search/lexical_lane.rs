//! The caller-tunable lexical lane: the content-scan switch and the BM25
//! candidate depth (#9258).
//!
//! Why: a caller measuring BM25 coverage cannot tell a BM25 hit from one the
//! content-scan lanes (`grep_fallback_search`, the #7675 exact-match lane)
//! added, because both reach the page — a scan-only hit carries the
//! `fallback:ripgrep` label. It also cannot bound how deep BM25 reaches. Both
//! knobs are per query, with daemon-config defaults; the per-query value wins.
//! What: [`LexicalLaneDefaults`] (the daemon-config pair),
//! [`SearchQuery::apply_lexical_defaults`] (fills unset fields and validates),
//! and the two readers the search orchestrator uses. An invalid limit is an
//! error, never a clamp.
//! Test: `tests_lexical_lane_9258` in `service::server`, and this module's
//! unit tests.

use super::super::SearchQuery;

/// Largest accepted `lexical_limit`. A deeper BM25 walk is refused, not clamped.
pub const MAX_LEXICAL_LIMIT: usize = 10_000;

/// Daemon-wide defaults for the two lexical-lane knobs (#9258).
///
/// Why: a measurement harness wants BM25-only answers without editing every
/// request; the daemon config (`[search]` in `config.toml`) supplies them.
/// What: `ripgrep_fallback` defaults to `true` and `lexical_limit` to `None`
/// (`top_k × HNSW_OVERSAMPLE`), so an absent config changes nothing.
/// Test: `config_defaults_apply_when_the_query_is_silent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LexicalLaneDefaults {
    /// Whether the content-scan lanes may add candidates.
    pub ripgrep_fallback: bool,
    /// BM25 / content-scan candidate depth; `None` keeps the oversample.
    pub lexical_limit: Option<usize>,
}

impl Default for LexicalLaneDefaults {
    fn default() -> Self {
        Self {
            ripgrep_fallback: true,
            lexical_limit: None,
        }
    }
}

/// An out-of-range `lexical_limit`, naming where the value came from.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LexicalLimitError {
    /// `0` would silently empty every lexical lane.
    #[error("lexical_limit must be at least 1, got 0 (from {origin})")]
    Zero { origin: &'static str },
    /// Above [`MAX_LEXICAL_LIMIT`].
    #[error("lexical_limit {value} exceeds the maximum {max} (from {origin})")]
    TooLarge {
        value: usize,
        max: usize,
        origin: &'static str,
    },
}

/// `origin` label for a value the request carried.
pub const ORIGIN_REQUEST: &str = "the request";
/// `origin` label for a value the daemon config supplied.
pub const ORIGIN_CONFIG: &str = "daemon config [search].lexical_limit";

/// Accept `limit` when it lies in `1..=MAX_LEXICAL_LIMIT`.
///
/// Test: `lexical_limit_bounds_are_enforced`.
pub fn check_lexical_limit(limit: usize, origin: &'static str) -> Result<usize, LexicalLimitError> {
    match limit {
        0 => Err(LexicalLimitError::Zero { origin }),
        v if v > MAX_LEXICAL_LIMIT => Err(LexicalLimitError::TooLarge {
            value: v,
            max: MAX_LEXICAL_LIMIT,
            origin,
        }),
        v => Ok(v),
    }
}

impl SearchQuery {
    /// Fill the unset lexical-lane fields from `defaults`, then validate.
    ///
    /// Why: one resolution step shared by the per-index and fan-out handlers,
    /// so an explicit per-query value always beats the daemon config.
    /// What: an unset `ripgrep_fallback` takes the config value; an unset
    /// `lexical_limit` takes the config value. The effective limit is checked
    /// against `1..=MAX_LEXICAL_LIMIT`, and the error names its origin.
    /// Test: `explicit_query_values_override_config_defaults`,
    /// `an_invalid_lexical_limit_is_rejected_not_clamped`.
    pub fn apply_lexical_defaults(
        &mut self,
        defaults: &LexicalLaneDefaults,
    ) -> Result<(), LexicalLimitError> {
        self.ripgrep_fallback
            .get_or_insert(defaults.ripgrep_fallback);
        match (self.lexical_limit, defaults.lexical_limit) {
            (Some(v), _) => check_lexical_limit(v, ORIGIN_REQUEST).map(drop),
            (None, Some(v)) => {
                self.lexical_limit = Some(check_lexical_limit(v, ORIGIN_CONFIG)?);
                Ok(())
            }
            (None, None) => Ok(()),
        }
    }

    /// Whether the content-scan lanes may add candidates (default `true`).
    pub(crate) fn ripgrep_lane_on(&self) -> bool {
        self.ripgrep_fallback.unwrap_or(true)
    }

    /// The lexical lanes' candidate depth: `lexical_limit`, else `oversample`.
    ///
    /// Why: a library caller that skipped [`Self::apply_lexical_defaults`]
    /// must still get the error, never a clamp.
    /// Test: `lexical_limit_bounds_are_enforced`.
    pub(crate) fn lexical_want(&self, oversample: usize) -> Result<usize, LexicalLimitError> {
        self.lexical_limit
            .map_or(Ok(oversample), |v| check_lexical_limit(v, ORIGIN_REQUEST))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_limit_bounds_are_enforced() {
        assert_eq!(check_lexical_limit(1, ORIGIN_REQUEST), Ok(1));
        assert_eq!(
            check_lexical_limit(MAX_LEXICAL_LIMIT, ORIGIN_REQUEST),
            Ok(MAX_LEXICAL_LIMIT)
        );
        assert!(matches!(
            check_lexical_limit(0, ORIGIN_REQUEST),
            Err(LexicalLimitError::Zero { .. })
        ));
        assert!(matches!(
            check_lexical_limit(MAX_LEXICAL_LIMIT + 1, ORIGIN_CONFIG),
            Err(LexicalLimitError::TooLarge { origin, .. }) if origin == ORIGIN_CONFIG
        ));
        let q = SearchQuery {
            lexical_limit: Some(0),
            ..SearchQuery::default()
        };
        assert!(q.lexical_want(40).is_err(), "an unvalidated 0 must not run");
        assert_eq!(SearchQuery::default().lexical_want(40), Ok(40));
    }

    #[test]
    fn absent_config_leaves_the_query_at_todays_defaults() {
        let mut q = SearchQuery::default();
        q.apply_lexical_defaults(&LexicalLaneDefaults::default())
            .expect("defaults are valid");
        assert!(q.ripgrep_lane_on());
        assert_eq!(q.lexical_limit, None);
    }
}
