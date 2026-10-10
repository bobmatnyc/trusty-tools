//! The two lexical-lane arguments on the search tools (#9258).
//!
//! Why: `search`, the four per-lane tools and the `search_all` fan-out all
//! forward the same pair to the daemon; one reader keeps the type checks and
//! the schema text identical across them.
//! What: [`forward`] copies `ripgrep_fallback` (boolean) and `lexical_limit`
//! (positive integer) into a daemon body, rejecting a wrong type. The daemon
//! owns the range check. [`annotate`] adds both to each tool's schema.
//! Test: `lexical_lane_args_are_forwarded_and_type_checked`,
//! `search_tools_advertise_the_lexical_lane_args` in `tests_lexical_args.rs`.

use serde_json::Value;

use super::types::{optional_bool, DispatchError};

/// Tools whose schema carries the two arguments.
pub(super) const LEXICAL_LANE_TOOLS: &[&str] = &[
    "search",
    "search_lexical",
    "search_semantic",
    "search_kg",
    "search_all",
];

const RIPGREP_NOTE: &str = "false stops the content-scan lanes from adding rows, so no hit is labelled `fallback:ripgrep` (issue #9258). Omit for the daemon default (on).";
const LIMIT_NOTE: &str = "Candidate depth of each lexical lane (BM25, grep, exact-match), 1..=10000 (issue #9258). Omit for the daemon default (top_k x 4). Out of range is an error, never a clamp.";

/// Copy the lexical-lane arguments from `args` into the daemon `body`.
///
/// Why: a dropped or retyped value would answer a different question than the
/// one asked, the #7927 trap.
/// What: absent or `null` forwards nothing; a non-boolean `ripgrep_fallback`
/// or a non-integer `lexical_limit` is `InvalidParams`.
/// Test: `lexical_lane_args_are_forwarded_and_type_checked`.
pub(super) fn forward(args: &Value, body: &mut Value) -> Result<(), DispatchError> {
    if let Some(on) = optional_bool(args, "ripgrep_fallback", RIPGREP_NOTE)? {
        body["ripgrep_fallback"] = Value::Bool(on);
    }
    match args.get("lexical_limit") {
        None | Some(Value::Null) => {}
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| {
                DispatchError::InvalidParams(format!(
                    "lexical_limit must be a positive integer ({LIMIT_NOTE}); got {v}"
                ))
            })?;
            body["lexical_limit"] = Value::from(n);
        }
    }
    Ok(())
}

/// Add both arguments to every tool in [`LEXICAL_LANE_TOOLS`].
///
/// Test: `search_tools_advertise_the_lexical_lane_args`.
pub(super) fn annotate(defs: &mut Value) {
    let Some(tools) = defs.as_array_mut() else {
        return;
    };
    for tool in tools.iter_mut() {
        let named = tool
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|n| LEXICAL_LANE_TOOLS.contains(&n));
        if !named {
            continue;
        }
        let Some(props) = tool
            .get_mut("inputSchema")
            .and_then(|s| s.get_mut("properties"))
            .and_then(Value::as_object_mut)
        else {
            continue;
        };
        props.insert(
            "ripgrep_fallback".into(),
            serde_json::json!({ "type": "boolean", "description": RIPGREP_NOTE }),
        );
        props.insert(
            "lexical_limit".into(),
            serde_json::json!({ "type": "integer", "minimum": 1, "maximum": crate::core::indexer::MAX_LEXICAL_LIMIT, "description": LIMIT_NOTE }),
        );
    }
}
