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
use crate::webhook::spool::{PendingListing, SPOOL_SCHEMA_VERSION, SpoolError};
use trusty_common::webhook_relay::RELAY_METHOD;

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
    /// Answer `ack` only after a delay, so a second relay has time to race the
    /// first. This is what makes the sweep-versus-request window observable.
    AckAfter(Duration),
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
            if let StubTarget::AckAfter(delay) = behaviour {
                tokio::time::sleep(delay).await;
            }
            let reply: Option<&[u8]> = match behaviour {
                StubTarget::Ack | StubTarget::AckAfter(_) => Some(br#"{"result":{"ack":true}}"#),
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

/// A stub that announces when it has received a frame, then waits for the test
/// to release it before answering.
///
/// Why: the previous version of the sweep-race test slept 150 ms and hoped
/// `ingest` had reached its claim by then. That asserts on a window the test
/// does not control — and it does not control it: under CI scheduling the
/// spawned task had not even persisted the entry yet, so the sweep listed an
/// empty spool and the assertion failed for a reason unrelated to claims.
/// A rendezvous removes the wall clock entirely: the target has the frame, so
/// the relay is provably in flight, so the claim is provably held.
/// What: returns the socket, the captured frames, a receiver that fires once
/// the first frame has been read, and a sender that lets the stub answer. Any
/// connection after the first is answered immediately, so a second relay — the
/// defect — is observable rather than hanging.
/// Test: `sweep_does_not_relay_an_entry_the_request_path_is_still_relaying`.
fn spawn_gated_target(
    dir: &StdPath,
) -> (
    PathBuf,
    Captured,
    tokio::sync::oneshot::Receiver<()>,
    tokio::sync::oneshot::Sender<()>,
) {
    let socket = dir.join("sockets").join("gated.sock");
    let listener: UnixListener = bind_hardened(&socket).expect("bind gated stub");
    let captured: Captured = Default::default();
    let sink = captured.clone();
    let (received_tx, received_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        async fn absorb(conn: &mut tokio::net::UnixStream, sink: &Captured) {
            let mut raw = Vec::new();
            let _ = conn.read_to_end(&mut raw).await;
            if let Ok(frame) = serde_json::from_slice::<serde_json::Value>(&raw) {
                sink.lock().await.push(frame);
            }
        }
        async fn ack(conn: &mut tokio::net::UnixStream) {
            let _ = conn.write_all(br#"{"result":{"ack":true}}"#).await;
            let _ = conn.write_all(b"\n").await;
            let _ = conn.flush().await;
        }

        let Ok((mut first, _)) = listener.accept().await else {
            return;
        };
        absorb(&mut first, &sink).await;
        let _ = received_tx.send(());
        let _ = release_rx.await;
        ack(&mut first).await;

        while let Ok((mut extra, _)) = listener.accept().await {
            absorb(&mut extra, &sink).await;
            ack(&mut extra).await;
        }
    });

    (socket, captured, received_rx, release_tx)
}

/// A schedule that admits every pending entry immediately.
///
/// Most cases here are asserting on relay and spool behaviour, not on timing;
/// the real 5 s grace and 30 s base would make each of them sleep. The
/// `backoff_*` and `sweep_honours_*` cases use the real policy instead.
fn no_backoff() -> BackoffPolicy {
    BackoffPolicy {
        first_attempt_grace: Duration::ZERO,
        base: Duration::ZERO,
        ceiling: Duration::ZERO,
        max_attempts: u32::MAX,
    }
}

/// An ingress whose single `review` target points at `socket`.
fn ingress_for(spool: Spool, socket: PathBuf) -> WebhookIngress {
    ingress_with(spool, socket, Duration::from_secs(2)).with_backoff(no_backoff())
}

/// [`ingress_for`] keeping the production backoff schedule and taking an
/// explicit relay timeout.
fn ingress_with(spool: Spool, socket: PathBuf, relay_timeout: Duration) -> WebhookIngress {
    WebhookIngress::new(
        spool,
        SECRET.to_string(),
        SECRET_ENV.to_string(),
        vec![Target {
            source: "review".to_string(),
            relay: UdsRelay::new(socket).with_timeout(relay_timeout),
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

/// How many uncommitted temp files the spool directory holds.
///
/// Every failure path must clean up after itself; a leaked temp file is junk
/// the health scan then has to learn to ignore.
fn temp_files_in(spool: &Spool) -> usize {
    std::fs::read_dir(spool.root())
        .expect("read spool root")
        .filter_map(Result::ok)
        .filter(|e| e.path().to_string_lossy().ends_with(".tmp"))
        .count()
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

    let path = spool.persist_new(&entry).expect("durable write");
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
        .persist_new(&sample_entry("d-1", 1))
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
fn spool_persist_new_refuses_to_clobber_an_existing_entry() {
    // The entry already at that path may be one console has ALREADY
    // acknowledged. Overwriting it destroys a delivery GitHub will never
    // re-send, so `persist_new` commits with `hard_link` (atomic EEXIST) rather
    // than `rename` (silent replace).
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let first = sample_entry("d-collide", 1_700_000_000_000);
    spool.persist_new(&first).expect("first write");

    // Same millisecond, same (missing-header-derived) id: the collision case.
    let mut second = sample_entry("d-collide", 1_700_000_000_000);
    second.event = "issues".to_string();

    let err = spool
        .persist_new(&second)
        .expect_err("clobbering an existing entry must not report success");
    assert!(
        matches!(err, SpoolError::AlreadyExists { .. }),
        "expected AlreadyExists, got {err:?}"
    );

    let listing = listing_of(&spool);
    assert_eq!(listing.pending.len(), 1);
    assert_eq!(
        listing.pending[0].entry, first,
        "the entry already on disk must survive untouched"
    );
    assert_eq!(
        temp_files_in(&spool),
        0,
        "a refused write must not leave its temp file behind"
    );
}

#[test]
fn spool_persist_update_fails_when_the_final_path_is_a_directory() {
    // `record_attempt` wants clobber semantics, so it commits with `rename` —
    // which still has to fail loudly when the destination cannot be replaced.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let entry = sample_entry("d-1", 1_700_000_000_000);
    std::fs::create_dir_all(spool.entry_path(&entry)).expect("occupy the final path");

    let err = spool
        .persist_update(&entry)
        .expect_err("a rewrite that cannot be committed must not report success");
    assert!(
        matches!(err, SpoolError::Commit { .. }),
        "expected Commit, got {err:?}"
    );
    assert_eq!(
        temp_files_in(&spool),
        0,
        "a failed commit must not leave its temp file behind"
    );
}

#[test]
fn spool_record_attempt_increments_durably() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let mut entry = sample_entry("d-1", 1_700_000_000_000);
    spool.persist_new(&entry).expect("durable write");

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
    let path = spool.persist_new(&entry).expect("durable write");

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
        spool
            .persist_new(&sample_entry(id, at))
            .expect("durable write");
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
        .persist_new(&sample_entry("d-young", now - 30_000))
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
    spool.persist_new(&aged).expect("durable write");
    spool
        .persist_new(&sample_entry("d-young", now - 1_000))
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
    assert_eq!(h.oldest_pending_attempts, Some(14));
    assert_eq!(h.exhausted, 0, "nothing has been given up on yet");
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
        .persist_new(&sample_entry("d-1", 1_700_000_000_000))
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
        .persist_new(&sample_entry("d-1", 1_700_000_000_000))
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
    ingress.spool().persist_new(&orphan).expect("durable write");

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
        .persist_new(&sample_entry("d-stuck", 1))
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

        let health = ingress.health().await;
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

        let final_health = with_target.health().await;
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
        assert_eq!(ingress.health().await.status, ServiceHealth::Ok);
    }
    .await;

    swap_env(
        trusty_common::DATA_DIR_OVERRIDE_ENV,
        prior_data_dir.as_deref(),
    );
    swap_env(SECRET_ENV, prior_secret.as_deref());
    outcome
}

// ─── concurrency: one delivery, one relay ───────────────────────────────────

#[tokio::test]
async fn sweep_does_not_relay_an_entry_the_request_path_is_still_relaying() {
    // 🔴 Two defects met here. The claim used to be taken AFTER the durable
    // write, leaving a window in which the entry was on disk and unclaimed —
    // and a sweep landing in it relayed a delivery `ingest` was about to relay
    // itself. Measured at the pre-fix head: a sweep 20 ms into `ingest`
    // reported `acked: 1`, two relays for one delivery.
    //
    // The test itself was also racy: it slept 150 ms and assumed `ingest` had
    // reached its claim. Under CI scheduling it had not even persisted yet, so
    // the sweep listed an empty spool. The rendezvous below removes the wall
    // clock — the target holding the frame is proof the relay is in flight.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let (socket, captured, received, release) = spawn_gated_target(tmp.path());
    let ingress = ingress_for(spool, socket);

    let requester = {
        let ingress = ingress.clone();
        tokio::spawn(async move {
            ingress
                .ingest("review", &signed_headers(BODY, "d-race"), BODY)
                .await
        })
    };

    // The target has the frame, so the relay is in flight and the claim is
    // held. No sleep, no guess.
    received
        .await
        .expect("the target received the relayed frame");

    let sweep = ingress.retry_pending_once().await;
    assert_eq!(
        sweep.in_flight, 1,
        "the sweep must see the entry as claimed, not free to relay: {sweep:?}"
    );
    assert_eq!(sweep.acked, 0, "and must not relay it: {sweep:?}");
    assert_eq!(sweep.still_pending, 0, "{sweep:?}");

    release.send(()).expect("release the target");
    match requester.await.expect("ingest task") {
        IngestOutcome::Accepted { relay, .. } => assert!(relay.is_acked()),
        other => panic!("expected Accepted, got {other:?}"),
    }

    assert_eq!(
        captured.lock().await.len(),
        1,
        "one delivery must produce exactly one relay"
    );
    assert!(listing_of(ingress.spool()).pending.is_empty());

    // The claim must be released once the relay settles, not leaked.
    let after = ingress.retry_pending_once().await;
    assert_eq!(after.in_flight, 0, "the claim must not outlive the relay");
}

#[tokio::test]
async fn two_concurrent_sweeps_relay_each_entry_only_once() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let (socket, captured) = spawn_target(
        tmp.path(),
        StubTarget::AckAfter(Duration::from_millis(400)),
        6,
    );
    let ingress = ingress_for(spool, socket);
    for id in ["d-a", "d-b"] {
        ingress
            .spool()
            .persist_new(&sample_entry(id, 1_700_000_000_000))
            .expect("durable write");
    }

    let (left, right) = tokio::join!(
        {
            let i = ingress.clone();
            async move { i.retry_pending_once().await }
        },
        {
            let i = ingress.clone();
            async move { i.retry_pending_once().await }
        }
    );

    assert_eq!(
        left.acked + right.acked,
        2,
        "both entries must be acknowledged exactly once between the two passes"
    );
    assert_eq!(
        captured.lock().await.len(),
        2,
        "two entries must produce exactly two relays across two overlapping sweeps"
    );
    assert!(listing_of(ingress.spool()).pending.is_empty());
}

#[tokio::test]
async fn two_concurrent_deliveries_are_each_spooled_and_relayed_once() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let (socket, captured) = spawn_target(
        tmp.path(),
        StubTarget::AckAfter(Duration::from_millis(200)),
        6,
    );
    let ingress = ingress_for(spool, socket);

    let a = {
        let i = ingress.clone();
        tokio::spawn(async move {
            i.ingest("review", &signed_headers(BODY, "d-one"), BODY)
                .await
        })
    };
    let b = {
        let i = ingress.clone();
        tokio::spawn(async move {
            i.ingest("review", &signed_headers(BODY, "d-two"), BODY)
                .await
        })
    };

    for handle in [a, b] {
        match handle.await.expect("ingest task") {
            IngestOutcome::Accepted { relay, .. } => assert!(relay.is_acked()),
            other => panic!("expected Accepted, got {other:?}"),
        }
    }
    assert_eq!(captured.lock().await.len(), 2);
    assert!(listing_of(ingress.spool()).pending.is_empty());
}

#[test]
fn claim_set_refuses_a_second_claim_on_the_same_path() {
    let claims = schedule::ClaimSet::new();
    let path = std::path::Path::new("/tmp/spool/entry.json");
    let first = claims.claim(path).expect("first claim");
    assert!(
        claims.claim(path).is_none(),
        "a claimed entry must not be claimable twice"
    );
    assert_eq!(claims.len(), 1);
    drop(first);
    assert!(
        claims.claim(path).is_some(),
        "the claim must be released on drop"
    );
}

#[test]
fn claim_set_releases_on_drop_even_when_the_holder_panics() {
    // A relay that panics must not leave its entry permanently unrelayable.
    let claims = schedule::ClaimSet::new();
    let path = std::path::Path::new("/tmp/spool/entry.json");
    let result = std::panic::catch_unwind({
        let claims = claims.clone();
        move || {
            let _held = claims.claim(path).expect("claim");
            panic!("relay blew up");
        }
    });
    assert!(result.is_err());
    assert!(claims.is_empty(), "the claim must not outlive the panic");
    assert!(claims.claim(path).is_some());
}

// ─── backoff ────────────────────────────────────────────────────────────────

#[test]
fn backoff_holds_off_a_freshly_spooled_entry() {
    // The request path may still be relaying it; the sweep must not race in.
    let policy = BackoffPolicy::default();
    let now = 1_700_000_000_000u64;
    let entry = sample_entry("d-1", now);
    assert!(!policy.is_due(&entry, now));
    assert!(!policy.is_due(&entry, now + 4_000));
    assert!(policy.is_due(&entry, now + policy.first_attempt_grace.as_millis() as u64));
}

#[test]
fn backoff_spacing_grows_with_attempts() {
    let policy = BackoffPolicy::default();
    assert_eq!(policy.delay_after(1), Duration::from_secs(30));
    assert_eq!(policy.delay_after(2), Duration::from_secs(60));
    assert_eq!(policy.delay_after(3), Duration::from_secs(120));
    assert_eq!(policy.delay_after(4), Duration::from_secs(240));
}

#[test]
fn backoff_respects_the_ceiling() {
    let policy = BackoffPolicy::default();
    for attempts in [10u32, 20, 1_000, u32::MAX - 1] {
        assert_eq!(
            policy.delay_after(attempts),
            policy.ceiling,
            "attempts={attempts} must clamp to the ceiling, never wrap to a short delay"
        );
    }
}

#[test]
fn backoff_admits_an_entry_past_its_delay() {
    let policy = BackoffPolicy::default();
    let now = 1_700_000_000_000u64;
    let mut entry = sample_entry("d-1", now - 1_000_000);
    entry.attempts = 2;
    entry.last_attempt_at_unix_ms = Some(now - 59_000);
    assert!(
        !policy.is_due(&entry, now),
        "59s < the 60s delay for attempt 2"
    );
    entry.last_attempt_at_unix_ms = Some(now - 61_000);
    assert!(policy.is_due(&entry, now));
}

#[test]
fn backoff_stops_at_max_attempts() {
    // An exhausted entry is never relayed again — and never deleted. It holds
    // the health signal red until an operator intervenes, which is the honest
    // state for an undeliverable webhook.
    let policy = BackoffPolicy::default();
    let now = 1_700_000_000_000u64;
    let mut entry = sample_entry("d-1", 1);
    entry.attempts = policy.max_attempts;
    entry.last_attempt_at_unix_ms = Some(1);
    assert!(policy.is_exhausted(&entry));
    assert!(!policy.is_due(&entry, now), "no elapsed time can revive it");
}

#[tokio::test]
async fn sweep_honours_backoff_between_ticks() {
    // 🔴 Fails against the pre-fix head, which relayed every pending entry on
    // every tick and rewrote its whole body plus two fsyncs each time.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    // Real schedule, but no first-attempt grace so the first pass runs at once.
    let policy = BackoffPolicy {
        first_attempt_grace: Duration::ZERO,
        ..BackoffPolicy::default()
    };
    let ingress = ingress_with(
        spool,
        tmp.path().join("sockets").join("absent.sock"),
        Duration::from_millis(300),
    )
    .with_backoff(policy);
    ingress
        .spool()
        .persist_new(&sample_entry("d-1", 1_700_000_000_000))
        .expect("durable write");

    let first = ingress.retry_pending_once().await;
    assert_eq!(first.still_pending, 1, "the first pass relays it");

    let second = ingress.retry_pending_once().await;
    assert_eq!(
        second.not_due, 1,
        "the second pass must respect the 30s spacing, not relay again"
    );
    assert_eq!(second.still_pending, 0);

    let listing = listing_of(ingress.spool());
    assert_eq!(
        listing.pending.len(),
        1,
        "and it is still pending, not lost"
    );
    assert_eq!(
        listing.pending[0].entry.attempts, 1,
        "a skipped pass must not bump the attempt count or rewrite the body"
    );
}

#[tokio::test]
async fn sweep_stops_relaying_an_exhausted_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let policy = BackoffPolicy {
        first_attempt_grace: Duration::ZERO,
        base: Duration::ZERO,
        ceiling: Duration::ZERO,
        max_attempts: 3,
    };
    let ingress = ingress_with(
        spool,
        tmp.path().join("sockets").join("absent.sock"),
        Duration::from_millis(300),
    )
    .with_backoff(policy);
    ingress
        .spool()
        .persist_new(&sample_entry("d-1", 1_700_000_000_000))
        .expect("durable write");

    for _ in 0..3 {
        assert_eq!(ingress.retry_pending_once().await.still_pending, 1);
    }
    let report = ingress.retry_pending_once().await;
    assert_eq!(report.exhausted, 1);
    assert_eq!(report.still_pending, 0);

    // 🔴 Updated in review round 2. The previous version asserted the exhausted
    // entry STAYS in the live set, which is what let it be re-read and
    // re-decoded by every later sweep and metrics request forever. It is now
    // moved aside — kept, because it is still an unacknowledged webhook, but off
    // both hot paths.
    assert!(
        listing_of(ingress.spool()).pending.is_empty(),
        "an exhausted entry must leave the live set"
    );
    let census = ingress.spool().scan_metadata().expect("census");
    assert_eq!(census.live.len(), 0);
    assert_eq!(
        census.exhausted.len(),
        1,
        "giving up on relaying is not the same as discarding the delivery"
    );
    assert_eq!(census.exhausted[0].delivery_id, "d-1");

    let quarantined = ingress
        .spool()
        .load(&census.exhausted[0].path)
        .expect("the quarantined entry is still readable");
    assert_eq!(quarantined.attempts, 3);
    assert_eq!(
        BASE64.decode(&quarantined.body_b64).expect("decode"),
        BODY,
        "and it still holds the original body for a manual redelivery"
    );

    let health = ingress.health().await;
    assert_eq!(
        health.status,
        ServiceHealth::Error,
        "an entry we have given up relaying must hold the health signal red"
    );
    assert_eq!(health.exhausted, 1);
    assert_eq!(health.exhausted_delivery_ids, vec!["d-1".to_string()]);
    assert_eq!(health.pending, 0);
}

// ─── HIGH-A: exhausted entries leave the hot paths ──────────────────────────

#[test]
fn spool_quarantine_moves_an_entry_out_of_the_live_set() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let entry = sample_entry("d-quarantine", 1_700_000_000_000);
    let path = spool.persist_new(&entry).expect("durable write");

    let moved = spool.quarantine(&path).expect("quarantine");
    assert!(!path.exists(), "the entry must leave the live directory");
    assert!(moved.starts_with(spool.exhausted_root()));
    assert_eq!(
        spool.load(&moved).expect("still readable"),
        entry,
        "quarantine moves the delivery, it does not alter or discard it"
    );
    assert!(listing_of(&spool).pending.is_empty());
}

#[test]
fn spool_list_pending_ignores_the_exhausted_subdirectory() {
    // 🔴 Fails against `92c1ed3fc`, where nothing ever left the live set: both
    // `retry_pending_once` and `scan_health` called `list_pending`, which reads
    // and JSON-decodes every file. An entry that can never be relayed again was
    // paid for on every sweep tick and every metrics request, forever.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let live = spool
        .persist_new(&sample_entry("d-live", 1_700_000_005_000))
        .expect("durable write");
    let dead = spool
        .persist_new(&sample_entry("d-dead", 1_700_000_000_000))
        .expect("durable write");
    spool.quarantine(&dead).expect("quarantine");

    let listing = listing_of(&spool);
    assert_eq!(
        listing.pending.len(),
        1,
        "the decode-every-file path must see only the live entry"
    );
    assert_eq!(listing.pending[0].path, live);
    assert!(listing.undecodable.is_empty());
}

#[test]
fn spool_scan_metadata_avoids_decoding_and_load_reads_one() {
    // `entry_path`'s stated rationale — the timestamp prefix exists so the age
    // scan "does not have to parse every file" — is only true if something
    // actually reads it. This is that reader.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    for (id, at) in [("d-c", 300u64), ("d-a", 100), ("d-b", 200)] {
        spool
            .persist_new(&sample_entry(id, at))
            .expect("durable write");
    }
    // Corrupt every file. A census that opened them would notice; one that
    // reads names only cannot, which is exactly the property under test.
    for dirent in std::fs::read_dir(spool.root()).expect("read spool") {
        let path = dirent.expect("dirent").path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            std::fs::write(&path, b"{ not json").expect("corrupt");
        }
    }

    let census = spool.scan_metadata().expect("census");
    assert_eq!(
        census
            .live
            .iter()
            .map(|m| m.delivery_id.as_str())
            .collect::<Vec<_>>(),
        vec!["d-a", "d-b", "d-c"],
        "the census must read names only, oldest first"
    );
    assert_eq!(census.live[0].received_at_unix_ms, 100);
    assert!(census.unparsable.is_empty());

    // `load` is the one place bytes are read, and it reports the corruption.
    assert!(spool.load(&census.live[0].path).is_err());
}

#[test]
fn spool_scan_metadata_separates_live_from_exhausted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let dead = spool
        .persist_new(&sample_entry("d-dead", 100))
        .expect("durable write");
    spool
        .persist_new(&sample_entry("d-live", 200))
        .expect("durable write");
    spool.quarantine(&dead).expect("quarantine");

    let census = spool.scan_metadata().expect("census");
    assert_eq!(census.live.len(), 1);
    assert_eq!(census.live[0].delivery_id, "d-live");
    assert_eq!(census.exhausted.len(), 1);
    assert_eq!(census.exhausted[0].delivery_id, "d-dead");
}

#[test]
fn spool_scan_metadata_reports_a_stray_file_rather_than_dating_it_zero() {
    // A name with no timestamp prefix must not parse as age 0 — that would
    // read as the oldest entry in the spool and hijack every diagnostic.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    std::fs::write(spool.root().join("stray.json"), b"{}").expect("write stray");

    let census = spool.scan_metadata().expect("census");
    assert!(census.live.is_empty());
    assert_eq!(census.unparsable.len(), 1);
}

#[tokio::test]
async fn sweep_quarantines_an_exhausted_entry_and_stops_paying_for_it() {
    // 🔴 Fails against `92c1ed3fc`: the sweep counted `exhausted` and
    // `continue`d, so the entry stayed in the live set and every subsequent
    // pass decoded it again. The module doc says no target binds a listener
    // until step 4, so in this PR's intended configuration that is EVERY
    // delivery, permanently.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let policy = BackoffPolicy {
        first_attempt_grace: Duration::ZERO,
        base: Duration::ZERO,
        ceiling: Duration::ZERO,
        max_attempts: 1,
    };
    let ingress = ingress_with(
        spool,
        tmp.path().join("sockets").join("absent.sock"),
        Duration::from_millis(300),
    )
    .with_backoff(policy);
    for id in ["d-1", "d-2"] {
        ingress
            .spool()
            .persist_new(&sample_entry(id, 1_700_000_000_000))
            .expect("durable write");
    }

    assert_eq!(ingress.retry_pending_once().await.still_pending, 2);
    let second = ingress.retry_pending_once().await;
    assert_eq!(second.exhausted, 2);

    let census = ingress.spool().scan_metadata().expect("census");
    assert_eq!(
        census.live.len(),
        0,
        "an exhausted entry must stop costing a decode on every pass"
    );
    assert_eq!(census.exhausted.len(), 2, "and must still be kept");

    // Every later pass sees nothing to do at all.
    let third = ingress.retry_pending_once().await;
    assert_eq!(third, SweepReport::default());
}

// ─── HIGH-3: an absent spool directory is red, not green ────────────────────

#[test]
fn health_reports_error_when_the_spool_directory_is_gone() {
    // 🔴 Fails against the pre-fix head, which answered ENOENT with an empty
    // listing: the console data dir removed (or its volume unmounted) made
    // every POST 500 while `/api/console/metrics/webhooks` reported
    // {"status":"ok","pending":0}. Ingress dead, alarm green.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("spool");
    let spool = Spool::open(&root).expect("open spool");
    spool
        .persist_new(&sample_entry("d-1", 1_700_000_000_000))
        .expect("durable write");

    std::fs::remove_dir_all(&root).expect("simulate the directory going away");

    let h = health::scan_health(&spool, 1_700_000_100_000, Duration::from_secs(600));
    assert_eq!(
        h.status,
        ServiceHealth::Error,
        "a spool whose directory vanished is broken, not empty"
    );
    assert!(
        h.scan_error.is_some(),
        "and the reason must be reported: {h:?}"
    );
}

#[test]
fn a_never_opened_spool_directory_is_still_legitimately_empty() {
    // The other side of the same rule: `Spool::at` on a path that was never
    // created has genuinely nothing pending, and must not report red.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::at(tmp.path().join("never-created"));
    let listing = spool.list_pending().expect("an unopened spool lists empty");
    assert!(listing.pending.is_empty());
    let h = health::scan_health(&spool, 1_700_000_000_000, Duration::from_secs(600));
    assert_eq!(h.status, ServiceHealth::Ok);
    assert_eq!(h.scan_error, None);
}

#[tokio::test]
async fn metrics_route_reports_red_when_the_spool_directory_is_gone() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("spool");
    let spool = Spool::open(&root).expect("open spool");
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"));
    std::fs::remove_dir_all(&root).expect("simulate the directory going away");

    let (status, body) = get_json(router_for(ingress), "/api/console/metrics/webhooks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "error");
    assert!(body["metrics"]["scan_error"].is_string());
}

// ─── body limit ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn route_accepts_a_body_larger_than_the_axum_default_limit() {
    // 🔴 Fails against the pre-fix head: axum's 2 MiB DefaultBodyLimit 413s a
    // 3 MiB delivery BEFORE the handler runs — no spool entry, no metric, no
    // log. GitHub payloads are legal to 25 MB and push/pull_request bodies
    // routinely exceed 2 MiB.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"));

    let big = format!(
        r#"{{"action":"opened","filler":"{}"}}"#,
        "x".repeat(3 * 1024 * 1024)
    );
    let body = big.as_bytes();

    let (status, json) = post_webhook(
        router_for(ingress.clone()),
        "review",
        body,
        signed_headers(body, "d-big"),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "a 3 MiB delivery must reach the handler, not be 413'd by the framework"
    );
    assert_eq!(json["delivery_id"], "d-big");

    let listing = listing_of(ingress.spool());
    assert_eq!(listing.pending.len(), 1);
    assert_eq!(
        BASE64
            .decode(&listing.pending[0].entry.body_b64)
            .expect("decode"),
        body,
        "and the whole 3 MiB must be spooled byte-exact"
    );
}

// ─── HIGH-B: the signal keeps saying something new after it goes red ────────

#[test]
fn health_diagnostics_track_the_live_entry_not_the_exhausted_one() {
    // 🔴 Fails against `92c1ed3fc`. Exhausted entries are by construction the
    // oldest, and `scan_health` took `listing.pending.first()`, so
    // `oldest_pending_delivery_id`, `oldest_pending_last_error` and
    // `oldest_pending_age_secs` pinned to the first poisoned delivery forever.
    // Day 1 one delivery exhausts; day 30 a genuinely new one gets stuck and
    // nothing an operator or alert rule reads moves.
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let now = 1_700_000_000_000u64;

    // Day 1: a delivery that has been given up on. Oldest thing in the spool.
    let mut dead = sample_entry("d-day-one", now - 30 * 86_400_000);
    dead.attempts = 24;
    dead.last_error = Some("no listener bound".to_string());
    let dead_path = spool.persist_new(&dead).expect("durable write");
    spool.quarantine(&dead_path).expect("quarantine");

    // Day 30: a NEW delivery is now failing. This is what an operator needs.
    let mut live = sample_entry("d-day-thirty", now - 900_000);
    live.attempts = 3;
    live.last_error = Some("target rejected the frame: code -32000 — store locked".to_string());
    live.last_attempt_at_unix_ms = Some(now - 60_000);
    spool.persist_new(&live).expect("durable write");

    let h = health::scan_health(&spool, now, Duration::from_secs(600));

    assert_eq!(h.status, ServiceHealth::Error);
    assert_eq!(
        h.oldest_pending_delivery_id.as_deref(),
        Some("d-day-thirty"),
        "the diagnostics must describe the LIVE failure, not the 30-day-old corpse"
    );
    assert_eq!(
        h.oldest_pending_last_error.as_deref(),
        Some("target rejected the frame: code -32000 — store locked")
    );
    assert_eq!(h.oldest_pending_attempts, Some(3));
    assert_eq!(h.oldest_pending_age_secs, Some(900));
    assert_eq!(h.pending, 1);

    // The exhausted one is still reported — in its own fields, where it cannot
    // crowd out a live failure.
    assert_eq!(h.exhausted, 1);
    assert_eq!(h.exhausted_delivery_ids, vec!["d-day-one".to_string()]);
    assert_eq!(h.oldest_exhausted_age_secs, Some(30 * 86_400));
}

#[test]
fn health_is_red_for_an_exhausted_entry_even_with_nothing_live() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let now = 1_700_000_000_000u64;
    let path = spool
        .persist_new(&sample_entry("d-dead", now - 1_000))
        .expect("durable write");
    spool.quarantine(&path).expect("quarantine");

    let h = health::scan_health(&spool, now, Duration::from_secs(600));
    assert_eq!(
        h.status,
        ServiceHealth::Error,
        "nothing clears an exhausted entry without an operator, so it stays red"
    );
    assert_eq!(h.pending, 0);
    assert_eq!(h.exhausted, 1);
    assert_eq!(h.oldest_pending_delivery_id, None);
}

#[tokio::test]
async fn metrics_route_surfaces_exhausted_separately_from_live() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let now = 1_700_000_000_000u64;
    let dead = spool
        .persist_new(&sample_entry("d-dead", now - 86_400_000))
        .expect("durable write");
    spool.quarantine(&dead).expect("quarantine");
    spool
        .persist_new(&sample_entry("d-live", now - 1_000))
        .expect("durable write");
    let ingress = ingress_for(spool, tmp.path().join("sockets").join("absent.sock"));

    let (status, body) = get_json(router_for(ingress), "/api/console/metrics/webhooks").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "error");
    assert_eq!(body["metrics"]["pending"], 1);
    assert_eq!(body["metrics"]["exhausted"], 1);
    assert_eq!(body["metrics"]["oldest_pending_delivery_id"], "d-live");
    assert_eq!(body["metrics"]["exhausted_delivery_ids"][0], "d-dead");
}

// ─── End to end against the real receiver (#5182) ───────────────────────────
//
// Every case above answers console with a hand-rolled stub. These drive the
// production receiver — `trusty_common::webhook_relay`'s listener, the same one
// `trusty-review webhook-listen` and `trusty-analyze webhook-listen` run — so
// the two halves are proven against each other rather than against a fixture
// that agrees with whichever half was edited last.

/// Bind the real listener under `dir`, backed by `sink`, and return its socket.
async fn spawn_real_listener(
    dir: &StdPath,
    sink: std::sync::Arc<dyn trusty_common::webhook_relay::DeliverySink>,
) -> PathBuf {
    let socket = dir.join("sockets").join("real-target.sock");
    let listener = bind_hardened(&socket).expect("bind real listener");
    tokio::spawn(async move {
        trusty_common::webhook_relay::serve_until(
            listener,
            sink,
            trusty_common::webhook_relay::ServeOptions::default(),
            std::future::pending::<()>(),
        )
        .await;
    });
    socket
}

/// A sink that always refuses, standing in for a receiver whose disk is gone.
#[derive(Debug)]
struct UndurableSink;

impl trusty_common::webhook_relay::DeliverySink for UndurableSink {
    fn take_ownership(
        &self,
        _delivery: &trusty_common::webhook_relay::RelayDelivery,
    ) -> Result<(), trusty_common::webhook_relay::SinkRejection> {
        Err(trusty_common::webhook_relay::SinkRejection::not_durable(
            "inbox is unwritable",
        ))
    }
}

#[tokio::test]
async fn delivery_reaches_the_real_target_and_is_spooled_there_before_the_ack() {
    // 🔴 The whole point of #5182. Against the pre-fix commit this fails at the
    // first assertion: nothing bound the socket, so the outcome was
    // `Unreachable` and the entry stayed pending forever.
    //
    // The ordering claim is checked the only way it can be from this side: the
    // ack has already been received when the assertions run, and the receiver's
    // inbox already holds the delivery. Were the ack sent first, the inbox could
    // still be empty here — and console would have deleted its own copy, which
    // the spool assertion below would then be unable to distinguish from
    // success. The receiver-side ordering itself is pinned in trusty-common by
    // `dispatch_acks_only_after_the_sink_has_taken_ownership`.
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = trusty_common::webhook_relay::Inbox::open(tmp.path().join("inbox"))
        .expect("open receiver inbox");
    let socket = spawn_real_listener(tmp.path(), std::sync::Arc::new(inbox.clone())).await;
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = ingress_for(spool.clone(), socket);

    let outcome = ingress
        .ingest("review", &signed_headers(BODY, "e2e-real-1"), BODY)
        .await;

    let IngestOutcome::Accepted { relay, .. } = outcome else {
        panic!("expected Accepted, got {outcome:?}");
    };
    assert_eq!(
        relay,
        RelayOutcome::Acked,
        "the real listener must acknowledge a verified delivery"
    );

    let held = inbox.list().expect("list the receiver inbox");
    assert_eq!(held.len(), 1, "the receiver must hold the delivery");
    assert_eq!(held[0].1.delivery_id, "e2e-real-1");
    assert_eq!(
        held[0].1.body_b64,
        base64::engine::general_purpose::STANDARD.encode(BODY),
        "the raw body must arrive byte-exact, so the HMAC stays re-checkable"
    );
    assert!(
        held[0].1.provenance.verified,
        "the receiver must be told what console verified"
    );

    assert!(
        listing_of(&spool).pending.is_empty(),
        "an acknowledged delivery is the one case that clears the spool"
    );
}

#[tokio::test]
async fn a_receiver_that_cannot_own_the_work_never_produces_an_ack() {
    // 🔴 The failure this change must not introduce. A receiver that answers but
    // cannot take durable responsibility must leave console's spool entry alone
    // — that entry is the only remaining copy, because GitHub does not re-send
    // an acknowledged delivery.
    //
    // Against the pre-fix commit there is no counterpart: no listener existed at
    // all, so "reached the target and it could not own the work" was not a
    // reachable state.
    let tmp = tempfile::tempdir().expect("tempdir");
    let socket = spawn_real_listener(tmp.path(), std::sync::Arc::new(UndurableSink)).await;
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");
    let ingress = ingress_for(spool.clone(), socket);

    let outcome = ingress
        .ingest("review", &signed_headers(BODY, "e2e-refuse-1"), BODY)
        .await;

    let IngestOutcome::Accepted { relay, .. } = outcome else {
        panic!("expected Accepted, got {outcome:?}");
    };
    assert!(
        !relay.is_acked(),
        "a receiver that cannot own the work must not read as acknowledged, got {relay:?}"
    );
    assert!(
        relay.reason().contains("inbox is unwritable"),
        "the durable record must carry the receiver's own words, got {:?}",
        relay.reason()
    );

    let pending = listing_of(&spool).pending;
    assert_eq!(pending.len(), 1, "the only copy must survive");
    assert_eq!(pending[0].entry.delivery_id, "e2e-refuse-1");
    assert_eq!(
        pending[0].entry.attempts, 1,
        "the failed attempt must be recorded durably, not only logged"
    );
}

#[tokio::test]
async fn the_real_listener_rejects_a_method_that_is_not_webhook_deliver() {
    // Console only ever sends `webhook.deliver`, so this drives the listener
    // directly. A listener that served anything it was handed would take
    // responsibility for payloads no sender in this workspace produces.
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = trusty_common::webhook_relay::Inbox::open(tmp.path().join("inbox"))
        .expect("open receiver inbox");
    let socket = spawn_real_listener(tmp.path(), std::sync::Arc::new(inbox.clone())).await;

    let request = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "webhook.execute",
        "id": "bad-method-1",
        "params": {
            "delivery_id": "bad-method-1",
            "source": "review",
            "event": "pull_request",
            "headers": {},
            "body_b64": "e30=",
            "provenance": {
                "algorithm": "hmac-sha256",
                "key_id": "GITHUB_WEBHOOK_SECRET",
                "verified": true
            },
            "received_at_unix_ms": 1_700_000_000_000u64,
            "attempts": 0
        }
    });
    let response: trusty_common::webhook_relay::RelayResponse =
        trusty_common::uds::send_framed_request(&socket, &request, Duration::from_secs(5))
            .await
            .expect("the listener must answer rather than hang up");

    assert!(!response.is_ack(), "an unknown method must not be acked");
    let err = response.error.expect("a refusal carries an error");
    assert_eq!(err.code, -32601, "unknown method is method-not-found");
    assert!(
        err.message.contains("webhook.execute") && err.message.contains(RELAY_METHOD),
        "the refusal must name both what arrived and what is served, got {:?}",
        err.message
    );
    assert!(
        inbox.list().expect("list").is_empty(),
        "a refused method must leave nothing durably owned"
    );
}

// ─── Supervised on-demand spawn (#5182) ─────────────────────────────────────

#[test]
fn spawn_maps_each_source_to_its_binary() {
    assert_eq!(spawn::target_binary_name("review"), "trusty-review");
    assert_eq!(spawn::target_binary_name("analyze"), "trusty-analyze");
}

#[tokio::test]
async fn spawn_adopts_a_socket_that_is_already_served() {
    // The path that keeps a supervised target cheap: something is already
    // serving, so `ensure_running` neither locates a binary nor launches one.
    // It also never resolves the binary, which is what keeps this test from
    // starting a real trusty-review on the developer's machine.
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = trusty_common::webhook_relay::Inbox::open(tmp.path().join("inbox"))
        .expect("open receiver inbox");
    let socket = spawn_real_listener(tmp.path(), std::sync::Arc::new(inbox)).await;
    let supervisor = spawn::TargetSupervisor::new();

    let resolved = supervisor
        .ensure_running("review", &socket)
        .await
        .expect("an already-served socket must be adopted");

    assert_eq!(resolved, socket);
    assert_eq!(
        supervisor.spawned_count(),
        0,
        "adoption must not launch a child"
    );
}

#[tokio::test]
async fn relay_with_a_supervisor_still_acks_against_a_live_target() {
    // The supervisor sits in front of the dial, so an attached one must not
    // change the outcome when the target is already up.
    let tmp = tempfile::tempdir().expect("tempdir");
    let inbox = trusty_common::webhook_relay::Inbox::open(tmp.path().join("inbox"))
        .expect("open receiver inbox");
    let socket = spawn_real_listener(tmp.path(), std::sync::Arc::new(inbox.clone())).await;
    let spool = Spool::open(tmp.path().join("spool")).expect("open spool");

    let ingress = WebhookIngress::new(
        spool.clone(),
        SECRET.to_string(),
        SECRET_ENV.to_string(),
        vec![Target {
            source: "review".to_string(),
            relay: UdsRelay::new(socket)
                .with_timeout(Duration::from_secs(2))
                .with_supervisor(
                    "review",
                    std::sync::Arc::new(spawn::TargetSupervisor::new()),
                ),
        }],
    )
    .with_backoff(no_backoff());

    let outcome = ingress
        .ingest("review", &signed_headers(BODY, "supervised-1"), BODY)
        .await;

    let IngestOutcome::Accepted { relay, .. } = outcome else {
        panic!("expected Accepted, got {outcome:?}");
    };
    assert_eq!(relay, RelayOutcome::Acked);
    assert_eq!(inbox.list().expect("list").len(), 1);
    assert!(listing_of(&spool).pending.is_empty());
}
