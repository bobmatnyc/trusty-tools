//! The machine-readable result of one per-invocation review (#6290).
//!
//! Why: retiring the review daemon removed `review.run`, the only way a caller
//! could get a review back as structured data instead of as prose on a
//! terminal. Everything that used it — CI gates, editor integrations, the 782
//! reviews #5028 measured through `/review` — needs the SAME shape back from
//! `trusty-review run --json`, or "the daemon is retired" is a data-format
//! break wearing a refactor's name.
//!
//! What: [`run_json_payload`] is that shape, and it is deliberately the
//! identity serialisation of [`ReviewResult`] — the exact value the retired
//! JSON-RPC router put in its `result` field. Writing it as a named function
//! rather than inlining `serde_json::to_value` at the call site is what gives
//! the parity test something to hold onto.
//!
//! [`run_is_failure`] is the other half, and it is a behaviour CHANGE, not a
//! move. `cmd_run` used to exit non-zero only for `ReviewStatus::Skipped`, so a
//! provider outage — which `pipeline::runner_helpers::abort_dry` records as
//! `error: Some(..)` on an otherwise `Completed` result — exited 0 with an
//! UNKNOWN verdict and no findings. A CI gate reading that exit code passed the
//! PR. Both failure shapes now exit non-zero, with the reason in the JSON.
//!
//! Test: `run_json_matches_the_rpc_result_shape`,
//! `run_is_failure_catches_a_provider_error`,
//! `run_is_failure_catches_a_skipped_review`,
//! `run_is_failure_passes_a_clean_review`.

use serde_json::Value;

use crate::models::ReviewResult;

/// The JSON `trusty-review run --json` prints for one review.
///
/// Why: byte-for-byte the payload the retired `review.run` method returned, so
/// a caller that parsed the daemon's answer parses this unchanged. See the
/// module docs for why that identity is the point rather than an accident.
///
/// What: `serde_json::to_value(result)` — [`ReviewResult`]'s own `Serialize`,
/// which is what the JSON-RPC router called too.
///
/// # Panics
///
/// Never. [`ReviewResult`] is a plain struct of `String`, numeric, `bool`,
/// `Option` and `Vec` fields with no map keys that could fail to serialise, so
/// the only documented failure mode of `to_value` is unreachable here; the
/// fallback preserves the error rather than unwrapping it.
///
/// Test: `run_json_matches_the_rpc_result_shape`.
pub fn run_json_payload(result: &ReviewResult) -> Value {
    serde_json::to_value(result).unwrap_or_else(
        |e| serde_json::json!({ "error": format!("failed to serialise the review result: {e}") }),
    )
}

/// Whether this review must exit the process non-zero.
///
/// Why (fail-open check, #6290): an exit code is the only thing a CI gate or a
/// shell pipeline reads, and a review that never reached a verdict must not
/// look like one that approved. Two distinct shapes reach here — a required
/// context dependency that was down (`ReviewStatus::Skipped`) and a pipeline
/// that aborted with a recorded error (`error: Some(..)`, status untouched) —
/// and only the first one used to be caught.
///
/// What: `true` when the run was skipped OR carries an error string. The JSON
/// is printed either way, so the caller gets the reason and the non-zero exit
/// together rather than one or the other.
///
/// Test: `run_is_failure_catches_a_provider_error`,
/// `run_is_failure_catches_a_skipped_review`,
/// `run_is_failure_passes_a_clean_review`.
pub fn run_is_failure(result: &ReviewResult) -> bool {
    result.status.is_skipped() || result.error.is_some()
}

/// The message a failed run exits with.
///
/// Why: `run_is_failure` answers whether to fail; this answers what to say. The
/// two are separate so the caller can print the JSON between them.
/// What: the recorded error when there is one, otherwise the skip's own
/// explanation, otherwise a last-resort sentence that still names the crate's
/// vocabulary rather than an empty string.
/// Test: `run_failure_reason_prefers_the_recorded_error`.
pub fn run_failure_reason(result: &ReviewResult) -> String {
    if let Some(error) = result.error.as_deref() {
        return error.to_owned();
    }
    if result.status.is_skipped() {
        return "review skipped — required code-context dependency unavailable".to_owned();
    }
    "review failed with no recorded reason".to_owned()
}

#[cfg(test)]
#[path = "run_output_tests.rs"]
mod tests;
