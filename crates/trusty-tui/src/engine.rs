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
//! Slice 2 (#3414). Slice 1.5 (#3413) adds two data-supply accessors,
//! `commands` and `picker`, so the shared crate can render slash-command
//! help and inline pickers from engine-supplied data instead of hardcoded
//! per-product lists (DOC-50 §3.2) — both default to "nothing supplied" so
//! no existing/future `TuiEngine` implementor breaks by not overriding
//! them. Statusline segments do NOT get a parallel accessor here: Slice 1's
//! `ReplEvent::StatuslineUpdate` (pushed from `setup`/
//! `subscribe_workstream_events`) already is the engine-supply mechanism
//! for those; adding a second, pull-based path would just give the shared
//! event loop two sources of truth to reconcile.
//!
//! # Spec References
//! - [`SPEC-TTUI-02~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-02~draft) — architecture, the engine-adapter seam.
//! - [`SPEC-TTUI-03~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-03~draft) — Slice 1 trait shape; §3.2, Slice 1.5 generalization layer.

use crate::event::ReplEvent;
use crate::model::{CommandDescriptor, PickerRequest};
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

    /// Domain slash-command entries this engine contributes (e.g. `/model`,
    /// `/workstream` for `AgentEngine`/`CodeEngine` respectively).
    ///
    /// Why: DOC-50 §6 Q4 (mixed routing) splits slash commands into
    /// client-side built-ins (`/help`, `/clear`, `/quit` — owned by
    /// `trusty-tui` itself, never listed here) and engine-routed domain
    /// commands. This is how `/help` (Slice 7) learns about the latter
    /// without the shared crate hardcoding tagent's or tcode's command set
    /// (replacing tagent's `SLASH_COMMANDS` array,
    /// `crates/trusty-agents/src/repl/tui/helpers.rs:156-181`).
    /// What: returns every [`CommandDescriptor`] this engine handles via
    /// `handle_input`. Entries should use [`crate::model::CommandRouting::Engine`]
    /// (the shared crate does not currently validate this, but a
    /// `BuiltIn`-routed entry here would never be dispatched — built-ins are
    /// intercepted before `handle_input` is called). Default: empty, for
    /// engines/test doubles with no domain commands.
    /// Test: `commands_defaults_to_empty` (this module).
    fn commands(&self) -> Vec<CommandDescriptor> {
        Vec::new()
    }

    /// Look up an engine-supplied picker by name (e.g. `"model"`,
    /// `"provider"`, `"workstream"`) — the data source behind DOC-50 §3.2's
    /// picker generalization.
    ///
    /// Why: generalizes tagent's hardcoded `PickerKind::{Model,Provider}`
    /// (`crates/trusty-agents/src/repl/tui/types.rs:28-35`), where the
    /// items and the Enter-dispatch command are both baked into the REPL
    /// itself. Here, the engine names the picker, supplies its items, and
    /// supplies the dispatch command — see [`PickerRequest`] for the full
    /// contract the shared event loop (Slice 4/7) builds on.
    /// What: `name` identifies which picker was requested (e.g. via a
    /// `/model` invocation reaching `handle_input`, which — instead of
    /// answering inline — can have the shared router call this first).
    /// Returns `None` when the engine has no picker under that name.
    /// Default: always `None`, for engines with no pickers yet.
    /// Test: `picker_defaults_to_none` (this module).
    fn picker(&self, name: &str) -> Option<PickerRequest> {
        let _ = name;
        None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CommandRouting;

    /// A minimal `TuiEngine` implementing only the two required methods —
    /// stands in for a test double or an MVP engine that hasn't grown
    /// domain commands/pickers yet.
    struct BareEngine;

    #[async_trait::async_trait]
    impl TuiEngine for BareEngine {
        async fn handle_input(
            &self,
            _line: String,
            _tx: UnboundedSender<ReplEvent>,
        ) -> Result<bool> {
            Ok(true)
        }

        async fn setup(&self, _tx: UnboundedSender<ReplEvent>) -> Result<()> {
            Ok(())
        }
    }

    /// An engine that overrides `commands`/`picker` — stands in for
    /// `AgentEngine`/`CodeEngine` once Slice 10/3 land, proving the trait
    /// defaults are genuinely overridable (not accidentally `sealed` or
    /// `final` via the macro expansion).
    struct RichEngine;

    #[async_trait::async_trait]
    impl TuiEngine for RichEngine {
        async fn handle_input(
            &self,
            _line: String,
            _tx: UnboundedSender<ReplEvent>,
        ) -> Result<bool> {
            Ok(true)
        }

        async fn setup(&self, _tx: UnboundedSender<ReplEvent>) -> Result<()> {
            Ok(())
        }

        fn commands(&self) -> Vec<CommandDescriptor> {
            vec![CommandDescriptor {
                name: "workstream".to_string(),
                summary: "List or activate a workstream".to_string(),
                routing: CommandRouting::Engine,
                args_hint: Some("activate <id>".to_string()),
            }]
        }

        fn picker(&self, name: &str) -> Option<PickerRequest> {
            (name == "model").then(|| PickerRequest {
                title: "Select a model".to_string(),
                items: Vec::new(),
                dispatch_command: "/model".to_string(),
            })
        }
    }

    /// An engine that implements only the required methods must still
    /// compile and yield "nothing supplied" for both new accessors — the
    /// whole point of giving them default bodies (existing/future
    /// implementors don't break).
    #[test]
    fn commands_and_picker_default_to_nothing_supplied() {
        let engine = BareEngine;
        assert!(engine.commands().is_empty());
        assert!(engine.picker("model").is_none());
        assert!(engine.picker("anything").is_none());
    }

    /// An engine that DOES override `commands`/`picker` must have its
    /// overrides take effect (not silently fall back to the trait default).
    #[test]
    fn commands_and_picker_overrides_take_effect() {
        let engine = RichEngine;

        let commands = engine.commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "workstream");
        assert_eq!(commands[0].routing, CommandRouting::Engine);

        let picker = engine.picker("model").expect("model picker present");
        assert_eq!(picker.title, "Select a model");
        assert_eq!(picker.dispatch_command, "/model");
        assert!(engine.picker("provider").is_none());
    }
}
