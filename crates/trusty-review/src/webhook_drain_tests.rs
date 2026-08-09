//! Tests for the review drain's decisions (#5192).
//!
//! The pipeline itself is LLM and network I/O and is not simulated. What IS
//! tested is every branch that decides a delivery's fate — the drain removes an
//! entry on `Ok` and keeps it on `Err`, so a misclassification here is either a
//! delivery deleted without a review or one retried forever.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use trusty_common::webhook_relay::{DeliveryProcessor, Disposition, Provenance, RelayDelivery};

use super::*;

fn body(action: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "action": action,
        "pull_request": { "number": 42, "user": { "login": "someone" },
                          "head": { "sha": "deadbeef" } },
        "repository": { "name": "trusty-tools", "owner": { "login": "bobmatnyc" } },
        "requested_reviewer": { "login": "trusty-reviewer" },
    }))
    .expect("encode")
}

fn delivery_with(event: &str, raw: &[u8]) -> RelayDelivery {
    RelayDelivery {
        delivery_id: "d-1".to_string(),
        source: "review".to_string(),
        event: event.to_string(),
        headers: BTreeMap::new(),
        body_b64: base64::engine::general_purpose::STANDARD.encode(raw),
        provenance: Provenance {
            algorithm: "hmac-sha256".to_string(),
            key_id: "GITHUB_WEBHOOK_SECRET".to_string(),
            verified: true,
        },
        received_at_unix_ms: 1_700_000_000_000,
        attempts: 0,
    }
}

// ─── Classification ──────────────────────────────────────────────────────────

#[test]
fn classify_accepts_a_review_request() {
    match classify_review_event(PR_EVENT, &body(REVIEW_ACTION)) {
        ReviewEventVerdict::Actionable(target) => {
            assert_eq!(target.owner, "bobmatnyc");
            assert_eq!(target.repo, "trusty-tools");
            assert_eq!(target.pr, 42);
            assert_eq!(target.head_sha, "deadbeef");
            assert_eq!(
                target.requested_reviewer.as_deref(),
                Some("trusty-reviewer")
            );
        }
        other => panic!("a review request must be actionable, got {other:?}"),
    }
}

#[test]
fn classify_ignores_a_non_pull_request_event() {
    assert!(matches!(
        classify_review_event("issues", &body(REVIEW_ACTION)),
        ReviewEventVerdict::Ignored(_)
    ));
}

#[test]
fn classify_ignores_an_action_that_is_not_review_requested() {
    // Spec REV-702: only `review_requested` dispatches. `opened` is finished
    // work, not a failure, so it must not be retried.
    for action in ["opened", "closed", "synchronize"] {
        assert!(
            matches!(
                classify_review_event(PR_EVENT, &body(action)),
                ReviewEventVerdict::Ignored(_)
            ),
            "{action} must be ignored"
        );
    }
}

#[test]
fn classify_reports_a_payload_with_no_pr_number_as_malformed() {
    let raw = serde_json::to_vec(&serde_json::json!({
        "action": REVIEW_ACTION,
        "repository": { "name": "trusty-tools", "owner": { "login": "bobmatnyc" } },
    }))
    .expect("encode");
    assert!(matches!(
        classify_review_event(PR_EVENT, &raw),
        ReviewEventVerdict::Malformed(_)
    ));
}

#[test]
fn classify_reports_a_non_json_body_as_malformed() {
    assert!(matches!(
        classify_review_event(PR_EVENT, b"{ not json"),
        ReviewEventVerdict::Malformed(_)
    ));
}

// ─── Processor dispositions ──────────────────────────────────────────────────

/// A pipeline whose outcome the test chooses, and which records its calls.
struct StubPipeline {
    fail: Option<String>,
    seen: Mutex<Vec<ReviewTarget>>,
}

#[async_trait::async_trait]
impl ReviewPipeline for StubPipeline {
    async fn review(&self, target: &ReviewTarget) -> anyhow::Result<()> {
        self.seen.lock().expect("seen").push(target.clone());
        match &self.fail {
            Some(why) => Err(anyhow::anyhow!("{why}")),
            None => Ok(()),
        }
    }
}

fn stub(fail: Option<&str>) -> (ReviewProcessor, Arc<StubPipeline>) {
    let pipeline = Arc::new(StubPipeline {
        fail: fail.map(str::to_string),
        seen: Mutex::new(Vec::new()),
    });
    (ReviewProcessor::with_pipeline(pipeline.clone()), pipeline)
}

#[tokio::test]
async fn processor_runs_the_pipeline_for_a_review_request() {
    let (processor, pipeline) = stub(None);
    let got = processor
        .process(&delivery_with(PR_EVENT, &body(REVIEW_ACTION)))
        .await;

    assert_eq!(got, Ok(Disposition::Processed));
    let seen = pipeline.seen.lock().expect("seen");
    assert_eq!(seen.len(), 1, "the pipeline must actually have been run");
    assert_eq!(seen[0].pr, 42);
}

#[tokio::test]
async fn processor_ignores_a_non_review_request() {
    let (processor, pipeline) = stub(None);
    let got = processor
        .process(&delivery_with(PR_EVENT, &body("opened")))
        .await;

    assert!(
        matches!(got, Ok(Disposition::Ignored { .. })),
        "an ignored action is accepted work, not a failure: {got:?}"
    );
    assert!(
        pipeline.seen.lock().expect("seen").is_empty(),
        "and it must not have cost a review"
    );
}

#[tokio::test]
async fn processor_reports_an_undecodable_body_as_permanent() {
    let (processor, _) = stub(None);
    let mut delivery = delivery_with(PR_EVENT, &body(REVIEW_ACTION));
    delivery.body_b64 = "!!! not base64 !!!".to_string();

    let failure = processor
        .process(&delivery)
        .await
        .expect_err("an undecodable body must fail");
    assert!(!failure.retryable, "{failure:?}");
    assert!(failure.reason.contains("base64"), "{}", failure.reason);
}

#[tokio::test]
async fn processor_reports_a_malformed_payload_as_permanent() {
    let (processor, _) = stub(None);
    let raw = serde_json::to_vec(&serde_json::json!({ "action": REVIEW_ACTION })).expect("encode");

    let failure = processor
        .process(&delivery_with(PR_EVENT, &raw))
        .await
        .expect_err("a malformed payload must fail");
    assert!(!failure.retryable, "{failure:?}");
}

#[tokio::test]
async fn processor_reports_a_failed_pipeline_as_retryable() {
    // 🔴 The failure path the drain's "not lost, not falsely counted" rule
    // hangs on, in the configuration the defect occurs in: an actionable review
    // request that is attempted and genuinely fails.
    let (processor, pipeline) = stub(Some("verifier liveness gate failed"));

    let failure = processor
        .process(&delivery_with(PR_EVENT, &body(REVIEW_ACTION)))
        .await
        .expect_err("a failed pipeline must fail");

    assert_eq!(pipeline.seen.lock().expect("seen").len(), 1);
    assert!(
        failure.retryable,
        "a transient failure must keep the delivery claimable: {failure:?}"
    );
    assert!(
        failure.reason.contains("bobmatnyc/trusty-tools#42"),
        "the report must name the PR that failed: {}",
        failure.reason
    );
    assert!(failure.reason.contains("liveness"), "{}", failure.reason);
}
