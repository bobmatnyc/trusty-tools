//! The slash-command parser/dispatcher — DOC-50 §5 Slice 7, §6 Q4 ("mixed
//! routing").
//!
//! Why: DOC-50 §6 Q4 resolves slash-command handling as a split: a small,
//! fixed set of client-side built-ins (`/help`, `/clear`, `/quit`/`/exit`)
//! that never reach a product's [`crate::engine::TuiEngine`], and every
//! other slash command (domain commands like `/model`, `/workstream`, plus
//! any command this crate has never heard of) forwarded verbatim to
//! `TuiEngine::handle_input` — "the client does NOT interpret domain
//! commands" (thin-client axiom, DOC-39 §2.1 C-1/C-2). This module is where
//! that split is decided. It replaces tagent's monolithic
//! `try_handle_slash`/`SLASH_COMMANDS` (`crates/trusty-agents/src/repl/*.rs`,
//! `crates/trusty-agents/src/repl/tui/helpers.rs:156-181`), which mixed
//! generic commands with tagent-specific ones and carried no routing
//! metadata at all.
//!
//! What: three pieces.
//! - [`route`] — pure, engine-free: given a submitted line, decide
//!   [`Route::BuiltIn`] or [`Route::Forward`]. Only needs the fixed
//!   [`built_in_commands`] table, so [`crate::app::ReplApp::submit_line`]
//!   (which has no engine handle — see `crate::app::reduce`'s module doc
//!   comment) can call it directly and apply a built-in's effect inline
//!   ([`Route::BuiltIn(BuiltIn::Clear)`](BuiltIn::Clear) reuses
//!   [`crate::app::ReplApp::clear_scrollback`], the same effect
//!   `ReplEvent::ClearScrollback` produces).
//! - [`resolve_forward`]/[`compose_selection`] — the picker half (DOC-50
//!   §3.2/Q6): a bare `/name` (no args) `Route::Forward` line whose name
//!   matches `TuiEngine::picker(name)` opens an inline picker instead of
//!   reaching `handle_input`; confirming a picker item resubmits
//!   `"{dispatch_command} {selected.id}"`. These DO need engine access, so
//!   they're free functions a product's async event-loop wiring calls
//!   (rather than living inside the sync [`crate::app::reduce::apply`]
//!   reducer, which cannot reach a `TuiEngine` — see that module's doc
//!   comment).
//! - [`dispatch_forward`] — composes the two into the actual integration
//!   point: given a `Route::Forward` line staged in
//!   [`crate::app::ReplApp::pending_submit`], either opens a picker or calls
//!   `engine.handle_input`.
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 7 deliverable (§5, Slice 7) and Q4 (mixed routing), Q6 (inline pickers).

use tokio::sync::mpsc::UnboundedSender;

use crate::app::ReplApp;
use crate::engine::TuiEngine;
use crate::event::ReplEvent;
use crate::model::{CommandDescriptor, CommandRouting, PickerItem, PickerRequest};

/// The fixed, client-side built-in command table (DOC-50 §6 Q4) — never
/// forwarded to `TuiEngine::handle_input`.
///
/// Why: a plain function (not a `const`) so each call returns an owned
/// `Vec` — cheap, and avoids a `static`/`OnceLock` for four short-lived
/// rows. `/quit` and `/exit` are both listed (two independent entries, not
/// one row with an alias note) so `/help`'s output and prefix-based
/// discovery treat them identically; [`route`] recognizes both names as
/// [`BuiltIn::Quit`].
/// What: `name` matches [`route`]'s match arms exactly; `summary` is what
/// [`render_help`] prints. None of these carry `args_hint` — all four take
/// no arguments.
/// Test: [`tests::built_in_commands_names_match_route_recognition`].
pub fn built_in_commands() -> Vec<CommandDescriptor> {
    vec![
        CommandDescriptor {
            name: "help".to_string(),
            summary: "Show available commands".to_string(),
            routing: CommandRouting::BuiltIn,
            args_hint: None,
        },
        CommandDescriptor {
            name: "clear".to_string(),
            summary: "Clear the scrollback".to_string(),
            routing: CommandRouting::BuiltIn,
            args_hint: None,
        },
        CommandDescriptor {
            name: "quit".to_string(),
            summary: "Exit".to_string(),
            routing: CommandRouting::BuiltIn,
            args_hint: None,
        },
        CommandDescriptor {
            name: "exit".to_string(),
            summary: "Exit (alias for /quit)".to_string(),
            routing: CommandRouting::BuiltIn,
            args_hint: None,
        },
    ]
}

/// The effect of a recognized built-in command — applied directly by
/// [`crate::app::ReplApp::submit_line`], never round-tripped through
/// `TuiEngine`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltIn {
    /// Render the combined built-in + engine command list.
    Help,
    /// Clear the scrollback — same effect as `ReplEvent::ClearScrollback`.
    Clear,
    /// Set `ReplApp::quit = true`.
    Quit,
}

/// Where a submitted line should route (DOC-50 §6 Q4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Handled entirely client-side — see [`BuiltIn`].
    BuiltIn(BuiltIn),
    /// Not a recognized built-in: forward this exact (trimmed) line to
    /// `TuiEngine::handle_input`. Covers domain commands (`/model`,
    /// `/workstream`), plain chat text, AND any slash command this crate
    /// doesn't recognize — the shared crate never rejects a command outright
    /// (thin-client axiom: only the engine knows what's valid).
    Forward(String),
}

/// Split a trimmed `/`-prefixed line into its command name (no leading `/`)
/// and trimmed argument tail (`""` when bare). `None` when `line` isn't a
/// slash command at all (doesn't start with `/`, or is exactly `/`).
fn command_name(line: &str) -> Option<(&str, &str)> {
    let trimmed = line.trim();
    let rest = trimmed.strip_prefix('/')?;
    if rest.is_empty() {
        return None;
    }
    match rest.split_once(char::is_whitespace) {
        Some((name, args)) => Some((name, args.trim())),
        None => Some((rest, "")),
    }
}

/// Classify one submitted line per DOC-50 §6 Q4's mixed-routing model.
///
/// Why: pure and engine-free (only consults the fixed [`built_in_commands`]
/// table) so [`crate::app::ReplApp::submit_line`] — which has no
/// `TuiEngine` handle — can call it directly. See the module doc comment
/// for why built-in recognition doesn't need `TuiEngine::commands()` at all:
/// the built-in set is fixed and product-independent.
/// What: non-slash lines (plain chat) and any `/name` not in
/// [`built_in_commands`] both resolve to `Route::Forward` — see that
/// variant's doc comment for why "unrecognized" and "domain command" are
/// treated identically.
/// Test: [`tests::route_recognizes_each_builtin_name`],
/// [`tests::route_forwards_plain_chat`],
/// [`tests::route_forwards_unrecognized_slash_command`],
/// [`tests::route_forwards_engine_domain_command`].
pub fn route(line: &str) -> Route {
    match command_name(line) {
        Some(("help", _)) => Route::BuiltIn(BuiltIn::Help),
        Some(("clear", _)) => Route::BuiltIn(BuiltIn::Clear),
        Some(("quit", _)) | Some(("exit", _)) => Route::BuiltIn(BuiltIn::Quit),
        _ => Route::Forward(line.trim().to_string()),
    }
}

/// Render `/help`'s output: the built-ins plus every engine-supplied
/// command, one line each.
///
/// Why: DOC-50 §5 Slice 7 requires `/help` to enumerate both sides of the
/// routing split — `engine_commands` is expected to be
/// [`crate::app::ReplApp::commands`] (the cache a product populates from
/// `TuiEngine::commands()` during setup; see that field's doc comment).
/// What: one `"  /{name}{ args_hint} — {summary}"` line per command,
/// built-ins first. Pure string formatting — no ratatui dependency, so this
/// stays usable from both the plain-text scrollback path and a future
/// richer help widget.
/// Test: [`tests::render_help_lists_builtins_and_engine_commands`],
/// [`tests::render_help_includes_args_hint_when_present`].
pub fn render_help(engine_commands: &[CommandDescriptor]) -> String {
    let mut lines = vec!["Available commands:".to_string()];
    for cmd in built_in_commands().iter().chain(engine_commands.iter()) {
        let hint = cmd
            .args_hint
            .as_deref()
            .map(|h| format!(" {h}"))
            .unwrap_or_default();
        lines.push(format!("  /{}{} — {}", cmd.name, hint, cmd.summary));
    }
    lines.join("\n")
}

/// `true` when `line` is a bare `/name` invocation (no arguments) —
/// convention for picker-eligible commands (DOC-50 §3.2
/// `TuiEngine::picker`'s doc comment: "a command's name doubles as its
/// picker name").
fn bare_command_name(line: &str) -> Option<&str> {
    match command_name(line) {
        Some((name, "")) => Some(name),
        _ => None,
    }
}

/// What the caller should do with a `Route::Forward` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Forward {
    /// Open this inline picker instead of calling `handle_input`.
    OpenPicker(PickerRequest),
    /// No matching picker: forward to `TuiEngine::handle_input` as-is.
    ToEngine(String),
}

/// Resolve a `Route::Forward` line against `engine`: does it open a picker,
/// or does it genuinely need `handle_input`?
///
/// Why: split out of [`dispatch_forward`] so the picker decision itself
/// (needs `&dyn TuiEngine`, but no `tx`/`ReplApp` mutation) is unit-testable
/// against a bare mock engine.
/// What: only a *bare* command (see [`bare_command_name`]) is picker-
/// eligible — `/model opus-4` (already has an argument) always forwards,
/// even if `engine.picker("model")` would return `Some`, matching tagent's
/// precedent (`crates/trusty-agents/src/repl/bridge.rs`: the picker overlay
/// only opens for the no-arg form; a supplied argument is assumed to be the
/// user directly naming their choice).
/// Test: [`tests::resolve_forward_opens_picker_for_bare_matching_command`],
/// [`tests::resolve_forward_ignores_picker_when_args_present`],
/// [`tests::resolve_forward_falls_back_to_engine_when_no_picker_matches`].
pub fn resolve_forward(line: &str, engine: &dyn TuiEngine) -> Forward {
    if let Some(name) = bare_command_name(line)
        && let Some(request) = engine.picker(name)
    {
        return Forward::OpenPicker(request);
    }
    Forward::ToEngine(line.to_string())
}

/// Compose the line to resubmit when the user confirms `selected` from
/// `request` (DOC-50 §3.2 `PickerRequest` contract:
/// `"{dispatch_command} {selected.id}"`).
///
/// Why: a free function (not a `PickerRequest` method — that type lives in
/// `crate::model`, which must stay free of any TUI-driver-shaped API) so
/// both this module's [`dispatch_forward`]-adjacent flow and a future
/// picker *widget*'s Enter-key handling share one composition rule.
/// Test: [`tests::compose_selection_matches_picker_request_contract`].
pub fn compose_selection(request: &PickerRequest, selected: &PickerItem) -> String {
    format!("{} {}", request.dispatch_command, selected.id)
}

/// The `Route::Forward` half of the dispatch pipeline: resolve a picker or
/// call `engine.handle_input`.
///
/// Why: the actual integration point a product's event-loop wiring calls
/// after draining a `Route::Forward` line from
/// [`crate::app::ReplApp::pending_submit`] — composes [`resolve_forward`]
/// (the routing decision) with the two possible effects (open a picker on
/// `app`, or await the engine call) in one place, so callers don't have to
/// re-derive the "which one am I supposed to do" branch themselves.
/// What: on [`Forward::OpenPicker`], stages the picker on `app` (via
/// [`crate::app::ReplApp::open_picker`]), clears [`crate::app::ReplApp::busy`],
/// and returns `Ok(true)` — the engine is never touched. Clearing `busy`
/// here (not in [`crate::app::ReplApp::submit_line`]) matters: `submit_line`
/// unconditionally sets `busy = true` for every `Route::Forward` line
/// because it can't know in advance whether this function will end up
/// opening a picker instead of actually calling `handle_input` — this arm
/// is the one place that knows the round-trip to the engine never
/// happened. Leaving `busy` set would strand the input composer's busy
/// spinner for as long as the picker stays open (nothing else clears it —
/// `busy` is only ever reset by an `AssistantOutput` event, which never
/// arrives on this path) and would spuriously gate `apply_up`'s
/// busy-triggered `pending_cancel` (`crate::app::reduce::apply_up`), firing
/// an unwanted `engine.cancel_session()` the next time a picker-open Up
/// arrow is drained. On [`Forward::ToEngine`], forwards verbatim to
/// `engine.handle_input`, returning its `Result<bool>` unchanged (per
/// `TuiEngine::handle_input`'s own contract: `Ok(false)` means quit) —
/// `busy` is left as `submit_line` set it, since the engine call is the
/// thing `busy` is meant to track.
/// Test: [`tests::dispatch_forward_opens_picker_without_calling_engine`],
/// [`tests::dispatch_forward_calls_engine_when_no_picker_matches`],
/// [`tests::dispatch_forward_clears_busy_set_by_submit_line_when_opening_picker`].
pub async fn dispatch_forward(
    app: &mut ReplApp,
    line: String,
    engine: &dyn TuiEngine,
    tx: &UnboundedSender<ReplEvent>,
) -> anyhow::Result<bool> {
    match resolve_forward(&line, engine) {
        Forward::OpenPicker(request) => {
            app.open_picker(request);
            app.busy = false;
            Ok(true)
        }
        Forward::ToEngine(line) => engine.handle_input(line, tx.clone()).await,
    }
}

#[cfg(test)]
mod tests;
