//! Bedrock `requestMetadata` tags for every Converse call trusty-review sends.
//!
//! Why: the Bedrock cost report attributes spend by reading model invocation
//! logs, and those logs carry a request's `requestMetadata` verbatim. Without a
//! tag, trusty-review's spend cannot be told apart from other use of the same
//! AWS account.
//! What: [`request_metadata`] returns the key/value pairs `call_once` attaches
//! to the Converse request: `caller=trusty-review` and `crate_version` always,
//! plus `role` when the request's response schema identifies the call's role.
//! A request with no schema, or a schema this table does not name, carries no
//! `role` pair rather than a guessed one.
//! Test: `converse_request_carries_caller_and_role_metadata`,
//! `schemaless_request_carries_caller_without_role`,
//! `every_schema_builder_maps_to_its_role` in `request_metadata_tests.rs`.

use crate::llm::LlmRequest;

/// `requestMetadata` key naming the tool that sent the request.
pub(crate) const CALLER_KEY: &str = "caller";
/// Value of [`CALLER_KEY`] on every trusty-review Converse request.
pub(crate) const CALLER: &str = "trusty-review";
/// `requestMetadata` key naming the call's pipeline role.
pub(crate) const ROLE_KEY: &str = "role";
/// `requestMetadata` key carrying this crate's version.
pub(crate) const CRATE_VERSION_KEY: &str = "crate_version";

/// Map a response-schema name to the pipeline role that sends it.
///
/// Why: `LlmRequest` has no role field (architect ruling: keep it that way),
/// but each response schema is built by exactly one role's module, so its
/// name identifies the role without guessing.
/// What: returns the role label for the six known schema names, `None` for
/// any other name. The literals mirror the schema builders; the test below
/// fails if a builder renames its schema.
/// Test: `every_schema_builder_maps_to_its_role`.
fn role_for_schema(name: &str) -> Option<&'static str> {
    match name {
        "review_output" => Some("reviewer"),
        "verification_judgment" | "verification_judgments" => Some("verifier"),
        "report_synthesis" => Some("synthesis"),
        "trace_verdict" | "repo_investigation" => Some("investigate"),
        _ => None,
    }
}

/// The `requestMetadata` pairs for one Converse request.
///
/// What: `caller` and `crate_version` always; `role` only when
/// [`role_for_schema`] knows the request's schema. At most three pairs, all
/// values far below AWS's 256-character limit.
/// Test: `converse_request_carries_caller_and_role_metadata`,
/// `schemaless_request_carries_caller_without_role`.
pub(crate) fn request_metadata(req: &LlmRequest) -> Vec<(&'static str, String)> {
    let mut pairs = vec![
        (CALLER_KEY, CALLER.to_string()),
        (CRATE_VERSION_KEY, env!("CARGO_PKG_VERSION").to_string()),
    ];
    if let Some(role) = req
        .response_schema
        .as_ref()
        .and_then(|s| role_for_schema(&s.name))
    {
        pairs.push((ROLE_KEY, role.to_string()));
    }
    pairs
}

#[cfg(test)]
#[path = "request_metadata_tests.rs"]
mod tests;
