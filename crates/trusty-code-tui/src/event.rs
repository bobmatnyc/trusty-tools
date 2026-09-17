//! The shared event vocabulary: [`ReplEvent`] and its small payload types.
//!
//! Why: both `TuiEngine` implementations (tagent's `AgentEngine`, tcode's
//! `CodeEngine`) and the shared event loop (Slice 2+) need one common
//! language for "things that happened" — a key press, a chunk of streamed
//! assistant output, a tool call, a workstream change. `ReplEvent` is that
//! language; generalizing it from tagent's existing (tagent-specific)
//! `ReplEvent` in `crates/trusty-agents/src/repl/tui/types.rs` is the whole
//! point of the extraction (DOC-50 §2.3, §3.2).
//!
//! What: this module defines the enum and the handful of payload types it
//! needs. It intentionally does NOT depend on `crossterm` — [`KeyInput`] is
//! trusty-code-tui's own minimal key representation; Slice 2 (#3414) adds the
//! crossterm-backed terminal layer that translates real `crossterm::event::KeyEvent`
//! values into this type at the boundary, keeping `ReplEvent` itself
//! terminal-library-agnostic.
//!
//! # Spec References
//! - [`SPEC-TTUI-03~draft`](docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-03~draft) — Slice 1 `ReplEvent` deliverable (§5, Slice 1).
//! - [`SPEC-TTUI-05~draft`](docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — per-variant slice ownership (Slices 5/6/8/9).

use crate::model::{PendingPermission, StatuslineSegment};
use serde::{Deserialize, Serialize};

/// Every event that can flow through the shared TUI's event channel.
///
/// Why: one `mpsc::UnboundedSender<ReplEvent>` is threaded through the whole
/// stack (engine adapters, the key-reader task, the render loop), mirroring
/// tagent's proven design in `crates/trusty-agents/src/repl/tui/run.rs`. A
/// single enum keeps that channel typed and keeps `TuiEngine` implementors
/// from needing bespoke channels per concern.
/// What: variants are grouped below by who produces them. Terminal-origin
/// variants (`Key`, `Resize`, `Scroll`) are produced by the Slice 2 terminal
/// layer; engine-origin variants (`AssistantOutput` and later) are produced
/// by `TuiEngine` implementations; `Submit` is synthesized by the shared
/// event loop itself (echoed input, picker selection, etc. — see tagent's
/// `process_event` for the precedent). The enum is exhaustive per DOC-50 §5
/// Slice 1's acceptance criterion ("`ReplEvent` enum covers all expected
/// event types"); new variants are additive as later slices land (tool
/// cards in Slice 8, permission prompts in Slice 9).
///
/// # Spec References
/// - [`SPEC-TTUI-05~draft`](docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft)
#[derive(Debug, Clone, PartialEq)]
pub enum ReplEvent {
    // ── Terminal-origin (Slice 2, #3414, wires the producer) ──────────
    /// Raw keyboard input from the terminal's key-reader task.
    Key(KeyInput),
    /// The terminal was resized to `(cols, rows)`.
    Resize(u16, u16),
    /// Mouse-wheel scroll delta. Negative scrolls toward older history
    /// (up), positive toward newer (down) — same convention as tagent's
    /// `ReplEvent::Scroll`.
    Scroll(isize),

    // ── User-input dispatch (shared event loop) ────────────────────────
    /// A line was submitted for the engine to process — either typed and
    /// confirmed with Enter, or synthesized (e.g. a picker selection
    /// resolving to a slash command). Consumed by `TuiEngine::handle_input`.
    Submit(String),
    /// The user requested cancellation of the in-flight request (Ctrl-C).
    /// The shared event loop relays this to `TuiEngine::cancel_session`
    /// (DOC-50 §5 Slice 5) rather than only clearing local UI state, per the
    /// thin-client axiom (C-2).
    Cancel,
    /// The engine's `handle_input` returned `Ok(false)` (DOC-50 §5 Slice 5) —
    /// synthesized by `crate::run::run`'s dispatch step, never by a key press
    /// directly (Ctrl-D sets `ReplApp::quit` straight from the reducer since
    /// it needs no round-trip through the engine). Exists because the
    /// dispatch step only holds `&mut M` synchronously inside `apply`; the
    /// spawned `handle_input` task that later learns "the engine wants to
    /// quit" can reach the model only by pushing an event back onto the
    /// shared channel, same as every other engine-originated signal here.
    Quit,
    /// A submitted turn's `TuiEngine::handle_input` call has returned, and
    /// `crate::run::dispatch_pending` determined no terminal signal
    /// (`AssistantOutput { done: true, .. }` or `Quit`) was ever relayed for
    /// it — synthesized as a safety net, never by a key press or a
    /// `TuiEngine` implementation directly.
    ///
    /// Why: `ReplApp::busy` is cleared ONLY by a terminal
    /// `AssistantOutput`/`Quit` reaching the reducer — but three real,
    /// spec-required `CodeEngine` paths return `Ok(true)` from
    /// `handle_input` without ever sending one (`/workstream list`/
    /// `activate`, which only push `StatusMessage`/`WorkstreamUpdated`; a
    /// reconnect-exhausted `pump_session_events` return; a daemon-initiated
    /// `SessionCancelled` that only pushes a `StatusMessage`). Combined with
    /// Slice 5's busy-gating (`ReplApp::submit_line` refuses a second turn
    /// while `busy`), any such path left `busy` stuck `true` forever —
    /// input permanently bricked, recoverable only by the accident of
    /// Ctrl-C's `on_cancelled` reset. This variant is the fix: a dedicated,
    /// minimal reset that touches ONLY `busy`/`streaming_idx`, deliberately
    /// NOT reusing an empty `AssistantOutput { done: true, .. }` for this
    /// (that would push a stray blank chat entry via the `None`-
    /// `streaming_idx` branch of `apply_assistant_output` — corrupting
    /// scrollback to fix a different bug).
    ///
    /// **Carries its own `generation`** (the turn number `crate::run::
    /// dispatch_pending` assigned it) so the reducer — not the spawned
    /// completion task — decides whether it's still relevant. A prior
    /// revision had the spawned task load-and-compare the live generation
    /// counter itself before deciding whether to send this event at all;
    /// that compare-then-send was a genuine TOCTOU race under
    /// multi-threaded tokio (a cancel + new submit could both run in the
    /// gap between the load and the send), letting a stale terminal signal
    /// from turn N clear `busy`/`streaming_idx` for a genuinely in-flight
    /// turn N+2. Fixed by construction: the completion task now always
    /// sends this event (stamped with its own generation, no load-then-
    /// branch), and the reducer — which runs serially on the same task that
    /// bumps the generation counter, so no cross-thread race is possible —
    /// applies it only if `generation` still matches
    /// `ReplApp::current_generation`.
    /// What: engine implementations should never construct this directly;
    /// it exists purely as `crate::run::dispatch_pending`'s per-turn
    /// completion safety net (see that function's doc comment).
    TurnFinished { generation: u64 },

    // ── Engine-origin (produced by `TuiEngine` implementations) ────────
    /// A chunk of streamed assistant output. `done` marks the final chunk of
    /// a response so the scrollback can stop showing a "thinking" indicator;
    /// `is_error` marks the text as an error message rather than a normal
    /// response (mirrors tagent's `LlmResponse { text, is_error }`).
    AssistantOutput {
        chunk: String,
        done: bool,
        is_error: bool,
    },
    /// A tool was invoked by the backend agent. `result` is `None` while the
    /// call is in flight and `Some(..)` once it completes. Rendering (text
    /// vs. fancy card) is a Slice 8 (#TBD, Phase 2) concern; the event shape
    /// is defined now so `TuiEngine` implementations don't need a breaking
    /// change later.
    ///
    /// `id` correlates a call's start (`result: None`) with its completion
    /// (`result: Some(..)`) — two separate `ToolInvocation` events sharing
    /// the same `id`. Required because `tool_name` alone can't disambiguate
    /// repeated or parallel calls to the same tool (e.g. two concurrent
    /// `fs.read` calls); the engine adapter mints this id (a UUID or a
    /// backend-supplied call id, whichever it has) and must reuse it across
    /// the start/complete pair.
    ///
    /// `agent_id` (#7940) is the opaque id of the agent that dispatched the
    /// call, matching [`Self::DelegationStarted`]'s `agent_id` when the
    /// caller is a delegated sub-agent. Empty when the engine has no
    /// attribution to offer. It is what lets the reducer render a delegated
    /// agent's tool calls INSIDE that agent's block rather than at the top
    /// level beside the primary agent's own.
    ///
    /// `failed` (#4596) is the backend's own verdict on the call, carried as
    /// data. Producers also write a human marker into `result` (trusty-code
    /// sends `FAILED: …` / `ERROR: …`), but that text is for the reader —
    /// rewording it must never change how the TUI renders the card, which is
    /// why the fact travels in its own field. Meaningless while `result` is
    /// `None`: a call that has not finished has not failed, so a start event
    /// sets `false`. Set `true` ONLY on a completion the backend reported as
    /// unsuccessful.
    ToolInvocation {
        id: String,
        agent_id: String,
        tool_name: String,
        args: serde_json::Value,
        result: Option<String>,
        failed: bool,
    },
    /// A chunk of streamed output attributed to a specific agent turn
    /// (#7940).
    ///
    /// Why: [`Self::AssistantOutput`] carries no attribution and the reducer
    /// accumulates it into ONE unkeyed in-progress bubble, so a primary
    /// agent and a delegated sub-agent streaming at the same time interleave
    /// their words into a single chat entry. This variant carries the
    /// `(agent_id, turn_id)` pair the reducer keys a separate bubble on, so
    /// concurrent streams stay separate by construction.
    /// What: `agent_id` is the opaque per-spawn agent id; `turn_id` is the
    /// producer's own per-turn id. Append every chunk sharing one
    /// `(agent_id, turn_id)` key in arrival order; `done: true` closes that
    /// bubble and no further chunk for the key will arrive. Unlike
    /// `AssistantOutput`, this variant never clears the model's `busy` flag:
    /// one agent's turn ending is not the human turn ending.
    AgentOutput {
        agent_id: String,
        turn_id: String,
        chunk: String,
        done: bool,
    },
    /// A delegated sub-agent started working on a task (#7940).
    ///
    /// Why: delegation is invisible in the scrollback without it — the
    /// sub-agent's output and tool calls arrive indistinguishable from the
    /// primary agent's. This opens a rendered block the sub-agent's activity
    /// hangs under.
    /// What: `agent_id` is an opaque, producer-minted id, unique per spawn
    /// (`agent` alone cannot separate two concurrent delegations of the same
    /// kind). It MAY be empty when the producer announces an intent to
    /// delegate before the sub-agent exists; a later `DelegationStarted`
    /// naming the same `agent` with a non-empty `agent_id` adopts the
    /// already-open block rather than opening a second one. `task` is a
    /// short human summary of what was delegated.
    DelegationStarted {
        agent_id: String,
        agent: String,
        task: String,
    },
    /// A delegated sub-agent's run ended (#7940) — closes the block
    /// [`Self::DelegationStarted`] opened, keyed by the same `agent_id`.
    DelegationFinished {
        agent_id: String,
        agent: String,
        outcome: DelegationOutcome,
    },
    /// A tool call matched an `ask` permission rule and is SUSPENDED on the
    /// backend until this client answers (#3422, wire event from #7948).
    ///
    /// Why: the backend has already stopped the call and is waiting; the TUI
    /// has to say so and collect an answer, because silence resolves as a
    /// deny after the backend's timeout. The reducer opens a modal prompt on
    /// this event — see [`crate::app::ReplApp::pending_permission`].
    /// What: `request_id` is the correlation key the answer goes back on
    /// (NOT a tool-call id — the call has not run). `agent`/`agent_id` are
    /// the same attribution pair [`Self::DelegationStarted`] carries, so a
    /// prompt raised inside a delegation can name its sub-agent. `subject`
    /// is what the tool would act on, ALREADY redacted and length-bounded by
    /// the producer — this crate renders it verbatim and never re-derives it.
    /// `rule` is the matching policy pattern, shown so the user can see why
    /// they are being asked.
    PermissionRequested {
        request_id: String,
        agent: String,
        agent_id: String,
        tool: String,
        subject: String,
        rule: String,
    },
    /// A suspended permission request reached a decision (#3422, wire event
    /// from #7948) — the explicit close for the prompt
    /// [`Self::PermissionRequested`] opened.
    ///
    /// Why: the backend decides, not the TUI (thin-client axiom, ADR-0063):
    /// a request can resolve without this client answering at all (another
    /// client answered, the backend timed out, a remembered grant or an
    /// auto-allow mode covered it). This event — not the local key press —
    /// is what puts the outcome in the scrollback.
    /// What: `decision` and `source` are the producer's own words, rendered
    /// verbatim; this crate parses neither. A `request_id` matching the open
    /// prompt closes it, and one that does not is still recorded.
    PermissionResolved {
        request_id: String,
        agent: String,
        agent_id: String,
        decision: String,
        source: String,
    },
    /// Relaying an answer to `TuiEngine::respond_permission` FAILED (#3422)
    /// — the suspended call never heard it.
    ///
    /// Why: the prompt closes the moment a key is pressed, before the RPC
    /// even starts, so the keyboard is never held hostage by a slow backend.
    /// That optimism is only safe if a failure is reversible: without this
    /// event the operator loses the modal while the backend keeps holding
    /// the call until it times out, with no way to answer again. This is the
    /// undo.
    /// What: carries the original request verbatim, so the reducer can
    /// reopen exactly the prompt that was answered, plus the engine's own
    /// error text for the retry line and the transcript. Emitted ONLY by
    /// `crate::run::dispatch_pending`'s relay task, never by an engine.
    PermissionAnswerFailed {
        pending: PendingPermission,
        error: String,
    },
    /// A one-line status message (e.g. "cancelled", "Switched to: izzie").
    StatusMessage(String),
    /// Clear the scrollback buffer. Emitted by the shared `/clear` built-in
    /// slash command (DOC-50 §5 Slice 7) rather than handled ad hoc by each
    /// engine, so `TuiEngine` implementations never touch scrollback state
    /// directly.
    ClearScrollback,
    /// Engine-supplied statusline segments replacing the current set
    /// (session id, model, project, workstream, …). See
    /// [`StatuslineSegment`] (`crate::model`) for the full, Slice-1.5
    /// segment taxonomy.
    StatuslineUpdate(Vec<StatuslineSegment>),
    /// The active workstream's summary changed (initial load or refetch
    /// after an activation change).
    WorkstreamUpdated(WorkstreamSummary),
    /// The daemon activated a different workstream (DOC-48 §5.3
    /// `WorkstreamActivationChanged`), pushed via
    /// `TuiEngine::subscribe_workstream_events`. `prior_id` is `None` on the
    /// very first activation observed in this session. `new_active_id` is
    /// `None` when the active workstream was deactivated and none is now
    /// active (DOC-48 §4.2/§4.3) — a real, daemon-published state, not an
    /// absence of data; distinct from `prior_id: None`, which means "no
    /// activation happened before this one."
    ///
    /// Field names are chosen to match the DOC-48 §5.3 SSE wire event
    /// exactly (`new_active_id`, `prior_id`) — NOT DOC-50 §5 Slice 6's prose
    /// (`new_id`), which drifted from the actual wire schema. Since engine
    /// adapters deserialize this from JSON (Slice 3/6), a Rust/wire name
    /// mismatch here would not be caught at compile time, only at runtime
    /// deserialization — so the Rust field is kept byte-identical to the
    /// wire field on purpose, including its optionality: the wire event
    /// (`crate::events::Event::WorkstreamActivationChanged` in
    /// `trusty-code`) declares `new_active_id: Option<String>` for exactly
    /// this reason, and this variant now mirrors that shape rather than
    /// forcing a lossy `StatusMessage` fallback for the `None` case.
    WorkstreamActivationChanged {
        new_active_id: Option<String>,
        prior_id: Option<String>,
    },
    /// The backend connection was lost (daemon restart, SSE stream closed,
    /// HTTP timeout/502/503). The TUI shows a "Connection lost" status;
    /// input remains live and the next `handle_input`/
    /// `subscribe_workstream_events` call attempts to reconnect (DOC-50
    /// §2.4).
    ConnectionLost { reason: String },
}

/// How a delegated sub-agent's run ended (#7940) — the payload of
/// [`ReplEvent::DelegationFinished`].
///
/// Why: the block footer has to say more than "over": an engineer that
/// finished and one that blew up look identical otherwise, which is exactly
/// the thing an operator watching a delegation needs to see at a glance.
/// Two variants rather than a `Result<String, String>` so the rendered
/// vocabulary is this crate's own and no caller has to decide what the `Ok`
/// side of a `Result` means here.
/// What: `Finished` carries the producer's own status label (e.g.
/// `"success"`); `Failed` carries the error text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationOutcome {
    /// The sub-agent's loop returned normally; carries the producer's status
    /// label.
    Finished(String),
    /// The sub-agent's loop failed; carries the error text.
    Failed(String),
}

/// Minimal, `crossterm`-independent representation of a single key press.
///
/// Why: `ReplEvent` must not force a `crossterm` dependency onto every
/// `TuiEngine` consumer (only the Slice 2 terminal layer needs to know a
/// terminal library exists at all). This type is the boundary: Slice 2
/// translates `crossterm::event::KeyEvent` into `KeyInput` once, at the
/// key-reader task.
/// What: `code` identifies the key; `modifiers` are the held modifier keys.
/// Deliberately small — only what tagent's existing line editor
/// (`crates/trusty-agents/src/repl/tui/keys.rs`) actually switches on
/// (Ctrl-a/e/u/c/d, arrows, Enter, Backspace, printable chars).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyInput {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

/// The identity of a key, independent of any terminal library's own enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyCode {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Tab,
    Esc,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// A key not yet mapped to a named variant above. Slice 2 can extend
    /// this enum as the line editor grows new bindings; keeping this
    /// fallback avoids a breaking change for every new key tagent's editor
    /// happens to read.
    Other,
}

/// Modifier keys held alongside a [`KeyCode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

/// A minimal summary of a workstream (DOC-48 §2.1), enough for the status
/// line and `/workstream list` output.
///
/// Why: `ReplEvent::WorkstreamUpdated` needs a payload; the full
/// `Workstream` type lives in trusty-code/trusty-agents-common and must not
/// become a `trusty-code-tui` dependency (that would invert DOC-50 §2.2's
/// dependency direction — `trusty-code-tui` depends on nothing product-specific).
/// What: `id` and `name` are the two fields DOC-50 §5 Slice 6's status-line
/// example ("WS: Token rotation (a1b2c3d4)") actually needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkstreamSummary {
    pub id: String,
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ReplEvent` must round-trip through `Clone`/`PartialEq` for the
    /// event-loop tests Slice 2+ will add (asserting "this event was
    /// pushed"); a `Debug`/`Clone`/`PartialEq` derive failing to compile on
    /// any variant would be a stub with unusable payload types.
    #[test]
    fn repl_event_variants_are_cloneable_and_comparable() {
        let a = ReplEvent::StatusMessage("hello".to_string());
        let b = a.clone();
        assert_eq!(a, b);

        let ws = ReplEvent::WorkstreamUpdated(WorkstreamSummary {
            id: "a1b2c3d4".to_string(),
            name: "Token rotation".to_string(),
        });
        assert_eq!(ws.clone(), ws);
    }

    /// Two `ToolInvocation` events sharing the same `id` (a pending call and
    /// its completion) must be distinguishable ONLY by `result`, not by
    /// `id` — that's the whole point of adding the correlation id (Slice 8's
    /// "pending → complete" tool cards match on it, not on `tool_name`,
    /// which breaks for repeated/parallel calls to the same tool).
    #[test]
    fn tool_invocation_start_and_complete_share_a_correlation_id() {
        let start = ReplEvent::ToolInvocation {
            id: "call-1".to_string(),
            agent_id: String::new(),
            tool_name: "fs.read".to_string(),
            args: serde_json::json!({"path": "a.txt"}),
            result: None,
            failed: false,
        };
        let complete = ReplEvent::ToolInvocation {
            id: "call-1".to_string(),
            agent_id: String::new(),
            tool_name: "fs.read".to_string(),
            args: serde_json::json!({"path": "a.txt"}),
            result: Some("contents".to_string()),
            failed: false,
        };
        assert_ne!(start, complete);
        let ReplEvent::ToolInvocation { id: start_id, .. } = &start else {
            unreachable!()
        };
        let ReplEvent::ToolInvocation {
            id: complete_id, ..
        } = &complete
        else {
            unreachable!()
        };
        assert_eq!(start_id, complete_id);
    }

    /// `/clear` (DOC-50 §5 Slice 7) emits `ClearScrollback` — a unit
    /// variant, just confirming it exists and round-trips like any other.
    #[test]
    fn clear_scrollback_is_a_distinct_unit_variant() {
        assert_eq!(ReplEvent::ClearScrollback, ReplEvent::ClearScrollback);
        assert_ne!(ReplEvent::ClearScrollback, ReplEvent::Cancel);
    }

    /// `Quit` (DOC-50 §5 Slice 5) is a distinct unit variant, same as
    /// `ClearScrollback` above — `crate::run::run`'s dispatch step sends it
    /// when `TuiEngine::handle_input` returns `Ok(false)`.
    #[test]
    fn quit_is_a_distinct_unit_variant() {
        assert_eq!(ReplEvent::Quit, ReplEvent::Quit);
        assert_ne!(ReplEvent::Quit, ReplEvent::Cancel);
    }

    /// `TurnFinished` (the `dispatch_pending` completion safety net) carries
    /// its own `generation` and compares by value, same as any other field.
    #[test]
    fn turn_finished_carries_and_compares_its_generation() {
        assert_eq!(
            ReplEvent::TurnFinished { generation: 1 },
            ReplEvent::TurnFinished { generation: 1 }
        );
        assert_ne!(
            ReplEvent::TurnFinished { generation: 1 },
            ReplEvent::TurnFinished { generation: 2 }
        );
        assert_ne!(ReplEvent::TurnFinished { generation: 1 }, ReplEvent::Quit);
    }

    /// `WorkstreamActivationChanged`'s field is named `new_active_id` to
    /// match the DOC-48 §5.3 SSE wire event exactly (not DOC-50 §5 Slice 6's
    /// prose, which drifted to `new_id`) — a JSON field-name mismatch here
    /// would only surface at deserialization time, not compile time, so
    /// this test exists to keep the field name from silently drifting back.
    #[test]
    fn workstream_activation_changed_uses_wire_field_name() {
        let ev = ReplEvent::WorkstreamActivationChanged {
            new_active_id: Some("a1b2c3d4".to_string()),
            prior_id: Some("00000000".to_string()),
        };
        let ReplEvent::WorkstreamActivationChanged {
            new_active_id,
            prior_id,
        } = &ev
        else {
            unreachable!()
        };
        assert_eq!(new_active_id.as_deref(), Some("a1b2c3d4"));
        assert_eq!(prior_id.as_deref(), Some("00000000"));
    }

    /// `new_active_id: None` must be representable and distinguishable from
    /// `prior_id: None` — the whole point of this fix (issue tracked in the
    /// commit landing this test): the daemon legitimately publishes
    /// "deactivated, no replacement active" (DOC-48 §4.2/§4.3), and the
    /// shared event must carry that without falling back to free text.
    #[test]
    fn workstream_activation_changed_represents_deactivation_with_no_replacement() {
        let ev = ReplEvent::WorkstreamActivationChanged {
            new_active_id: None,
            prior_id: Some("a1b2c3d4".to_string()),
        };
        let ReplEvent::WorkstreamActivationChanged {
            new_active_id,
            prior_id,
        } = &ev
        else {
            unreachable!()
        };
        assert_eq!(*new_active_id, None);
        assert_eq!(prior_id.as_deref(), Some("a1b2c3d4"));
    }

    /// `KeyInput` default modifiers must be "nothing held" so a bare
    /// `KeyCode` translation (Slice 2) doesn't need to construct
    /// `KeyModifiers` by hand for the common case.
    #[test]
    fn key_modifiers_default_to_none_held() {
        let m = KeyModifiers::default();
        assert!(!m.ctrl && !m.alt && !m.shift);
    }

    /// `WorkstreamSummary` is the wire shape `ReplEvent::WorkstreamUpdated`
    /// carries; a (de)serialization round-trip is the cheapest guarantee
    /// that later HTTP-transported engines (CodeEngine) can carry it
    /// without a custom `Serialize` impl. (`StatuslineSegment`/`PickerItem`/
    /// `CommandDescriptor` round-trip tests moved to `crate::model` — Slice
    /// 1.5 relocated those types there.)
    #[test]
    fn workstream_summary_round_trips_through_json() {
        let ws = WorkstreamSummary {
            id: "a1b2c3d4".to_string(),
            name: "Token rotation".to_string(),
        };
        let json = serde_json::to_string(&ws).expect("serialize");
        assert_eq!(ws, serde_json::from_str(&json).expect("deserialize"));
    }
}
