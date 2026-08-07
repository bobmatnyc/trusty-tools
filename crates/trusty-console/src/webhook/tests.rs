//! Failure-path coverage for the console webhook ingress (#5089 step 3).
//!
//! Rung 5: the failure arms ARE the deliverable. Each of the four arms
//! ADR-0034 §2 names has at least one case here that fails against the shape
//! this step replaces:
//!
//! | Arm | Case |
//! |---|---|
//! | spool write fails | `ingest_returns_spool_failed_and_never_accepts_when_the_write_fails`, `route_returns_500_and_no_ack_when_the_spool_write_fails` |
//! | relay fails | `relay_failure_leaves_a_pending_entry_with_an_incremented_attempt_count` |
//! | connected but not acknowledged | `relay_treats_a_result_without_ack_as_refused`, `connected_without_ack_never_deletes_the_entry` |
//! | pending entry ages out | `metrics_route_reports_red_for_an_aged_pending_entry` |
//!
//! Against the pre-fix handlers every one of those is a `202` plus a log line.

use super::*;
use crate::webhook::relay::RELAY_METHOD;
use crate::webhook::spool::{PendingListing, SPOOL_SCHEMA_VERSION, SpoolError};

use std::collections::BTreeMap;
use std::path::Path as StdPath;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tower::ServiceExt as _;
use trusty_common::console_metrics::ServiceHealth;
use trusty_common::uds::bind_hardened;
use trusty_common::webhook_hmac::sign_github_body;

const SECRET: &str = "console-ingress-test-secret"; // pragma: allowlist secret
const BODY: &[u8] = br#"{"action":"review_requested","number":42}"#;

// ─── harness ────────────────────────────────────────────────────────────────

/// How a stub target answers one relayed frame.
#[derive(Clone)]
enum StubTarget {
    /// Answer `{"result":{"ack":true}}`.
    Ack,
    /// Answer with a result object that has no `ack` field at all.
    ResultWithoutAck,
    /// Answer with a JSON-RPC error.
    RpcError,
    /// Accept the connection, read the frame, then hang up without answering.
    ConnectThenHangUp,
}

/// Frames a stub captured, so a test can assert on what console actually sent.
type Captured = std::sync::Arc<tokio::sync::Mutex<Vec<serde_json::Value>>>;

/// Bind a hardened socket under `dir` and serve `count` connections.
fn spawn_target(dir: &StdPath, behaviour: StubTarget, count: usize) -> (PathBuf, Captured) {
    let socket = dir.join("sockets").join("target.sock");
    let listener: UnixListener = bind_hardened(&socket).expect("bind stub target");
    let captured: Captured = Default::default();
    let sink = captured.clone();
    tokio::spawn(async move {
        for _ in 0..count {
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            let mut raw = Vec::new();
            let _ = conn.read_to_end(&mut raw).await;
            if let Ok(frame) = serde_json::from_slice::<serde_json::Value>(&raw) {
                sink.lock().await.push(frame);
            }
            let reply: Option<&[u8]> = match behaviour {
                StubTarget::Ack => Some(br#"{"result":{"ack":true}}"#),
                StubTarget::ResultWithoutAck => Some(br#"{"result":{}}"#),
                StubTarget::RpcError => {
                    Some(br#"{"error":{"code":-32000,"message":"store locked"}}"#)
                }
                StubTarget::ConnectThenHangUp => None,
            };
            if let Some(bytes) = reply {
                let _ = conn.write_all(bytes).await;
                let _ = conn.write_all(b"\n").await;
                let _ = conn.flush().await;
            }
        }
    });
    (socket, captured)
}

/// An ingress whose single `review` target points at `socket`.
fn ingress_for(spool: Spool, socket: PathBuf) -> WebhookIngress {
    WebhookIngress::new(
        spool,
        SECRET.to_string(),
        SECRET_ENV.to_string(),
        vec![Target {
            source: "review".to_string(),
            relay: UdsRelay::new(socket).with_timeout(Duration::from_secs(2)),
        }],
    )
}

/// A signed request header set for `body`.
fn signed_headers(body: &[u8], delivery: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        SIGNATURE_HEADER,
        sign_github_body(SECRET, body).parse().expect("sig header"),
    );
    headers.insert("x-github-event", "pull_request".parse().expect("event"));
    headers.insert("x-github-delivery", delivery.parse().expect("delivery"));
    headers
}

fn listing_of(spool: &Spool) -> PendingListing {
    spool.list_pending().expect("list spool")
}

fn sample_entry(delivery_id: &str, received_at_unix_ms: u64) -> SpoolEntry {
    SpoolEntry {
        schema_version: SPOOL_SCHEMA_VERSION,
        delivery_id: delivery_id.to_string(),
        source: "review".to_string(),
        event: "pull_request".to_string(),
        headers: BTreeMap::from([("x-github-event".to_string(), "pull_request".to_string())]),
        body_b64: BASE64.encode(BODY),
        provenance: Provenance {
            algorithm: HMAC_ALGORITHM.to_string(),
            key_id: SECRET_ENV.to_string(),
            verified: true,
        },
        received_at_unix_ms,
        attempts: 0,
        last_error: None,
        last_attempt_at_unix_ms: None,
    }
}

// ─── spool: durability ──────────────────────────────────────────────────────

#[test]
fn spool_open_creates_the_directory_at_0700() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("webhook-spool");
    let spool = Spool::open(&root).expect("open spool");
    let mode = std::fs::metadata(spool.root())
        .expect("stat spool root")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o700, "a spooled webhook body is not public data");
}

#[test]
fn spool_persists_and_reloads_an_entry_byte_exact() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let entry = sample_entry("d-1", 1_700_000_000_000);

    let path = spool.persist(&entry).expect("durable write");
    assert!(path.exists(), "the entry must be on disk before any ack");

    let listing = listing_of(&spool);
    assert_eq!(listing.pending.len(), 1);
    assert_eq!(listing.pending[0].entry, entry);
    assert_eq!(
        BASE64
            .decode(&listing.pending[0].entry.body_b64)
            .expect("decode body"),
        BODY,
        "the body must survive the round trip byte-exact — the HMAC covers it"
    );
    assert!(
        listing.undecodable.is_empty(),
        "no temp files may leak into the listing"
    );
}

#[test]
fn spool_persist_fails_when_the_root_is_not_a_directory() {
    // The spool-write-failure arm. A regular file where the directory belongs
    // makes every `File::create` inside it fail with ENOTDIR for every uid,
    // including root — so this is deterministic in any CI container.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("not-a-dir");
    std::fs::write(&root, b"blocking file").expect("write blocker");

    let spool = Spool::at(&root);
    let err = spool
        .persist(&sample_entry("d-1", 1))
        .expect_err("a delivery that cannot be written must not report success");
    assert!(
        matches!(
            err,
            SpoolError::Write { .. } | SpoolError::PrepareDir { .. }
        ),
        "expected a write failure, got {err:?}"
    );
}

#[test]
fn spool_persist_fails_when_the_final_path_is_occupied_by_a_directory() {
    // Second failure point: the write lands but the atomic rename cannot.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let entry = sample_entry("d-1", 1_700_000_000_000);
    std::fs::create_dir_all(spool.entry_path(&entry)).expect("occupy the final path");

    let err = spool
        .persist(&entry)
        .expect_err("a delivery that cannot be committed must not report success");
    assert!(
        matches!(err, SpoolError::Commit { .. }),
        "expected Commit, got {err:?}"
    );
    assert!(
        !spool.entry_path(&entry).with_extension("json.tmp").exists(),
        "a failed commit must not leave its temp file behind"
    );
}

#[test]
fn spool_record_attempt_increments_durably() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let mut entry = sample_entry("d-1", 1_700_000_000_000);
    spool.persist(&entry).expect("durable write");

    spool
        .record_attempt(&mut entry, "no listener".to_string(), 1_700_000_005_000)
        .expect("record attempt");
    spool
        .record_attempt(&mut entry, "no listener".to_string(), 1_700_000_010_000)
        .expect("record attempt");

    let listing = listing_of(&spool);
    assert_eq!(listing.pending.len(), 1, "attempts must not fork the entry");
    let reloaded = &listing.pending[0].entry;
    assert_eq!(reloaded.attempts, 2);
    assert_eq!(reloaded.last_error.as_deref(), Some("no listener"));
    assert_eq!(reloaded.last_attempt_at_unix_ms, Some(1_700_000_010_000));
}

#[test]
fn spool_remove_acked_deletes_the_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let entry = sample_entry("d-1", 1);
    let path = spool.persist(&entry).expect("durable write");

    spool.remove_acked(&path).expect("remove");
    assert!(listing_of(&spool).pending.is_empty());
}

#[test]
fn spool_remove_acked_tolerates_an_already_removed_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let path = spool.root().join("0000000000001-gone.json");
    spool
        .remove_acked(&path)
        .expect("a missing entry is not an error");
}

#[test]
fn spool_entry_path_sanitises_a_hostile_delivery_id() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    // The delivery header is not covered by the HMAC, so it is attacker input.
    let entry = sample_entry("../../../etc/passwd", 7);
    let path = spool.entry_path(&entry);
    assert_eq!(
        path.parent(),
        Some(spool.root()),
        "a delivery id must never escape the spool directory: {}",
        path.display()
    );
    assert!(!path.to_string_lossy().contains(".."));
}

#[test]
fn spool_list_pending_orders_oldest_first() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    for (id, at) in [("c", 300u64), ("a", 100), ("b", 200)] {
        spool.persist(&sample_entry(id, at)).expect("durable write");
    }
    let ids: Vec<String> = listing_of(&spool)
        .pending
        .into_iter()
        .map(|p| p.entry.delivery_id)
        .collect();
    assert_eq!(ids, vec!["a", "b", "c"]);
}

#[test]
fn spool_list_pending_reports_an_undecodable_entry() {
    // An unreadable pending delivery is not an absent one. Silently skipping it
    // would report the spool empty while a delivery sits unrelayed.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    std::fs::write(spool.root().join("0000000000001-junk.json"), b"{not json").expect("write junk");

    let listing = listing_of(&spool);
    assert!(listing.pending.is_empty());
    assert_eq!(listing.undecodable.len(), 1);
}

// ─── relay: only an explicit ack counts ─────────────────────────────────────

#[tokio::test]
async fn relay_frame_carries_provenance_and_byte_exact_body() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, captured) = spawn_target(tmp.path(), StubTarget::Ack, 1);
    let relay = UdsRelay::new(socket).with_timeout(Duration::from_secs(2));

    let entry = sample_entry("d-frame", 1_700_000_000_000);
    assert_eq!(relay.deliver(&entry).await, RelayOutcome::Acked);

    let frames = captured.lock().await;
    let frame = frames.first().expect("target received a frame");
    assert_eq!(frame["jsonrpc"], "2.0");
    assert_eq!(frame["method"], RELAY_METHOD);
    assert_eq!(frame["id"], "d-frame");
    let params = &frame["params"];
    assert_eq!(params["provenance"]["algorithm"], HMAC_ALGORITHM);
    assert_eq!(params["provenance"]["key_id"], SECRET_ENV);
    assert_eq!(params["provenance"]["verified"], true);
    assert_eq!(
        BASE64
            .decode(params["body_b64"].as_str().expect("body_b64 is a string"))
            .expect("decode relayed body"),
        BODY,
        "the target must receive the exact bytes GitHub signed"
    );
    assert!(
        !frame.to_string().contains(SECRET),
        "the secret must never leave console"
    );
}

#[tokio::test]
async fn relay_acked_response_is_the_only_ack() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _) = spawn_target(tmp.path(), StubTarget::Ack, 1);
    let relay = UdsRelay::new(socket).with_timeout(Duration::from_secs(2));
    assert!(relay.deliver(&sample_entry("d-1", 1)).await.is_acked());
}

#[tokio::test]
async fn relay_treats_a_result_without_ack_as_refused() {
    // The "connected but has not acknowledged" arm. A target that answers with
    // an empty result object has done nothing; treating the successful
    // round trip as success is the same silent loss one layer down.
    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _) = spawn_target(tmp.path(), StubTarget::ResultWithoutAck, 1);
    let relay = UdsRelay::new(socket).with_timeout(Duration::from_secs(2));

    let outcome = relay.deliver(&sample_entry("d-1", 1)).await;
    assert!(
        matches!(outcome, RelayOutcome::Refused { .. }),
        "expected Refused, got {outcome:?}"
    );
    assert!(!outcome.is_acked());
}

#[tokio::test]
async fn relay_treats_a_jsonrpc_error_as_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _) = spawn_target(tmp.path(), StubTarget::RpcError, 1);
    let relay = UdsRelay::new(socket).with_timeout(Duration::from_secs(2));

    let outcome = relay.deliver(&sample_entry("d-1", 1)).await;
    assert!(!outcome.is_acked());
    assert!(
        outcome.reason().contains("store locked"),
        "the target's own reason must survive into the durable record: {}",
        outcome.reason()
    );
}

#[tokio::test]
async fn relay_reports_unreachable_when_no_listener_is_bound() {
    // The expected state for every delivery until #5089 step 4 binds the
    // target's listener.
    let tmp = tempfile::tempdir().expect("tempdir");
    let relay = UdsRelay::new(tmp.path().join("sockets").join("absent.sock"))
        .with_timeout(Duration::from_secs(2));
    let outcome = relay.deliver(&sample_entry("d-1", 1)).await;
    assert!(
        matches!(outcome, RelayOutcome::Unreachable { .. }),
        "expected Unreachable, got {outcome:?}"
    );
}

#[tokio::test]
async fn relay_reports_unreachable_when_the_target_hangs_up_without_answering() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (socket, _) = spawn_target(tmp.path(), StubTarget::ConnectThenHangUp, 1);
    let relay = UdsRelay::new(socket).with_timeout(Duration::from_secs(2));
    let outcome = relay.deliver(&sample_entry("d-1", 1)).await;
    assert!(
        matches!(outcome, RelayOutcome::Unreachable { .. }),
        "a connection that succeeds and then dies is not a delivery: {outcome:?}"
    );
}

// ─── ingest: the ordering guarantee ─────────────────────────────────────────

#[tokio::test]
async fn ingest_rejects_an_unknown_source() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"));

    let outcome = ingress
        .ingest("nope", &signed_headers(BODY, "d-1"), BODY)
        .await;
    assert!(matches!(outcome, IngestOutcome::UnknownSource { .. }));
    assert!(
        listing_of(ingress.spool()).pending.is_empty(),
        "an unroutable source must not consume spool space"
    );
}

#[tokio::test]
async fn ingest_fails_closed_when_no_secret_is_configured() {
    // ADR-0034 §2 unifies the policy to trusty-review's fail-closed answer.
    // trusty-analyze's "log a warning and process it anyway" is gone.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = WebhookIngress::new(
        spool,
        String::new(),
        SECRET_ENV.to_string(),
        vec![Target {
            source: "review".to_string(),
            relay: UdsRelay::new(tmp.path().join("sockets").join("absent.sock")),
        }],
    );

    let outcome = ingress
        .ingest("review", &signed_headers(BODY, "d-1"), BODY)
        .await;
    assert_eq!(outcome, IngestOutcome::SecretMissing);
    assert!(
        listing_of(ingress.spool()).pending.is_empty(),
        "an unverified body must never reach the spool"
    );
}

#[tokio::test]
async fn ingest_rejects_a_forged_signature() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"));

    // Signed correctly, then the body is changed by one byte in flight.
    let headers = signed_headers(BODY, "d-1");
    let mut tampered = BODY.to_vec();
    let last = tampered.len() - 1;
    tampered[last] ^= 0x01;

    let outcome = ingress.ingest("review", &headers, &tampered).await;
    assert_eq!(outcome, IngestOutcome::InvalidSignature);
    assert!(listing_of(ingress.spool()).pending.is_empty());
}

#[tokio::test]
async fn ingest_returns_spool_failed_and_never_accepts_when_the_write_fails() {
    // 🔴 The regression this step exists for. The pre-fix handlers return 202
    // and log; here the durable write is a precondition of accepting at all.
    let tmp = tempfile::tempdir().expect("tempdir");
    let blocked = tmp.path().join("not-a-dir");
    std::fs::write(&blocked, b"blocking file").expect("write blocker");
    // The stub WOULD ack — proving the refusal comes from the spool, not the
    // relay, and that no relay ever runs when the write fails.
    let (socket, captured) = spawn_target(tmp.path(), StubTarget::Ack, 1);
    let ingress = ingress_for(Spool::at(&blocked), socket);

    let outcome = ingress
        .ingest("review", &signed_headers(BODY, "d-1"), BODY)
        .await;

    assert!(
        matches!(outcome, IngestOutcome::SpoolFailed { .. }),
        "a delivery that could not be recorded must not be accepted: {outcome:?}"
    );
    assert!(
        !matches!(outcome, IngestOutcome::Accepted { .. }),
        "no ack may be issued when the spool write failed"
    );
    assert!(
        captured.lock().await.is_empty(),
        "the relay must not run before the delivery is durable"
    );
}

#[tokio::test]
async fn ingest_accepts_and_deletes_on_an_explicit_ack() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let (socket, _) = spawn_target(tmp.path(), StubTarget::Ack, 1);
    let ingress = ingress_for(spool, socket);

    let outcome = ingress
        .ingest("review", &signed_headers(BODY, "d-ack"), BODY)
        .await;

    match outcome {
        IngestOutcome::Accepted {
            delivery_id,
            relay,
            bookkeeping_error,
        } => {
            assert_eq!(delivery_id, "d-ack");
            assert!(relay.is_acked());
            assert_eq!(bookkeeping_error, None);
        }
        other => panic!("expected Accepted, got {other:?}"),
    }
    assert!(
        listing_of(ingress.spool()).pending.is_empty(),
        "an acknowledged delivery is the only one that may be deleted"
    );
}

#[tokio::test]
async fn relay_failure_leaves_a_pending_entry_with_an_incremented_attempt_count() {
    // 🔴 The second regression. The pre-fix handlers drop the payload here.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    // No listener: the state every delivery is in until #5089 step 4.
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"));

    let outcome = ingress
        .ingest("review", &signed_headers(BODY, "d-pending"), BODY)
        .await;

    match &outcome {
        IngestOutcome::Accepted { relay, .. } => assert!(
            matches!(relay, RelayOutcome::Unreachable { .. }),
            "expected Unreachable, got {relay:?}"
        ),
        other => panic!("expected Accepted, got {other:?}"),
    }

    let listing = listing_of(ingress.spool());
    assert_eq!(
        listing.pending.len(),
        1,
        "a failed relay must NEVER delete the entry"
    );
    let entry = &listing.pending[0].entry;
    assert_eq!(entry.delivery_id, "d-pending");
    assert_eq!(entry.attempts, 1, "the attempt must be recorded durably");
    assert!(entry.last_error.is_some(), "and so must the reason");
    assert!(entry.last_attempt_at_unix_ms.is_some());
    assert_eq!(
        BASE64.decode(&entry.body_b64).expect("decode"),
        BODY,
        "the pending entry must still hold the original body for redelivery"
    );
}

#[tokio::test]
async fn connected_without_ack_never_deletes_the_entry() {
    // 🔴 The third arm: the connection succeeded, so a naive implementation
    // would call this done. ADR-0034 §2 makes the explicit ack the only
    // delete trigger.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let (socket, captured) = spawn_target(tmp.path(), StubTarget::ResultWithoutAck, 1);
    let ingress = ingress_for(spool, socket);

    let outcome = ingress
        .ingest("review", &signed_headers(BODY, "d-noack"), BODY)
        .await;
    assert!(matches!(outcome, IngestOutcome::Accepted { .. }));
    assert_eq!(
        captured.lock().await.len(),
        1,
        "the target really was reached"
    );

    let listing = listing_of(ingress.spool());
    assert_eq!(
        listing.pending.len(),
        1,
        "reaching the target is not the same as the target acknowledging"
    );
    assert_eq!(listing.pending[0].entry.attempts, 1);
}

// ─── health: oldest-pending age is red, on demand ───────────────────────────

#[test]
fn health_reports_ok_on_an_empty_spool() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let h = health::scan_health(&spool, 1_700_000_000_000, Duration::from_secs(600));
    assert_eq!(h.status, ServiceHealth::Ok);
    assert_eq!(h.pending, 0);
    assert_eq!(h.oldest_pending_age_secs, None);
    assert_eq!(h.scan_error, None);
}

#[test]
fn health_reports_degraded_for_a_young_pending_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let now = 1_700_000_000_000u64;
    spool
        .persist(&sample_entry("d-young", now - 30_000))
        .expect("durable write");

    let h = health::scan_health(&spool, now, Duration::from_secs(600));
    assert_eq!(h.status, ServiceHealth::Degraded);
    assert_eq!(h.pending, 1);
    assert_eq!(h.oldest_pending_age_secs, Some(30));
}

#[test]
fn health_reports_error_once_the_oldest_entry_passes_the_threshold() {
    // 🔴 The fourth arm. Red is a state a dashboard renders, not a warn! line.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let now = 1_700_000_000_000u64;
    let mut aged = sample_entry("d-aged", now - 900_000); // 15 minutes
    aged.attempts = 14;
    aged.last_error = Some("no listener bound".to_string());
    spool.persist(&aged).expect("durable write");
    spool
        .persist(&sample_entry("d-young", now - 1_000))
        .expect("durable write");

    let h = health::scan_health(&spool, now, Duration::from_secs(600));
    assert_eq!(h.status, ServiceHealth::Error);
    assert_eq!(h.pending, 2);
    assert_eq!(h.oldest_pending_age_secs, Some(900));
    assert_eq!(h.oldest_pending_delivery_id.as_deref(), Some("d-aged"));
    assert_eq!(
        h.oldest_pending_last_error.as_deref(),
        Some("no listener bound")
    );
    assert_eq!(h.total_failed_attempts, 14);
}

#[test]
fn health_reports_error_for_an_undecodable_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    std::fs::write(spool.root().join("0000000000001-junk.json"), b"{not json").expect("write junk");

    let h = health::scan_health(&spool, 1_700_000_000_000, Duration::from_secs(600));
    assert_eq!(
        h.status,
        ServiceHealth::Error,
        "a pending delivery that cannot be read is not a healthy spool"
    );
    assert_eq!(h.undecodable.len(), 1);
}

#[test]
fn health_reports_error_when_the_spool_cannot_be_read() {
    // A spool that cannot be listed must not read as an empty (healthy) one —
    // that is the fail-quiet shape one level down from the one being fixed.
    let tmp = tempfile::tempdir().expect("tempdir");
    let blocked = tmp.path().join("not-a-dir");
    std::fs::write(&blocked, b"blocking file").expect("write blocker");

    let h = health::scan_health(
        &Spool::at(&blocked),
        1_700_000_000_000,
        Duration::from_secs(600),
    );
    assert_eq!(h.status, ServiceHealth::Error);
    assert!(h.scan_error.is_some(), "the scan failure must be reported");
}

// ─── retry sweep ────────────────────────────────────────────────────────────

#[tokio::test]
async fn retry_sweep_acks_and_clears_a_pending_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let (socket, _) = spawn_target(tmp.path(), StubTarget::Ack, 1);
    let ingress = ingress_for(spool, socket);
    ingress
        .spool()
        .persist(&sample_entry("d-1", 1_700_000_000_000))
        .expect("durable write");

    let report = ingress.retry_pending_once().await;
    assert_eq!(report.acked, 1);
    assert_eq!(report.still_pending, 0);
    assert!(listing_of(ingress.spool()).pending.is_empty());
}

#[tokio::test]
async fn retry_sweep_leaves_an_unrelayable_entry_pending_with_more_attempts() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"));
    ingress
        .spool()
        .persist(&sample_entry("d-1", 1_700_000_000_000))
        .expect("durable write");

    for expected in 1..=3u32 {
        let report = ingress.retry_pending_once().await;
        assert_eq!(report.still_pending, 1);
        assert_eq!(report.acked, 0);
        let listing = listing_of(ingress.spool());
        assert_eq!(
            listing.pending.len(),
            1,
            "the entry must survive every sweep"
        );
        assert_eq!(listing.pending[0].entry.attempts, expected);
    }
}

#[tokio::test]
async fn retry_sweep_reports_an_orphaned_entry_without_deleting_it() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"));
    let mut orphan = sample_entry("d-orphan", 1_700_000_000_000);
    orphan.source = "retired-target".to_string();
    ingress.spool().persist(&orphan).expect("durable write");

    let report = ingress.retry_pending_once().await;
    assert_eq!(report.orphaned, 1);
    assert_eq!(
        listing_of(ingress.spool()).pending.len(),
        1,
        "a config change must not silently discard a delivery"
    );
}

// ─── HTTP surface ───────────────────────────────────────────────────────────

/// A router carrying only the webhook sub-surface plus the real console routes.
fn router_for(ingress: WebhookIngress) -> axum::Router {
    crate::server::build_router_with_webhooks(
        crate::server::AppState::new(Vec::new()),
        crate::routes::origin_guard::SelfOrigins::default(),
        ingress,
    )
}

async fn post_webhook(
    router: axum::Router,
    source: &str,
    body: &[u8],
    headers: HeaderMap,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(format!("/api/webhooks/{source}"))
        .header("content-type", "application/json");
    for (name, value) in headers.iter() {
        req = req.header(name, value);
    }
    let response = router
        .oneshot(req.body(Body::from(body.to_vec())).expect("build request"))
        .await
        .expect("route the request");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

#[tokio::test]
async fn route_returns_202_after_a_durable_write() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"));

    let (status, body) = post_webhook(
        router_for(ingress.clone()),
        "review",
        BODY,
        signed_headers(BODY, "d-http"),
    )
    .await;

    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["status"], "accepted");
    assert_eq!(
        body["relay"], "pending",
        "the ack reports the relay state honestly rather than claiming success"
    );
    assert_eq!(listing_of(ingress.spool()).pending.len(), 1);
}

#[tokio::test]
async fn route_returns_500_and_no_ack_when_the_spool_write_fails() {
    // 🔴 GitHub must see a failed delivery it can redeliver from its UI.
    let tmp = tempfile::tempdir().expect("tempdir");
    let blocked = tmp.path().join("not-a-dir");
    std::fs::write(&blocked, b"blocking file").expect("write blocker");
    let ingress = ingress_for(
        Spool::at(&blocked),
        tmp.path().join("sockets").join("absent.sock"),
    );

    let (status, body) = post_webhook(
        router_for(ingress),
        "review",
        BODY,
        signed_headers(BODY, "d-http"),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a 2xx here would make the delivery permanently unrecoverable"
    );
    assert!(!status.is_success());
    assert_eq!(body["status"], serde_json::Value::Null);
}

#[tokio::test]
async fn route_returns_401_for_an_unset_secret() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = WebhookIngress::new(
        spool,
        String::new(),
        SECRET_ENV.to_string(),
        vec![Target {
            source: "review".to_string(),
            relay: UdsRelay::new(tmp.path().join("sockets").join("absent.sock")),
        }],
    );

    let (status, _) = post_webhook(
        router_for(ingress.clone()),
        "review",
        BODY,
        signed_headers(BODY, "d-http"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(listing_of(ingress.spool()).pending.is_empty());
}

#[tokio::test]
async fn route_returns_401_for_a_forged_signature() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"));

    let mut headers = HeaderMap::new();
    headers.insert(SIGNATURE_HEADER, "sha256=deadbeef".parse().expect("sig"));
    let (status, _) = post_webhook(router_for(ingress.clone()), "review", BODY, headers).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(listing_of(ingress.spool()).pending.is_empty());
}

#[tokio::test]
async fn route_returns_404_for_an_unknown_source() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"));

    let (status, _) = post_webhook(
        router_for(ingress),
        "not-a-target",
        BODY,
        signed_headers(BODY, "d-http"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn webhook_route_is_not_shadowed_by_the_service_proxy() {
    // `/api/{service}/{*path}` would swallow `/api/webhooks/review` if matchit
    // preferred the capture. It does not — but a router edit could reorder
    // things, and a proxied webhook is a silently dropped one.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"));

    let (status, body) = post_webhook(
        router_for(ingress),
        "review",
        BODY,
        signed_headers(BODY, "d-http"),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["delivery_id"], "d-http");
}

async fn get_json(router: axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let response = router
        .oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("route the request");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
        .await
        .expect("read body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn metrics_route_reports_ok_on_an_empty_spool() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"));

    let (status, body) = get_json(router_for(ingress), "/api/console/metrics/webhooks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["service_id"], health::WEBHOOK_SERVICE_ID);
    assert_eq!(body["status"], "ok");
    assert_eq!(body["metrics"]["pending"], 0);
}

#[tokio::test]
async fn metrics_route_reports_red_for_an_aged_pending_entry() {
    // 🔴 The detection arm, end to end: the route scans the spool on THIS
    // request, so the red state does not depend on a background loop still
    // being alive.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"))
        // Anything already on disk is instantly past a zero threshold.
        .with_red_after(Duration::from_secs(0));
    ingress
        .spool()
        .persist(&sample_entry("d-stuck", 1))
        .expect("durable write");

    let (status, body) = get_json(router_for(ingress), "/api/console/metrics/webhooks").await;
    assert_eq!(status, StatusCode::OK, "a red state is data, not an outage");
    assert_eq!(body["status"], "error");
    assert_eq!(body["metrics"]["pending"], 1);
    assert_eq!(body["metrics"]["oldest_pending_delivery_id"], "d-stuck");
    assert!(body["metrics"]["oldest_pending_age_secs"].is_number());
}

#[tokio::test]
async fn metrics_route_reports_red_when_the_spool_cannot_be_read() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let blocked = tmp.path().join("not-a-dir");
    std::fs::write(&blocked, b"blocking file").expect("write blocker");
    let ingress = ingress_for(
        Spool::at(&blocked),
        tmp.path().join("sockets").join("absent.sock"),
    );

    let (status, body) = get_json(router_for(ingress), "/api/console/metrics/webhooks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "error");
    assert!(body["metrics"]["scan_error"].is_string());
}

// ─── integration (`--include-ignored`) ──────────────────────────────────────

#[test]
fn default_spool_root_lives_under_the_console_data_dir() {
    // Pure assertion on the path convention — ADR-0034 puts the spool under
    // console's own state directory, which `lib.rs:476` already treats as
    // canonical. No env mutation, so this one runs in the default suite.
    let root = default_spool_root().expect("resolve the default spool root");
    assert!(
        root.ends_with(std::path::Path::new(spool::SPOOL_DIR_NAME)),
        "spool root {} must be the webhook-spool subdirectory",
        root.display()
    );
    let data_dir = trusty_common::resolve_data_dir("trusty-console").expect("resolve data dir");
    assert_eq!(
        root.parent(),
        Some(data_dir.as_path()),
        "the spool must not invent a new location convention"
    );
}

/// Serialises the tests below, which mutate process-global environment
/// variables that every concurrently-running sibling test can observe.
/// An async mutex, not a `std` one: the guard is held across `.await` for the
/// whole body, which is exactly what a blocking guard must not do.
static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Set `key` to `value` (or remove it), returning the prior value.
///
/// `set_var`/`remove_var` are `unsafe` in edition 2024 because they are not
/// thread-safe; [`ENV_LOCK`] plus the `#[ignore]` tag is what makes these call
/// sites sound in this binary.
fn swap_env(key: &str, value: Option<&str>) -> Option<String> {
    let prior = std::env::var(key).ok();
    match value {
        Some(v) => unsafe { std::env::set_var(key, v) },
        None => unsafe { std::env::remove_var(key) },
    }
    prior
}

#[tokio::test]
#[ignore = "mutates TRUSTY_DATA_DIR_OVERRIDE and GITHUB_WEBHOOK_SECRET, which every \
            concurrently-running test in this binary can observe; run with --include-ignored"]
async fn integration_from_env_delivery_survives_a_console_restart() {
    // The full ADR-0034 §2 loop, against the real `from_env` wiring:
    //   verified delivery -> durable spool -> relay fails (no listener yet)
    //   -> a FRESH ingress over the same directory still finds it pending
    //   -> the target comes up and acks -> the entry is gone.
    // This is the property the pre-fix handlers lack entirely: after their
    // 202, nothing on disk records that the delivery ever existed.
    let _guard = ENV_LOCK.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");

    let prior_data_dir = swap_env(
        trusty_common::DATA_DIR_OVERRIDE_ENV,
        Some(&tmp.path().to_string_lossy()),
    );
    let prior_secret = swap_env(SECRET_ENV, Some(SECRET));

    let outcome = async {
        // ── first boot: nothing is listening, as until #5089 step 4 ──────────
        let ingress = WebhookIngress::from_env().expect("build ingress from env");
        assert!(
            ingress
                .spool()
                .root()
                .starts_with(tmp.path().join("trusty-console")),
            "spool must land under the overridden console data dir: {}",
            ingress.spool().root().display()
        );

        let (status, body) = post_webhook(
            router_for(ingress.clone()),
            "review",
            BODY,
            signed_headers(BODY, "d-restart"),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["relay"], "pending");

        let health = ingress.health();
        assert_eq!(health.pending, 1);
        assert_eq!(health.status, ServiceHealth::Degraded);

        // ── restart: a brand-new ingress over the same directory ─────────────
        let restarted = WebhookIngress::from_env().expect("rebuild ingress from env");
        let listing = listing_of(restarted.spool());
        assert_eq!(
            listing.pending.len(),
            1,
            "the delivery must outlive the process that accepted it"
        );
        assert_eq!(listing.pending[0].entry.delivery_id, "d-restart");
        assert_eq!(listing.pending[0].entry.attempts, 1);
        assert_eq!(
            BASE64
                .decode(&listing.pending[0].entry.body_b64)
                .expect("decode"),
            BODY
        );

        // ── the target finally comes up and acknowledges ─────────────────────
        let (socket, captured) = spawn_target(tmp.path(), StubTarget::Ack, 1);
        let with_target = ingress_for(Spool::at(restarted.spool().root().to_path_buf()), socket);
        let report = with_target.retry_pending_once().await;
        assert_eq!(report.acked, 1);
        assert_eq!(report.still_pending, 0);
        assert_eq!(captured.lock().await.len(), 1);

        let final_health = with_target.health();
        assert_eq!(final_health.pending, 0);
        assert_eq!(final_health.status, ServiceHealth::Ok);
    }
    .await;

    swap_env(
        trusty_common::DATA_DIR_OVERRIDE_ENV,
        prior_data_dir.as_deref(),
    );
    swap_env(SECRET_ENV, prior_secret.as_deref());
    outcome
}

#[tokio::test]
#[ignore = "mutates TRUSTY_DATA_DIR_OVERRIDE and GITHUB_WEBHOOK_SECRET; \
            run with --include-ignored"]
async fn integration_from_env_fails_closed_when_the_secret_is_unset() {
    // ADR-0034 §2's unified policy, against the real env wiring: an operator
    // who never set the secret gets a 401 and an empty spool, not
    // trusty-analyze's "skip verification and process it anyway".
    let _guard = ENV_LOCK.lock().await;
    let tmp = tempfile::tempdir().expect("tempdir");

    let prior_data_dir = swap_env(
        trusty_common::DATA_DIR_OVERRIDE_ENV,
        Some(&tmp.path().to_string_lossy()),
    );
    let prior_secret = swap_env(SECRET_ENV, None);

    let outcome = async {
        let ingress = WebhookIngress::from_env().expect("build ingress from env");
        let (status, _) = post_webhook(
            router_for(ingress.clone()),
            "review",
            BODY,
            signed_headers(BODY, "d-nosecret"),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(listing_of(ingress.spool()).pending.is_empty());
        assert_eq!(ingress.health().status, ServiceHealth::Ok);
    }
    .await;

    swap_env(
        trusty_common::DATA_DIR_OVERRIDE_ENV,
        prior_data_dir.as_deref(),
    );
    swap_env(SECRET_ENV, prior_secret.as_deref());
    outcome
}
