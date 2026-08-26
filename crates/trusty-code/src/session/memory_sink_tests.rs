//! Unit tests for [`super::TurnMemorySink`] and
//! [`super::derive_palace_id_for_project`] (#2345, #2424).
//!
//! Why: the turn recorder must never block/fail a running turn even when
//! trusty-memory is unreachable, must dual-write both `chat_turn_append` and
//! `memory_remember` — via the MCP `tools/call` envelope, the ONLY dispatch
//! shape trusty-memory accepts for `chat_*` tools (#2424; the previous
//! direct-method dispatch failed `-32601` on 100% of writes in the #2343
//! soak) — with the documented params/tags against a live mock daemon, must
//! ensure the target palace exists exactly once before the first write
//! (#2424), and must apply its documented "drop newest" overflow policy —
//! none of that is exercised anywhere else.
//! What: the mock daemons here run on a temp Unix socket (#6286 — the axum
//! `/rpc` route they used to bind is retired) and reply with the REAL daemon's
//! `tools/call` response shape (the tool result STRINGIFIED inside
//! `result.content[0].text` — mirrors `transport/rpc.rs`'s `tools/call`
//! arm), so the assertions cover both the request envelope
//! (`method == "tools/call"`, `params.name`/`params.arguments`) and the
//! response unwrap. `enqueue_drops_newest_when_queue_full` exercises the
//! overflow policy directly against the raw channel (no networking, fully
//! deterministic); `write_turn_is_fail_open_on_unreachable_daemon` proves an
//! unreachable socket never panics or hangs;
//! `derive_palace_id_for_project_*` cover the palace-derivation fallback
//! chain.
//! Test: this file.

use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::sync::mpsc;

use crate::uds_mock::{self, MockMemoryDaemon, RpcError};

use super::*;

/// One captured RPC call: the JSON-RPC `method` and its `params`.
type Captured = Arc<Mutex<Vec<(String, Value)>>>;

/// An observer that keeps every outcome it is handed, in arrival order.
#[derive(Default)]
struct RecordingObserver(Mutex<Vec<MemoryTurnOutcome>>);

impl MemoryDurabilityObserver for RecordingObserver {
    fn observe(&self, outcome: MemoryTurnOutcome) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(outcome);
    }
}

const REDACTION_CHILD_ENV: &str = "TCODE_MEMORY_WARNING_REDACTION_CHILD";
const REDACTION_CHILD_TEST_PATH: &str =
    "session::memory_sink::tests::memory_warning_redaction_child";
/// Server-controlled markers the daemon mock plants in its error and `reason`
/// text. A default warning must carry none of them.
const CREDENTIAL_SENTINEL: &str = "sk-live-memory-credential-do-not-leak";
const REASON_SENTINEL: &str = "Bearer credential-preview-do-not-leak";
const OVERSIZED_SENTINEL: &str = "OVERSIZED_MEMORY_ERROR_DO_NOT_LEAK";

/// Drive one dual-write against a daemon that plants credential-shaped text in
/// both its error message and its `reason` field.
///
/// Runs only under the parent below — a bare `cargo test` sees it return
/// immediately.
#[tokio::test]
async fn memory_warning_redaction_child() {
    if std::env::var_os(REDACTION_CHILD_ENV).is_none() {
        return;
    }

    fn handle(params: Value) -> Result<Value, RpcError> {
        match tool_name(&params) {
            "chat_turn_append" => Err(RpcError::internal(format!(
                "{CREDENTIAL_SENTINEL} {}",
                OVERSIZED_SENTINEL.repeat(512)
            ))),
            "memory_remember" => Ok(wrapped_result(&json!({
                "status": "skipped",
                "reason": REASON_SENTINEL
            }))),
            _ => Ok(wrapped_result(&json!({"ok": true}))),
        }
    }

    let daemon = uds_mock::spawn(move |_method, params| {
        let response = handle(params);
        Box::pin(async move { response })
    })
    .await;

    crate::logging::init_tracing_for_test();
    let outcome = write_turn(
        daemon.socket(),
        "test-palace",
        &QueuedTurn {
            sequence: 1,
            session_id: "session-redaction".into(),
            prompt: "prompt".into(),
            response: "response".into(),
        },
    )
    .await;
    assert!(matches!(outcome, MemoryTurnOutcome::Degraded { .. }));
}

/// #2425: the default turn-recorder warnings must name the failure category and
/// carry NO daemon-controlled payload.
///
/// A subprocess because the assertion is about what reaches stderr, and this
/// binary's tracing subscriber is process-global.
#[test]
fn default_memory_warnings_redact_server_payloads_in_subprocess() {
    let output = Command::new(std::env::current_exe().expect("current test binary"))
        .args([
            "--exact",
            REDACTION_CHILD_TEST_PATH,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(REDACTION_CHILD_ENV, "1")
        .env("RUST_LOG", "warn")
        .output()
        .expect("run memory warning redaction child");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "child failed: {stderr}");
    for secret in [CREDENTIAL_SENTINEL, REASON_SENTINEL, OVERSIZED_SENTINEL] {
        assert!(
            !stderr.contains(secret),
            "default warning leaked server-controlled payload marker {secret}: {stderr}"
        );
    }
    assert!(stderr.contains("failure_category"), "{stderr}");
    assert!(stderr.contains("chat_turn_append"), "{stderr}");
    assert!(stderr.contains("memory_remember_skipped"), "{stderr}");
}

/// Wrap `inner` as the real daemon's `tools/call` response envelope: the
/// tool result STRINGIFIED inside `content[0].text` (mirrors the
/// `transport/rpc.rs` `tools/call` arm).
fn wrapped_result(inner: &Value) -> Value {
    json!({"content": [{"type": "text", "text": inner.to_string()}]})
}

/// Extract the tool name from a captured `tools/call` params object.
fn tool_name(params: &Value) -> &str {
    params["name"].as_str().unwrap_or_default()
}

/// Shared mock behaviour: whether `palace_info` should report the palace as
/// already existing.
#[derive(Clone, Copy)]
enum PalaceState {
    Exists,
    MissingUntilCreated,
}

/// Spin up a tiny in-process `/rpc` mock that captures every request and
/// replies with the daemon's real `tools/call` envelope shape.
///
/// Why: `TurnMemorySink`'s drain task must be observed making REAL HTTP
/// calls with the correct `tools/call` method/params shape (#2424) — a mock
/// server is the only way to assert that without a live trusty-memory
/// daemon.
/// What: binds a socket under a `TempDir`, serves until the returned daemon
/// drops, and returns it plus the shared capture buffer. `palace_state`
/// controls the `palace_info` reply: `Exists` always succeeds;
/// `MissingUntilCreated` returns a JSON-RPC error (the daemon's "palace
/// metadata missing" signal) until a `palace_create` lands, after which it
/// succeeds — a stateful stand-in for the real registry.
async fn spawn_capturing_mock(palace_state: PalaceState) -> (MockMemoryDaemon, Captured) {
    let captured: Captured = Arc::new(Mutex::new(Vec::new()));
    let store = Arc::clone(&captured);
    let created = Arc::new(Mutex::new(matches!(palace_state, PalaceState::Exists)));

    let daemon = uds_mock::spawn(move |method: &str, params: Value| {
        let store = Arc::clone(&store);
        let created = Arc::clone(&created);
        let method = method.to_string();
        Box::pin(async move {
            store
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((method, params.clone()));
            let name = tool_name(&params);
            if name == "palace_create" {
                *created.lock().unwrap_or_else(|e| e.into_inner()) = true;
            }
            if name == "palace_info" && !*created.lock().unwrap_or_else(|e| e.into_inner()) {
                return Err(RpcError::internal("palace metadata missing"));
            }
            Ok(wrapped_result(&json!({"ok": true})))
        })
    })
    .await;

    (daemon, captured)
}

/// Poll `captured` until it holds at least `n` entries or `timeout` elapses.
async fn wait_for_captures(captured: &Captured, n: usize, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if captured.lock().unwrap_or_else(|e| e.into_inner()).len() >= n
            || tokio::time::Instant::now() >= deadline
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Enqueuing one turn against a live mock daemon must land BOTH
/// `chat_turn_append` (exact record) and `memory_remember` (semantic recall
/// surface, tagged `session:<id>` + `turn`) — each as a `tools/call`
/// envelope (#2424) with the documented inner arguments.
#[tokio::test]
async fn enqueue_drain_happy_path() {
    let (daemon, captured) = spawn_capturing_mock(PalaceState::Exists).await;
    let sink = TurnMemorySink::new(daemon.socket().to_path_buf(), "test-palace".to_string(), PalaceCreation::Allowed);

    sink.enqueue("sess-1", "what is 2+2?", "4");
    // 3 calls: palace_info (ensure, #2424) + the two dual-write calls.
    wait_for_captures(&captured, 3, Duration::from_secs(2)).await;

    let calls = captured.lock().unwrap_or_else(|e| e.into_inner()).clone();
    assert_eq!(calls.len(), 3, "expected ensure + both dual-write calls");
    for (method, _) in &calls {
        assert_eq!(
            method, "tools/call",
            "#2424: every write must use the tools/call envelope — \
             direct dispatch of chat_* methods is -32601"
        );
    }

    let append = calls
        .iter()
        .find(|(_, p)| tool_name(p) == "chat_turn_append")
        .expect("chat_turn_append call");
    let args = &append.1["arguments"];
    assert_eq!(args["palace"], "test-palace");
    assert_eq!(args["session_id"], "sess-1");
    assert_eq!(args["prompt"], "what is 2+2?");
    assert_eq!(args["response"], "4");

    let remember = calls
        .iter()
        .find(|(_, p)| tool_name(p) == "memory_remember")
        .expect("memory_remember call");
    let args = &remember.1["arguments"];
    assert_eq!(args["palace"], "test-palace");
    let tags: Vec<String> = args["tags"]
        .as_array()
        .expect("tags array")
        .iter()
        .map(|t| t.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(tags.contains(&"session:sess-1".to_string()));
    assert!(tags.contains(&"turn".to_string()));
    assert!(
        args["text"]
            .as_str()
            .unwrap_or_default()
            .contains("what is 2+2?")
    );
    // #2363: every turn-recorder `memory_remember` write must pass
    // `force: true` to bypass the dedup gate documented as hostile to
    // sequential conversational turns.
    assert_eq!(args["force"], json!(true));
}

/// (#2424) A missing palace must be created via `palace_create`
/// (`force: true` — the spec-001 app-managed-palace bypass) exactly ONCE;
/// later turns must reuse the cached ensure and add zero extra RPCs.
#[tokio::test]
async fn ensure_palace_creates_missing_palace_once() {
    let (daemon, captured) = spawn_capturing_mock(PalaceState::MissingUntilCreated).await;
    let sink = TurnMemorySink::new(daemon.socket().to_path_buf(), "test-palace".to_string(), PalaceCreation::Allowed);

    sink.enqueue("sess-1", "p1", "r1");
    sink.enqueue("sess-1", "p2", "r2");
    // 6 calls: (palace_info + palace_create) once, then 2 writes per turn.
    wait_for_captures(&captured, 6, Duration::from_secs(2)).await;

    let calls = captured.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let names: Vec<&str> = calls.iter().map(|(_, p)| tool_name(p)).collect();
    assert_eq!(
        names,
        vec![
            "palace_info",
            "palace_create",
            "chat_turn_append",
            "memory_remember",
            "chat_turn_append",
            "memory_remember",
        ],
        "ensure must run (probe + create) exactly once, before the first write"
    );

    let create = calls
        .iter()
        .find(|(_, p)| tool_name(p) == "palace_create")
        .expect("palace_create call");
    assert_eq!(create.1["arguments"]["name"], "test-palace");
    assert_eq!(
        create.1["arguments"]["force"],
        json!(true),
        "app-managed palace slugs need the spec-001 force bypass — the \
         daemon's cwd-derived project slug will not match"
    );
}

/// (#2424) An already-existing palace must NOT be re-created —
/// `handle_palace_create` overwrites `palace.json` (resetting
/// `created_at`/`description`), so the ensure probes with `palace_info`
/// first and never calls `palace_create` when it succeeds; the ensure is
/// also cached, so a second turn adds no extra probe.
#[tokio::test]
async fn ensure_palace_skips_create_when_palace_exists() {
    let (daemon, captured) = spawn_capturing_mock(PalaceState::Exists).await;
    let sink = TurnMemorySink::new(daemon.socket().to_path_buf(), "test-palace".to_string(), PalaceCreation::Allowed);

    sink.enqueue("sess-1", "p1", "r1");
    sink.enqueue("sess-1", "p2", "r2");
    // 5 calls: palace_info once, then 2 writes per turn.
    wait_for_captures(&captured, 5, Duration::from_secs(2)).await;

    let calls = captured.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let names: Vec<&str> = calls.iter().map(|(_, p)| tool_name(p)).collect();
    assert_eq!(
        names.iter().filter(|n| **n == "palace_info").count(),
        1,
        "ensure result must be cached — one probe for the whole session"
    );
    assert!(
        !names.contains(&"palace_create"),
        "an existing palace must never be re-created (metadata clobber)"
    );
}

/// (#4638) THE BOUND, at the RPC layer: a [`PalaceCreation::Forbidden`] sink
/// must never issue `palace_create`, so it can never increase the palace count
/// no matter how many turns it drains.
///
/// Why: this is the leak itself. The palace id is derived from the session's
/// project root, so a `tempfile::TempDir` root yields a per-RUN-unique id and
/// the `Allowed` path auto-created one permanent, unreadable palace for every
/// such run — 5,667 `t-tmp<random>` orphans, 97.8% of every palace on the
/// machine. Asserting only that the happy path still works would not have
/// caught that; the absence of `palace_create` is the whole property.
/// What: drives the SAME `MissingUntilCreated` mock that
/// `ensure_palace_creates_missing_palace_once` proves DOES create under
/// `Allowed`, and asserts the Forbidden sink's entire RPC trace is probes —
/// no `palace_create`, and no doomed `chat_turn_append`/`memory_remember`
/// against a palace that does not exist.
#[tokio::test]
async fn forbidden_creation_never_creates_a_palace() {
    let (daemon, captured) = spawn_capturing_mock(PalaceState::MissingUntilCreated).await;
    let sink = TurnMemorySink::new(
        daemon.socket().to_path_buf(),
        "t-tmpdeadbeef".to_string(),
        PalaceCreation::Forbidden,
    );

    for i in 0..5 {
        sink.enqueue("sess-ephemeral", format!("p{i}"), format!("r{i}"));
    }
    // Nothing to wait FOR (the assertion is an absence), so give the drain
    // task ample time to do the wrong thing if the gate is broken.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let calls = captured.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let names: Vec<&str> = calls.iter().map(|(_, p)| tool_name(p)).collect();
    assert!(
        !names.contains(&"palace_create"),
        "a Forbidden sink must never mint a palace, however many turns it \
         drains (#4638); got {names:?}"
    );
    assert!(
        !names.contains(&"chat_turn_append") && !names.contains(&"memory_remember"),
        "writes into a palace known not to exist are guaranteed to fail — \
         they must be skipped, not issued (#4638); got {names:?}"
    );
    assert!(
        names.iter().all(|n| *n == "palace_info"),
        "the only traffic a homeless Forbidden sink may generate is its \
         re-probe; got {names:?}"
    );
}

/// (#4638) Withholding CREATE must not withhold RECORDING: a Forbidden sink
/// whose palace already exists must dual-write exactly like an Allowed one.
///
/// Why: the fix bounds palace CREATION, not turn recording. If Forbidden also
/// suppressed writes into an existing palace, a session bound to a durable
/// project that merely resolved through an unusual root would silently stop
/// recording — a regression of #2345 dressed up as a fix. This pins the
/// distinction the design rests on.
#[tokio::test]
async fn forbidden_creation_still_writes_to_an_existing_palace() {
    let (daemon, captured) = spawn_capturing_mock(PalaceState::Exists).await;
    let sink = TurnMemorySink::new(
        daemon.socket().to_path_buf(),
        "already-there".to_string(),
        PalaceCreation::Forbidden,
    );

    sink.enqueue("sess-1", "p1", "r1");
    // 3 calls: palace_info (probe) + both dual-write calls.
    wait_for_captures(&captured, 3, Duration::from_secs(2)).await;

    let calls = captured.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let names: Vec<&str> = calls.iter().map(|(_, p)| tool_name(p)).collect();
    assert_eq!(
        names,
        vec!["palace_info", "chat_turn_append", "memory_remember"],
        "an existing palace must still receive the full dual-write under \
         PalaceCreation::Forbidden (#4638)"
    );
}

/// (#2363) A `"status":"skipped"` `memory_remember` response (e.g. tripped by
/// a gate `force` does not bypass) must be observable via a `tracing::warn!`
/// rather than silently swallowed like an ordinary success.
///
/// Why: this is the belt-and-braces half of #2363 — `force: true` handles
/// the dedup gate specifically, but this detection covers any OTHER gate
/// that still returns a skipped envelope. Post-#2424 the skipped payload
/// arrives STRINGIFIED inside the `tools/call` envelope, so this also pins
/// that the detection reads the PARSED inner result, not the raw envelope.
/// What: mock server always replies with a wrapped `status: skipped` body
/// for `memory_remember`; the test only asserts the call still lands (no
/// panic, no hang) — the warning itself is inspected via the tracing
/// subscriber in a real deployment, not asserted on here (no test-local
/// tracing capture exists in this crate yet), so this test's assertion is:
/// the drain task tolerates a skipped envelope exactly like a stored one.
/// (#2425) It also pins that a skipped envelope reports `Degraded`, not
/// `Durable` — a thinned recall surface is a degraded turn.
#[tokio::test]
async fn write_turn_warns_on_skipped_status() {
    let daemon = uds_mock::spawn(|_method: &str, params: Value| {
        Box::pin(async move {
            let inner = if tool_name(&params) == "memory_remember" {
                json!({"palace": "test-palace", "status": "skipped", "reason": "content gate"})
            } else {
                json!({"ok": true})
            };
            Ok(wrapped_result(&inner))
        })
    })
    .await;

    let observer = Arc::new(RecordingObserver::default());
    let sink = TurnMemorySink::new_observed(
        daemon.socket().to_path_buf(),
        "test-palace".to_string(),
        PalaceCreation::Allowed,
        observer.clone(),
    );
    sink.enqueue("sess-skip", "prompt", "response");
    // The assertion is simply that this never panics/hangs; give the drain
    // task a moment to run.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(matches!(
        observer
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_slice(),
        [MemoryTurnOutcome::Degraded {
            category: MemoryFailureCategory::MemoryRememberSkipped,
            ..
        }]
    ));
}

/// (#2425) A turn that lost ONE half of the dual-write is exactly one degraded
/// turn, reported under the half that failed first — not two, and not durable.
#[tokio::test]
async fn partial_dual_write_counts_once_as_failed_turn() {
    let daemon = uds_mock::spawn(|_method: &str, params: Value| {
        Box::pin(async move {
            if tool_name(&params) == "chat_turn_append" {
                return Err(RpcError::internal("sensitive raw response"));
            }
            Ok(wrapped_result(&json!({"ok": true})))
        })
    })
    .await;

    let outcome = write_turn(
        daemon.socket(),
        "test-palace",
        &QueuedTurn {
            sequence: 1,
            session_id: "s".into(),
            prompt: "credential-shaped prompt".into(),
            response: "secret response".into(),
        },
    )
    .await;
    assert!(matches!(
        outcome,
        MemoryTurnOutcome::Degraded {
            category: MemoryFailureCategory::ChatTurnAppend,
            ..
        }
    ));
}

/// [`TurnMemorySink::socket`]/[`TurnMemorySink::palace`] must expose exactly
/// the values passed at construction, so #2348's `recall_session` tool can
/// reuse the same binding.
#[test]
fn socket_and_palace_expose_construction_args() {
    let (tx, _rx) = mpsc::channel(1);
    let sink = TurnMemorySink {
        tx,
        socket: std::path::PathBuf::from("/tmp/example-memory.sock"),
        palace: "a-palace".to_string(),
        creation: PalaceCreation::Allowed,
        observer: Arc::new(NoopMemoryDurabilityObserver),
        next_sequence: AtomicU64::new(1),
    };
    assert_eq!(
        sink.socket(),
        std::path::Path::new("/tmp/example-memory.sock")
    );
    assert_eq!(sink.palace(), "a-palace");
}

/// An unreachable socket must never panic or hang the drain task — the
/// core fail-open contract (#2345 acceptance criteria), which post-#2424
/// also covers a failed palace ensure (probe AND create both unreachable).
#[tokio::test]
async fn write_turn_is_fail_open_on_unreachable_daemon() {
    let sink = TurnMemorySink::new(
        std::path::PathBuf::from("/nonexistent/trusty-memory.sock"),
        "p".to_string(),
        PalaceCreation::Allowed,
    );
    sink.enqueue("sess-2", "prompt", "response");
    // Give the drain task a moment to attempt (and fail) the calls; the test
    // passing at all (no panic, no hang) is the assertion.
    tokio::time::sleep(Duration::from_millis(200)).await;
}

/// `enqueue` must drop the NEWEST turn (not block) once the bounded channel
/// is full, per its documented overflow policy.
///
/// Why: exercised directly against the raw channel (constructing
/// `TurnMemorySink` from a sender with no drain task consuming it) so the
/// overflow behaviour is deterministic — no networking, no timing races
/// against a live consumer.
#[test]
fn enqueue_drops_newest_when_queue_full() {
    let (tx, mut rx) = mpsc::channel(1);
    let sink = TurnMemorySink {
        tx,
        socket: std::path::PathBuf::new(),
        palace: String::new(),
        creation: PalaceCreation::Allowed,
        observer: Arc::new(NoopMemoryDurabilityObserver),
        next_sequence: AtomicU64::new(1),
    };

    sink.enqueue("s1", "p1", "r1");
    sink.enqueue("s1", "p2", "r2"); // queue is full; this one must be dropped

    let first = rx.try_recv().expect("first turn should have been queued");
    assert_eq!(first.prompt, "p1");
    assert!(
        rx.try_recv().is_err(),
        "second turn must have been dropped, not queued"
    );
}

/// (#2425) Both of `enqueue`'s synchronous drop paths report a degraded turn,
/// so a turn the drain never sees still reaches the session's status.
#[test]
fn queue_full_and_closed_drain_are_immediate_failures() {
    let observer = Arc::new(RecordingObserver::default());
    let (tx, mut rx) = mpsc::channel(1);
    let sink = TurnMemorySink {
        tx,
        socket: std::path::PathBuf::new(),
        palace: String::new(),
        creation: PalaceCreation::Allowed,
        observer: observer.clone(),
        next_sequence: AtomicU64::new(1),
    };
    sink.enqueue("s", "p1", "r1");
    sink.enqueue("s", "p2", "r2"); // queue full
    let _ = rx.try_recv();
    drop(rx); // drain gone
    sink.enqueue("s", "p3", "r3");

    let outcomes = observer.0.lock().unwrap_or_else(|e| e.into_inner());
    assert!(matches!(
        outcomes.as_slice(),
        [
            MemoryTurnOutcome::Degraded {
                sequence: 2,
                category: MemoryFailureCategory::QueueFull,
                ..
            },
            MemoryTurnOutcome::Degraded {
                sequence: 3,
                category: MemoryFailureCategory::DrainClosed,
                ..
            }
        ]
    ));
}

/// (#2425) The detached drain must release its observer when the sink drops —
/// otherwise a registry-backed observer would outlive every session.
#[tokio::test]
async fn dropping_sink_releases_detached_drain_observer() {
    let observer: Arc<dyn MemoryDurabilityObserver> = Arc::new(RecordingObserver::default());
    let observer_weak = Arc::downgrade(&observer);
    let sink = TurnMemorySink::with_capacity_observed(
        "http://127.0.0.1:1".into(),
        "p".into(),
        1,
        PalaceCreation::Allowed,
        Arc::clone(&observer),
    );
    drop(observer);
    assert!(
        observer_weak.upgrade().is_some(),
        "sink/drain must own the observer while the sink is live"
    );

    drop(sink);
    tokio::time::timeout(Duration::from_secs(2), async {
        while observer_weak.upgrade().is_some() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("detached drain did not release its observer after sender closure");
    assert!(observer_weak.upgrade().is_none());
}

/// No git remote, no override -> falls back to a non-empty, stable slug
/// derived from the directory (never panics, never empty).
///
/// Why: this is a plain (non-git) temp dir, so `derive_palace_id_for_project`
/// must exercise its final fallback branch rather than the git-remote or
/// override branches; the exact slug shape is `derive_palace_id`'s concern
/// (already unit-tested in `trusty-common`), so this only asserts the
/// contract this function adds: non-empty and deterministic.
#[test]
fn derive_palace_id_for_project_falls_back_to_dirname() {
    let tmp = TempDir::new().expect("tempdir");
    let first =
        derive_palace_id_for_project(tmp.path()).expect("a plain temp dir resolves to a palace");
    let second =
        derive_palace_id_for_project(tmp.path()).expect("a plain temp dir resolves to a palace");
    assert!(!first.is_empty(), "expected a non-empty fallback palace id");
    assert_eq!(
        first, second,
        "derivation must be deterministic for the same path"
    );
}

/// No git remote, no override -> agrees with the shared `derive_palace_id`
/// core (issue #1772).
///
/// Why: this function used to carry its own copy of the divergent 4th
/// fallback that `trusty_common::catchup::derive_palace_id_for` also carried
/// (a raw, unslugified `file_name()` basename), which could disagree with
/// `derive_palace_id` on the same inputs. Both call sites now delegate
/// entirely to `trusty_common::derive_palace_id` and only fall back to a
/// fixed, non-derived placeholder, so for a plain (non-git) temp dir with no
/// `TRUSTY_MEMORY_PALACE` override, this function's result must equal calling
/// `trusty_common::derive_palace_id(project_dir, None, None)` directly.
/// What: clears the env override for the duration of the test (no other test
/// in this crate mutates `TRUSTY_MEMORY_PALACE`, so no `#[serial]` guard is
/// needed), then asserts the two calls agree.
/// Test: itself.
#[test]
fn derive_palace_id_for_project_agrees_with_derive_palace_id_no_remote_no_env() {
    // SAFETY: no other test in this crate mutates TRUSTY_MEMORY_PALACE.
    unsafe {
        std::env::remove_var(trusty_common::PALACE_OVERRIDE_ENV);
    }
    let tmp = TempDir::new().expect("tempdir");

    let project_value =
        derive_palace_id_for_project(tmp.path()).expect("a plain temp dir resolves to a palace");
    let core_value = trusty_common::derive_palace_id(tmp.path(), None, None)
        .expect("a real temp dir always has a usable parent/dir slug");

    assert_eq!(
        project_value, core_value,
        "turn-recorder derivation must agree with the shared derive_palace_id core"
    );
}

/// A long project directory still derives an id `palace_create` accepts
/// (#2443).
///
/// Why: derivation had no length bound while trusty-memory's format gate caps a
/// slug at 63 bytes, so a project under a long directory name derived an id the
/// daemon refused. `ensure_palace` then failed on every turn and this sink
/// recorded nothing for that project, warning only.
/// What: derives from a 90-character directory under a plain (non-git) temp
/// dir — the `parent/dir` branch, the one a project with no remote takes — and
/// asserts the result satisfies the shared contract the daemon's
/// `validate_slug_format` reads.
/// Test: itself.
#[test]
fn a_long_project_dir_derives_an_acceptable_palace_id() {
    // SAFETY: no other test in this crate mutates TRUSTY_MEMORY_PALACE.
    unsafe {
        std::env::remove_var(trusty_common::PALACE_OVERRIDE_ENV);
    }
    let tmp = TempDir::new().expect("tempdir");
    let long_dir = tmp.path().join(
        "a-project-with-an-unreasonably-long-directory-name-nobody-would-type-on-purpose-really",
    );
    std::fs::create_dir_all(&long_dir).expect("create long project dir");

    let id = derive_palace_id_for_project(&long_dir).expect("a long dir still resolves");

    assert!(
        trusty_common::palace_id_is_valid(&id),
        "derived id must pass the daemon's creation gate, got {id:?} ({} bytes)",
        id.len()
    );
}

/// Write a project root whose committed pin exists but does not parse.
///
/// Why: three of the four palace-resolution failures are pin-trust failures, and
/// only a real file on disk reaches them. `.trusty-tools/` is itself a project
/// marker, so the returned directory IS the project root that `find_project_root`
/// stops at — no walk up into the repo the test runs from.
/// What: returns a `TempDir` holding `.trusty-tools/trusty-memory.yaml` with a
/// body that is not pin YAML. Keep the handle alive for the test's duration.
/// Test: `malformed_pin_is_an_error_not_the_shared_placeholder`.
fn project_root_with_malformed_pin() -> TempDir {
    let tmp = TempDir::new().expect("tempdir");
    let dir = tmp.path().join(".trusty-tools");
    std::fs::create_dir_all(&dir).expect("create .trusty-tools");
    std::fs::write(
        dir.join("trusty-memory.yaml"),
        "palace: [unclosed\n\t bad: :",
    )
    .expect("write malformed pin");
    tmp
}

/// A committed pin that does not parse is an error, never the shared
/// `"unknown-project"` placeholder (#5811).
///
/// Why: the placeholder is one id shared by every project that fails to resolve,
/// and `SessionRegistry::memory_sink_for` grants a durable root
/// `PalaceCreation::Allowed` — so two projects with broken pins auto-created and
/// then shared one palace holding both of their real prompts and responses. The
/// error has to survive out of this function for the caller to be able to
/// decline.
/// Test: itself.
#[test]
fn malformed_pin_is_an_error_not_the_shared_placeholder() {
    let tmp = project_root_with_malformed_pin();

    let err = derive_palace_id_for_project(tmp.path())
        .expect_err("a pin that does not parse must not resolve to any palace");

    assert!(
        matches!(
            err,
            trusty_common::palace_resolve::PalaceResolveError::PinMalformed { .. }
        ),
        "expected PinMalformed, got {err:?}"
    );
}
