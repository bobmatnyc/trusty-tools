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

/// A non-withheld local-diff review, shaped like what code-intelligence reads.
fn local_review() -> ReviewResult {
    let mut r = ReviewResult::new("local", "diff", 0, "local diff", "");
    r.verdict = crate::models::Verdict::ApproveWithReservations;
    r.grade = Some("C+".to_owned());
    r.review_body = "One overflow risk.".to_owned();
    r.model = "fake-model".to_owned();
    r.cost_estimate_usd = 0.01;
    r.timestamp = "2026-10-05T00:00:00Z".to_owned();
    r.review_version = "tr-test".to_owned();
    let mut f = crate::models::Finding::new(
        "src/a.rs",
        "overflow",
        "`a + b` overflows",
        "use checked_add",
        0.8,
        crate::models::Effort::Medium,
    );
    f.line = Some(3);
    f.consequence = "wraps in release".to_owned();
    f.verified = Some(crate::models::VerifyOutcome::Confirmed);
    r.findings = vec![f];
    r.findings_count = 1;
    r
}

/// #9188 compatibility (Bob 02:48Z; Architect 03:26Z item 4): for a review
/// that withheld nothing, `run --local-diff - --json` prints every field
/// code-intelligence parses with the same name, type and value as before
/// #9188, byte for byte, and no new key.
/// Test: this is the test; it passes on 09721c7bbb and on the #9188 branch.
#[test]
fn run_json_for_a_non_withheld_review_is_unchanged_by_9188() {
    let payload = run_json_payload(&local_review());
    assert_eq!(payload.to_string(), PRE_9188_RUN_JSON);
    assert_eq!(payload["verdict"], "APPROVE*");
    assert_eq!(payload["grade"], "C+");
    assert!(payload["findings"].is_array());
    assert_eq!(payload["findings_count"], 1);
    assert_eq!(payload["model"], "fake-model");
    assert!(payload["cost_estimate_usd"].is_f64());
    assert_eq!(payload["status"], "completed");
    for absent in [
        "summary",
        "grade_justification",
        "mcp_status",
        "infra_unavailable",
        "withheld_count",
        "withheld_by_reason",
        "withheld_findings",
    ] {
        assert!(
            payload.get(absent).is_none(),
            "`{absent}` appeared: {payload}"
        );
    }
    let f = &payload["findings"][0];
    for key in [
        "file",
        "line",
        "description",
        "consequence",
        "category",
        "suggestion",
        "confidence",
        "effort",
        "kind",
        "verified",
        "code_provable",
    ] {
        assert!(f.get(key).is_some(), "finding lost `{key}`: {f}");
    }
}

/// `run_json_for_a_non_withheld_review_is_unchanged_by_9188`'s payload, as
/// built at 09721c7bbb.
const PRE_9188_RUN_JSON: &str = r#"{"cost_estimate_usd":0.01,"dry_run":true,"findings":[{"category":"correctness","code_provable":false,"confidence":0.800000011920929,"consequence":"wraps in release","description":"`a + b` overflows","effort":"medium","file":"src/a.rs","issue_eligible":false,"kind":"overflow","line":3,"suggestion":"use checked_add","verified":"confirmed"}],"findings_count":1,"grade":"C+","head_sha":"","input_tokens":0,"latency_ms":0,"model":"fake-model","output_tokens":0,"owner":"local","posted":false,"pr_number":0,"pr_title":"local diff","pr_url":"","repo":"diff","review_body":"One overflow risk.","review_version":"tr-test","status":"completed","timestamp":"2026-10-05T00:00:00Z","unverified_count":0,"verdict":"APPROVE*","withheld_unverified_count":0}"#;

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

/// A value whose serialisation always fails (#9194).
struct Unserialisable;

impl serde::Serialize for Unserialisable {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("fixture refuses"))
    }
}

/// #9194 P3 (amendment 4): a ledger that cannot be serialised is an
/// `{"error": ...}` object naming the failure, never `null`.
#[test]
fn ledger_value_reports_a_serialisation_failure() {
    assert_eq!(
        super::ledger_value(&Unserialisable),
        serde_json::json!({ "error": "failed to serialise context_sources: fixture refuses" })
    );
    let rows = [crate::models::ContextSourceRecord::new(
        "search",
        crate::models::SourceState::Absent,
    )];
    assert_eq!(
        super::ledger_value(&rows[..]),
        serde_json::json!([{ "source": "search", "state": "absent" }])
    );
}

/// #9194 amendment 4: `run --json` and the MCP envelope both build their
/// `context_sources` through `ledger_value`.
#[test]
fn cli_and_mcp_paths_call_ledger_value() {
    for (path, source) in [
        ("commands/run.rs", include_str!("commands/run.rs")),
        ("mcp/tools.rs", include_str!("mcp/tools.rs")),
    ] {
        let calls = source
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .any(|l| l.contains("ledger_value("));
        assert!(calls, "{path} must build context_sources with ledger_value");
    }
}
