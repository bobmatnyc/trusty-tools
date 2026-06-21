//! The session-control seam the SM drives its fleet through (DOC-14 §2.6).
//!
//! Why: SM-8's delegation loop (§3.4) and the SM-STDIO adapter (§1A.1) both need
//! to drive managed sessions — launch, observe, send, stop — WITHOUT carrying
//! session-lifecycle business logic and WITHOUT a real tmux/workspace in tests.
//! Hiding that surface behind a narrow trait (Dependency Inversion) lets the
//! delegation loop and the dispatcher depend on the trait while production wires
//! the real `DaemonSessionControl` (in `daemon::sm_stdio`) and tests inject a
//! deterministic mock. SM-STDIO originally defined this trait under
//! `daemon::sm_stdio`; SM-8 RELOCATES it here (a feature-independent core
//! location) so the agent-side delegation loop can depend on it too, with
//! `sm_stdio` re-exporting it so its public surface is unchanged.
//! What: [`LaunchParams`] (the launch inputs), [`SessionControlError`] (the typed
//! failure), and [`SessionControl`] — the seven async verbs the spec's session
//! methods need, each returning a plain `serde_json::Value` (the wire result
//! body) or a [`SessionControlError`]. The production `DaemonSessionControl`
//! impl lives in `daemon::sm_stdio::control` (it needs daemon-internal handles);
//! the agent-side mock lives in `agent/delegate/mock_control.rs`.
//! Test: the dispatcher's `sm.sessions.*` round-trips and the SM-8 delegation
//! tests both inject mock impls of this trait.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::activity::cache::ActivityState;

/// Inputs for `sessions.launch` (§1A.1: `{ workdir, model?, prompt?, goal_id? }`).
///
/// Why: the spec's launch params differ from the managed-session `SpawnParams`
/// shape (`{ repo_url, ref, task }`); keeping the adapter's own owned input
/// struct lets callers (the dispatcher AND the SM-8 delegation loop) parse the
/// spec params once and hand them to ANY [`SessionControl`] impl (real or mock)
/// without re-deriving the mapping at the call site.
/// What: `workdir` (the directory/repo the session launches against, mapped to
/// the managed `repo_url`), an optional `model` (runtime selector), an optional
/// `prompt` (the task/intent, mapped to the managed `task`), and an optional
/// `goal_id` the caller wants the new session linked to (§9.3).
/// Test: `tests.rs::launch_round_trips` and `delegate` tests construct this.
#[derive(Debug, Clone)]
pub struct LaunchParams {
    /// Working directory / repository the session launches against.
    pub workdir: String,
    /// Optional model / runtime selector (mapped to the managed `runtime`).
    pub model: Option<String>,
    /// Optional initial prompt / task description (mapped to the managed `task`).
    pub prompt: Option<String>,
    /// Optional goal id to link the launched session to (§9.3).
    pub goal_id: Option<String>,
    /// Whether the launched session is EPHEMERAL (a test/throwaway session) (#1508).
    ///
    /// Why: the chat-driven launch path must be able to mark a session disposable
    /// so the bulk-teardown and age-based reap paths can clean it up. `None`/
    /// `Some(false)` → a normal durable session the automatic paths never touch.
    pub ephemeral: Option<bool>,
}

/// A failure surfaced by a [`SessionControl`] operation.
///
/// Why: callers map these onto JSON-RPC error responses (the dispatcher) or onto
/// goal-link failures (the SM-8 loop); a typed enum lets them choose
/// `INVALID_PARAMS` (bad id) vs `INTERNAL_ERROR` (control failure) by VARIANT
/// rather than string-matching, and keeps the adapter panic-free per the
/// workspace convention.
/// What: [`NotFound`](SessionControlError::NotFound) is reserved for a malformed
/// session id (UUID parse failure) OR a genuinely-absent session
/// (`ManagedError::SessionNotFound`); [`Backend`](SessionControlError::Backend)
/// for any other control-surface failure (provision/tmux/store/invalid-state).
/// Test: `sm_stdio::tests` mock returns both variants; the dispatcher asserts the
/// mapped JSON-RPC code.
#[derive(Debug, thiserror::Error)]
pub enum SessionControlError {
    /// The session id was invalid or not present.
    #[error("session not found: {0}")]
    NotFound(String),

    /// Any backend control failure (provisioning, tmux, store, resume).
    #[error("{0}")]
    Backend(String),
}

/// How an injected text is committed to a session's runtime (#1461).
///
/// Why: the harness-agnostic `inject_text` verb must express the THREE distinct
/// keystroke intents every runtime needs without leaking tmux argv into the
/// trait: "type this line and run it", "type this but DON'T submit" (e.g. stage
/// a partial command, or fill a prompt the human will edit), and "interrupt the
/// running task". Modelling them as one closed enum keeps the seam runtime-neutral
/// — a future tcode/SDK backend maps the same three variants onto its own
/// submit/interrupt primitives.
/// What: [`Enter`](Submit::Enter) sends the literal text then a newline (today's
/// `send_line`); [`NoSubmit`](Submit::NoSubmit) sends the literal text with NO
/// trailing newline; [`Interrupt`](Submit::Interrupt) ignores any text and sends
/// the runtime's interrupt (Ctrl-C in tmux). Serde-friendly (snake_case) so it
/// round-trips through the wire/config.
/// Test: `inject_dispatch_enter_sends_literal_then_enter`,
/// `inject_dispatch_nosubmit_sends_literal_only`,
/// `inject_dispatch_interrupt_sends_ctrl_c` in tests/session_control_api.rs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Submit {
    /// Send the literal text, then press Enter — the runtime executes the line.
    Enter,
    /// Send the literal text WITHOUT a trailing Enter — leaves it unsubmitted.
    NoSubmit,
    /// Ignore the text and send the runtime's interrupt (Ctrl-C in tmux).
    Interrupt,
}

/// A raw, LLM-free observation of a session's current surface (#1461).
///
/// Why: `observe` must ALWAYS be available — even on a host with no
/// `OPENROUTER_API_KEY` — so any harness can read a session's actual pane and
/// make its OWN inference (the session-manager-driver does exactly this). It is
/// the cheap, deterministic counterpart to [`Summary`]: no tokens, no network.
/// What: the captured `raw_pane` (last N lines), `runtime_active` (is the
/// runtime process still live in the pane?), and the two escalation fields the
/// managed surface already tracks — `pending_decision` (a question awaiting a
/// human) and its `proposed_default` answer.
/// Test: `observe_returns_raw_pane_without_llm`,
/// `observe_reports_runtime_active` in tests/session_control_api.rs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawObservation {
    /// The captured pane text (the last `lines` rows requested).
    pub raw_pane: String,
    /// True when the session's runtime is still live (tmux session exists).
    pub runtime_active: bool,
    /// A pending decision the session is blocked on, if any (awaiting a human).
    pub pending_decision: Option<String>,
    /// The conformant default answer proposed for `pending_decision`, if any.
    pub proposed_default: Option<String>,
}

/// An optional, LLM-derived summary of a session's activity state (#1461).
///
/// Why: `summarize` is the OPTIONAL, inference-backed counterpart to `observe`.
/// It MUST degrade gracefully: on a host with no `OPENROUTER_API_KEY` it returns
/// `state = Unknown` and `summary = None` rather than erroring, preserving the
/// LLM-optional invariant the activity monitor already guarantees.
/// What: reuses the existing [`ActivityState`] enum (NOT a duplicate) for the
/// classified `state`; `summary` is the human-readable note (None when no
/// genuine verdict was produced — i.e. degraded/unconfigured); `confidence` is
/// the classifier's `[0.0, 1.0]` score (0.0 when degraded).
/// Test: `summarize_degrades_to_unknown_without_key`,
/// `summarize_returns_fake_verdict`, `summarize_serves_cached_verdict` in
/// tests/session_control_api.rs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    /// The classified activity state (`Unknown` when inference is unavailable).
    pub state: ActivityState,
    /// The human-readable summary, or `None` when no genuine verdict exists.
    pub summary: Option<String>,
    /// Classifier confidence in `[0.0, 1.0]` (0.0 when degraded to `Unknown`).
    pub confidence: f32,
}

/// The narrow session-control surface the SM drives its fleet through (§2.6).
///
/// Why: keeps both `sm.sessions.*` (a thin translation) AND the SM-8 delegation
/// loop decoupled from the concrete managed-session surface. Callers depend on
/// this trait (Dependency Inversion) so production wires `DaemonSessionControl`
/// (the real managed-session surface) while tests wire a deterministic mock with
/// NO tmux/workspace. Every method returns the wire `result` body directly so the
/// dispatcher does no shaping.
/// What: `launch` → `{ session_id }`; `list` → `{ sessions: [...] }`; `get` →
/// the full record JSON; `send` → `{ ok }`; `stop`/`resume`/`kill` → `{ ok }`.
/// All are `Send + Sync` to live behind `Arc<dyn SessionControl>`.
/// Test: `sm_stdio::tests` mock; the SM-8 `delegate` tests mock; the real
/// `DaemonSessionControl` via the managed integration tests.
#[async_trait]
pub trait SessionControl: Send + Sync {
    /// Launch a managed session (`POST /sessions`, §2.6) and return its id.
    async fn launch(&self, params: LaunchParams) -> Result<serde_json::Value, SessionControlError>;

    /// List all managed sessions (`GET /sessions`, §2.6).
    async fn list(&self) -> Result<serde_json::Value, SessionControlError>;

    /// Get one session record (`GET /sessions/{id}`, §2.6).
    async fn get(&self, session_id: &str) -> Result<serde_json::Value, SessionControlError>;

    /// Send text into a session's pane (`POST /sessions/{id}/command`, §2.6).
    async fn send(
        &self,
        session_id: &str,
        text: &str,
    ) -> Result<serde_json::Value, SessionControlError>;

    /// Stop a session's runtime, keeping the workspace (`DELETE /sessions/{id}`).
    async fn stop(&self, session_id: &str) -> Result<serde_json::Value, SessionControlError>;

    /// Resume a stopped session (`POST /sessions/{id}/resume`, §2.6).
    async fn resume(&self, session_id: &str) -> Result<serde_json::Value, SessionControlError>;

    /// Force-stop / reap a session (`DELETE /sessions/{id}`, §2.6).
    async fn kill(&self, session_id: &str) -> Result<serde_json::Value, SessionControlError>;

    /// Inject `text` into a session, committed per `submit` (#1461).
    ///
    /// Why: the standard harness-agnostic write verb. Where [`send`](Self::send)
    /// returns the wire `{ ok }` body and is fixed to "type + Enter", this
    /// expresses ALL THREE keystroke intents ([`Submit::Enter`] /
    /// [`Submit::NoSubmit`] / [`Submit::Interrupt`]) through one seam so any
    /// runtime (tmux today, tcode/SDK later) is driven the same way.
    /// What: keyed by the opaque `session_id`; dispatches on `submit` to the
    /// runtime's literal-then-Enter, literal-only, or interrupt primitive.
    /// Returns `()` on success (no wire body to shape).
    /// Test: the inject-dispatch tests in tests/session_control_api.rs.
    async fn inject_text(
        &self,
        session_id: &str,
        text: &str,
        submit: Submit,
    ) -> Result<(), SessionControlError>;

    /// Observe a session's raw surface — ALWAYS available, NO LLM (#1461).
    ///
    /// Why: every harness needs to read what a session is actually showing
    /// WITHOUT depending on an LLM key; this is the deterministic, free read the
    /// driver makes its own inference from.
    /// What: captures the last `lines` pane rows, probes `runtime_active`, and
    /// surfaces any `pending_decision` / `proposed_default`. Never calls the LLM.
    /// Test: `observe_returns_raw_pane_without_llm`, `observe_reports_runtime_active`.
    async fn observe(
        &self,
        session_id: &str,
        lines: usize,
    ) -> Result<RawObservation, SessionControlError>;

    /// Summarize a session via the activity monitor — LLM-OPTIONAL (#1461).
    ///
    /// Why: the optional inference-backed counterpart to [`observe`](Self::observe).
    /// It MUST degrade to `state = Unknown` / `summary = None` (never error) when
    /// no `OPENROUTER_API_KEY` is configured, and serve a cached verdict when the
    /// pane content is unchanged (content-hash hit) so repeated polls are cheap.
    /// What: captures the pane, runs it through the shared
    /// `ActivityMonitor::check`, and maps the verdict onto a [`Summary`].
    /// Test: `summarize_degrades_to_unknown_without_key`,
    /// `summarize_returns_fake_verdict`, `summarize_serves_cached_verdict`.
    async fn summarize(&self, session_id: &str) -> Result<Summary, SessionControlError>;

    /// Adopt an EXISTING unmanaged tmux session into the managed store (#1433).
    ///
    /// Why: the action-capable coordinator chat (#1496) and the SM-STDIO surface
    /// need to CONNECT to a pane the operator already has (a hand-started session,
    /// an externally-created one, or one whose record was lost) and drive it
    /// through the rest of this trait. It is the explicit counterpart to the
    /// automatic boot-time adoption in
    /// [`reconcile_on_boot`](crate::session_manager::SessionManager::reconcile_on_boot).
    /// What: connects to the live pane named `tmux_name`, registering an `Active`
    /// record with the supplied `cwd` (REQUIRED — provenance is unknown), an
    /// optional `task`, and an optional `runtime` selector. Returns `{ session_id }`
    /// on success.
    ///
    /// A DEFAULT impl returns [`SessionControlError::Backend`] so the only impls
    /// that gain the verb are those that override it (the production
    /// `DaemonSessionControl`); test mocks and any other impls keep compiling
    /// unchanged without silently pretending to adopt.
    /// Test: `DaemonSessionControl::adopt` via the managed integration tests; the
    /// action-loop dispatch via `chat_action_tests.rs::execute_adopt_*`.
    async fn adopt(
        &self,
        tmux_name: &str,
        cwd: &str,
        task: Option<&str>,
        runtime: Option<&str>,
    ) -> Result<serde_json::Value, SessionControlError> {
        let _ = (tmux_name, cwd, task, runtime);
        Err(SessionControlError::Backend(
            "adopt is not supported by this control surface".to_string(),
        ))
    }

    /// Full terminal teardown of a session (#1524): kill runtime + tombstone record.
    ///
    /// Why: `sessions.decommission` is the action-loop's explicit "tear everything
    /// down" verb — semantically stronger than `stop` (which keeps the workspace
    /// resumable) and synonymous with the MCP `session_decommission` tool. A
    /// DEFAULT impl delegates to [`kill`](Self::kill) so existing impls gain the
    /// verb without change; only the production `DaemonSessionControl` (which
    /// already calls `mgr.decommission` from `kill`) benefits.
    /// What: delegates to `self.kill(session_id)`, which already calls
    /// `SessionManager::decommission` in production. Returns `{ ok: true }`.
    /// Test: `chat_action_tests.rs::execute_decommission_dispatches`.
    async fn decommission(
        &self,
        session_id: &str,
    ) -> Result<serde_json::Value, SessionControlError> {
        // The production DaemonSessionControl::kill calls mgr.decommission, so
        // this default is semantically correct for all real impls.
        self.kill(session_id).await
    }
}
