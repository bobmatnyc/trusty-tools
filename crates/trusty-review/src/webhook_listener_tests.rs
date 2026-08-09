//! Coverage for `trusty-review`'s half of the webhook UDS listener (#5182).
//!
//! The frame contract, the ack ordering and the socket hardening are proven in
//! `trusty-common`'s `webhook_relay` suite, which both targets mount. What is
//! specific to this crate — and what a wrong value here would break silently —
//! is the two path resolutions, so those are what most of these cover.
//!
//! #5181 added one more: with `POST /pr/github/webhook` retired, this is the
//! ONLY way a GitHub delivery reaches a review, so
//! `uds_delivery_reaches_the_review_pipeline_end_to_end` drives the whole path
//! — real socket, real frame, real ack, real drain — into this crate's own
//! processor rather than trusting the shared suite's stub.

use super::*;

#[test]
fn socket_path_matches_the_shared_contract() {
    // A literal spelled here instead of resolved from the contract is how the
    // sender and receiver end up dialling and binding different paths, which
    // presents as a delivery that is never received and never errors.
    assert_eq!(
        socket_path(),
        trusty_common::uds::scratch_socket_dir()
            .join(trusty_common::webhook_relay::REVIEW_SOCKET_FILE)
    );
    assert_eq!(
        socket_path(),
        trusty_common::webhook_relay::socket_path_for(trusty_common::webhook_relay::REVIEW_SOURCE)
            .expect("review is a configured source"),
        "the console route segment and this socket must agree"
    );
}

#[test]
fn inbox_root_matches_the_shared_contract() {
    // The console meters THIS directory to decide whether a backlog is stuck.
    // Asserting only the literal shape (below) leaves a hand-rolled path free to
    // drift from the shared one: the receiver writes to its own spelling, the
    // console counts the shared spelling, finds it empty, and reports healthy
    // while deliveries pile up. This is the assertion that fails on that drift.
    assert_eq!(
        inbox_root().expect("resolve inbox root"),
        trusty_common::webhook_relay::inbox_root_for(trusty_common::webhook_relay::REVIEW_SOURCE)
            .expect("review is a configured source")
            .expect("resolve the shared inbox root"),
        "the receiver's inbox and the path console meters must be one path"
    );
}

#[test]
fn inbox_root_lives_under_the_review_data_dir() {
    let root = inbox_root().expect("resolve inbox root");
    assert!(
        root.ends_with(trusty_common::webhook_relay::INBOX_DIR_NAME),
        "unexpected inbox root: {root:?}"
    );
    assert!(
        root.parent().is_some_and(|p| p.ends_with("trusty-review")),
        "the inbox must live under this crate's own data dir, got {root:?}"
    );
}

#[test]
fn listener_opens_against_a_temp_inbox() {
    // `listener()` resolves the real data dir, which a test must not create;
    // opening against a temp root proves the same construction path.
    let tmp = tempfile::tempdir().expect("tempdir");
    let listener = trusty_common::webhook_relay::WebhookListener::open(
        socket_path(),
        tmp.path().join("inbox"),
    )
    .expect("open listener");

    assert_eq!(listener.socket(), socket_path());
    assert!(listener.inbox().root().is_dir());
    assert!(
        !socket_path().exists() || listener.socket().exists(),
        "open must not bind"
    );
}

/// A `review_requested` delivery still runs a review, over the real transport.
///
/// Why: retiring the HTTP route removes the fallback. If the UDS path were
/// broken — a socket nobody binds, an ack that never comes, a drain that never
/// runs — there is no longer a second way in, and the failure would present as
/// PRs that are simply never reviewed. The shared suite proves the transport
/// with a stub sink; this proves it against `ReviewProcessor`, which is what
/// actually decides whether a review happens.
/// What: binds a `WebhookListener` on a temp socket with a stub pipeline,
/// dials it with a real `review_requested` frame, requires the ack, and
/// requires the pipeline to have been called for the right PR with the inbox
/// drained empty afterwards.
/// Test: this is the test.
#[tokio::test]
async fn uds_delivery_reaches_the_review_pipeline_end_to_end() {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use base64::Engine as _;
    use trusty_common::webhook_relay::{
        Provenance, RelayFrame, RelayResponse, WebhookListener, held_count,
    };

    use crate::webhook_drain::{ReviewPipeline, ReviewProcessor, ReviewTarget};

    /// Records the reviews it was asked for; never touches the network.
    struct RecordingPipeline(Mutex<Vec<ReviewTarget>>);

    #[async_trait::async_trait]
    impl ReviewPipeline for RecordingPipeline {
        async fn review(&self, target: &ReviewTarget) -> anyhow::Result<()> {
            self.0.lock().expect("seen").push(target.clone());
            Ok(())
        }
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let sock = tmp.path().join("sockets").join("review.sock");
    let inbox_root = tmp.path().join("inbox");
    let pipeline = Arc::new(RecordingPipeline(Mutex::new(Vec::new())));
    let listener = WebhookListener::open(&sock, &inbox_root)
        .expect("open listener")
        .with_processor(Arc::new(ReviewProcessor::with_pipeline(pipeline.clone())));
    let inbox = listener.inbox().clone();

    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let running = tokio::spawn(async move {
        listener
            .run(async {
                let _ = stop_rx.await;
            })
            .await
    });

    let body = serde_json::to_vec(&serde_json::json!({
        "action": "review_requested",
        "pull_request": { "number": 42, "user": { "login": "someone" },
                          "head": { "sha": "deadbeef" } },
        "repository": { "name": "trusty-tools", "owner": { "login": "bobmatnyc" } },
        "requested_reviewer": { "login": "trusty-review[bot]" },
    }))
    .expect("encode");
    let body_b64 = base64::engine::general_purpose::STANDARD.encode(&body);
    let provenance = Provenance {
        algorithm: "hmac-sha256".to_string(),
        key_id: "GITHUB_WEBHOOK_SECRET".to_string(),
        verified: true,
    };
    let headers = BTreeMap::new();

    // Poll for the bind rather than sleeping a fixed interval.
    let mut response: Option<RelayResponse> = None;
    for _ in 0..200 {
        let frame = RelayFrame::new(
            "uds-e2e-1",
            trusty_common::webhook_relay::REVIEW_SOURCE,
            "pull_request",
            &headers,
            &body_b64,
            &provenance,
            1_700_000_000_000,
            0,
        );
        if let Ok(resp) = trusty_common::uds::send_framed_request::<_, RelayResponse>(
            &sock,
            &frame,
            Duration::from_secs(5),
        )
        .await
        {
            response = Some(resp);
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        response.expect("the listener must answer").is_ack(),
        "a durably-held delivery must be acked"
    );

    // The ack rests on durability, so the review happens after it. Poll for the
    // drain rather than racing it on a fixed sleep.
    let mut reviewed = Vec::new();
    for _ in 0..400 {
        reviewed = pipeline.0.lock().expect("seen").clone();
        if !reviewed.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        reviewed.len(),
        1,
        "the relayed delivery must have run exactly one review"
    );
    assert_eq!(reviewed[0].owner, "bobmatnyc");
    assert_eq!(reviewed[0].repo, "trusty-tools");
    assert_eq!(reviewed[0].pr, 42);
    assert_eq!(reviewed[0].head_sha, "deadbeef");

    stop_tx.send(()).expect("signal shutdown");
    running.await.expect("join").expect("clean exit");
    assert_eq!(
        held_count(inbox.root()).expect("held"),
        0,
        "a reviewed delivery must not stay held"
    );
}
