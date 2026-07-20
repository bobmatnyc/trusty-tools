//! Engine-supplied UI data: [`StatuslineSegment`], [`PickerItem`]/[`PickerRequest`],
//! and [`CommandDescriptor`].
//!
//! Why: DOC-50 §3.2 (Slice 1.5) requires the statusline, pickers, and
//! slash-command help to be driven entirely by data the engine supplies, not
//! by constants baked into the shared crate. Slice 1 shipped provisional
//! stubs for these three types directly in `crate::event`, flagged as
//! zero-consumer placeholders; this module is their real, spec-shaped home.
//! Splitting them out of `event.rs` also keeps that module's focus narrow
//! (the `ReplEvent` wire vocabulary) versus this module's focus (the
//! semantic data an engine hands the TUI to render).
//!
//! What: three families of type, one per generalized surface:
//! - [`StatuslineSegment`] — an enum, per DOC-50 §3.2's explicit correction
//!   of Slice 1's flat `{label, value}` struct.
//! - [`PickerItem`] / [`PickerRequest`] — a named, engine-supplied picker
//!   (replacing tagent's hardcoded `PickerKind::{Model,Provider}`,
//!   `crates/trusty-agents/src/repl/tui/types.rs:28-35`).
//! - [`CommandDescriptor`] / [`CommandRouting`] — a slash-command registry
//!   entry that records whether the shared crate or the engine handles it
//!   (replacing tagent's hardcoded `SLASH_COMMANDS` array,
//!   `crates/trusty-agents/src/repl/tui/helpers.rs:156-181`).
//!
//! None of these types encode product-specific formulas or lists (e.g.
//! tagent's OpenRouter haiku cost formula, `crates/trusty-agents/src/repl/tui/status.rs:145-155`,
//! or its `/model`/`/provider` literals) — that logic stays in each engine
//! adapter (`AgentEngine`, `CodeEngine`); this crate only defines the shapes
//! the engine populates.
//!
//! # Spec References
//! - [`SPEC-TTUI-03~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-03~draft) — §3.2, the generalization layer this module implements.
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 1.5 deliverable and acceptance criteria.

use serde::{Deserialize, Serialize};

/// One labeled segment of the status line, supplied by the engine.
///
/// Why: DOC-50 §3.2 specifies the statusline as "enum-driven, engine
/// populated" — each variant is a semantic kind of thing an engine can
/// report (a session, the active model, the project, the active
/// workstream, an approximate cost), not an arbitrary string pair. This
/// replaces Slice 1's provisional flat `StatuslineSegment { label, value }`
/// (zero consumers today, so the redefinition is free — see that struct's
/// removal from `crate::event`).
/// What: `ReplEvent::StatuslineUpdate(Vec<StatuslineSegment>)` carries these
/// in render order; the Slice 4 status-line widget matches on variant to
/// decide layout/styling per kind, falling back to [`Self::Custom`] for
/// anything not worth a first-class variant (e.g. tagent's TM/claude-mpm
/// session counts, `status.rs`'s `tm_chunk`/`local_model_chunk`).
/// [`Self::Cost`] carries a pre-formatted string rather than raw
/// token/pricing data — per DOC-50 Q9, cost display is a Phase 2 concern
/// pending a daemon API, and the shared crate must never contain a pricing
/// formula (tagent's OpenRouter haiku formula stays in `AgentEngine`, not
/// here).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum StatuslineSegment {
    /// The active session's identifier (e.g. a short session id).
    SessionId(String),
    /// The active LLM provider + model (e.g. `provider: "anthropic"`,
    /// `name: "claude-sonnet-5"`). Kept as two fields (not one formatted
    /// string) so the widget can style them independently, mirroring
    /// tagent's `LLM: {provider} ({model})` chunk without hardcoding the
    /// `provider (model)` formatting choice into the shared crate.
    Model { provider: String, name: String },
    /// The active project name or path.
    Project(String),
    /// The active workstream (DOC-48 §2.1): `id` + `name`, matching
    /// `WorkstreamSummary` and the Slice 6 status-line example
    /// ("WS: Token rotation (a1b2c3d4)").
    Workstream { id: String, name: String },
    /// A pre-formatted, engine-computed cost string (e.g. `"$0.0034"`).
    /// Deferred to Phase 2 per DOC-50 Q9 (no daemon API yet to source real
    /// cost from) — this variant exists so the wire shape doesn't need a
    /// breaking change once that API lands; no engine populates it yet.
    ///
    /// **Multi-cost case:** `Cost` carries a single string, not a label +
    /// value pair, so an engine that wants to show more than one cost
    /// figure at once (tagent shows both the current session's cost and
    /// the day's running total, `status.rs`'s `session_cost`/`daily_cost`)
    /// bakes the distinction into the string itself and emits multiple
    /// `Cost` segments — e.g. `Cost("$0.0034 session")` and
    /// `Cost("$0.0145 today")` — matching tagent's existing `"{cost}
    /// session"` / `"{cost} today"` suffix convention
    /// (`crates/trusty-agents/src/repl/tui/status.rs`'s `format_cost_value`
    /// call sites) rather than inventing a new labeled-cost shape here.
    Cost(String),
    /// Escape hatch for an engine-specific segment that doesn't warrant a
    /// first-class variant (e.g. tagent's `TM: N sessions` / `MPM: N
    /// sessions` counts). `label` is the short prefix, `value` is the
    /// rendered text.
    Custom { label: String, value: String },
}

/// One selectable row in an engine-supplied picker (model, provider,
/// workstream, …).
///
/// Why: DOC-50 §3.3 requires picker data sources to be engine-provided
/// rather than hardcoded lists (tagent's model/provider pickers today are
/// hardcoded in `pickers.rs`, with items carried as bare `Vec<String>` in
/// `PickerState.items`, `crates/trusty-agents/src/repl/tui/types.rs:44-50`).
/// What: `id` is what gets sent back to the engine on selection (via
/// [`PickerRequest::dispatch_command`]); `label` is the display text (may
/// differ from `id`, e.g. a friendly name over a model slug);
/// `description` is optional secondary text (e.g. a model's context-window
/// size) the Slice 4 widget may render alongside the label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerItem {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
}

/// A complete engine-supplied picker: what to show, and what selecting an
/// item does.
///
/// Why: tagent hardcodes two picker kinds (`PickerKind::Model`,
/// `PickerKind::Provider`, `crates/trusty-agents/src/repl/tui/types.rs:28-35`)
/// with Enter-dispatch bound to the literal strings `"/model"` /
/// `"/provider"` — the shared crate must not know either of those concepts
/// exist. `PickerRequest` generalizes this: the engine names the picker
/// (`title`), supplies the rows (`items`), and supplies the command prefix
/// to dispatch on selection (`dispatch_command`) — the shared event loop
/// (Slice 4/7) confirms a selection by emitting
/// `ReplEvent::Submit(format!("{} {}", dispatch_command, selected.id))`,
/// reproducing tagent's exact `/model <selected>` / `/provider <selected>`
/// behavior without the shared crate hardcoding either command name.
/// What: `title` renders in the picker's border/header; `items` is the
/// (already-fetched) list of choices — no lazy paging in this slice.
/// `TuiEngine::picker` (see `crate::engine`) is how the shared event loop
/// asks an engine for one of these by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickerRequest {
    pub title: String,
    pub items: Vec<PickerItem>,
    pub dispatch_command: String,
}

/// One entry in an engine-supplied slash-command registry.
///
/// Why: DOC-50 §5 Slice 7 / Q4 splits slash commands into client-side
/// built-ins (`/help`, `/clear`, `/quit`, handled entirely inside
/// `trusty-tui`, never reaching `TuiEngine::handle_input`) and
/// engine-routed domain commands (`/model`, `/workstream`, forwarded to
/// `TuiEngine::handle_input`). `/help` needs to enumerate both, so
/// [`CommandRouting`] records which side owns dispatch — generalizing
/// tagent's flat `SLASH_COMMANDS: &[(&str, &str)]` array
/// (`crates/trusty-agents/src/repl/tui/helpers.rs:156-181`), which mixes
/// generic commands (`/help`, `/clear`) with tagent-specific ones
/// (`/agent`, `/local`, `/tm`) and carries no routing information at all.
/// What: `name` is the command without its leading `/` (e.g.
/// `"workstream"`); `summary` is the one-line help text `/help` renders;
/// `args_hint` is optional completion metadata shown after the name (e.g.
/// `Some("<id>")` renders as `/workstream activate <id>` in autocomplete),
/// mirroring tagent's `update_slash_completions` prefix-match behavior
/// without hardcoding argument shapes into the shared crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandDescriptor {
    pub name: String,
    pub summary: String,
    pub routing: CommandRouting,
    pub args_hint: Option<String>,
}

/// Which side of the seam dispatches a [`CommandDescriptor`] (DOC-50 §6 Q4,
/// "mixed routing").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandRouting {
    /// Handled entirely inside `trusty-tui` (`/help`, `/clear`, `/quit`);
    /// never reaches `TuiEngine::handle_input`.
    BuiltIn,
    /// Forwarded to `TuiEngine::handle_input` for the engine to execute
    /// (`/model`, `/workstream`, and other domain commands).
    Engine,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `StatuslineSegment` variant must round-trip through JSON — the
    /// wire shape an HTTP-transported engine (`CodeEngine`, Slice 3) will
    /// carry these over.
    #[test]
    fn statusline_segment_variants_round_trip_through_json() {
        let segments = vec![
            StatuslineSegment::SessionId("a1b2c3d4".to_string()),
            StatuslineSegment::Model {
                provider: "anthropic".to_string(),
                name: "claude-sonnet-5".to_string(),
            },
            StatuslineSegment::Project("trusty-tools".to_string()),
            StatuslineSegment::Workstream {
                id: "a1b2c3d4".to_string(),
                name: "Token rotation".to_string(),
            },
            StatuslineSegment::Cost("$0.0034".to_string()),
            StatuslineSegment::Custom {
                label: "TM".to_string(),
                value: "2 sessions".to_string(),
            },
        ];
        for seg in segments {
            let json = serde_json::to_string(&seg).expect("serialize");
            let back: StatuslineSegment = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(seg, back);
        }
    }

    /// `StatuslineSegment` is an enum per DOC-50 §3.2 — confirm distinct
    /// variants (even ones sharing a `String` payload shape) are not equal,
    /// which would be the failure mode of accidentally collapsing back to a
    /// flat struct.
    #[test]
    fn statusline_segment_variants_are_distinct() {
        let session = StatuslineSegment::SessionId("x".to_string());
        let project = StatuslineSegment::Project("x".to_string());
        assert_ne!(session, project);
    }

    /// `PickerItem`/`PickerRequest` must round-trip through JSON for the
    /// same HTTP-transport reason as statusline segments.
    #[test]
    fn picker_request_round_trips_through_json() {
        let req = PickerRequest {
            title: "Select a model".to_string(),
            items: vec![
                PickerItem {
                    id: "opus-4".to_string(),
                    label: "Claude Opus 4".to_string(),
                    description: Some("Most capable".to_string()),
                },
                PickerItem {
                    id: "haiku-4-5".to_string(),
                    label: "Claude Haiku 4.5".to_string(),
                    description: None,
                },
            ],
            dispatch_command: "/model".to_string(),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        let back: PickerRequest = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(req, back);
    }

    /// The whole point of `dispatch_command`: the shared event loop
    /// synthesizes `"<dispatch_command> <selected.id>"` on confirmation,
    /// reproducing tagent's `/model <selected>` behavior generically. This
    /// test locks the exact composition so a future event-loop
    /// implementation (Slice 4/7) has a documented contract to build
    /// against.
    #[test]
    fn picker_request_dispatch_command_composes_with_selected_item_id() {
        let req = PickerRequest {
            title: "Select a provider".to_string(),
            items: vec![PickerItem {
                id: "openrouter".to_string(),
                label: "OpenRouter".to_string(),
                description: None,
            }],
            dispatch_command: "/provider".to_string(),
        };
        let selected = &req.items[0];
        let synthesized = format!("{} {}", req.dispatch_command, selected.id);
        assert_eq!(synthesized, "/provider openrouter");
    }

    /// `CommandDescriptor`/`CommandRouting` must round-trip through JSON,
    /// covering both routing kinds so DOC-50 Q4's mixed-routing model is
    /// exercised, not just the default.
    #[test]
    fn command_descriptor_round_trips_through_json_for_both_routing_kinds() {
        let builtin = CommandDescriptor {
            name: "clear".to_string(),
            summary: "clear chat".to_string(),
            routing: CommandRouting::BuiltIn,
            args_hint: None,
        };
        let json = serde_json::to_string(&builtin).expect("serialize");
        assert_eq!(builtin, serde_json::from_str(&json).expect("deserialize"));

        let engine_routed = CommandDescriptor {
            name: "workstream".to_string(),
            summary: "List or activate a workstream".to_string(),
            routing: CommandRouting::Engine,
            args_hint: Some("activate <id>".to_string()),
        };
        let json = serde_json::to_string(&engine_routed).expect("serialize");
        assert_eq!(
            engine_routed,
            serde_json::from_str(&json).expect("deserialize")
        );
    }
}
