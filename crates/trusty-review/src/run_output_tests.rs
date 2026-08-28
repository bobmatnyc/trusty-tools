//! Unit tests for `run_output` — the per-invocation result shape and its exit
//! rule (#6290).
//!
//! Why: split from `run_output.rs` so that file stays a contract statement
//! rather than a contract plus its proof.
//! What: pins the payload to `ReviewResult`'s own serialisation and drives the
//! three exit-rule cases.
//! Test: this is the test module.

use super::{run_failure_reason, run_is_failure, run_json_payload};
use crate::models::{ReviewResult, ReviewStatus};

/// A result shaped like a clean review, so each test can perturb one field.
fn clean_result() -> ReviewResult {
    let mut r = ReviewResult::new(
        "acme",
        "widget",
        42,
        "Add a widget",
        "https://example/pr/42",
    );
    r.model = "fake-model".to_owned();
    r.head_sha = "deadbeef".to_owned();
    r
}

/// Why (#6290): the retired `review.run` method's `result` field WAS
/// `serde_json::to_value(&ReviewResult)` — `service::rpc`'s router serialised
/// the handler's return value with no envelope of its own. A `run --json` that
/// wrapped, renamed or trimmed anything would silently break every caller that
/// parsed the daemon's answer, and the break would surface as a missing field
/// at the consumer rather than as a failure here.
/// What: asserts the payload IS that identity serialisation, and spot-checks
/// the four fields a consumer keys off.
/// Test: this is the test.
#[test]
fn run_json_matches_the_rpc_result_shape() {
    let result = clean_result();
    let payload = run_json_payload(&result);

    assert_eq!(
        payload,
        serde_json::to_value(&result).expect("ReviewResult serialises"),
        "run --json must emit exactly what the RPC router put in `result`"
    );
    for field in ["owner", "repo", "pr_number", "verdict"] {
        assert!(
            payload.get(field).is_some(),
            "the daemon's result carried `{field}`, so this must too: {payload}"
        );
    }
}

/// Why (fail-open check, #6290): `abort_dry` records a provider or transport
/// failure as `error: Some(..)` and leaves `status` at its `Completed` default,
/// so the pre-#6290 exit rule — which tested `status.is_skipped()` alone —
/// exited 0 on an UNKNOWN verdict with zero findings. A CI gate reading that
/// exit code passed the PR on an outage.
/// What: a result carrying only an error is a failure.
/// Test: this is the test.
#[test]
fn run_is_failure_catches_a_provider_error() {
    let mut result = clean_result();
    result.error = Some("bedrock: connection reset".to_owned());
    assert_eq!(result.status, ReviewStatus::Completed);

    assert!(
        run_is_failure(&result),
        "a recorded pipeline error must exit non-zero even on a Completed status"
    );
    assert_eq!(run_failure_reason(&result), "bedrock: connection reset");
}

/// Why: the skip path is the one the old rule DID catch, and it must keep
/// working — this change broadens the rule, it does not move it.
/// What: a `Skipped` result with no error string is still a failure, and
/// explains itself.
/// Test: this is the test.
#[test]
fn run_is_failure_catches_a_skipped_review() {
    let mut result = clean_result();
    result.status = ReviewStatus::Skipped;
    result.infra_unavailable = true;

    assert!(run_is_failure(&result));
    assert!(
        run_failure_reason(&result).contains("skipped"),
        "the reason must name the skip: {}",
        run_failure_reason(&result)
    );
}

/// Why: a rule that fails everything is as useless as one that fails nothing.
/// A completed review with no error is the overwhelmingly common case and must
/// exit 0, including the `Degraded` variant — an operator who opted out of a
/// context dependency asked for that review and gets a labelled verdict, not a
/// non-zero exit.
/// What: `Completed` and `Degraded` with no error both pass.
/// Test: this is the test.
#[test]
fn run_is_failure_passes_a_clean_review() {
    assert!(!run_is_failure(&clean_result()));

    let mut degraded = clean_result();
    degraded.status = ReviewStatus::Degraded;
    assert!(
        !run_is_failure(&degraded),
        "an opted-in context-free review carries a verdict and must exit 0"
    );
}

/// Why: a result can carry both an error and a skip, and the error is the more
/// specific of the two — it names what actually broke.
/// What: the recorded error wins over the generic skip sentence.
/// Test: this is the test.
#[test]
fn run_failure_reason_prefers_the_recorded_error() {
    let mut result = clean_result();
    result.status = ReviewStatus::Skipped;
    result.error = Some("trusty-search unreachable at /tmp/search.sock".to_owned());
    assert_eq!(
        run_failure_reason(&result),
        "trusty-search unreachable at /tmp/search.sock"
    );
}
