//! Daemon-config resolution of the lexical-lane knobs for both search routes
//! (#9258).
//!
//! Why: the per-index route and the fan-out must apply the `[search]` config
//! the same way and refuse a bad limit with the same body.
//! What: [`resolve`] fills unset `ripgrep_fallback` / `lexical_limit` from
//! [`SearchAppState::lexical_defaults`] and turns an out-of-range limit into
//! `400 invalid_lexical_limit`.
//! Test: `tests_lexical_lane_9258`.

use axum::http::StatusCode;

use super::SearchAppState;
use crate::core::indexer::{SearchQuery, MAX_LEXICAL_LIMIT};

/// Apply the daemon's lexical-lane defaults to `query`, or refuse it.
///
/// Why: an explicit per-query value must win over config, and an invalid
/// limit must stop the request instead of running at some other depth.
/// What: delegates to [`SearchQuery::apply_lexical_defaults`]; its error
/// becomes a 400 whose `message` names the bad value and where it came from.
/// Test: `an_invalid_lexical_limit_is_rejected_not_clamped`,
/// `explicit_query_values_override_config_defaults`.
pub(super) fn resolve(
    state: &SearchAppState,
    query: &mut SearchQuery,
) -> Result<(), (StatusCode, serde_json::Value)> {
    query
        .apply_lexical_defaults(&state.lexical_defaults)
        .map_err(|e| {
            (
                StatusCode::BAD_REQUEST,
                serde_json::json!({
                    "error": "invalid_lexical_limit",
                    "message": e.to_string(),
                    "max_lexical_limit": MAX_LEXICAL_LIMIT,
                }),
            )
        })
}
