//! Turn recorder (#2345): durable per-turn dual-write to trusty-memory.
//!
//! Why: epic #2343 (Infinite Sessions) requires every PM prompt/response
//! turn to be durably recorded in trusty-memory, independent of the
//! in-process `Transcript` (#2344), which only lives as long as the daemon
//! process. Without this, a daemon restart or crash loses the entire
//! conversation; #2348's future `recall_session` tool also needs a semantic
//! recall surface over session history that a bare `chat_turn_append` record
//! alone would not give it (it stores the exact turn but is not
//! embedded/indexed for recall the way `memory_remember` is).
//! What: [`TurnMemorySink`] owns a bounded `tokio::sync::mpsc` queue and a
//! background drain task. [`TurnMemorySink::enqueue`] is the non-blocking
//! producer side, called from `task::executor::run_and_record` at each turn
//! boundary. The drain task calls BOTH `chat_turn_append` (the exact
//! chronological record) and `memory_remember` (tagged
//! `["session:<id>", "turn"]` — the semantic recall surface for #2348) via
//! `trusty_common::mcp::memory_rpc::call_memory_tool_at`, against a base URL
//! resolved ONCE at construction — never blocking or failing the calling
//! turn: any RPC failure is logged via `tracing::warn!` and dropped.
//! [`derive_palace_id_for_project`] mirrors
//! `trusty_common::catchup`'s (private) palace-derivation convention so a
//! session's turns land in the same palace its PM catch-up digest reads
//! from.
//! Test: `memory_sink::tests::*`.

use std::path::Path;

use serde_json::json;
use tokio::sync::mpsc;
use tracing::warn;
use trusty_common::mcp::memory_rpc::call_memory_tool_at;

/// Bounded mpsc queue capacity (#2345 scope: "~50").
///
/// Why: bounds memory use when trusty-memory is slow or unreachable for a
/// long stretch; a session realistically produces at most one turn per LLM
/// round trip, so 50 in-flight turns is generous slack before the overflow
/// policy below kicks in.
/// Test: `memory_sink::tests::enqueue_drops_newest_when_queue_full`.
pub const QUEUE_CAPACITY: usize = 50;

/// One user-prompt/assistant-response turn queued for durable dual-write.
#[derive(Debug, Clone)]
struct QueuedTurn {
    session_id: String,
    prompt: String,
    response: String,
}

/// Async, fire-and-forget durable-write sink for one session's turns (#2345).
///
/// Why: see module docs.
/// What: `enqueue` never blocks the calling turn and never fails visibly —
/// see its docs for the overflow policy. The background drain task owns the
/// only receiver, so it keeps running for exactly as long as this sink (and
/// therefore the channel's sender half) stays alive — the session's
/// `SessionEntry` holds the constructed `Arc<TurnMemorySink>` for the
/// session's lifetime (built once, lazily, on the session's first
/// `task.run` — see `SessionRegistry::memory_sink_for`), so the drain task
/// naturally survives across every run on that session, not just one.
pub struct TurnMemorySink {
    tx: mpsc::Sender<QueuedTurn>,
}

impl TurnMemorySink {
    /// Construct a sink writing to `palace` at the given (already-resolved)
    /// `base_url`, and spawn its background drain task with the default
    /// [`QUEUE_CAPACITY`].
    /// Test: `memory_sink::tests::enqueue_drain_happy_path`.
    pub fn new(base_url: String, palace: String) -> Self {
        Self::with_capacity(base_url, palace, QUEUE_CAPACITY)
    }

    /// Same as [`Self::new`] with an explicit queue capacity — tests use a
    /// tiny capacity to exercise the overflow policy cheaply.
    ///
    /// Why: `base_url` is resolved ONCE by the caller (mirroring
    /// `catchup::pm_catchup_context`'s own
    /// `resolve_memory_base_url_or_unreachable()` call) rather than
    /// re-resolved on every enqueued turn — the daemon's bound address does
    /// not change mid-session, and re-resolving on every turn would add
    /// discovery-file I/O to the hot drain path for no benefit. Tests inject
    /// a mock server's URL directly here instead of mutating the
    /// process-global `TRUSTY_MEMORY_URL` env var (unsafe across parallel
    /// tests).
    /// What: spawns [`drain`] as a detached `tokio::spawn`ed task owning the
    /// receiver half of a `capacity`-bounded channel; returns the sink
    /// holding only the sender half.
    /// Test: `memory_sink::tests::enqueue_drops_newest_when_queue_full`.
    pub fn with_capacity(base_url: String, palace: String, capacity: usize) -> Self {
        let (tx, rx) = mpsc::channel(capacity);
        tokio::spawn(drain(base_url, palace, rx));
        Self { tx }
    }

    /// Enqueue one turn for durable dual-write, never blocking the caller.
    ///
    /// Why: turn recording must NEVER stall or fail a running turn (#2345
    /// acceptance criteria) — a slow or wedged drain task must not back up
    /// into the agent loop.
    /// What: `try_send`s onto the bounded channel. Overflow policy: DROP THE
    /// NEWEST turn (this call's turn) rather than evicting an
    /// already-queued older one — the simplest policy `mpsc::Sender::
    /// try_send` supports directly (no peek/pop-front on the sender side
    /// without a different channel type), logged via `tracing::warn!` so an
    /// operator can see it happened. A closed receiver (the drain task
    /// panicked or was dropped) degrades the same way: logged, dropped, no
    /// error surfaced to the caller.
    /// Test: `memory_sink::tests::enqueue_drops_newest_when_queue_full`.
    pub fn enqueue(
        &self,
        session_id: impl Into<String>,
        prompt: impl Into<String>,
        response: impl Into<String>,
    ) {
        let turn = QueuedTurn {
            session_id: session_id.into(),
            prompt: prompt.into(),
            response: response.into(),
        };
        match self.tx.try_send(turn) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                warn!("turn_recorder: queue full (capacity reached) — dropping newest turn");
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                warn!("turn_recorder: drain task gone — dropping turn");
            }
        }
    }
}

/// Background drain loop: pop turns off the channel and dual-write each one,
/// fail-open (see [`write_turn`]).
async fn drain(base_url: String, palace: String, mut rx: mpsc::Receiver<QueuedTurn>) {
    while let Some(turn) = rx.recv().await {
        write_turn(&base_url, &palace, &turn).await;
    }
}

/// Dual-write one turn: `chat_turn_append` (the exact chronological record)
/// THEN `memory_remember` (the semantic recall surface, #2348).
///
/// Why: the exact and semantic representations are independent trusty-memory
/// endpoints; a mid-outage failure of one must not block the other, so each
/// call's error is handled separately rather than short-circuiting on the
/// first failure.
/// What: never propagates an error — every failure is logged via
/// `tracing::warn!` and swallowed, matching
/// `resolve_memory_base_url_or_unreachable`'s fail-open contract (mirrored
/// here, not reused directly, since `base_url` is already resolved by the
/// caller of [`TurnMemorySink::new`]).
/// Test: `memory_sink::tests::enqueue_drain_happy_path`,
/// `memory_sink::tests::write_turn_is_fail_open_on_unreachable_daemon`.
async fn write_turn(base_url: &str, palace: &str, turn: &QueuedTurn) {
    let append_params = json!({
        "palace": palace,
        "session_id": turn.session_id,
        "prompt": turn.prompt,
        "response": turn.response,
    });
    if let Err(e) = call_memory_tool_at(base_url, "chat_turn_append", append_params).await {
        warn!(
            session_id = %turn.session_id,
            error = %e,
            "turn_recorder: chat_turn_append failed (fail-open)"
        );
    }

    let remember_params = json!({
        "palace": palace,
        "text": format!("User: {}\n\nAssistant: {}", turn.prompt, turn.response),
        "tags": [format!("session:{}", turn.session_id), "turn"],
    });
    if let Err(e) = call_memory_tool_at(base_url, "memory_remember", remember_params).await {
        warn!(
            session_id = %turn.session_id,
            error = %e,
            "turn_recorder: memory_remember failed (fail-open)"
        );
    }
}

/// Derive the palace id for a project directory (#2345).
///
/// Why: mirrors `trusty_common::catchup`'s own (private-to-that-module)
/// `derive_palace_id_for` convention exactly, so a session's turns land in
/// the SAME palace `catchup::pm_catchup_context` reads its digest from — the
/// PM's own catch-up section and the turn recorder's writes must agree on
/// "which palace is this project."
/// What: probes `git config --get remote.origin.url` from `project_dir`,
/// then calls `trusty_common::derive_palace_id` (explicit override env ->
/// git owner/repo slug -> parent/dir slug), falling back to the directory's
/// basename (or `"unknown-project"`) when all three yield `None`.
/// Test: `memory_sink::tests::derive_palace_id_for_project_falls_back_to_dirname`.
pub fn derive_palace_id_for_project(project_dir: &Path) -> String {
    let remote = std::process::Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());

    let override_val = trusty_common::palace_override_from_env();
    trusty_common::derive_palace_id(project_dir, remote.as_deref(), override_val.as_deref())
        .unwrap_or_else(|| {
            project_dir
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown-project")
                .to_string()
        })
}

#[cfg(test)]
#[path = "memory_sink_tests.rs"]
mod tests;
