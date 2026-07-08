//! Opt-in wire-level LLM round-trip capture for orchestration debugging
//! (#2264).
//!
//! Why: `run_task::recorder::RecordingLlmClient` already produces
//! `tcode_report.json`'s lightweight transcript (role, assistant text, tool
//! call NAMES, usage) — enough to render a human report, but not enough to
//! root-cause WHY the model made a given decision. Debugging turn-count
//! spikes or re-delegation loops needs the full wire-level exchange: the
//! entire system prompt, tool schemas, and message history actually sent
//! (including prompt-cache markers), the full assistant response (text AND
//! every tool-call's arguments, not just its name), and — via the next
//! request's own message history — the tool results fed back. This module
//! is that capture, gated behind `TCODE_DEBUG_TRANSCRIPT` so it costs
//! nothing when unset.
//! What: [`DebugCaptureSink`] resolves the env var once per run and owns the
//! output file + a monotonic turn counter shared across every wrapped
//! client for that run; [`DebugCaptureLlmClient`] is a transparent
//! `LlmClientTrait` decorator that appends one JSONL [`DebugCaptureRecord`]
//! per `chat()` call (request verbatim, response verbatim including
//! tool-call arguments, or the error, plus usage) before forwarding to the
//! wrapped inner client; [`wrap_with_debug_capture`] is the single call-site
//! helper — when no sink is configured it returns `inner` UNCHANGED (no
//! allocation, no indirection), which is what makes the default (unset)
//! path zero-overhead. Wrapping happens at the `LlmClientTrait::chat`
//! boundary — the same object-safe seam `DispatchingLlmClient` (OpenRouter +
//! Bedrock) and `RecordingLlmClient` already share across both the
//! in-process CLI path (`run_task::execute_run_task`) and the daemon path
//! (`task::executor::run_and_record`) — so this is the ONE place capture
//! logic lives, never duplicated per-transport or per-run-path.
//!
//! SAFETY / SENSITIVITY NOTE: a capture file is a byte-for-byte dump of every
//! prompt and response in a run — it WILL contain the full task description,
//! any file contents the model read or wrote, and full tool-call arguments
//! (e.g. `write_file` bodies). Treat capture files like source code / build
//! logs, not something to paste into a chat or share casually. This module
//! never serialises transport/auth headers or API keys — only the
//! already-constructed `ChatRequest`/`ChatResponse` BODY types, which never
//! carry credentials (see `llm::client::LlmClient::chat`, which injects the
//! `Authorization` header separately, after the body is built).
//! Test: `debug_capture::tests::*` — env-unset is a no-op (zero behaviour
//! change), env-set produces a JSONL record with the full request messages,
//! tool-call arguments, and tool results (visible in the NEXT turn's message
//! history), directory-mode picks a fresh per-run file, and a failed inner
//! `chat()` call is still recorded (with `error` set, `response: null`).

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Serialize;

use super::{ChatRequest, ChatResponse, LlmClientTrait, LlmError};
use crate::perf::TokenUsage;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

/// Environment variable that turns capture on: a file path (records are
/// appended) or a directory path (one fresh file per run).
///
/// Why: opt-in via env var (not a CLI flag) is the primary/required
/// mechanism per #2264 — the bake-off runner sets env vars, not flags, and
/// it must work identically whether `run-task` executes in-process
/// (`--legacy-in-process`) or spawns the daemon subprocess (the default thin
/// client) — a spawned child inherits the parent's environment automatically
/// (`cli_client::stdio::StdioRpcClient::spawn` never clears it), so setting
/// this once in the invoking shell covers every execution path with no
/// extra plumbing.
/// What: read once per run via [`DebugCaptureSink::from_env`].
pub const ENV_VAR: &str = "TCODE_DEBUG_TRANSCRIPT";

/// One recorded LLM round-trip: the full request, the full response (or the
/// error), and per-turn metadata.
///
/// Why: This is the shape that closes the `tcode_report.json` gap (#2264) —
/// unlike `run_task::recorder::TurnRecord` (role + text + tool-call NAMES),
/// this carries the entire `ChatRequest` (system prompt, tool schemas, full
/// message history, cache_control markers) and the entire `ChatResponse`
/// (assistant text AND every tool-call's arguments), so a reader can see
/// exactly what the model was shown and exactly what it decided, turn by
/// turn.
/// What: `turn_index` is a monotonic counter shared by every role in one run
/// (so interleaved pm/engineer turns stay globally ordered); `role` is the
/// delegation segment label (`"pm"` or the engineer's agent name, e.g.
/// `"python-engineer"` — this crate delegates to exactly one engineer role
/// per run today, so `role` doubles as the segment identifier); `request` is
/// the verbatim `ChatRequest` serialised with its normal wire `Serialize`
/// impl (so `ChatMessage`'s cache_control content-block shape round-trips
/// exactly as sent); `response` is `Some` on success, `None` on failure (in
/// which case `error` carries the `LlmError`'s `Display` text); `usage` is
/// the zero value on failure.
/// Test: `debug_capture::tests::record_serialises_full_request_and_response`.
#[derive(Debug, Clone, Serialize)]
struct DebugCaptureRecord<'a> {
    turn_index: u64,
    role: &'a str,
    timestamp: String,
    request: &'a ChatRequest,
    response: Option<ChatResponse>,
    error: Option<String>,
    usage: TokenUsage,
}

/// Shared, thread-safe sink for one run's debug-capture output.
///
/// Why: a directory-mode capture must produce ONE file per run shared by
/// BOTH the pm and engineer recorders (mirroring
/// `run_task::recorder::SharedTranscript`'s "one shared accumulator per
/// run" shape) so the file's turn order reflects the true interleaving of
/// pm/engineer calls, not two disjoint per-role files.
/// What: Wraps a `Mutex<File>` (append-mode) and an `AtomicU64` turn
/// counter. Constructed once per run via [`Self::from_env`] and cloned
/// (`Arc`) into every [`DebugCaptureLlmClient`] wrapping that run's clients.
/// Test: `debug_capture::tests::*`.
pub struct DebugCaptureSink {
    file: Mutex<std::fs::File>,
    turn_counter: AtomicU64,
}

impl DebugCaptureSink {
    /// Resolve [`ENV_VAR`] and open (or create) the capture file, if set.
    ///
    /// Why: the single per-run construction point every call site
    /// (`run_task::execute_run_task`, `task::executor::run_and_record`)
    /// invokes exactly once; a capture failure (bad path, permissions) must
    /// degrade the run to "no capture" rather than abort it — this is
    /// debugging tooling, not a correctness-critical dependency.
    /// What: `None` when [`ENV_VAR`] is unset or empty (the zero-overhead
    /// default). Otherwise resolves the path (directory → fresh per-run
    /// file; anything else → literal file, appended) and opens it,
    /// returning `Some(Arc<Self>)` on success or `None` (after printing an
    /// actionable stderr warning — never stdout, which must stay clean for
    /// MCP JSON-RPC framing) if the file could not be opened.
    /// Test: `debug_capture::tests::from_env_none_when_unset`,
    /// `debug_capture::tests::from_env_opens_literal_file_path`,
    /// `debug_capture::tests::from_env_directory_gets_fresh_per_run_file`.
    pub fn from_env() -> Option<Arc<Self>> {
        let raw = std::env::var(ENV_VAR).ok().filter(|s| !s.is_empty())?;
        match Self::open(Path::new(&raw)) {
            Ok(sink) => Some(Arc::new(sink)),
            Err(e) => {
                eprintln!(
                    "tcode: {ENV_VAR}='{raw}' but the debug-capture file could not be opened \
                     ({e}) — continuing without capture"
                );
                None
            }
        }
    }

    /// Resolve `raw` to a concrete file path and open it for appending.
    ///
    /// Why: split from `from_env` so path resolution is independently
    /// testable without touching the process environment.
    /// What: creates any missing parent directories, then opens
    /// (create-if-missing, append) the resolved path.
    fn open(raw: &Path) -> std::io::Result<Self> {
        let path = resolve_capture_path(raw);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            file: Mutex::new(file),
            turn_counter: AtomicU64::new(0),
        })
    }

    /// Claim the next monotonic turn index for this run.
    ///
    /// Why: `fetch_add` under one shared counter is what keeps pm/engineer
    /// turns in one globally-ordered sequence, matching the ORDER they were
    /// actually dispatched (not just the order each role's own calls ran
    /// in).
    /// What: Returns the previous counter value, then increments.
    fn next_turn_index(&self) -> u64 {
        self.turn_counter.fetch_add(1, Ordering::SeqCst)
    }

    /// Serialise and append one round-trip record.
    ///
    /// Why: centralises the "build the record, serialise, append a line"
    /// sequence so [`DebugCaptureLlmClient::chat`] stays a thin dispatch
    /// wrapper; failures here (serialisation, disk I/O) are logged to
    /// stderr and otherwise swallowed — a debug-capture write must never
    /// fail the underlying LLM call it is observing.
    /// What: builds a [`DebugCaptureRecord`] from `req` and `result`
    /// (cloning the `ChatResponse` on success; recording `LlmError`'s
    /// `Display` text on failure), serialises it to one JSON line, and
    /// appends it (with a trailing newline) to the sink's file under its
    /// mutex.
    fn record(
        &self,
        turn_index: u64,
        role: &str,
        req: &ChatRequest,
        result: &Result<ChatResponse, LlmError>,
    ) {
        let (response, error, usage) = match result {
            Ok(resp) => (Some(resp.clone()), None, resp.clone().token_usage()),
            Err(e) => (None, Some(e.to_string()), TokenUsage::default()),
        };
        let record = DebugCaptureRecord {
            turn_index,
            role,
            timestamp: chrono::Utc::now().to_rfc3339(),
            request: req,
            response,
            error,
            usage,
        };
        let line = match serde_json::to_string(&record) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("tcode: debug-capture record serialisation failed: {e}");
                return;
            }
        };
        // A poisoned lock must not abort the run; recover the inner file
        // handle and keep writing (mirrors `RecordingLlmClient`'s "a
        // poisoned lock is a no-op record, not a panic" convention).
        let mut guard = match self.file.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(e) = writeln!(guard, "{line}") {
            eprintln!("tcode: debug-capture write failed: {e}");
        }
    }
}

/// Resolve the configured [`ENV_VAR`] value to a concrete capture file path.
///
/// Why: the env var doubles as either a literal file (append across the
/// whole process lifetime) or a directory (one fresh file per run) per
/// #2264's requirements; this is the single place that distinction is made.
/// What: `raw` is treated as a directory when it already exists as one, OR
/// its string form ends with the platform path separator (so a caller can
/// force directory semantics for a not-yet-created directory, e.g.
/// `TCODE_DEBUG_TRANSCRIPT=/tmp/captures/`). In that case returns
/// `raw.join("tcode-debug-<uuid>.jsonl")`; otherwise returns `raw` unchanged
/// (literal file path).
/// Test: `debug_capture::tests::resolve_capture_path_*`.
fn resolve_capture_path(raw: &Path) -> PathBuf {
    let looks_like_dir = raw.is_dir() || raw.to_string_lossy().ends_with(std::path::MAIN_SEPARATOR);
    if looks_like_dir {
        raw.join(format!("tcode-debug-{}.jsonl", uuid::Uuid::new_v4()))
    } else {
        raw.to_path_buf()
    }
}

/// A transparent `LlmClientTrait` decorator that appends a full wire-level
/// record of each round-trip to a shared [`DebugCaptureSink`].
///
/// Why: the same "wrap, observe, forward unchanged" shape
/// `run_task::recorder::RecordingLlmClient` already uses — this decorator
/// sits at the identical `LlmClientTrait::chat` seam, so it is transport-
/// and run-path-agnostic by construction (whatever `inner` resolves to —
/// `DispatchingLlmClient` routing to OpenRouter or Bedrock, or a test mock —
/// is invisible to this wrapper).
/// What: Holds the inner client, a role label, and the shared sink. `chat`
/// calls the inner client, records the request/response(or error) pair
/// under the next turn index, and returns the inner result unchanged
/// (including on error — the caller sees the exact same `Result` it would
/// have without capture enabled).
/// Test: `debug_capture::tests::*`.
struct DebugCaptureLlmClient {
    inner: Arc<dyn LlmClientTrait>,
    role: String,
    sink: Arc<DebugCaptureSink>,
}

#[async_trait]
impl LlmClientTrait for DebugCaptureLlmClient {
    async fn chat(&self, req: &ChatRequest) -> Result<ChatResponse, LlmError> {
        let turn_index = self.sink.next_turn_index();
        let result = self.inner.chat(req).await;
        self.sink.record(turn_index, &self.role, req, &result);
        result
    }
}

/// Wrap `inner` in debug capture when `sink` is configured; otherwise return
/// `inner` UNCHANGED.
///
/// Why: this is the single call-site helper `run_task::execute_run_task` and
/// `task::executor::run_and_record` both call (once per role: `"pm"` and the
/// engineer's agent name) — returning `inner` verbatim when capture is off
/// means the disabled path allocates no wrapper and adds no indirection,
/// satisfying #2264's "zero overhead and no behavior change" requirement
/// exactly.
/// What: `Some(sink)` → wraps in a new `DebugCaptureLlmClient` cloning
/// `sink`'s `Arc`; `None` → returns `inner`.
/// Test: `debug_capture::tests::wrap_with_debug_capture_*`.
pub fn wrap_with_debug_capture(
    inner: Arc<dyn LlmClientTrait>,
    role: impl Into<String>,
    sink: Option<&Arc<DebugCaptureSink>>,
) -> Arc<dyn LlmClientTrait> {
    match sink {
        Some(sink) => Arc::new(DebugCaptureLlmClient {
            inner,
            role: role.into(),
            sink: Arc::clone(sink),
        }),
        None => inner,
    }
}
