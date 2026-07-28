//! Peer-bus test suite (DOC-60 §5.3 daemon foundation).
//!
//! Why: DOC-60's testing requirement is that failures reach the sender rather
//! than being dropped, so the failure paths carry as much coverage here as the
//! happy path — in particular the instance-bypass failure mode, which is the
//! one design decision this change had to resolve on its own.
//! What: envelope schema round-trips, registry resolution in both addressing
//! modes, publish delivery and its three distinguishable failures, the durable
//! §9 record, and the HTTP surface.
//! Test: this module IS the test module.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use super::*;
use crate::daemon::api::router;
use crate::daemon::state::DaemonState;

// ── helpers ───────────────────────────────────────────────────────────────────

fn bus() -> (tempfile::TempDir, PeerBus) {
    let dir = tempfile::tempdir().unwrap();
    let bus = PeerBus::new(dir.path());
    (dir, bus)
}

fn peer_caller(instance_id: &str, definition_id: &str) -> CallerIdentity {
    CallerIdentity {
        kind: CallerKind::AssistantInstance,
        instance_id: Some(instance_id.into()),
        definition_id: Some(definition_id.into()),
        channel_origin: None,
    }
}

fn chat(text: &str) -> BusPayload {
    BusPayload::ChatText { text: text.into() }
}

/// Read every envelope written to the bus's durable JSONL stream.
fn read_log(bus: &PeerBus) -> Vec<BusEnvelope> {
    let contents = std::fs::read_to_string(bus.log_path()).unwrap_or_default();
    contents
        .lines()
        .map(|l| serde_json::from_str(l).expect("every bus log line must parse as an envelope"))
        .collect()
}

// ── envelope schema (DOC-60 §11) ──────────────────────────────────────────────

#[test]
fn envelope_round_trips_json() {
    let env = BusEnvelope::new(
        BusEdge::AssistantAssistant,
        peer_caller("izzie~aaaa1111", "izzie"),
        Recipient {
            instance_id: Some("cto-assistant~bbbb2222".into()),
            definition_id: Some("cto-assistant".into()),
        },
        BusPayload::PeerRequest {
            text: "review this ADR".into(),
        },
        Some("prior-message".into()),
        DeliveryState::Delivered,
    );
    let json = serde_json::to_string(&env).unwrap();
    let back: BusEnvelope = serde_json::from_str(&json).unwrap();
    assert_eq!(back, env);
}

#[test]
fn envelope_matches_doc60_field_names() {
    // The durable log must be readable against §11's illustrative JSON, so the
    // wire names are pinned rather than left to derive defaults.
    let env = BusEnvelope::new(
        BusEdge::AssistantAssistant,
        peer_caller("izzie~aaaa1111", "izzie"),
        Recipient {
            instance_id: Some("cto-assistant~bbbb2222".into()),
            definition_id: Some("cto-assistant".into()),
        },
        chat("hello"),
        None,
        DeliveryState::Delivered,
    );
    let v = serde_json::to_value(&env).unwrap();
    assert!(v.get("message_id").is_some());
    assert!(v.get("ts").is_some());
    assert_eq!(v["edge"], "assistant_assistant");
    assert_eq!(v["from"]["kind"], "assistant_instance");
    assert_eq!(v["delivery_state"], "delivered");
    assert_eq!(v["payload"]["type"], "chat_text");
    assert_eq!(v["to"]["definition_id"], "cto-assistant");
}

#[test]
fn envelope_ids_are_unique() {
    let mk = || {
        BusEnvelope::new(
            BusEdge::AssistantAssistant,
            peer_caller("izzie~aaaa1111", "izzie"),
            Recipient {
                instance_id: None,
                definition_id: Some("cto-assistant".into()),
            },
            chat("x"),
            None,
            DeliveryState::Dropped,
        )
    };
    assert_ne!(mk().message_id, mk().message_id);
}

#[test]
fn peer_request_is_declinable() {
    // ADR-0024's virtual-twin principle (DOC-60 §5.3, normative) requires a
    // peer REQUEST to be answerable with a refusal. That is only expressible
    // if the schema distinguishes a request from informational chat and
    // carries an explicit accept/decline field.
    let decline = BusPayload::PeerResponse {
        accepted: false,
        text: "declining — out of my scope".into(),
    };
    let v = serde_json::to_value(&decline).unwrap();
    assert_eq!(v["type"], "peer_response");
    assert_eq!(v["accepted"], false);

    // A request and informational chat must not serialize to the same shape,
    // or a recipient could not tell which one it is obliged to answer.
    let req = serde_json::to_value(BusPayload::PeerRequest {
        text: "do x".into(),
    })
    .unwrap();
    let info = serde_json::to_value(chat("do x")).unwrap();
    assert_ne!(req["type"], info["type"]);
}

#[test]
fn caller_validation_rejects_bare_peer() {
    let bare = CallerIdentity {
        kind: CallerKind::AssistantInstance,
        instance_id: None,
        definition_id: Some("izzie".into()),
        channel_origin: None,
    };
    assert!(matches!(bare.validate(), Err(BusError::InvalidCaller(_))));
}

#[test]
fn caller_validation_accepts_user() {
    let user = CallerIdentity {
        kind: CallerKind::User,
        instance_id: None,
        definition_id: None,
        channel_origin: None,
    };
    assert!(user.validate().is_ok());
}

#[test]
fn channel_caller_requires_origin() {
    let bad = CallerIdentity {
        kind: CallerKind::Channel,
        instance_id: None,
        definition_id: None,
        channel_origin: None,
    };
    assert!(matches!(bad.validate(), Err(BusError::InvalidCaller(_))));

    let good = CallerIdentity {
        kind: CallerKind::Channel,
        instance_id: None,
        definition_id: None,
        channel_origin: Some(ChannelOrigin {
            connector: "slack".into(),
            human_sender: "U012ABC".into(),
            channel: "C0123".into(),
            workspace: "T0123".into(),
        }),
    };
    assert!(good.validate().is_ok());
}

// ── instance registry (DOC-60 §6b) ────────────────────────────────────────────

#[test]
fn register_mints_prefixed_id() {
    let reg = InstanceRegistry::default();
    let meta = reg.register("izzie", None).unwrap();
    assert!(meta.instance_id.starts_with("izzie~"));
    assert_eq!(meta.definition_id, "izzie");
    assert!(!meta.registered_at.is_empty());
    // Two registrations of one definition are distinct instances.
    let second = reg.register("izzie", None).unwrap();
    assert_ne!(meta.instance_id, second.instance_id);
    assert_eq!(reg.len(), 2);
}

#[test]
fn register_rejects_bad_definition() {
    let reg = InstanceRegistry::default();
    for bad in [
        "",
        "has~tilde",
        "has space",
        "has#hash",
        "has/slash",
        "has?q",
    ] {
        assert!(
            matches!(
                reg.register(bad, None),
                Err(BusError::InvalidDefinitionId { .. })
            ),
            "{bad:?} must be rejected"
        );
    }
    assert!(reg.is_empty());
}

#[tokio::test]
async fn instance_id_is_url_path_safe() {
    // A minted id travels in a URL path segment (DELETE/subscribe). If it
    // carried a URI-reserved character, a conforming client would truncate or
    // rewrite it before the request was sent — silent misrouting, which is the
    // exact class of bug this bus exists to eliminate. So the id must survive
    // an unencoded round trip through the real router.
    let (_dir, state) = test_state();
    let meta = state
        .bus()
        .registry()
        .register("cto-assistant", None)
        .unwrap();
    assert!(
        !meta.instance_id.contains('#'),
        "minted id must not carry the URI fragment delimiter"
    );

    let (status, _) = call(
        &state,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/bus/instances/{}", meta.instance_id))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(state.bus().registry().is_empty());
}

#[test]
fn resolve_definition_picks_most_recent() {
    let reg = InstanceRegistry::default();
    let _first = reg.register("izzie", None).unwrap();
    let second = reg.register("izzie", None).unwrap();
    let resolved = reg.resolve_definition("izzie").unwrap();
    assert_eq!(resolved.meta.instance_id, second.instance_id);
}

#[test]
fn resolve_definition_missing_errors() {
    let reg = InstanceRegistry::default();
    assert!(matches!(
        reg.resolve_definition("nobody-home"),
        Err(BusError::NoLiveInstance { .. })
    ));
}

#[test]
fn deregister_makes_instance_gone() {
    let reg = InstanceRegistry::default();
    let meta = reg.register("izzie", None).unwrap();
    assert!(reg.resolve_instance(&meta.instance_id).is_ok());
    assert!(reg.deregister(&meta.instance_id));
    assert!(reg.is_empty());
    assert!(matches!(
        reg.resolve_instance(&meta.instance_id),
        Err(BusError::InstanceGone { .. })
    ));
    // A second deregistration is a no-op, not a panic.
    assert!(!reg.deregister(&meta.instance_id));
}

#[test]
fn list_returns_live_instances() {
    let reg = InstanceRegistry::default();
    let a = reg.register("izzie", Some("trusty-tools".into())).unwrap();
    let b = reg.register("cto-assistant", None).unwrap();
    let live = reg.live();
    assert_eq!(live.len(), 2);
    // Ordered by registration sequence.
    assert_eq!(live[0].instance_id, a.instance_id);
    assert_eq!(live[1].instance_id, b.instance_id);
    assert_eq!(live[0].project.as_deref(), Some("trusty-tools"));
}

// ── the instance-bypass failure mode (the decision this change resolves) ──────

#[test]
fn bypass_to_dead_instance_errors_not_falls_back() {
    // THE decision: a sender holding an instance_id whose instance died between
    // learning it and sending gets an explicit error. It is NOT silently
    // redirected to a live sibling instance of the same definition, because
    // that would break the thread continuity that motivated bypass at all.
    let reg = InstanceRegistry::default();
    let dead = reg.register("izzie", None).unwrap();
    reg.deregister(&dead.instance_id);
    // A live sibling of the SAME definition exists — the tempting fallback.
    let sibling = reg.register("izzie", None).unwrap();
    assert!(reg.resolve_definition("izzie").is_ok());

    let err = reg
        .resolve(&PeerTarget::Instance(dead.instance_id.clone()))
        .unwrap_err();
    assert_eq!(
        err,
        BusError::InstanceGone {
            instance_id: dead.instance_id.clone()
        }
    );
    // Crucially: the live sibling was NOT substituted.
    assert_ne!(
        err,
        BusError::NoLiveInstance {
            definition_id: "izzie".into()
        }
    );
    assert!(reg.resolve_instance(&sibling.instance_id).is_ok());
}

#[test]
fn bypass_failure_is_distinguishable_from_never_existed() {
    // A client must be able to implement "re-address by definition" recovery
    // from the status alone, without parsing a message string.
    assert_eq!(
        BusError::InstanceGone {
            instance_id: "izzie~dead".into()
        }
        .status(),
        StatusCode::GONE
    );
    assert_eq!(
        BusError::NoLiveInstance {
            definition_id: "izzie".into()
        }
        .status(),
        StatusCode::NOT_FOUND
    );
}

#[test]
fn publish_bypass_to_dead_instance_does_not_deliver_to_sibling() {
    // The no-fallback rule must hold through the full publish path, not just
    // in the registry: a live sibling subscriber must receive nothing.
    let (_dir, bus) = bus();
    let dead = bus.registry().register("izzie", None).unwrap();
    bus.registry().deregister(&dead.instance_id);
    let sibling = bus.registry().register("izzie", None).unwrap();
    let mut sibling_rx = bus.subscribe(&sibling.instance_id).unwrap();

    let err = bus
        .publish(
            peer_caller("cto-assistant~cccc3333", "cto-assistant"),
            &PeerTarget::Instance(dead.instance_id.clone()),
            chat("continue our thread"),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, BusError::InstanceGone { .. }));
    assert!(
        sibling_rx.try_recv().is_err(),
        "a live sibling must NOT receive a message addressed to a dead instance"
    );
}

// ── publish / delivery ────────────────────────────────────────────────────────

#[test]
fn publish_delivers_to_definition_addressed_instance() {
    let (_dir, bus) = bus();
    let target = bus.registry().register("cto-assistant", None).unwrap();
    let mut rx = bus.subscribe(&target.instance_id).unwrap();

    let sent = bus
        .publish(
            peer_caller("izzie~aaaa1111", "izzie"),
            &PeerTarget::Definition("cto-assistant".into()),
            chat("what do you think?"),
            None,
        )
        .unwrap();

    let got = rx.try_recv().unwrap();
    assert_eq!(got, sent);
    assert_eq!(got.delivery_state, DeliveryState::Delivered);
    assert_eq!(
        got.to.instance_id.as_deref(),
        Some(target.instance_id.as_str())
    );
}

#[test]
fn bypass_publish_stamps_both_ids() {
    // Bypass is a REQUEST-time addressing mode; the stamped envelope still
    // satisfies §11's "to.definition_id always present" on a delivered record.
    let (_dir, bus) = bus();
    let target = bus.registry().register("cto-assistant", None).unwrap();
    let _rx = bus.subscribe(&target.instance_id).unwrap();

    let sent = bus
        .publish(
            peer_caller("izzie~aaaa1111", "izzie"),
            &PeerTarget::Instance(target.instance_id.clone()),
            chat("same thread"),
            None,
        )
        .unwrap();
    assert_eq!(
        sent.to.instance_id.as_deref(),
        Some(target.instance_id.as_str())
    );
    assert_eq!(sent.to.definition_id.as_deref(), Some("cto-assistant"));
}

#[test]
fn publish_reaches_only_target_instance() {
    // Delivery is addressed per instance, not broadcast-then-filtered — the
    // defect DOC-60 §2 identifies in the existing session event path.
    let (_dir, bus) = bus();
    let a = bus.registry().register("izzie", None).unwrap();
    let b = bus.registry().register("cto-assistant", None).unwrap();
    let mut rx_a = bus.subscribe(&a.instance_id).unwrap();
    let mut rx_b = bus.subscribe(&b.instance_id).unwrap();

    bus.publish(
        peer_caller("izzie~aaaa1111", "izzie"),
        &PeerTarget::Instance(b.instance_id.clone()),
        chat("for b only"),
        None,
    )
    .unwrap();

    assert!(rx_b.try_recv().is_ok());
    assert!(
        rx_a.try_recv().is_err(),
        "a non-target must receive nothing"
    );
}

#[test]
fn publish_threads_replies() {
    let (_dir, bus) = bus();
    let target = bus.registry().register("cto-assistant", None).unwrap();
    let _rx = bus.subscribe(&target.instance_id).unwrap();
    let first = bus
        .publish(
            peer_caller("izzie~aaaa1111", "izzie"),
            &PeerTarget::Definition("cto-assistant".into()),
            BusPayload::PeerRequest {
                text: "please review".into(),
            },
            None,
        )
        .unwrap();
    let reply = bus
        .publish(
            peer_caller("izzie~aaaa1111", "izzie"),
            &PeerTarget::Definition("cto-assistant".into()),
            BusPayload::PeerResponse {
                accepted: false,
                text: "declining".into(),
            },
            Some(first.message_id.clone()),
        )
        .unwrap();
    assert_eq!(
        reply.in_reply_to.as_deref(),
        Some(first.message_id.as_str())
    );
}

#[test]
fn publish_without_subscriber_errors() {
    // Registered but not attached: fail-closed per §4 rather than dropping.
    // DOC-60 §7's durable inbox is what will make this queue instead.
    let (_dir, bus) = bus();
    let target = bus.registry().register("cto-assistant", None).unwrap();
    let err = bus
        .publish(
            peer_caller("izzie~aaaa1111", "izzie"),
            &PeerTarget::Instance(target.instance_id.clone()),
            chat("anyone there?"),
            None,
        )
        .unwrap_err();
    assert_eq!(
        err,
        BusError::NoSubscriber {
            instance_id: target.instance_id
        }
    );
}

#[test]
fn publish_rejects_unattributable_caller() {
    let (_dir, bus) = bus();
    let target = bus.registry().register("cto-assistant", None).unwrap();
    let _rx = bus.subscribe(&target.instance_id).unwrap();
    let bad = CallerIdentity {
        kind: CallerKind::AssistantInstance,
        instance_id: None,
        definition_id: Some("izzie".into()),
        channel_origin: None,
    };
    assert!(matches!(
        bus.publish(
            bad,
            &PeerTarget::Definition("cto-assistant".into()),
            chat("x"),
            None
        ),
        Err(BusError::InvalidCaller(_))
    ));
}

#[test]
fn subscribe_to_dead_instance_errors() {
    let (_dir, bus) = bus();
    assert!(matches!(
        bus.subscribe("izzie~never-existed"),
        Err(BusError::InstanceGone { .. })
    ));
}

// ── the durable §9 record ─────────────────────────────────────────────────────

#[test]
fn publish_writes_durable_jsonl_record() {
    let (dir, bus) = bus();
    assert!(bus.log_path().starts_with(dir.path().join("bus")));

    let target = bus.registry().register("cto-assistant", None).unwrap();
    let _rx = bus.subscribe(&target.instance_id).unwrap();
    let sent = bus
        .publish(
            peer_caller("izzie~aaaa1111", "izzie"),
            &PeerTarget::Definition("cto-assistant".into()),
            chat("durable"),
            None,
        )
        .unwrap();

    let log = read_log(&bus);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0], sent);
    assert_eq!(log[0].delivery_state, DeliveryState::Delivered);
}

#[test]
fn failed_publish_logs_dropped_envelope() {
    // A log containing only successes cannot answer "was it sent, or just
    // never read?" — the question ADR-0019 exists to answer. Failures are
    // recorded too.
    let (_dir, bus) = bus();
    let err = bus
        .publish(
            peer_caller("izzie~aaaa1111", "izzie"),
            &PeerTarget::Definition("nobody-home".into()),
            chat("into the void"),
            None,
        )
        .unwrap_err();
    assert!(matches!(err, BusError::NoLiveInstance { .. }));

    let log = read_log(&bus);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].delivery_state, DeliveryState::Dropped);
    assert_eq!(log[0].to.definition_id.as_deref(), Some("nobody-home"));
}

#[test]
fn dropped_bypass_has_no_definition() {
    // Documented consequence of the no-fallback rule: a dead instance leaves
    // nothing to resolve a definition from, so the dropped record cannot name
    // one. This is the single exception to §11's always-present invariant.
    let (_dir, bus) = bus();
    let dead = bus.registry().register("izzie", None).unwrap();
    bus.registry().deregister(&dead.instance_id);
    let _ = bus.publish(
        peer_caller("cto-assistant~cccc3333", "cto-assistant"),
        &PeerTarget::Instance(dead.instance_id.clone()),
        chat("gone"),
        None,
    );
    let log = read_log(&bus);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].delivery_state, DeliveryState::Dropped);
    assert_eq!(
        log[0].to.instance_id.as_deref(),
        Some(dead.instance_id.as_str())
    );
    assert!(log[0].to.definition_id.is_none());
}

#[test]
fn no_subscriber_is_logged_as_dropped_not_delivered() {
    let (_dir, bus) = bus();
    let target = bus.registry().register("cto-assistant", None).unwrap();
    let _ = bus.publish(
        peer_caller("izzie~aaaa1111", "izzie"),
        &PeerTarget::Instance(target.instance_id.clone()),
        chat("unheard"),
        None,
    );
    let log = read_log(&bus);
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].delivery_state, DeliveryState::Dropped);
}

// ── addressing-mode selection ─────────────────────────────────────────────────

#[test]
fn target_prefers_instance_when_both_supplied() {
    let t = routes::PublishTarget {
        instance_id: Some("izzie~aaaa1111".into()),
        definition_id: Some("izzie".into()),
    };
    assert_eq!(
        t.to_peer_target().unwrap(),
        PeerTarget::Instance("izzie~aaaa1111".into())
    );
}

#[test]
fn target_requires_one_id() {
    let t = routes::PublishTarget {
        instance_id: None,
        definition_id: None,
    };
    assert!(matches!(
        t.to_peer_target(),
        Err(BusError::InvalidTarget(_))
    ));
}

#[test]
fn bus_error_status_codes_map() {
    assert_eq!(
        BusError::NoSubscriber {
            instance_id: "x".into()
        }
        .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        BusError::InvalidTarget("x".into()).status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        BusError::InvalidCaller("x".into()).status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        BusError::InvalidDefinitionId {
            definition_id: "x".into(),
            reason: "y".into()
        }
        .status(),
        StatusCode::BAD_REQUEST
    );
}

#[test]
fn instance_gone_is_410() {
    assert_eq!(
        BusError::InstanceGone {
            instance_id: "izzie~dead".into()
        }
        .status(),
        StatusCode::GONE
    );
}

// ── HTTP surface ──────────────────────────────────────────────────────────────

fn test_state() -> (tempfile::TempDir, Arc<DaemonState>) {
    let dir = tempfile::tempdir().unwrap();
    let state = Arc::new(DaemonState::with_root(dir.path().to_path_buf()));
    (dir, state)
}

async fn call(state: &Arc<DaemonState>, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = router(Arc::clone(state)).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, body)
}

fn post(path: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn route_register_returns_instance_id() {
    let (_dir, state) = test_state();
    let (status, body) = call(
        &state,
        post(
            "/api/v1/bus/instances",
            serde_json::json!({ "definition_id": "izzie" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["definition_id"], "izzie");
    assert!(body["instance_id"].as_str().unwrap().starts_with("izzie~"));
}

#[tokio::test]
async fn route_register_rejects_bad_definition() {
    let (_dir, state) = test_state();
    let (status, _) = call(
        &state,
        post(
            "/api/v1/bus/instances",
            serde_json::json!({ "definition_id": "bad#id" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn route_list_instances() {
    let (_dir, state) = test_state();
    call(
        &state,
        post(
            "/api/v1/bus/instances",
            serde_json::json!({ "definition_id": "izzie" }),
        ),
    )
    .await;
    let (status, body) = call(
        &state,
        Request::builder()
            .uri("/api/v1/bus/instances")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["instances"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn route_deregister_removes_instance() {
    let (_dir, state) = test_state();
    let (_, created) = call(
        &state,
        post(
            "/api/v1/bus/instances",
            serde_json::json!({ "definition_id": "izzie" }),
        ),
    )
    .await;
    let id = created["instance_id"].as_str().unwrap();
    let (status, _) = call(
        &state,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/bus/instances/{id}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(state.bus().registry().is_empty());
}

#[tokio::test]
async fn route_publish_delivers_and_returns_envelope() {
    let (_dir, state) = test_state();
    let target = state
        .bus()
        .registry()
        .register("cto-assistant", None)
        .unwrap();
    let mut rx = state.bus().subscribe(&target.instance_id).unwrap();

    let (status, body) = call(
        &state,
        post(
            "/api/v1/bus/publish",
            serde_json::json!({
                "from": { "kind": "assistant_instance", "instance_id": "izzie~aaaa1111",
                          "definition_id": "izzie" },
                "to": { "definition_id": "cto-assistant" },
                "payload": { "type": "peer_request", "text": "review please" }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["delivery_state"], "delivered");
    assert!(body["message_id"].as_str().is_some());
    assert!(rx.try_recv().is_ok());
}

#[tokio::test]
async fn route_publish_to_dead_instance_is_410() {
    // End-to-end proof of the failure-mode decision over HTTP: the sender is
    // told GONE, and is not silently served by the live sibling.
    let (_dir, state) = test_state();
    let dead = state.bus().registry().register("izzie", None).unwrap();
    state.bus().registry().deregister(&dead.instance_id);
    let sibling = state.bus().registry().register("izzie", None).unwrap();
    let mut sibling_rx = state.bus().subscribe(&sibling.instance_id).unwrap();

    let (status, body) = call(
        &state,
        post(
            "/api/v1/bus/publish",
            serde_json::json!({
                "from": { "kind": "assistant_instance", "instance_id": "cto-assistant~cccc3333",
                          "definition_id": "cto-assistant" },
                "to": { "instance_id": dead.instance_id, "definition_id": "izzie" },
                "payload": { "type": "chat_text", "text": "continue" }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
    assert!(body["error"].as_str().unwrap().contains("no longer live"));
    assert!(
        sibling_rx.try_recv().is_err(),
        "supplying definition_id alongside a dead instance_id must NOT fall back"
    );
}

#[tokio::test]
async fn route_publish_to_unknown_definition_is_404() {
    let (_dir, state) = test_state();
    let (status, _) = call(
        &state,
        post(
            "/api/v1/bus/publish",
            serde_json::json!({
                "from": { "kind": "user" },
                "to": { "definition_id": "nobody-home" },
                "payload": { "type": "chat_text", "text": "hi" }
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn route_subscribe_to_dead_instance_is_410() {
    let (_dir, state) = test_state();
    let (status, _) = call(
        &state,
        Request::builder()
            .uri("/api/v1/bus/subscribe/izzie~never")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::GONE);
}
