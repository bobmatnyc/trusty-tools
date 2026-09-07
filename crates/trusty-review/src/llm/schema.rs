//! OpenAI strict-mode JSON Schema enforcement.
//!
//! Why: OpenAI's structured-output (`response_format: json_schema`, `strict:
//! true`) mode — which OpenRouter forwards verbatim for `openai/*` models —
//! rejects any schema in which an `object` node either omits
//! `"additionalProperties": false` or fails to list every one of its
//! `properties` keys in `"required"`.  Hand-written schemas reliably miss one
//! of these on nested objects (e.g. `findings.items`), producing
//! `Invalid schema for response_format 'review_output': ...
//! 'additionalProperties' is required to be supplied and to be false`, which
//! blocks EVERY OpenAI review.  Bedrock/Anthropic and Gemini are lenient and
//! accept the stricter-but-valid form unchanged, so enforcing strict mode for
//! all providers is safe.
//!
//! What: [`enforce_strict_mode`] delegates to
//! [`trusty_common::inference::strict_json_schema`], which recursively walks a
//! `serde_json::Value` JSON Schema and, for every node typed `"object"` (or
//! carrying a `properties` map), sets `"additionalProperties": false` and
//! rewrites `"required"` to list every property key.  It also descends into
//! array `items`, nested object `properties`, `$defs`/`definitions`,
//! `anyOf`/`oneOf`/`allOf`, and the `additionalProperties` schema when it is
//! itself an object schema (rather than the boolean `false`).
//!
//! Test: see `schema_tests.rs` — covers the canonical review/verify shapes,
//! nested arrays-of-objects, idempotency, and the recursive invariant
//! (`assert_object_nodes_strict`).

use serde_json::Value;
use trusty_common::inference::strict_json_schema;

/// Recursively make a JSON Schema OpenAI strict-mode compliant in place.
///
/// Why: a single source of truth for strict-mode compliance means individual
/// schema builders (`review_response_schema`, `verify_response_schema`) never
/// have to hand-maintain `additionalProperties`/`required` on every nested
/// object — they declare the shape and call this once, eliminating the class
/// of bug where a nested object silently violates strict mode. #7082: that
/// source of truth is now `trusty-common`'s, because the same defect reached
/// trusty-git-analytics through a schema this crate never sees, and the
/// workspace's rule is one implementation per cross-crate capability. This
/// function stays as the crate's name for it, so `ResponseSchema::new` and the
/// `all_schemas_tests.rs` scan are unchanged.
/// What: replaces `schema` with
/// [`trusty_common::inference::strict_json_schema`] applied to it — for every
/// object node, `"additionalProperties": false` and a `"required"` listing
/// exactly the keys under `"properties"` (sorted for deterministic output),
/// recursing through `properties`, array `items` (object or tuple),
/// `$defs`/`definitions`, `anyOf`/`oneOf`/`allOf`, and a schema-valued
/// `additionalProperties`. Non-object values are returned unchanged.
/// Test: `enforces_additional_properties_false`, `marks_all_properties_required`,
/// `recurses_into_array_items`, `is_idempotent` in `schema_tests.rs`.
pub fn enforce_strict_mode(schema: &mut Value) {
    // #7082: one implementation, in trusty-common, on the wire path every
    // OpenAI-dialect caller already crosses.
    *schema = strict_json_schema(schema);
}

#[cfg(test)]
#[path = "schema_tests.rs"]
mod tests;

// Re-export the recursive strict-mode assertion so the prompt-schema tests in
// `pipeline::prompt_tests` reuse the single canonical implementation instead of
// duplicating it. `pub(crate)` keeps it test-only (the `tests` module is
// `#[cfg(test)]`) and crate-private.
#[cfg(test)]
pub(crate) use tests::assert_object_nodes_strict;
