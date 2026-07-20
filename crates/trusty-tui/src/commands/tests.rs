//! Tests for `super` (`crate::commands`) — split into its own file per this
//! crate's established test/prod split convention (e.g.
//! `crate::app::reduce`/`crate::app::reduce::tests`), keeping the
//! production module under the 500-SLOC cap (`scripts/check_line_cap.sh`).

use super::*;
use crate::app::ReplApp;
use tokio::sync::mpsc;

/// A `TuiEngine` test double with a single named picker (or none) — enough
/// to exercise [`resolve_forward`]/[`dispatch_forward`] without any real
/// backend.
struct MockEngine {
    picker_name: Option<&'static str>,
}

impl MockEngine {
    fn none() -> Self {
        Self { picker_name: None }
    }

    fn with_picker(name: &'static str) -> Self {
        Self {
            picker_name: Some(name),
        }
    }
}

#[async_trait::async_trait]
impl TuiEngine for MockEngine {
    async fn handle_input(
        &self,
        line: String,
        tx: UnboundedSender<ReplEvent>,
    ) -> anyhow::Result<bool> {
        let _ = tx.send(ReplEvent::StatusMessage(format!("handled: {line}")));
        Ok(true)
    }

    async fn setup(&self, _tx: UnboundedSender<ReplEvent>) -> anyhow::Result<()> {
        Ok(())
    }

    fn picker(&self, name: &str) -> Option<PickerRequest> {
        if self.picker_name == Some(name) {
            Some(PickerRequest {
                title: "Select a model".to_string(),
                items: vec![
                    PickerItem {
                        id: "opus-4".to_string(),
                        label: "Claude Opus 4".to_string(),
                        description: None,
                    },
                    PickerItem {
                        id: "haiku-4-5".to_string(),
                        label: "Claude Haiku 4.5".to_string(),
                        description: None,
                    },
                ],
                dispatch_command: format!("/{name}"),
            })
        } else {
            None
        }
    }
}

fn engine_command(name: &str, args_hint: Option<&str>) -> CommandDescriptor {
    CommandDescriptor {
        name: name.to_string(),
        summary: format!("do {name}"),
        routing: CommandRouting::Engine,
        args_hint: args_hint.map(str::to_string),
    }
}

// ── built_in_commands / route ──────────────────────────────────────────

/// Every name in `built_in_commands()` must be recognized by `route` as a
/// `Route::BuiltIn` — the two tables (the display list and the match arms)
/// must stay in lockstep, or `/help` would advertise a command that
/// actually falls through to `Route::Forward`.
#[test]
fn built_in_commands_names_match_route_recognition() {
    for cmd in built_in_commands() {
        let routed = route(&format!("/{}", cmd.name));
        assert!(
            matches!(routed, Route::BuiltIn(_)),
            "built-in `{}` must route as BuiltIn, got {routed:?}",
            cmd.name
        );
    }
}

/// `/help`, `/clear`, `/quit`, and `/exit` each resolve to their documented
/// `BuiltIn` variant — pins the exact mapping, not just "is a BuiltIn".
#[test]
fn route_recognizes_each_builtin_name() {
    assert_eq!(route("/help"), Route::BuiltIn(BuiltIn::Help));
    assert_eq!(route("/clear"), Route::BuiltIn(BuiltIn::Clear));
    assert_eq!(route("/quit"), Route::BuiltIn(BuiltIn::Quit));
    assert_eq!(route("/exit"), Route::BuiltIn(BuiltIn::Quit));
}

/// Plain chat text (no leading `/`) is never a built-in — it's the normal
/// free-form input `TuiEngine::handle_input` treats as a chat turn.
#[test]
fn route_forwards_plain_chat() {
    assert_eq!(
        route("hello there"),
        Route::Forward("hello there".to_string())
    );
}

/// A slash command this crate has never heard of forwards exactly like
/// plain chat — the shared crate never rejects a command outright; only the
/// engine can decide it's actually invalid (thin-client axiom).
#[test]
fn route_forwards_unrecognized_slash_command() {
    assert_eq!(
        route("/nope --flag"),
        Route::Forward("/nope --flag".to_string())
    );
}

/// A recognized domain command (`/workstream`, engine-routed per
/// `CommandDescriptor::routing`) is NOT special-cased differently from an
/// unrecognized one — both are `Route::Forward`, because `route` never
/// consults `TuiEngine::commands()` at all (see the module doc comment).
#[test]
fn route_forwards_engine_domain_command() {
    assert_eq!(
        route("/workstream activate a1b2c3d4"),
        Route::Forward("/workstream activate a1b2c3d4".to_string())
    );
}

/// Leading/trailing whitespace around a submitted line must not defeat
/// built-in recognition or leak into the forwarded text.
#[test]
fn route_trims_surrounding_whitespace() {
    assert_eq!(route("  /clear  "), Route::BuiltIn(BuiltIn::Clear));
    assert_eq!(
        route("  hello world  "),
        Route::Forward("hello world".to_string())
    );
}

// ── render_help ─────────────────────────────────────────────────────────

/// `/help` output must list every built-in AND every engine-supplied
/// command (DOC-50 §5 Slice 7: "render the command list from
/// `engine.commands()` + built-ins") — using a command list shaped like a
/// mock engine's `commands()` return value.
#[test]
fn render_help_lists_builtins_and_engine_commands() {
    let engine_commands = vec![engine_command("workstream", None)];
    let text = render_help(&engine_commands);
    assert!(text.contains("/help"));
    assert!(text.contains("/clear"));
    assert!(text.contains("/quit"));
    assert!(text.contains("/exit"));
    assert!(text.contains("/workstream"));
    assert!(text.contains("do workstream"));
}

/// An engine command with `args_hint` renders it inline after the name.
#[test]
fn render_help_includes_args_hint_when_present() {
    let engine_commands = vec![engine_command("workstream", Some("activate <id>"))];
    let text = render_help(&engine_commands);
    assert!(text.contains("/workstream activate <id> — do workstream"));
}

// ── resolve_forward ─────────────────────────────────────────────────────

/// A bare `/model` (no args) whose name matches `engine.picker("model")`
/// opens the picker instead of forwarding.
#[test]
fn resolve_forward_opens_picker_for_bare_matching_command() {
    let engine = MockEngine::with_picker("model");
    let outcome = resolve_forward("/model", &engine);
    match outcome {
        Forward::OpenPicker(req) => {
            assert_eq!(req.title, "Select a model");
            assert_eq!(req.items.len(), 2);
        }
        Forward::ToEngine(_) => panic!("expected OpenPicker, got ToEngine"),
    }
}

/// `/model opus-4` (an argument supplied) forwards even though
/// `engine.picker("model")` would return `Some` — matches tagent's
/// precedent that a supplied argument means the user already made their
/// choice, so the overlay must not open.
#[test]
fn resolve_forward_ignores_picker_when_args_present() {
    let engine = MockEngine::with_picker("model");
    let outcome = resolve_forward("/model opus-4", &engine);
    assert_eq!(outcome, Forward::ToEngine("/model opus-4".to_string()));
}

/// No picker registered under that name: falls back to forwarding.
#[test]
fn resolve_forward_falls_back_to_engine_when_no_picker_matches() {
    let engine = MockEngine::none();
    let outcome = resolve_forward("/model", &engine);
    assert_eq!(outcome, Forward::ToEngine("/model".to_string()));
}

/// Plain chat (no leading `/`) is never picker-eligible, even if a picker
/// happened to be registered under a name that collides with the text.
#[test]
fn resolve_forward_never_opens_picker_for_plain_chat() {
    let engine = MockEngine::with_picker("model");
    let outcome = resolve_forward("model", &engine);
    assert_eq!(outcome, Forward::ToEngine("model".to_string()));
}

// ── compose_selection ───────────────────────────────────────────────────

/// Locks the exact `"{dispatch_command} {selected.id}"` composition DOC-50
/// §3.2's `PickerRequest` contract documents (mirrors
/// `crate::model::tests::picker_request_dispatch_command_composes_with_selected_item_id`,
/// exercised here as `crate::commands`'s own call site).
#[test]
fn compose_selection_matches_picker_request_contract() {
    let request = PickerRequest {
        title: "Select a provider".to_string(),
        items: vec![PickerItem {
            id: "openrouter".to_string(),
            label: "OpenRouter".to_string(),
            description: None,
        }],
        dispatch_command: "/provider".to_string(),
    };
    let composed = compose_selection(&request, &request.items[0]);
    assert_eq!(composed, "/provider openrouter");
}

// ── dispatch_forward ─────────────────────────────────────────────────────

/// Opening a picker must NOT call `engine.handle_input` — confirmed by
/// checking `tx` received nothing (the mock engine's `handle_input` always
/// sends a `StatusMessage`, so silence proves it was never invoked) and
/// that `app.active_picker` is populated.
#[tokio::test]
async fn dispatch_forward_opens_picker_without_calling_engine() {
    let mut app = ReplApp::new("demo", "u");
    let engine = MockEngine::with_picker("model");
    let (tx, mut rx) = mpsc::unbounded_channel();

    let ok = dispatch_forward(&mut app, "/model".to_string(), &engine, &tx)
        .await
        .expect("dispatch_forward must not error");

    assert!(ok);
    assert!(app.active_picker.is_some(), "picker must be staged");
    drop(tx);
    assert!(
        rx.recv().await.is_none(),
        "engine.handle_input must never have been called"
    );
}

/// No matching picker: `dispatch_forward` calls `engine.handle_input`,
/// which the mock proves by sending a `StatusMessage` onto `tx`.
#[tokio::test]
async fn dispatch_forward_calls_engine_when_no_picker_matches() {
    let mut app = ReplApp::new("demo", "u");
    let engine = MockEngine::none();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let ok = dispatch_forward(&mut app, "/workstream list".to_string(), &engine, &tx)
        .await
        .expect("dispatch_forward must not error");

    assert!(ok);
    assert!(app.active_picker.is_none());
    let event = rx.recv().await.expect("engine must have sent an event");
    assert_eq!(
        event,
        ReplEvent::StatusMessage("handled: /workstream list".to_string())
    );
}

// ── ReplApp::submit_line built-in short-circuiting ──────────────────────

/// `/clear` clears the scrollback (the `ReplEvent::ClearScrollback` effect)
/// entirely client-side: `pending_submit` stays `None` and `busy` stays
/// `false` — nothing was forwarded to an engine.
#[test]
fn submit_line_builtin_clear_clears_scrollback() {
    let mut app = ReplApp::new("demo", "u");
    app.push_assistant("earlier response", false);
    assert!(!app.chat.is_empty());

    app.submit_line("/clear".to_string());

    assert!(app.chat.is_empty(), "scrollback must be cleared");
    assert!(app.pending_submit.is_none(), "must never forward /clear");
    assert!(!app.busy, "/clear must not mark the app busy");
}

/// `/quit` sets `ReplApp::quit` directly, never touching `pending_submit`.
#[test]
fn submit_line_builtin_quit_sets_quit() {
    let mut app = ReplApp::new("demo", "u");
    app.submit_line("/quit".to_string());
    assert!(app.quit);
    assert!(app.pending_submit.is_none());

    let mut app2 = ReplApp::new("demo", "u");
    app2.submit_line("/exit".to_string());
    assert!(app2.quit, "/exit must be recognized as the /quit alias");
}

/// `/help` pushes a status entry listing built-ins plus `app.commands`
/// (populated as if from a mock engine's `commands()`), never forwards.
#[test]
fn submit_line_builtin_help_lists_commands() {
    let mut app = ReplApp::new("demo", "u");
    app.commands = vec![engine_command("workstream", Some("activate <id>"))];

    app.submit_line("/help".to_string());

    assert!(app.pending_submit.is_none());
    // Echo line + help status line.
    assert_eq!(app.chat.len(), 2);
    assert!(app.chat[1].text.contains("/workstream activate <id>"));
    assert!(app.chat[1].text.contains("/clear"));
}

/// A non-built-in submission (plain chat or a domain command) is echoed,
/// marks the app busy, and stages `pending_submit` for the caller to
/// forward — unchanged behavior from before Slice 7 for this path.
#[test]
fn submit_line_forwards_non_builtin_and_marks_busy() {
    let mut app = ReplApp::new("demo", "u");
    app.submit_line("/workstream list".to_string());
    assert!(app.busy);
    assert_eq!(
        app.pending_submit.take(),
        Some("/workstream list".to_string())
    );
}

// ── ReplApp picker state ─────────────────────────────────────────────────

/// Confirming a valid index composes the selection and closes the picker.
#[test]
fn confirm_picker_selection_composes_and_closes() {
    let mut app = ReplApp::new("demo", "u");
    app.open_picker(PickerRequest {
        title: "Select a model".to_string(),
        items: vec![PickerItem {
            id: "opus-4".to_string(),
            label: "Claude Opus 4".to_string(),
            description: None,
        }],
        dispatch_command: "/model".to_string(),
    });

    let composed = app.confirm_picker_selection(0);

    assert_eq!(composed, Some("/model opus-4".to_string()));
    assert!(app.active_picker.is_none(), "picker must close on confirm");
}

/// An out-of-range index (or no picker open at all) is a no-op — returns
/// `None` and leaves any open picker untouched.
#[test]
fn confirm_picker_selection_out_of_range_is_noop() {
    let mut app = ReplApp::new("demo", "u");
    assert_eq!(app.confirm_picker_selection(0), None);

    app.open_picker(PickerRequest {
        title: "Select a model".to_string(),
        items: vec![PickerItem {
            id: "opus-4".to_string(),
            label: "Claude Opus 4".to_string(),
            description: None,
        }],
        dispatch_command: "/model".to_string(),
    });
    assert_eq!(app.confirm_picker_selection(5), None);
    assert!(
        app.active_picker.is_some(),
        "an out-of-range confirm must not close the picker"
    );
}
