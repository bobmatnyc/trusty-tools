//! The engine-adapter seam: [`TuiEngine`].
//!
//! Why: `trusty-tui` renders one interaction model for two different
//! products — trusty-code (daemon-backed coding agent) and trusty-agents
//! (tagent's REPL). Rather than fork the TUI per product, the TUI stays
//! engine-agnostic and each product supplies a thin [`TuiEngine`]
//! implementation that translates user input into its own backend calls and
//! backend responses into [`crate::ReplEvent`] values pushed onto the shared
//! channel. See DOC-50 §2.2 for the full architecture rationale.
//!
//! What: this module defines only the trait — no event loop, no rendering.
//! The event loop that drives `handle_input`/`setup`/`shutdown` lands in
//! Slice 2 (#3414).
//!
//! # Spec References
//! - [`SPEC-TTUI-02~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-02~draft) — architecture, the engine-adapter seam.
//! - [`SPEC-TTUI-03~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-03~draft) — Slice 1 trait shape.

use crate::event::ReplEvent;
use anyhow::Result;
use tokio::sync::mpsc::UnboundedSender;

/// Translates one product's backend (tcode daemon, tagent's in-process LLM
/// dispatch) into the shared TUI's vocabulary.
///
/// Why: this is the entire "seam" DOC-50 §2.2 describes — the TUI knows
/// nothing about tcode or tagent, only about `TuiEngine`. Keeping the trait
/// to a handful of methods keeps engine adapters "thin" per the thin-client
/// axiom (DOC-50 §2.1, C-1/C-2): all real work (running the agent,
/// cancelling a session, subscribing to workstream events) happens on the
/// far side of `handle_input`/`cancel_session`/`subscribe_workstream_events`,
/// never inside the TUI itself.
///
/// What: implementors push zero or more [`ReplEvent`] values onto `tx` for
/// every method; the shared event loop (Slice 2+) owns turning those events
/// into screen updates. `cancel_session` and `subscribe_workstream_events`
/// default to no-ops so an engine that doesn't support in-flight cancellation
/// or workstream awareness (e.g. an MVP or test double) needs no boilerplate.
///
/// Thread-safety: `Send + Sync` because the shared event loop dispatches
/// `handle_input` onto a spawned `tokio::task` per submitted line (mirroring
/// `crates/trusty-agents/src/repl/tui/events.rs`), so the engine must be
/// shareable across that task boundary (typically via `Arc<dyn TuiEngine>`).
///
/// # Spec References
/// - [`SPEC-TTUI-02~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-02~draft)
#[async_trait::async_trait]
pub trait TuiEngine: Send + Sync {
    /// Process one submitted line — either free-form chat input or a slash
    /// command the shared router didn't handle client-side.
    ///
    /// Why: this is the single request/response entry point between the TUI
    /// and the product backend; keeping it to one method (vs. exposing many
    /// backend-specific calls) is what lets the TUI stay backend-agnostic.
    /// What: `line` is the raw text the user submitted (echoing to the
    /// scrollback already happened before this is called, mirroring tagent's
    /// existing `process_event` behavior). Implementations push output
    /// (assistant text, tool invocations, status messages, errors) onto `tx`
    /// as [`ReplEvent`] values. Returns `Ok(true)` to keep the REPL running,
    /// `Ok(false)` to quit; an `Err` is surfaced to the user as an error
    /// event by the caller, not propagated as a panic.
    /// Test: exercised via a mock `TuiEngine` in Slice 2+ integration tests
    /// (`crates/trusty-tui/tests/`); no runtime behavior to unit-test yet in
    /// Slice 1.
    async fn handle_input(&self, line: String, tx: UnboundedSender<ReplEvent>) -> Result<bool>;

    /// Load initial state (session id, model, workstream, roster, …) before
    /// the render loop starts.
    ///
    /// Why: the REPL needs to show a populated status line and scrollback on
    /// first frame rather than blank state that fills in asynchronously with
    /// a visible flash.
    /// What: implementations push whatever startup [`ReplEvent`] values are
    /// relevant (e.g. `WorkstreamUpdated`, `StatusMessage`) onto `tx`. Called
    /// exactly once, before the first render.
    /// Test: see `handle_input`.
    async fn setup(&self, tx: UnboundedSender<ReplEvent>) -> Result<()>;

    /// Cancel the in-flight request started by the most recent
    /// `handle_input` call, if any.
    ///
    /// Why: DOC-50 §5 Slice 5 requires Ctrl-C to relay a real cancellation to
    /// the backend (thin-client axiom C-2 — the backend performs the
    /// cancellation, not just the UI stopping its own render). Defaulting to
    /// a no-op lets engines without a cancellable backend operation (or test
    /// doubles) skip the override.
    /// What: implementations that support cancellation should call through
    /// to their backend's cancel/abort API; the shared event loop calls this
    /// exactly once per Ctrl-C while a request is in flight.
    /// Test: see `handle_input`.
    async fn cancel_session(&self) -> Result<()> {
        Ok(())
    }

    /// Open (or re-open) a subscription to backend-pushed events that aren't
    /// direct responses to `handle_input` — chiefly workstream activation
    /// changes (DOC-48 §5.3).
    ///
    /// Why: workstream awareness (DOC-50 §5 Slice 6) is push-driven — the
    /// daemon can activate a different workstream out-of-band (another
    /// client, another session) and the TUI must reflect that without the
    /// user typing anything. Modeling this as a trait method (rather than a
    /// bespoke SSE client living in the TUI) keeps the thin-client axiom
    /// intact: only the engine adapter knows the transport (SSE over HTTP
    /// for tcode; nothing, for tagent today).
    /// What: implementations that support push events spawn their own
    /// listener and forward `WorkstreamUpdated` / `WorkstreamActivationChanged`
    /// (and, on transport loss, `ConnectionLost`) onto `tx`. The default
    /// no-op is correct for engines with no push transport; the shared event
    /// loop calls this once, after `setup`.
    /// Test: see `handle_input`.
    async fn subscribe_workstream_events(&self, tx: UnboundedSender<ReplEvent>) -> Result<()> {
        let _ = tx;
        Ok(())
    }

    /// Graceful shutdown — close any open backend connections.
    ///
    /// Why: mirrors the original `ReplHandler` contract in
    /// `crates/trusty-agents/src/repl/tui/run.rs`; kept optional since most
    /// engines have nothing to flush.
    /// What: called once, after the render loop exits (`app.quit == true` or
    /// Ctrl-D). Errors are logged, not fatal.
    /// Test: see `handle_input`.
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
