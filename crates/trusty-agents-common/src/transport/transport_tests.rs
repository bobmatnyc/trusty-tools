//! Tests for `transport::aggregate_live` (DOC-48 §5.3.1, AC-7; issue #3299).
//!
//! Why: exercises the generic combinator directly against a minimal
//! in-memory `EventSource`/`MembershipProvider` pair — no tcode, no axum, no
//! `WorkstreamStore` — to prove the combinator's contract holds independent
//! of any one harness's event type, mirroring the real design goal (AC-7.1).

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;
use tokio_stream::wrappers::BroadcastStream;

use super::*;

/// A toy event payload: just a tag, standing in for tcode's `Event` enum or
/// a future `trusty-agents` harness event type.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToyPayload {
    Turn(u32),
    /// A group-scoped event that names its own group directly, with no
    /// owning session — stands in for tcode's `WorkstreamActivationChanged`.
    GroupBroadcast {
        target_group: String,
    },
}

/// An `EventSource` over a plain `tokio::sync::broadcast` channel — the same
/// primitive `trusty-code`'s real adapter wraps.
struct ToySource(tokio::sync::broadcast::Sender<SourceEvent<ToyPayload>>);

impl EventSource for ToySource {
    type Payload = ToyPayload;

    fn subscribe(&self) -> BoxEventStream<ToyPayload> {
        let rx = self.0.subscribe();
        Box::pin(BroadcastStream::new(rx).filter_map(|item| async move { item.ok() }))
    }
}

/// A `MembershipProvider` backed by an in-memory set — stands in for
/// tcode's store-backed `SharedWorkstreamStore` lookup.
#[derive(Clone)]
struct ToyMembership(Arc<Mutex<Vec<String>>>);

#[async_trait::async_trait]
impl MembershipProvider<String> for ToyMembership {
    async fn contains(&self, group: &String, session_id: &str) -> bool {
        if group == "vanished-group" {
            // Simulates a lookup failure — must be treated as "no match",
            // never a panic or a closed stream.
            return false;
        }
        self.0.lock().await.iter().any(|s| s == session_id)
    }
}

fn classify(payload: &ToyPayload, group: &String) -> Option<bool> {
    match payload {
        ToyPayload::GroupBroadcast { target_group } => Some(target_group == group),
        ToyPayload::Turn(_) => None,
    }
}

fn source_event(session_id: &str, payload: ToyPayload) -> SourceEvent<ToyPayload> {
    SourceEvent {
        session_id: session_id.to_string(),
        event_type: "toy".to_string(),
        payload,
    }
}

/// An event from a session that IS a member of the group must be forwarded,
/// tagged with the original session id and event type.
#[tokio::test]
async fn forwards_event_when_membership_matches() {
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let source = ToySource(tx.clone());
    let membership = ToyMembership(Arc::new(Mutex::new(vec!["s1".to_string()])));
    let mut stream = std::pin::pin!(aggregate_live(
        "g1".to_string(),
        source,
        membership,
        classify
    ));

    tx.send(source_event("s1", ToyPayload::Turn(1))).unwrap();

    let got = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("timed out")
        .expect("stream ended early");
    assert_eq!(got.session_id, "s1");
    assert_eq!(got.event_type, "toy");
    assert_eq!(got.payload, ToyPayload::Turn(1));
}

/// An event from a session NOT in the group must be filtered out.
#[tokio::test]
async fn filters_event_when_membership_does_not_match() {
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let source = ToySource(tx.clone());
    let membership = ToyMembership(Arc::new(Mutex::new(vec!["s1".to_string()])));
    let mut stream = std::pin::pin!(aggregate_live(
        "g1".to_string(),
        source,
        membership,
        classify
    ));

    tx.send(source_event("unbound", ToyPayload::Turn(1)))
        .unwrap();

    let outcome = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
    assert!(
        outcome.is_err(),
        "an event from a non-member session must not be forwarded"
    );
}

/// A `classify` bypass (`Some(true)`) forwards an event that names its own
/// target group directly, even with zero members and a session id that
/// matches nothing.
#[tokio::test]
async fn classify_bypass_skips_membership_check() {
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let source = ToySource(tx.clone());
    let membership = ToyMembership(Arc::new(Mutex::new(vec![])));
    let mut stream = std::pin::pin!(aggregate_live(
        "g1".to_string(),
        source,
        membership,
        classify
    ));

    tx.send(source_event(
        "",
        ToyPayload::GroupBroadcast {
            target_group: "g1".to_string(),
        },
    ))
    .unwrap();

    let got = tokio::time::timeout(Duration::from_secs(2), stream.next())
        .await
        .expect("timed out")
        .expect("stream ended early");
    assert_eq!(
        got.payload,
        ToyPayload::GroupBroadcast {
            target_group: "g1".to_string()
        }
    );

    // A broadcast naming a DIFFERENT group must not be forwarded here.
    tx.send(source_event(
        "",
        ToyPayload::GroupBroadcast {
            target_group: "other".to_string(),
        },
    ))
    .unwrap();
    let outcome = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
    assert!(
        outcome.is_err(),
        "a broadcast for another group must not be forwarded"
    );
}

/// Cross-subscriber isolation — the property epic #3052's multiple
/// concurrent iOS attaches depend on: TWO `aggregate_live` streams over
/// clones of ONE broadcast sender, scoped to different groups with disjoint
/// membership sets, must each receive exactly their own events from a
/// single shared publish batch — never the other subscriber's, and one
/// subscriber's presence must not consume or suppress events destined for
/// the other (broadcast semantics: every subscriber sees every published
/// event; scoping happens per-stream in the combinator).
#[tokio::test]
async fn concurrent_streams_over_one_source_stay_isolated() {
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let membership_a = ToyMembership(Arc::new(Mutex::new(vec!["sa".to_string()])));
    let membership_b = ToyMembership(Arc::new(Mutex::new(vec!["sb".to_string()])));
    let mut stream_a = std::pin::pin!(aggregate_live(
        "ga".to_string(),
        ToySource(tx.clone()),
        membership_a,
        classify
    ));
    let mut stream_b = std::pin::pin!(aggregate_live(
        "gb".to_string(),
        ToySource(tx.clone()),
        membership_b,
        classify
    ));

    // One shared publish batch, interleaving both groups' traffic plus a
    // group-scoped broadcast naming only ga.
    tx.send(source_event("sa", ToyPayload::Turn(1))).unwrap();
    tx.send(source_event("sb", ToyPayload::Turn(2))).unwrap();
    tx.send(source_event(
        "",
        ToyPayload::GroupBroadcast {
            target_group: "ga".to_string(),
        },
    ))
    .unwrap();

    // Stream A sees sa's turn then ga's broadcast — in publish order.
    let a1 = tokio::time::timeout(Duration::from_secs(2), stream_a.next())
        .await
        .expect("timed out on a1")
        .expect("stream a ended early");
    assert_eq!(a1.session_id, "sa");
    assert_eq!(a1.payload, ToyPayload::Turn(1));
    let a2 = tokio::time::timeout(Duration::from_secs(2), stream_a.next())
        .await
        .expect("timed out on a2")
        .expect("stream a ended early");
    assert_eq!(
        a2.payload,
        ToyPayload::GroupBroadcast {
            target_group: "ga".to_string()
        }
    );

    // Stream B sees ONLY sb's turn — sa's event and ga's broadcast never
    // reach it, even though all three flowed over the same sender.
    let b1 = tokio::time::timeout(Duration::from_secs(2), stream_b.next())
        .await
        .expect("timed out on b1")
        .expect("stream b ended early");
    assert_eq!(b1.session_id, "sb");
    assert_eq!(b1.payload, ToyPayload::Turn(2));

    // Neither stream has anything further pending.
    let a_rest = tokio::time::timeout(Duration::from_millis(200), stream_a.next()).await;
    assert!(a_rest.is_err(), "stream a must not receive b-scoped events");
    let b_rest = tokio::time::timeout(Duration::from_millis(200), stream_b.next()).await;
    assert!(
        b_rest.is_err(),
        "stream b must not receive a-scoped events or ga's broadcast"
    );
}

/// A `MembershipProvider::contains` implementation that treats an unresolvable
/// group as "no match" (per this module's documented contract) must not
/// panic or close the stream — it just yields nothing for that event.
#[tokio::test]
async fn membership_lookup_failure_is_treated_as_no_match() {
    let (tx, _rx) = tokio::sync::broadcast::channel(16);
    let source = ToySource(tx.clone());
    let membership = ToyMembership(Arc::new(Mutex::new(vec!["s1".to_string()])));
    let mut stream = std::pin::pin!(aggregate_live(
        "vanished-group".to_string(),
        source,
        membership,
        classify
    ));

    tx.send(source_event("s1", ToyPayload::Turn(1))).unwrap();

    let outcome = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
    assert!(
        outcome.is_err(),
        "an unresolvable group must not forward any event"
    );
}
