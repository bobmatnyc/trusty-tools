//! End-to-end claude-mpm ⟷ SM ⟷ t-mpm smoke test (DOC-14 §1A.2).
//!
//! Why: the SM's prime acceptance is that a parent `claude-mpm`/PM can drive the
//! whole Session Manager headlessly over the SM-STDIO JSON-RPC surface and see a
//! real conversational turn AND a real delegation that LAUNCHES a managed session
//! — the §1A.2 topology `claude-mpm ⟷ SM ⟷ t-mpm`. Every existing test drives the
//! `SmDispatcher` *in-crate* (calling `dispatch` directly, using `#[cfg(test)]`
//! mock seams). This file is the FIRST test that exercises the chain the way an
//! external driver actually does: from `crates/trusty-mpm/tests/` (an out-of-crate
//! integration target that sees ONLY the public API), over a real
//! newline-delimited JSON-RPC wire, with test doubles for the LLM provider and the
//! session-control surface so the run is fully hermetic — NO Anthropic/OpenRouter
//! key, NO real `claude` binary, NO tmux, NO network.
//!
//! What: this target builds a real [`SessionManagerAgent`] over a public-trait mock
//! provider/resolver and a real [`SmDispatcher`] over a public-trait mock
//! [`SessionControl`], then frames `sm.*` requests as JSON lines through a
//! production-faithful stdio loop (the same line framing
//! `trusty_common::mcp::run_stdio_loop` uses) over an in-memory duplex pipe. It
//! drives `sm.health` → `sm.chat` (a goal-style message) → `sm.sessions.list`
//! always, and — under `--features sm-memory` — the SM-8 delegation path
//! (`sm.delegate`) which launches a managed session, confirming via
//! `sm.sessions.list` that a session was created. A RAII teardown guard asserts the
//! mock control leaked no live sessions.
//!
//! Test: this file IS the test. Run with
//! `cargo test -p trusty-mpm --test sm_e2e_smoke -- --nocapture`
//! (add `--features sm-memory` to include the delegation chain).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use trusty_common::mcp::{Request, Response, error_codes};

use trusty_mpm::activity::cache::ActivityState;
use trusty_mpm::core::sm::agent::SessionManagerAgent;
use trusty_mpm::core::sm::providers::{
    LlmProvider, LlmRequest, LlmResponse, ProviderKind, ResolvedCall, SmLlmError, SmModelTier,
    TierResolver,
};
use trusty_mpm::core::sm::{
    LaunchParams, RawObservation, SessionControl, SessionControlError, SessionManagerConfig,
    SmInferenceConfig, Submit, Summary,
};
use trusty_mpm::daemon::sm_stdio::SmDispatcher;

// ── Mock LLM provider + resolver (public-trait test doubles) ────────────────────

/// A deterministic, recording [`LlmProvider`] for the smoke test (no network).
///
/// Why: the smoke test must produce a REAL conversational turn (and a real
/// DECOMPOSE decision for delegation) without any provider credentials or HTTP. A
/// fixed-reply provider makes `sm.chat`/`sm.delegate` deterministic while still
/// exercising the full §7.5 assembly + record-round path the agent runs per turn.
/// What: returns `reply` verbatim with a fixed cost; records the request count so
/// the test can assert the provider was actually invoked (proving the turn ran).
/// Test: drives the chat + delegate smoke assertions below.
struct MockProvider {
    reply: String,
    calls: AtomicUsize,
}

impl MockProvider {
    /// Build a mock provider that always replies `reply`.
    ///
    /// Why: the test scripts the SM's reply (or a DECOMPOSE decision JSON) so the
    /// chain is deterministic. What: stores the reply and a zeroed call counter.
    /// Test: used by every dispatcher built below.
    fn new(reply: impl Into<String>) -> Self {
        Self {
            reply: reply.into(),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, SmLlmError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(LlmResponse {
            text: self.reply.clone(),
            model: "mock/model".to_string(),
            input_tokens: 8,
            output_tokens: 8,
            latency_ms: 1,
            cost_usd: 0.0001,
        })
    }
}

/// A [`TierResolver`] that always hands back the same mock provider.
///
/// Why: the agent resolves a provider per tier (orchestration + compaction); the
/// production `ProviderRegistry` would need credentials. This injectable resolver
/// (Dependency Inversion via the public trait) makes EVERY tier resolve to the
/// scripted mock so the whole turn runs offline.
/// What: wraps an `Arc<MockProvider>` and returns a [`ResolvedCall`] over it for
/// any tier.
/// Test: built by `build_dispatcher` below.
struct MockResolver {
    provider: Arc<MockProvider>,
}

#[async_trait]
impl TierResolver for MockResolver {
    async fn resolve(
        &self,
        _cfg: &SmInferenceConfig,
        _tier: SmModelTier,
    ) -> Result<ResolvedCall, SmLlmError> {
        Ok(ResolvedCall {
            provider: self.provider.clone(),
            model: "mock/model".to_string(),
            kind: ProviderKind::Anthropic,
        })
    }
}

// ── Mock session control (the t-mpm session surface, leak-checked) ──────────────

/// A deterministic [`SessionControl`] double standing in for the managed t-mpm
/// session surface — and a leak detector.
///
/// Why: the delegation chain (and `sm.sessions.*`) must LAUNCH a managed session
/// without tmux/workspace, and the smoke test must prove no session is leaked at
/// teardown (the #1452 tmux-lifecycle discipline). This mock records every
/// launched session id and tracks how many are still "live" so a RAII guard can
/// assert clean teardown.
/// What: `launch` mints a sequential session id, marks it live, and returns
/// `{ session_id }`; `get` returns a pane that carries `evidence` (so the SM-8
/// verification gate can pass); `list` returns the live ids; `stop`/`kill` mark a
/// session not-live; `send` records delivery. All offline, no panics.
/// Test: shared by the chat/sessions smoke and the delegation smoke.
#[derive(Default)]
struct MockControl {
    /// Pane evidence `get`/`observe` surface (a PR URL ⇒ the §3.5 gate passes).
    evidence: String,
    /// Monotonic counter minting `s-1`, `s-2`, … session ids.
    next: AtomicUsize,
    /// Every launched session id, in order (for the launch assertion).
    launched: std::sync::Mutex<Vec<String>>,
    /// The currently-live session ids (drained by `stop`/`kill`; the leak check).
    live: std::sync::Mutex<Vec<String>>,
    /// Recorded `(session_id, text)` deliveries (proves #1299 task delivery).
    sends: std::sync::Mutex<Vec<(String, String)>>,
}

impl MockControl {
    /// Build a control whose sessions OBSERVE as carrying `evidence`.
    ///
    /// Why: the SM-8 gate marks a task `Verified` only with observed evidence; a
    /// PR-URL pane lets the delegation smoke drive the gate to a real close.
    /// What: stores `evidence`; everything else defaults empty.
    /// Test: used by the delegation smoke.
    fn with_evidence(evidence: impl Into<String>) -> Self {
        Self {
            evidence: evidence.into(),
            ..Self::default()
        }
    }

    /// How many sessions are still live (the teardown leak metric).
    ///
    /// Why: the RAII guard asserts this is zero so the test proves it leaks no
    /// session. What: returns the live-id count. Test: the teardown guard reads it.
    fn live_count(&self) -> usize {
        self.live.lock().expect("live lock").len()
    }

    /// Snapshot the launched session ids (for the launch assertion).
    ///
    /// Only consulted by the `sm-memory`-gated delegation smoke.
    #[cfg(feature = "sm-memory")]
    fn launched_ids(&self) -> Vec<String> {
        self.launched.lock().expect("launched lock").clone()
    }

    /// Snapshot recorded deliveries (for the #1299 delivery assertion).
    ///
    /// Only consulted by the `sm-memory`-gated delegation smoke.
    #[cfg(feature = "sm-memory")]
    fn sent(&self) -> Vec<(String, String)> {
        self.sends.lock().expect("sends lock").clone()
    }
}

#[async_trait]
impl SessionControl for MockControl {
    async fn launch(&self, _params: LaunchParams) -> Result<Value, SessionControlError> {
        let n = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        let id = format!("s-{n}");
        self.launched
            .lock()
            .expect("launched lock")
            .push(id.clone());
        self.live.lock().expect("live lock").push(id.clone());
        Ok(json!({ "session_id": id }))
    }

    async fn list(&self) -> Result<Value, SessionControlError> {
        let live = self.live.lock().expect("live lock").clone();
        let sessions: Vec<Value> = live
            .iter()
            .map(|id| json!({ "id": id, "state": "active" }))
            .collect();
        Ok(json!({ "sessions": sessions }))
    }

    async fn get(&self, session_id: &str) -> Result<Value, SessionControlError> {
        Ok(json!({
            "session": { "id": session_id, "state": "running", "pane": self.evidence },
        }))
    }

    async fn send(&self, id: &str, text: &str) -> Result<Value, SessionControlError> {
        self.sends
            .lock()
            .expect("sends lock")
            .push((id.to_string(), text.to_string()));
        Ok(json!({ "ok": true }))
    }

    async fn stop(&self, id: &str) -> Result<Value, SessionControlError> {
        self.live.lock().expect("live lock").retain(|s| s != id);
        Ok(json!({ "ok": true }))
    }

    async fn resume(&self, _id: &str) -> Result<Value, SessionControlError> {
        Ok(json!({ "ok": true }))
    }

    async fn kill(&self, id: &str) -> Result<Value, SessionControlError> {
        self.live.lock().expect("live lock").retain(|s| s != id);
        Ok(json!({ "ok": true }))
    }

    async fn inject_text(
        &self,
        id: &str,
        text: &str,
        _submit: Submit,
    ) -> Result<(), SessionControlError> {
        self.sends
            .lock()
            .expect("sends lock")
            .push((id.to_string(), text.to_string()));
        Ok(())
    }

    async fn observe(
        &self,
        _id: &str,
        _lines: usize,
    ) -> Result<RawObservation, SessionControlError> {
        Ok(RawObservation {
            raw_pane: self.evidence.clone(),
            runtime_active: true,
            pending_decision: None,
            proposed_default: None,
        })
    }

    async fn summarize(&self, _id: &str) -> Result<Summary, SessionControlError> {
        Ok(Summary {
            state: ActivityState::Unknown,
            summary: None,
            confidence: 0.0,
        })
    }
}

/// RAII teardown guard asserting the SM smoke test leaked no live session.
///
/// Why: the #1452 tmux-lifecycle discipline requires a managed session never to
/// outlive its driver. This guard holds the [`MockControl`] and, on drop, asserts
/// every launched session was torn down (live count zero) — so a future change
/// that forgets to stop/kill a launched session fails the test at teardown rather
/// than silently leaking. Modelled as a `Drop` guard so it fires even on an early
/// assertion panic.
/// What: on drop, panics if `control.live_count() != 0` (unless already panicking,
/// to avoid masking the original failure).
/// Test: every smoke body constructs one; an intact teardown is the pass.
struct TeardownGuard {
    control: Arc<MockControl>,
}

impl Drop for TeardownGuard {
    fn drop(&mut self) {
        // Avoid a double-panic that would abort and mask the real assertion.
        if std::thread::panicking() {
            return;
        }
        let live = self.control.live_count();
        assert_eq!(
            live, 0,
            "session leak: {live} managed session(s) still live at teardown \
             (every launched session must be stopped/killed — #1452 lifecycle)"
        );
    }
}

// ── Dispatcher + wire harness builders ──────────────────────────────────────────

/// An enabled SM config for the smoke test (provider-backed, default tiers).
fn enabled_config() -> SessionManagerConfig {
    SessionManagerConfig {
        enabled: true,
        ..SessionManagerConfig::default()
    }
}

/// Build a real [`SmDispatcher`] over a scripted provider + a mock control.
///
/// Why: the dispatcher is the exact surface `tm sm serve --stdio` builds and the
/// parent driver talks to. Constructing the REAL dispatcher (not a stub) over
/// public-trait doubles is what makes this an end-to-end test of the SM chain.
/// What: builds a [`SessionManagerAgent`] via the public `with_runtime` over the
/// mock resolver, then a [`SmDispatcher`] over the mock control. Under
/// `sm-memory` it wires an in-memory goal store over a no-op palace so the goal /
/// delegation surfaces are live; without it, goals are `None` (the no-memory
/// build's graceful-unavailable path).
/// Test: used by both smoke bodies.
fn build_dispatcher(
    provider_reply: &str,
    control: Arc<MockControl>,
    data_root: &std::path::Path,
) -> SmDispatcher {
    let cfg = enabled_config();
    let provider = Arc::new(MockProvider::new(provider_reply));
    let resolver: Arc<dyn TierResolver> = Arc::new(MockResolver { provider });

    #[cfg(feature = "sm-memory")]
    let agent = Arc::new(SessionManagerAgent::with_runtime(
        cfg.clone(),
        resolver,
        data_root.to_path_buf(),
        None,
    ));
    #[cfg(not(feature = "sm-memory"))]
    let agent = Arc::new(SessionManagerAgent::with_runtime(
        cfg.clone(),
        resolver,
        data_root.to_path_buf(),
    ));

    let sessions: Arc<dyn SessionControl> = control;

    #[cfg(feature = "sm-memory")]
    let goals = Some(in_memory_goal_store(data_root));
    #[cfg(not(feature = "sm-memory"))]
    let goals = None;

    SmDispatcher::new(agent, cfg, data_root.to_path_buf(), sessions, goals)
}

/// Build an in-memory SM goal store over a no-op palace (`sm-memory` only).
///
/// Why: the delegation + goals surfaces persist through the SM-6 store; the smoke
/// test must drive them without a real palace (no ONNX/usearch). A no-op
/// `GoalMemory` gives the store a seam that always succeeds with no durable
/// entries, so creates/links/closes are visible in-memory within one run.
/// What: constructs `SmGoalStore::new` over a no-op `GoalMemory` rooted at
/// `data_root`, behind the `Arc<Mutex<…>>` the dispatcher expects.
/// Test: used by `build_dispatcher` under `sm-memory`.
#[cfg(feature = "sm-memory")]
fn in_memory_goal_store(
    data_root: &std::path::Path,
) -> Arc<tokio::sync::Mutex<trusty_mpm::core::sm::SmGoalStore>> {
    use trusty_mpm::core::sm::{GoalMemory, SmGoalStore};

    struct NoopMem;
    #[async_trait]
    impl GoalMemory for NoopMem {
        async fn remember_goal(&self, _json: String, _tag: &str) -> Result<(), String> {
            Ok(())
        }
        async fn list_goals(&self, _tag: &str) -> Result<Vec<String>, String> {
            Ok(Vec::new())
        }
    }
    let store = SmGoalStore::new(Arc::new(NoopMem), data_root.to_path_buf());
    Arc::new(tokio::sync::Mutex::new(store))
}

/// Drive the SM dispatcher over a production-faithful JSON-RPC stdio wire.
///
/// Why: the smoke test's whole point is to exercise the SM the way `claude-mpm`
/// does — as newline-delimited JSON-RPC lines over a pipe, not by calling
/// `dispatch` directly. `trusty_common::mcp::run_stdio_loop` is hard-wired to the
/// process's real stdin/stdout, so this mirrors its loop body over an injected
/// in-memory duplex (identical framing: one request line in → one
/// non-suppressed JSON response line out). The dispatcher runs on a background
/// task so the test can write requests and read replies concurrently, exactly
/// like a parent process speaking to a child over pipes.
/// What: spawns a task running the loop over `(server_rx, server_tx)`; returns a
/// [`WireClient`] that writes request lines into the server and reads response
/// lines back. Dropping the client closes the write half (EOF), so the loop task
/// returns `Ok` and the wire shuts down cleanly.
/// Test: both smoke bodies build one and round-trip every method through it.
fn spawn_wire(dispatcher: Arc<SmDispatcher>) -> WireClient {
    let (client_tx, server_rx) = tokio::io::duplex(64 * 1024);
    let (server_tx, client_rx) = tokio::io::duplex(64 * 1024);

    let loop_task = tokio::spawn(async move {
        let mut lines = BufReader::new(server_rx).lines();
        let mut writer = server_tx;
        while let Some(line) = lines.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let response = match serde_json::from_str::<Request>(trimmed) {
                Ok(req) => dispatcher.dispatch(req).await,
                Err(e) => Response::err(
                    None,
                    error_codes::PARSE_ERROR,
                    format!("invalid JSON-RPC: {e}"),
                ),
            };
            // Notifications produce a suppressed response — write nothing for them.
            if response.suppress {
                continue;
            }
            let serialised = serde_json::to_string(&response)?;
            writer.write_all(serialised.as_bytes()).await?;
            writer.write_all(b"\n").await?;
            writer.flush().await?;
        }
        Ok::<(), anyhow::Error>(())
    });

    WireClient {
        tx: client_tx,
        rx: BufReader::new(client_rx).lines(),
        loop_task: Some(loop_task),
        next_id: 0,
    }
}

/// Max wall-clock a single `sm.*` wire call waits for its response line.
///
/// Why: `WireClient::call` reads the response on a real async wire; if the
/// dispatcher hangs/deadlocks (or writes no response) the read would block
/// forever and the test would only fail at the CI job timeout with no
/// actionable message. Bounding the read turns a hang into a descriptive panic.
/// What: 10s — generous for an offline, mock-backed turn yet far below any CI
/// job timeout. Test: enforced in `WireClient::call`.
const WIRE_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// A parent-side JSON-RPC client over the SM stdio wire (test driver).
///
/// Why: stands in for `claude-mpm` driving `tm sm serve --stdio` — it frames one
/// request per line, reads one response line back, and auto-increments the id so a
/// scripted sequence reads naturally. What: holds the duplex write half, a
/// line-reader over the read half, and the loop task handle (awaited on shutdown).
/// Test: the smoke bodies call `call` / `shutdown` on it.
struct WireClient {
    tx: tokio::io::DuplexStream,
    rx: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    loop_task: Option<tokio::task::JoinHandle<Result<(), anyhow::Error>>>,
    next_id: i64,
}

impl WireClient {
    /// Send one `sm.*` request over the wire and return its parsed JSON response.
    ///
    /// Why: the single operation the scripted smoke sequence needs — write a framed
    /// request line, read exactly one framed response line back, and surface it as
    /// JSON for assertions. Auto-assigning the id keeps call sites terse and
    /// guarantees the response id echoes the request (asserted here).
    /// What: serialises `{ jsonrpc, id, method, params }` + `\n`, flushes, reads
    /// one line, parses it, and asserts the id round-tripped; returns the response
    /// value. Panics (test failure) on I/O error or a missing/garbled response.
    /// Test: every method call in the smoke bodies.
    async fn call(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let line = serde_json::to_string(&req).expect("serialise request");
        self.tx
            .write_all(format!("{line}\n").as_bytes())
            .await
            .expect("write request line");
        self.tx.flush().await.expect("flush request");

        let resp_line = tokio::time::timeout(WIRE_CALL_TIMEOUT, self.rx.next_line())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "sm wire call '{method}' timed out after {}s — dispatcher hung or wrote no response",
                    WIRE_CALL_TIMEOUT.as_secs()
                )
            })
            .expect("read response line")
            .unwrap_or_else(|| panic!("no response line for {method} (wire closed early)"));
        let resp: Value = serde_json::from_str(&resp_line).expect("response is valid JSON");
        assert_eq!(resp["jsonrpc"], "2.0", "framing: jsonrpc 2.0 for {method}");
        assert_eq!(resp["id"], json!(id), "framing: id echoed for {method}");
        resp
    }

    /// Close the wire (EOF) and await the loop task's clean return.
    ///
    /// Why: a parent driver finishes by closing the child's stdin; the loop must
    /// then return `Ok` on EOF (no hang, no error). Asserting that here proves the
    /// adapter shuts down cleanly. What: drops the write half (EOF) and awaits the
    /// loop task, asserting `Ok`. Test: called at the end of each smoke body.
    async fn shutdown(self) {
        // Destructure to own each part: dropping `tx` signals EOF to the loop, then
        // we await the loop task and assert its clean `Ok` return.
        let WireClient {
            tx,
            rx: _,
            loop_task,
            ..
        } = self;
        drop(tx);
        let task = loop_task.expect("loop task present");
        let joined = task.await.expect("loop task joins");
        joined.expect("stdio loop returns Ok on EOF");
    }
}

/// Assert a JSON-RPC response carries a `result` (not an `error`); return it.
///
/// Why: every successful smoke step expects a `result`; centralising the check
/// gives a clear failure message naming the offending step. What: panics with the
/// error payload if `error` is present; otherwise returns `result`. Test: used
/// throughout the smoke bodies.
fn result_of(resp: &Value, step: &str) -> Value {
    // A valid JSON-RPC success response carries NO `error` key at all — not even
    // `error: null`. Asserting absence (rather than absent-or-null) prevents a
    // malformed `{"error": null, …}` from masquerading as success.
    assert!(
        resp.get("error").is_none(),
        "step `{step}` did not return a clean success response (expected no `error` key): {resp:?}"
    );
    resp.get("result")
        .cloned()
        .unwrap_or_else(|| panic!("step `{step}` has no result: {resp:?}"))
}

// ── Smoke test: chat + sessions (always runs, no sm-memory required) ─────────────

/// The base SM chain a parent driver always exercises: health → chat → sessions.
///
/// Why: proves the SM-STDIO surface is drivable end-to-end over the wire with a
/// real conversational turn, independent of the `sm-memory` feature. `sm.chat`
/// runs the FULL SM turn (prompt assembly + provider + record-round) and must
/// return a real reply + a conv_id; a parent can then launch and list managed
/// sessions through the same wire. This is the offline, always-green core of the
/// §1A.2 topology.
/// What: builds the dispatcher over a scripted provider + a leak-checked control,
/// drives `sm.health`, `sm.chat` (a goal-style message), a manual
/// `sm.sessions.launch`, and `sm.sessions.list`, asserting a real turn and a
/// managed session appearing in the list; then tears the launched session down and
/// lets the RAII guard assert no leak.
/// Test: this is the test.
#[tokio::test]
async fn sm_e2e_chat_and_sessions_over_wire() {
    let tmp = TempDir::new().expect("tempdir");
    let control = Arc::new(MockControl::with_evidence("ready"));
    let _guard = TeardownGuard {
        control: control.clone(),
    };

    let dispatcher = Arc::new(build_dispatcher(
        "Understood. I will delegate this goal to a managed session.",
        control.clone(),
        tmp.path(),
    ));
    let mut wire = spawn_wire(dispatcher);

    // 1) health — the parent probes SM readiness first (provider-backed ⇒ ok).
    let health = wire.call("sm.health", json!({})).await;
    let h = result_of(&health, "sm.health");
    assert_eq!(h["ok"], true, "provider-backed SM reports healthy");
    assert_eq!(h["degraded"], false, "a wired provider is not degraded");

    // 2) chat — a real conversational turn over a goal-style message.
    let chat = wire
        .call(
            "sm.chat",
            json!({ "message": "Ship the new login flow end to end.", "conv_id": "smoke-1" }),
        )
        .await;
    let c = result_of(&chat, "sm.chat");
    assert_eq!(
        c["reply"], "Understood. I will delegate this goal to a managed session.",
        "the SM returned the scripted conversational reply"
    );
    assert_eq!(c["conv_id"], "smoke-1", "the conv_id round-trips");
    assert!(
        c["cost"].as_f64().expect("cost is a number") >= 0.0,
        "a per-call cost is reported"
    );

    // 3) launch a managed session through the same wire (the t-mpm hop).
    let launch = wire
        .call(
            "sm.sessions.launch",
            json!({ "workdir": "/repo", "prompt": "implement login" }),
        )
        .await;
    let launched = result_of(&launch, "sm.sessions.launch");
    let session_id = launched["session_id"]
        .as_str()
        .expect("session_id present")
        .to_string();
    assert!(!session_id.is_empty(), "a managed session was launched");

    // 4) list — the launched session is visible to the parent driver.
    let list = wire.call("sm.sessions.list", json!({})).await;
    let l = result_of(&list, "sm.sessions.list");
    let sessions = l["sessions"].as_array().expect("sessions array");
    assert!(
        sessions.iter().any(|s| s["id"] == json!(session_id)),
        "the launched session appears in sm.sessions.list"
    );

    // 5) tear the session down cleanly so the RAII guard sees no leak.
    let stop = wire
        .call("sm.sessions.stop", json!({ "session_id": session_id }))
        .await;
    assert_eq!(result_of(&stop, "sm.sessions.stop")["ok"], true);

    wire.shutdown().await;
    assert_eq!(
        control.live_count(),
        0,
        "no managed session is left live after teardown"
    );
}

// ── Smoke test: full delegation chain (requires sm-memory) ───────────────────────

/// THE §1A.2 capstone: `claude-mpm ⟷ SM ⟷ t-mpm` delegation over the wire.
///
/// Why: this is the friction the task surfaces — the SM-8 delegation path
/// (`sm.delegate`) persists a tracked goal, so it is gated behind `sm-memory`.
/// Under that feature this proves the END-TO-END chain a parent driver cares about:
/// send a goal, the SM DECOMPOSEs it (one mocked LLM call), LAUNCHES a managed
/// session, DELIVERS the task (#1299), OBSERVES evidence, VERIFIES through the §3.5
/// gate, and the goal CLOSES — confirmed by `sm.sessions.list` showing a session was
/// launched and `sm.goals.list` showing the goal `done`. Fully offline (scripted
/// provider + mock control). Without `sm-memory`, this body is not compiled (the
/// no-memory build's `sm.delegate` returns a graceful unavailable error, covered by
/// the dispatcher unit tests).
/// What: scripts the provider to reply with a `delegate` decision JSON and a control
/// whose pane carries a PR URL; drives `sm.delegate`, asserting a launched session,
/// `goal_done == true`, task delivery, the session visible in `sm.sessions.list`,
/// and the goal `done` in `sm.goals.list`; then kills the session and lets the RAII
/// guard assert no leak.
/// Test: this is the test (run with `--features sm-memory`).
#[cfg(feature = "sm-memory")]
#[tokio::test]
async fn sm_e2e_delegation_chain_over_wire() {
    let tmp = TempDir::new().expect("tempdir");
    let control = Arc::new(MockControl::with_evidence(
        "Opened PR https://github.com/acme/login/pull/42",
    ));
    let _guard = TeardownGuard {
        control: control.clone(),
    };

    // The SM's only LLM call in the loop is DECOMPOSE; script a delegate decision.
    //
    // Decision-parsing contract this test depends on: `SessionManagerAgent` /
    // `SmDispatcher` parse this RAW assistant message as a structured DECOMPOSE
    // decision — `action: "delegate"` ⇒ each `tasks[]` entry becomes a launched
    // session — then run the SM-8 loop (launch → deliver → observe → verify) and
    // mark the goal `done` once the §3.5 verification gate passes. If that parsing
    // contract changes (field names, the raw-JSON-vs-tool-call shape, or the
    // gate), this scripted reply stops decoding to a delegate decision and the
    // `goal_done == true` assertion below fails — making the contract break
    // visible here rather than silently downstream.
    let decision =
        r#"{"action":"delegate","tasks":[{"workdir":"/repo","prompt":"open the login PR"}]}"#;
    let dispatcher = Arc::new(build_dispatcher(decision, control.clone(), tmp.path()));
    let mut wire = spawn_wire(dispatcher);

    // 1) delegate — the full launch → deliver → observe → verify → close loop.
    let delegate = wire
        .call(
            "sm.delegate",
            json!({ "message": "Open the PR for the new login flow." }),
        )
        .await;
    let d = result_of(&delegate, "sm.delegate");

    let launched = d["launched"].as_array().expect("launched array");
    assert_eq!(launched.len(), 1, "delegation launched exactly one session");
    assert_eq!(
        d["goal_done"], true,
        "observed PR-URL evidence ⇒ the §3.5 gate passed and the goal closed"
    );
    assert_eq!(
        d["goal_status"], "Done",
        "goal_status reflects the closed goal"
    );
    let goal_id = d["goal_id"].as_str().expect("goal_id present").to_string();

    // 2) #1299: the task prompt was delivered to the launched session.
    let sent = control.sent();
    assert!(
        sent.iter().any(|(_, text)| text == "open the login PR"),
        "the decomposed task was delivered to the launched session: {sent:?}"
    );

    // 3) the launched session is visible to the parent driver via sm.sessions.list.
    let launched_id = control
        .launched_ids()
        .first()
        .cloned()
        .expect("a session id was minted");
    let list = wire.call("sm.sessions.list", json!({})).await;
    let l = result_of(&list, "sm.sessions.list");
    assert!(
        l["sessions"]
            .as_array()
            .expect("sessions array")
            .iter()
            .any(|s| s["id"] == json!(launched_id)),
        "the delegated session appears in sm.sessions.list"
    );

    // 4) the goal is visible as `done` through the goals surface.
    let goals = wire.call("sm.goals.list", json!({})).await;
    let g = result_of(&goals, "sm.goals.list");
    let found = g["goals"]
        .as_array()
        .expect("goals array")
        .iter()
        .find(|goal| goal["id"] == json!(goal_id))
        .expect("the delegated goal is listed");
    assert_eq!(found["status"], "done", "the goal closed through the gate");

    // 5) reap the launched session so the RAII guard sees no leak.
    let kill = wire
        .call("sm.sessions.kill", json!({ "session_id": launched_id }))
        .await;
    assert_eq!(result_of(&kill, "sm.sessions.kill")["ok"], true);

    wire.shutdown().await;
    assert_eq!(
        control.live_count(),
        0,
        "no managed session is left live after the delegation chain"
    );
}
