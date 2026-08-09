//! Tests for the analyze drain's decisions (#5192).
//!
//! The pipeline itself is network I/O and is not simulated here. What IS tested
//! is every branch that decides a delivery's fate — because the drain removes
//! an entry on `Ok` and keeps it on `Err`, so a misclassification here is a
//! delivery deleted without work or retried forever.

use std::collections::BTreeMap;

use base64::Engine as _;
use trusty_common::webhook_relay::{DeliveryProcessor, Disposition, Provenance, RelayDelivery};

use super::*;

fn body(action: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "action": action,
        "pull_request": { "number": 42, "head": { "sha": "deadbeef" } },
        "repository": { "name": "trusty-tools", "owner": { "login": "bobmatnyc" } },
    }))
    .expect("encode")
}

fn delivery_with(event: &str, raw: &[u8]) -> RelayDelivery {
    RelayDelivery {
        delivery_id: "d-1".to_string(),
        source: "analyze".to_string(),
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
fn classify_accepts_an_actionable_pull_request() {
    for action in ACTIONABLE_ACTIONS {
        match classify_pr_event(PR_EVENT, &body(action)) {
            PrEventVerdict::Actionable(target) => {
                assert_eq!(target.owner, "bobmatnyc");
                assert_eq!(target.repo, "trusty-tools");
                assert_eq!(target.pr, 42);
                assert_eq!(target.head_sha, "deadbeef");
            }
            other => panic!("{action} must be actionable, got {other:?}"),
        }
    }
}

#[test]
fn classify_ignores_a_non_pull_request_event() {
    assert!(matches!(
        classify_pr_event("issues", &body("opened")),
        PrEventVerdict::Ignored(_)
    ));
}

#[test]
fn classify_ignores_an_unactionable_action() {
    // `closed` is not an error and must not be retried — it is finished work.
    assert!(matches!(
        classify_pr_event(PR_EVENT, &body("closed")),
        PrEventVerdict::Ignored(_)
    ));
}

#[test]
fn classify_reports_a_payload_with_no_pr_number_as_malformed() {
    // Malformed, not Ignored: the sender asked for an analysis and we cannot
    // do it. Treating it as Ignored would delete the delivery silently.
    let raw = serde_json::to_vec(&serde_json::json!({
        "action": "opened",
        "repository": { "name": "trusty-tools", "owner": { "login": "bobmatnyc" } },
    }))
    .expect("encode");
    assert!(matches!(
        classify_pr_event(PR_EVENT, &raw),
        PrEventVerdict::Malformed(_)
    ));
}

#[test]
fn classify_reports_a_non_json_body_as_malformed() {
    assert!(matches!(
        classify_pr_event(PR_EVENT, b"{ not json"),
        PrEventVerdict::Malformed(_)
    ));
}

// ─── Processor dispositions ──────────────────────────────────────────────────

/// A pipeline whose outcome the test chooses, and which records its calls.
struct StubPipeline {
    fail: Option<String>,
    seen: std::sync::Mutex<Vec<PrTarget>>,
}

#[async_trait::async_trait]
impl PrPipeline for StubPipeline {
    async fn analyse(&self, target: &PrTarget) -> anyhow::Result<()> {
        self.seen.lock().expect("seen").push(target.clone());
        match &self.fail {
            Some(why) => Err(anyhow::anyhow!("{why}")),
            None => Ok(()),
        }
    }
}

fn stub(fail: Option<&str>) -> (AnalyzeProcessor, std::sync::Arc<StubPipeline>) {
    let pipeline = std::sync::Arc::new(StubPipeline {
        fail: fail.map(str::to_string),
        seen: std::sync::Mutex::new(Vec::new()),
    });
    (AnalyzeProcessor::with_pipeline(pipeline.clone()), pipeline)
}

fn processor() -> AnalyzeProcessor {
    stub(None).0
}

#[tokio::test]
async fn processor_ignores_a_non_pull_request_delivery() {
    let got = processor()
        .process(&delivery_with("issues", &body("opened")))
        .await;
    assert!(
        matches!(got, Ok(Disposition::Ignored { .. })),
        "an ignored event is accepted work, not a failure: {got:?}"
    );
}

#[tokio::test]
async fn processor_reports_an_undecodable_body_as_permanent() {
    // A permanent failure is quarantined on the first pass rather than retried
    // five times against bytes that cannot change.
    let mut delivery = delivery_with(PR_EVENT, &body("opened"));
    delivery.body_b64 = "!!! not base64 !!!".to_string();

    let failure = processor()
        .process(&delivery)
        .await
        .expect_err("an undecodable body must fail");
    assert!(!failure.retryable, "{failure:?}");
    assert!(failure.reason.contains("base64"), "{}", failure.reason);
}

#[tokio::test]
async fn processor_reports_a_malformed_payload_as_permanent() {
    let raw = serde_json::to_vec(&serde_json::json!({ "action": "opened" })).expect("encode");
    let failure = processor()
        .process(&delivery_with(PR_EVENT, &raw))
        .await
        .expect_err("a malformed payload must fail");
    assert!(!failure.retryable, "{failure:?}");
}

#[tokio::test]
async fn processor_runs_the_pipeline_for_an_actionable_delivery() {
    let (processor, pipeline) = stub(None);
    let got = processor
        .process(&delivery_with(PR_EVENT, &body("opened")))
        .await;

    assert_eq!(got, Ok(Disposition::Processed));
    let seen = pipeline.seen.lock().expect("seen");
    assert_eq!(seen.len(), 1, "the pipeline must actually have been run");
    assert_eq!(seen[0].pr, 42);
}

#[tokio::test]
async fn processor_reports_a_failed_pipeline_as_retryable() {
    // 🔴 The failure path the drain's "not lost, not falsely counted" rule
    // hangs on, in the configuration the defect occurs in: an actionable PR
    // whose analysis is attempted and genuinely fails. A retryable verdict is
    // what makes the drain keep the entry instead of removing it.
    let (processor, pipeline) = stub(Some("github returned 503"));

    let failure = processor
        .process(&delivery_with(PR_EVENT, &body("opened")))
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
    assert!(failure.reason.contains("503"), "{}", failure.reason);
}
