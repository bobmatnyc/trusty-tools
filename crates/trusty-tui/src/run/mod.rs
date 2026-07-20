//! The shared render/event loop (Slice 2, #3414).
//!
//! Why: DOC-50 §2.2 puts the event loop itself in `trusty-tui` so tagent and
//! tcode stop each owning a bespoke copy — this module is the shared engine
//! that shape lives in, generalized from the proven design in
//! `crates/trusty-agents/src/repl/tui/run.rs::event_loop` (100ms redraw tick,
//! `tokio::select!` over the tick and the event channel, a dedicated OS
//! thread for the key read so it never parks the tokio runtime).
//!
//! What: three pieces. [`spawn_key_reader`] is the OS thread that polls for
//! crossterm events and forwards translated [`ReplEvent`]s (key presses via
//! [`crate::keys::translate_key_event`], resizes, mouse-wheel scroll) onto an
//! `mpsc` channel; it returns a [`KeyReaderGuard`] rather than a bare
//! `JoinHandle` (see that type's doc comment for why). [`event_loop`] is the
//! terminal-generic `tick`/`recv` select loop — generic over
//! `ratatui::backend::Backend` (not just `CrosstermBackend`) specifically so
//! it can be unit-tested against `ratatui::backend::TestBackend` without a
//! real TTY. [`run`] is the opinionated top-level entry point that wires a
//! [`TerminalGuard`], the key reader, and an `E: TuiEngine` together the way
//! a real product binary actually wants to call this crate.
//!
//! Deliberately NOT decided here: what `M` (the render/reducer model) looks
//! like inside a product. Slice 4 owns the shared chat/statusline/picker
//! widgets and the concrete model they render from; this module only needs
//! to know when to redraw (every tick or every event) and when to stop
//! (`M::should_quit`). That is the full contract `apply`/`render`/[`TuiModel`]
//! below capture — deliberately mirroring `crates/trusty-agents/src/repl/tui/run.rs::event_loop`'s
//! `app: Arc<Mutex<ReplApp>>` + `draw(f, &snap)` + `process_event` shape,
//! generalized to not require `Arc<Mutex<_>>` (the caller decides its own
//! interior mutability, if any).
//!
//! A prior revision of [`run`] blocked `crossterm::event::read()` directly on
//! the key-reader thread and shut it down with `drop(tx); thread.join()`.
//! That hangs: `read()` only notices the channel closed on its NEXT `send()`,
//! which can't happen until a key is actually pressed — and by the time `run`
//! reaches shutdown, the user has usually just pressed the key that made
//! `should_quit()` true, so the thread is parked in a SECOND, indefinite
//! `read()`. Worse, the terminal restore ran after that join, so the
//! terminal stayed frozen in raw/alt-screen mode with no feedback while
//! hung. It also leaked the thread entirely on the `engine.setup`/
//! `subscribe_workstream_events` error paths, which returned via `?` before
//! ever reaching the join. [`KeyReaderGuard`] and the restructured [`run`]
//! fix both: the thread polls with a bounded timeout and checks an atomic
//! stop flag every poll, and terminal restoration happens unconditionally,
//! before the (now-bounded) thread join, on every exit path.
//!
//! # Spec References
//! - [`SPEC-TTUI-05~draft`](../../../docs/specs/DOC-50-tcode-tui-claude-code-clone.md#SPEC-TTUI-05~draft) — Slice 2 deliverable (§5, Slice 2): the event loop.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use crossterm::event::{self, Event as CtEvent, KeyEventKind, MouseEvent, MouseEventKind};
use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::engine::TuiEngine;
use crate::event::ReplEvent;
use crate::keys::translate_key_event;
use crate::terminal::TerminalGuard;

/// How often [`event_loop`] redraws when no event arrives.
///
/// Why: matches the tick cadence in
/// `crates/trusty-agents/src/repl/tui/run.rs::event_loop` — fast enough for a
/// smoothly animating spinner/elapsed-time display, cheap enough (a full diff
/// against an idle frame) not to burn meaningful CPU.
pub const TICK: Duration = Duration::from_millis(100);

/// How often the key-reader thread's `crossterm::event::poll` call returns so
/// it can re-check [`KeyReaderGuard`]'s stop flag.
///
/// Why: this bounds `KeyReaderGuard::drop`'s join to roughly one interval
/// after shutdown is requested, instead of the prior blocking-`read()`
/// design's "whenever the user next happens to press a key" (see the module
/// doc comment). Short enough that shutdown feels immediate; long enough not
/// to busy-loop.
const KEY_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Mouse-wheel scroll delta per notch, matching tagent's existing convention
/// (`crates/trusty-agents/src/repl/tui/run.rs`): negative scrolls toward
/// older history, positive toward newer.
const SCROLL_DELTA: isize = 3;

/// A render/reducer model the shared [`event_loop`]/[`run`] can drive
/// without knowing its concrete shape.
///
/// Why: [`event_loop`] needs exactly one piece of information it cannot get
/// from `ReplEvent` alone — whether the model just decided to quit (e.g. the
/// user typed `/quit`, hit `Ctrl-D`, or the engine returned `Ok(false)` from
/// `handle_input`). [`run`] (Slice 5, DOC-50 §5) needs two more: whether
/// `apply` just staged a line to submit or a cancel signal to relay, so it
/// can dispatch both to `TuiEngine` without `event_loop` itself needing to
/// know `TuiEngine` exists (keeping `event_loop` terminal-generic and
/// testable against `TestBackend` with no engine at all — see that
/// function's doc comment). Everything else about `M` stays opaque to this
/// crate; Slice 4 defines the real model ([`crate::app::ReplApp`]).
/// What: three methods, all defaulted so a minimal `TuiModel` (like this
/// module's own test-only `CountingModel`) compiles unchanged — only
/// `should_quit` is required to mean anything for [`event_loop`] alone to
/// work; the pending-take methods only matter to callers that go through
/// [`run`]'s engine-dispatch wrapper.
/// Test: [`crate::app::ReplApp`]'s implementation is exercised via
/// `crate::app::reduce::tests`; this module's own dispatch wiring is
/// exercised in [`tests::dispatch_pending`] below.
pub trait TuiModel {
    /// Whether the render loop should exit after the current frame.
    fn should_quit(&self) -> bool;

    /// Take the line staged by `apply` for `TuiEngine::handle_input`, if any.
    ///
    /// Why: `apply` (a synchronous `FnMut(&mut M, ReplEvent)`) cannot itself
    /// call an `async fn` on the engine, so it stages the line on `M`
    /// instead (mirrors [`crate::app::ReplApp::pending_submit`]); [`run`]
    /// drains it right after `apply` returns and dispatches it.
    /// What: a drain-on-read `Option::take`-shaped accessor. Default `None`
    /// — a model with nothing to submit (or one not wired through [`run`]
    /// at all) needs no override.
    fn take_pending_submit(&mut self) -> Option<String> {
        None
    }

    /// Take-and-clear the cancel signal staged by `apply` (Ctrl-C, or Up-
    /// arrow while busy), if any.
    ///
    /// Why: same shape as [`Self::take_pending_submit`], for
    /// `TuiEngine::cancel_session` instead of `handle_input`.
    /// What: default `false` — a model with no cancellation concept needs no
    /// override.
    fn take_pending_cancel(&mut self) -> bool {
        false
    }

    /// Called by [`run`]'s dispatch step immediately after a drained cancel
    /// signal, before the `cancel_session` call is even dispatched.
    ///
    /// Why: the actual `TuiEngine::cancel_session` RPC is async and runs on
    /// a spawned task (so a slow/hung backend never freezes the render
    /// loop), but the visible "busy" state should clear the moment the user
    /// asked to cancel, not whenever the RPC eventually resolves — direct
    /// parity with tagent's real cancel path
    /// (`crates/trusty-agents/src/repl/tui/events.rs::process_event`), which
    /// resets `thinking`/`busy_since` synchronously, before `h.abort()` even
    /// runs. DOC-50 §5 Slice 5's "blocks user input until cancel completes"
    /// is implemented literally, not reasoned away: `crate::app::ReplApp`'s
    /// `submit_line` refuses a second turn while `busy` is `true`
    /// ([`crate::app::ReplApp::submit_line`]'s doc comment), so this method
    /// clearing `busy` is exactly the moment new input becomes acceptable
    /// again — before that, Enter/Submit is a genuine no-op, not merely
    /// cosmetically blocked. [`dispatch_pending`] separately bumps the
    /// generation counter in the SAME cancel branch this method is called
    /// from, so any output still in flight from the just-cancelled turn is
    /// dropped rather than rendered once it eventually arrives (see that
    /// function's doc comment for the generation mechanism).
    /// What: default no-op — a model with no busy/streaming state to reset
    /// needs no override.
    fn on_cancelled(&mut self) {}

    /// Record the generation number [`dispatch_pending`] just assigned to a
    /// new turn (submit) or bumped past (cancel), so a later
    /// `ReplEvent::TurnFinished { generation }` can compare against it.
    ///
    /// Why: a prior revision had `dispatch_pending`'s spawned completion
    /// task load the live `AtomicU64` generation counter and compare it to
    /// its own turn's number before deciding whether to send
    /// `TurnFinished` — a genuine TOCTOU race under multi-threaded tokio
    /// (a cancel and a new submit could both run in the gap between that
    /// load and the send, so a stale terminal signal from turn N could
    /// clear `busy`/`streaming_idx` for a genuinely in-flight turn N+2).
    /// Fixed by construction instead of narrowing the window: this method
    /// is called from `dispatch_pending` ONLY on the single serial
    /// event-loop task (the same task `apply`/the reducer run on, and the
    /// same task that owns the `AtomicU64` bump) — so a model tracking its
    /// own copy of "the current generation" lets the REDUCER do the
    /// comparison with no cross-thread race possible at all, rather than a
    /// spawned task racing a shared counter.
    /// What: default no-op — a model with no generation concept (or one not
    /// wired through [`run`]) needs no override.
    fn set_current_generation(&mut self, generation: u64) {
        let _ = generation;
    }
}

/// Pure classification of one crossterm `Event` into an optional [`ReplEvent`].
///
/// Why: pulled out of [`spawn_key_reader`]'s thread body specifically so it
/// is unit-testable without a real TTY or `crossterm::event::read()` — the
/// thread itself is a thin loop that calls this once per polled event.
/// What: filters `Key` events to `Press`/`Repeat` only, dropping `Release`
/// (some terminals — notably on Windows — emit key-release events crossterm
/// surfaces there; tagent's precedent drops them so a single physical
/// keystroke doesn't fire twice) and translating the rest via
/// [`translate_key_event`]. `Resize` passes straight through. Mouse-wheel
/// `ScrollUp`/`ScrollDown` map to [`ReplEvent::Scroll`]. Everything else
/// (other mouse events, focus/paste events depending on which crossterm
/// event kinds are enabled) yields `None`.
/// Test: [`tests::classify_filters_key_release`],
/// [`tests::classify_maps_key_press_and_repeat`],
/// [`tests::classify_maps_resize`],
/// [`tests::classify_maps_mouse_scroll_up_and_down`],
/// [`tests::classify_ignores_other_mouse_events`].
fn classify(ev: CtEvent) -> Option<ReplEvent> {
    match ev {
        CtEvent::Key(k) => {
            if k.kind != KeyEventKind::Press && k.kind != KeyEventKind::Repeat {
                None
            } else {
                Some(ReplEvent::Key(translate_key_event(k)))
            }
        }
        CtEvent::Resize(cols, rows) => Some(ReplEvent::Resize(cols, rows)),
        CtEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            ..
        }) => Some(ReplEvent::Scroll(-SCROLL_DELTA)),
        CtEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            ..
        }) => Some(ReplEvent::Scroll(SCROLL_DELTA)),
        _ => None,
    }
}

/// RAII handle for the key-reader thread spawned by [`spawn_key_reader`].
///
/// Why: a bare `JoinHandle` let every fallible step between spawning the
/// thread and the function's tail cleanup code leak it — `engine.setup`/
/// `subscribe_workstream_events` returning via `?` skipped straight past the
/// `drop(tx); thread.join()` lines that used to live at the end of [`run`].
/// Wrapping the handle in a type whose `Drop` does that join means EVERY
/// exit path — success, an early `?`, or an unwinding panic — cleans the
/// thread up, the same guarantee [`TerminalGuard`] gives the terminal itself.
/// What: holds the thread's `JoinHandle` plus an `Arc<AtomicBool>` stop flag
/// the thread polls every [`KEY_POLL_INTERVAL`]. `Drop` sets the flag then
/// joins — bounded to roughly one poll interval, not "whenever the user
/// next presses a key" (the shutdown hang described in the module doc
/// comment).
/// Test: [`tests::key_reader_guard_drop_completes_promptly_without_a_keypress`].
pub struct KeyReaderGuard {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for KeyReaderGuard {
    /// Why: see [`KeyReaderGuard`] — runs on every exit path, including a
    /// panic unwind, which is the entire reason this type exists rather than
    /// a bare `JoinHandle` cleaned up by hand at the end of `run`.
    /// What: signals the stop flag, then joins — the thread notices within
    /// one [`KEY_POLL_INTERVAL`] because its loop condition checks the flag
    /// on every iteration, not just when a `send()` fails.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn the dedicated OS thread that polls for crossterm events and forwards
/// translated events onto `tx`, returning a [`KeyReaderGuard`] that stops and
/// joins it on drop.
///
/// Why: crossterm's blocking read is happiest on its own OS thread rather
/// than parking a tokio worker — matches
/// `crates/trusty-agents/src/repl/tui/run.rs`'s `key_thread`. Polling (rather
/// than that precedent's blocking `read()`) is what makes shutdown bounded
/// instead of tied to the next physical keystroke — see the module doc
/// comment for the hang this replaces.
/// What: loops `crossterm::event::poll(KEY_POLL_INTERVAL)` until the stop
/// flag is set, `poll`/`read` errors (e.g. no input reader available, which
/// is expected in a TTY-less sandbox and simply ends the loop), or `tx.send`
/// fails (the receiver dropped). Each polled event is classified by
/// [`classify`]; `None` results (filtered keys, unhandled mouse events) are
/// silently dropped.
/// Test: [`tests::key_reader_guard_drop_completes_promptly_without_a_keypress`]
/// proves the returned guard's `Drop` doesn't block on a keystroke; the
/// event-classification logic itself is covered directly via [`classify`]'s
/// tests (no TTY needed for those).
pub fn spawn_key_reader(tx: UnboundedSender<ReplEvent>) -> KeyReaderGuard {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let handle = std::thread::spawn(move || {
        while !stop_for_thread.load(Ordering::Relaxed) {
            match event::poll(KEY_POLL_INTERVAL) {
                Ok(true) => match event::read() {
                    Ok(ev) => {
                        if let Some(replay) = classify(ev)
                            && tx.send(replay).is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                },
                Ok(false) => continue, // timed out — loop back and re-check `stop`
                Err(_) => break,
            }
        }
    });
    KeyReaderGuard {
        stop,
        handle: Some(handle),
    }
}

/// The terminal-generic tick/event select loop.
///
/// Why: split out from [`run`] specifically so it can be exercised in tests
/// against `ratatui::backend::TestBackend` — `run` itself requires a real
/// TTY (via [`TerminalGuard::enter`]) and so cannot run in CI/sandboxes.
/// What: draws once immediately, then loops a biased `tokio::select!` between
/// the [`TICK`] interval (redraw only) and `rx.recv()` (apply the event via
/// `apply`, then redraw). Exits when `model.should_quit()` becomes true or
/// `rx` closes (all senders dropped — mirrors
/// `crates/trusty-agents/src/repl/tui/run.rs::event_loop`'s `None => return
/// Ok(())`). Returns the final model so callers/tests can inspect it.
/// Test: [`tests::event_loop_applies_events_and_redraws`],
/// [`tests::event_loop_stops_when_model_requests_quit`],
/// [`tests::event_loop_stops_when_channel_closes`],
/// [`tests::event_loop_redraws_on_tick_even_without_events`].
pub async fn event_loop<B, M>(
    terminal: &mut Terminal<B>,
    mut model: M,
    mut rx: UnboundedReceiver<ReplEvent>,
    mut apply: impl FnMut(&mut M, ReplEvent),
    mut render: impl FnMut(&mut ratatui::Frame, &M),
) -> anyhow::Result<M>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
    M: TuiModel,
{
    terminal.draw(|f| render(f, &model))?;

    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => {
                terminal.draw(|f| render(f, &model))?;
                if model.should_quit() {
                    return Ok(model);
                }
            }
            ev = rx.recv() => {
                match ev {
                    Some(ev) => {
                        apply(&mut model, ev);
                        terminal.draw(|f| render(f, &model))?;
                        if model.should_quit() {
                            return Ok(model);
                        }
                    }
                    None => return Ok(model),
                }
            }
        }
    }
}

/// The top-level entry point: enter the terminal, spawn the key reader, run
/// `engine.setup`/`subscribe_workstream_events`, drive [`event_loop`] to
/// completion, then restore the terminal and call `engine.shutdown`.
///
/// Why: bundles the pieces every real product binary needs in the order
/// tagent's `run_tui` already proves out, but routed through [`TerminalGuard`]
/// and [`KeyReaderGuard`] so a panic OR an early `?` anywhere in
/// `engine.setup`/`subscribe_workstream_events`/`apply`/`render` still
/// restores the terminal and stops the key-reader thread — see the module
/// doc comment for the hang and the leak this fixes relative to a prior
/// revision.
/// What: generic over `E: TuiEngine` (Slice 1's adapter trait) and `M:
/// TuiModel` (this module's minimal render/reducer contract) so it drives
/// either tagent's future adapter or tcode's. The fallible setup + the event
/// loop itself run inside one `async` block so `run` always reaches its two
/// explicit `drop()` calls afterward, in a fixed order, regardless of which
/// step inside failed: `guard` (terminal restore) drops FIRST — never gated
/// behind the thread join — then `key_guard` (bounded to roughly
/// [`KEY_POLL_INTERVAL`]) drops second. `engine.shutdown`'s error is logged,
/// not propagated, mirroring `TuiEngine::shutdown`'s own doc comment
/// ("errors are logged, not fatal").
/// Test: requires a real TTY (via `TerminalGuard::enter`) so it is not
/// unit-tested directly — the terminal-generic core it delegates to
/// ([`event_loop`]) carries the loop's test coverage, and [`TerminalGuard`]/
/// [`KeyReaderGuard`]'s own tests cover the panic-safety and prompt-shutdown
/// contracts this function relies on.
/// The in-flight `handle_input` task, if any — `std::sync::Mutex` (not
/// `tokio::sync::Mutex`) because [`dispatch_pending`] only ever locks it
/// synchronously (take-and-abort, or store-and-replace), never across an
/// `.await`, so the cheaper std primitive is correct and avoids an
/// accidental `.await` inside the lock.
type CurrentTask = Arc<StdMutex<Option<tokio::task::JoinHandle<()>>>>;

/// The current turn's generation number.
///
/// Why: `JoinHandle::abort()` only takes effect at the aborted task's NEXT
/// `.await` point — a chunk the engine already pushed onto its `tx` clone
/// BEFORE the abort request lands is already sitting in a channel buffer,
/// unaffected by the abort. Without a way to mark that chunk "stale", it
/// still gets forwarded to the render loop and rendered — a real bug a
/// code-review pass on PR #3477 caught: cancelling turn N doesn't stop turn
/// N's already-in-flight output from being drawn as a fresh reply once it
/// eventually arrives. This counter is the fix: every NEW submitted turn
/// gets the next generation number, cancelling bumps it too (invalidating
/// whatever generation was in flight even if nothing is found to abort), and
/// [`dispatch_pending`]'s per-turn forwarder (see that function) drops any
/// event whose turn's generation no longer matches the live one before it
/// ever reaches `tx` / the reducer.
/// What: a plain `Arc<AtomicU64>` — `SeqCst` throughout since this is a
/// low-frequency, cross-task correctness flag, not a hot path worth a
/// weaker ordering.
type Generation = Arc<std::sync::atomic::AtomicU64>;

/// Drain `turn_rx` and forward each event onto `real_tx`, but ONLY while
/// `generation` still equals `my_gen` — the per-turn forwarder [`Generation`]'s
/// doc comment describes. Pulled out of [`dispatch_pending`] as its own
/// function specifically so it is unit-testable in complete isolation from
/// real task scheduling: a test can pre-populate `turn_rx` with a message,
/// bump `generation` past `my_gen` (simulating "a cancel already happened"),
/// then run this function to completion and assert nothing reached
/// `real_tx` — deterministic, no race against when a spawned task happens to
/// get polled relative to another.
/// Why: exits on its own once every `turn_tx` clone (held by the
/// `handle_input` task in [`dispatch_pending`]) drops and `turn_rx.recv()`
/// returns `None` — normal completion or `abort()` both drop that clone, so
/// nothing leaks this task.
/// What: no special handling needed for the mismatched-generation branch
/// beyond "don't forward" — silently dropping is the correct, documented
/// behavior (rendering stale output from an already-cancelled turn would be
/// the bug, not the silence). `saw_terminal` is flipped to `true` whenever a
/// relayed (generation still matching) event is `AssistantOutput { done:
/// true, .. }` or `Quit` — [`dispatch_pending`]'s completion step reads it,
/// after this function has fully drained, to detect a `handle_input` that
/// returned `Ok(true)` without ever producing a terminal signal (see
/// `ReplEvent::TurnFinished`'s doc comment for the stuck-`busy` deadlock
/// that gap causes). A message dropped for generation mismatch does NOT set
/// `saw_terminal` — that turn was already superseded/cancelled, and
/// `TuiModel::on_cancelled` already reset `busy` for it independently.
/// Test: [`tests::forward_while_current_generation_drops_stale_generation_message`],
/// [`tests::forward_while_current_generation_forwards_matching_generation_message`],
/// [`tests::forward_while_current_generation_flags_terminal_assistant_output`],
/// [`tests::forward_while_current_generation_flags_quit`],
/// [`tests::forward_while_current_generation_does_not_flag_non_terminal_events`].
async fn forward_while_current_generation(
    mut turn_rx: UnboundedReceiver<ReplEvent>,
    real_tx: UnboundedSender<ReplEvent>,
    generation: Generation,
    my_gen: u64,
    saw_terminal: Arc<AtomicBool>,
) {
    while let Some(ev) = turn_rx.recv().await {
        if generation.load(Ordering::SeqCst) == my_gen {
            if matches!(
                &ev,
                ReplEvent::AssistantOutput { done: true, .. } | ReplEvent::Quit
            ) {
                saw_terminal.store(true, Ordering::SeqCst);
            }
            let _ = real_tx.send(ev);
        }
    }
}

/// Drain whatever [`TuiModel::take_pending_submit`]/
/// [`TuiModel::take_pending_cancel`] report on `model` (staged by the
/// caller's `apply` immediately before this runs) and dispatch each to
/// `engine`, per DOC-50 §5 Slice 5's `process_event(event, app, handler)`
/// deliverable — generalized here to two drains instead of one dispatch
/// function, matching this crate's `pending_submit`/`pending_cancel` split
/// (see [`crate::app::reduce`]'s doc comment) rather than tagent's single
/// `Option<String>` return.
///
/// Why: `apply` is a synchronous `FnMut`, so it cannot itself `.await`
/// `TuiEngine::handle_input`/`cancel_session` — this is the seam that picks
/// up where `apply` stopped, mirroring tagent's `process_event`
/// (`crates/trusty-agents/src/repl/tui/events.rs`): spawn `handle_input` on
/// its own task (so the render loop keeps redrawing while a request is in
/// flight) and stash the `JoinHandle` in `current_task` so a later cancel
/// can abort it — same `current_task: Arc<Mutex<Option<JoinHandle<()>>>>`
/// shape tagent uses, adapted to `std::sync::Mutex` since nothing here holds
/// the lock across an `.await`.
///
/// **Generation tagging (see [`Generation`]):** every submit is assigned the
/// next generation number and gets its OWN private `mpsc` channel
/// (`turn_tx`/`turn_rx`) instead of the shared `tx` directly — `engine.
/// handle_input` is handed `turn_tx`, and a second, small forwarder task
/// drains `turn_rx` and re-sends each event onto the real `tx` ONLY while
/// `generation` still equals the turn's own number; once superseded
/// (cancelled), the forwarder silently drops everything still queued for
/// that turn instead of relaying it. This works without changing
/// `TuiEngine::handle_input`'s signature at all (it still receives a plain
/// `UnboundedSender<ReplEvent>`) and without adding a generation field to
/// `ReplEvent` itself (irrelevant to `event.rs`'s public shape, so Slice 6/7
/// engines never need to know this mechanism exists). The forwarder task
/// exits on its own once every `turn_tx` clone drops (the `handle_input`
/// task finishing OR being aborted both drop their clone), so nothing leaks.
///
/// **Why FIX 1 (busy-gating in `ReplApp::submit_line`) doesn't make this
/// redundant:** with submits gated, at most one turn is EVER in flight, so
/// generation mismatches can only happen via cancel (Ctrl-C), never via a
/// second overlapping submit — but that one case is real and common enough
/// (any Ctrl-C during a streaming response) that dropping it silently is
/// still required.
///
/// What: cancel is drained and dispatched FIRST — bumps `generation`
/// (invalidating the in-flight turn's forwarder before anything else
/// happens), calls [`TuiModel::on_cancelled`] synchronously so the UI's busy
/// state clears immediately (see that method's doc comment for why), aborts
/// the stashed `JoinHandle`, then relays `engine.cancel_session()` on its
/// own task so a slow backend never blocks the render loop. Submit is
/// drained second: assigned the next generation, any previous task in
/// `current_task` is aborted (defensive — busy-gating should prevent
/// overlapping submits, same caveat as tagent's precedent) and replaced.
/// `Ok(false)` from `handle_input` is relayed back as `ReplEvent::Quit` (the
/// spawned task can't reach `&mut M` directly — see that variant's doc
/// comment); an `Err` is relayed as an error-flavored `ReplEvent::
/// AssistantOutput` chunk, the closest existing vocabulary to tagent's
/// `LlmResponse { text: format!("error: {e:#}"), is_error: true }`. Both
/// relays go through the SAME generation-tagged `turn_tx`, so a stale
/// `Quit`/error from an already-cancelled turn is dropped exactly like a
/// stale `AssistantOutput` chunk would be.
///
/// **Stuck-`busy` safety net (post-review fix, PR #3477):** `ReplApp::busy`
/// is cleared ONLY by a terminal `AssistantOutput { done: true, .. }`/`Quit`
/// reaching the reducer — but `handle_input` returning `Ok(true)` is NOT a
/// guarantee one was ever sent (three real `CodeEngine` paths legitimately
/// return `Ok(true)` having pushed only a `StatusMessage`/`WorkstreamUpdated`:
/// `/workstream list`/`activate`, a reconnect-exhausted event pump, and a
/// daemon-initiated session cancellation). Combined with FIX 1's busy-gating,
/// that left `busy` stuck forever — input permanently bricked. The fix:
/// after `handle_input` resolves (`Ok`/`Err`, any branch), the completion
/// code drops its `turn_tx` clone and awaits the forwarder's `JoinHandle` —
/// this is the synchronization point that guarantees `saw_terminal`
/// reflects everything the engine actually sent, not a racy snapshot taken
/// the instant `handle_input`'s future resolves. If `saw_terminal` is still
/// `false`, a `ReplEvent::TurnFinished { generation: my_gen }` is sent
/// unconditionally on the real `tx` — see that variant's doc comment for why
/// it is a dedicated variant rather than a reused empty `AssistantOutput {
/// done: true, .. }` (the latter would push a stray blank chat entry via
/// that event's `None`-`streaming_idx` branch).
///
/// **By-construction TOCTOU fix (second re-review round):** an earlier
/// revision ALSO compared `generation.load(..) == my_gen` on this spawned
/// task before deciding whether to send `TurnFinished` at all — a genuine
/// race under multi-threaded tokio (a cancel + a new submit could both run
/// in the gap between that load and the send, letting a stale terminal from
/// turn N clear state for a genuinely in-flight turn N+2). Fixed by moving
/// the comparison into the REDUCER instead of narrowing the window: this
/// task now always sends `TurnFinished` stamped with its own `my_gen`, and
/// `dispatch_pending` calls `model.set_current_generation` on THIS (serial,
/// single event-loop) task at the exact point it bumps `generation` (both
/// branches above) — so the reducer's later `generation ==
/// app.current_generation` compare (`ReplApp`'s implementation) runs
/// serially against a value only ever written from the same task that reads
/// it back, with no cross-thread race possible at all.
/// Net invariant: every `handle_input` completion path — `Ok(true)` with a
/// done chunk, `Ok(true)` with none, `Ok(false)`, and `Err` — deterministically
/// leaves `busy == false` once no task is in flight, and a stale completion
/// from a superseded turn never clobbers a newer one.
/// Test: [`tests::dispatch_pending_submit_reaches_handle_input`],
/// [`tests::dispatch_pending_cancel_reaches_cancel_session`],
/// [`tests::dispatch_pending_cancel_aborts_genuinely_in_flight_submit_task`],
/// [`tests::dispatch_pending_noop_when_nothing_pending`],
/// [`tests::forward_while_current_generation_drops_stale_generation_message`],
/// [`tests::dispatch_pending_second_submit_while_busy_does_not_start_second_task`],
/// [`tests::dispatch_pending_ok_true_with_no_terminal_output_still_clears_busy`],
/// [`tests::dispatch_pending_ok_true_with_only_status_message_still_clears_busy`],
/// [`tests::dispatch_pending_err_clears_busy_and_surfaces_visible_error`].
fn dispatch_pending<E, M>(
    model: &mut M,
    engine: &Arc<E>,
    tx: &UnboundedSender<ReplEvent>,
    current_task: &CurrentTask,
    generation: &Generation,
) where
    E: TuiEngine + 'static,
    M: TuiModel,
{
    if model.take_pending_cancel() {
        // Bump FIRST: invalidates the in-flight turn's forwarder before the
        // abort even runs, so a chunk already sitting in `turn_rx`'s buffer
        // (pushed before `abort()` takes effect at the task's next
        // `.await`) is dropped rather than relayed once the forwarder next
        // polls it. `set_current_generation` runs on THIS (serial,
        // event-loop) task, same as every reducer `apply` call — see
        // `TuiModel::set_current_generation`'s doc comment for why that
        // makes the model's copy of "the live generation" race-free against
        // a spawned completion task's `TurnFinished` send.
        let new_gen = generation.fetch_add(1, Ordering::SeqCst) + 1;
        model.set_current_generation(new_gen);
        model.on_cancelled();
        if let Some(handle) = current_task.lock().unwrap().take() {
            handle.abort();
        }
        let engine = Arc::clone(engine);
        let tx = tx.clone();
        tokio::spawn(async move {
            if let Err(e) = engine.cancel_session().await {
                let _ = tx.send(ReplEvent::StatusMessage(format!("cancel failed: {e:#}")));
            }
        });
    }

    if let Some(line) = model.take_pending_submit() {
        let my_gen = generation.fetch_add(1, Ordering::SeqCst) + 1;
        model.set_current_generation(my_gen);

        // Private per-turn channel: `engine.handle_input` only ever sees
        // `turn_tx`, never the real `tx` — see the doc comment above for why
        // this is the seam that makes stale output droppable without
        // touching `TuiEngine`'s signature or `ReplEvent`'s shape.
        let (turn_tx, turn_rx) = mpsc::unbounded_channel::<ReplEvent>();
        let real_tx = tx.clone();
        let forwarder_generation = Arc::clone(generation);
        let saw_terminal = Arc::new(AtomicBool::new(false));
        let forwarder_saw_terminal = Arc::clone(&saw_terminal);
        let forwarder_handle = tokio::spawn(forward_while_current_generation(
            turn_rx,
            real_tx,
            forwarder_generation,
            my_gen,
            forwarder_saw_terminal,
        ));

        let engine = Arc::clone(engine);
        let completion_tx = tx.clone();
        let handle = tokio::spawn(async move {
            let result = engine.handle_input(line, turn_tx.clone()).await;
            match &result {
                Ok(true) => {}
                Ok(false) => {
                    let _ = turn_tx.send(ReplEvent::Quit);
                }
                Err(e) => {
                    // Surface the error as a visible chat entry (also
                    // clears `busy` via `done: true` — see
                    // `apply_assistant_output`) rather than leaving the
                    // failure silent.
                    let _ = turn_tx.send(ReplEvent::AssistantOutput {
                        chunk: format!("error: {e:#}"),
                        done: true,
                        is_error: true,
                    });
                }
            }
            // Drop our own `turn_tx` clone BEFORE awaiting the forwarder:
            // its channel only closes (and `forward_while_current_generation`
            // only returns) once every sender clone — including the one
            // `engine.handle_input` held, already dropped when its future
            // resolved above — is gone. Awaiting first would deadlock.
            drop(turn_tx);
            let _ = forwarder_handle.await;
            // Safety net: `handle_input` returning `Ok(true)` is not a
            // guarantee a terminal signal was ever sent (see this
            // function's doc comment for the real `CodeEngine` paths that
            // don't). Top one up so `busy` cannot stay stuck forever.
            //
            // Deliberately UNCONDITIONAL on the live generation here — no
            // load-and-compare against the shared `AtomicU64` on this
            // spawned task. An earlier revision did that compare here,
            // which was a genuine TOCTOU race under multi-threaded tokio: a
            // cancel + a new submit could both run in the gap between this
            // task's load and its send, so a stale `TurnFinished` from turn
            // N could still clear `busy`/`streaming_idx` for a genuinely
            // in-flight turn N+2. Fixed by construction instead: this event
            // always carries `my_gen`, and the REDUCER (serial, same task
            // that bumps the counter) is what decides whether `generation`
            // still matches — see `ReplEvent::TurnFinished`'s and
            // `TuiModel::set_current_generation`'s doc comments.
            if !saw_terminal.load(Ordering::SeqCst) {
                let _ = completion_tx.send(ReplEvent::TurnFinished { generation: my_gen });
            }
        });
        let mut slot = current_task.lock().unwrap();
        if let Some(prev) = slot.take() {
            prev.abort();
        }
        *slot = Some(handle);
    }
}

pub async fn run<E, M>(
    engine: Arc<E>,
    model: M,
    mut apply: impl FnMut(&mut M, ReplEvent),
    render: impl FnMut(&mut ratatui::Frame, &M),
) -> anyhow::Result<()>
where
    E: TuiEngine + 'static,
    M: TuiModel,
{
    let (guard, mut terminal) = TerminalGuard::enter()?;
    let (tx, rx) = mpsc::unbounded_channel::<ReplEvent>();
    let key_guard = spawn_key_reader(tx.clone());
    let current_task: CurrentTask = Arc::new(StdMutex::new(None));
    let generation: Generation = Arc::new(std::sync::atomic::AtomicU64::new(0));

    let result: anyhow::Result<M> = async {
        engine.setup(tx.clone()).await?;
        engine.subscribe_workstream_events(tx.clone()).await?;

        // Wrap the caller's `apply` so every event still gets the caller's
        // reducer behavior first, then drains whatever `ReplApp`-shaped
        // pending state (Slice 5, DOC-50 §5) that reducer just staged and
        // dispatches it to `engine` — see [`dispatch_pending`] and
        // [`TuiModel::take_pending_submit`]/[`TuiModel::take_pending_cancel`]
        // for why this can't live inside `event_loop` itself (kept
        // engine-agnostic and `TestBackend`-testable).
        let dispatch_engine = Arc::clone(&engine);
        let dispatch_tx = tx.clone();
        let dispatching_apply = move |model: &mut M, ev: ReplEvent| {
            apply(model, ev);
            dispatch_pending(
                model,
                &dispatch_engine,
                &dispatch_tx,
                &current_task,
                &generation,
            );
        };

        event_loop(&mut terminal, model, rx, dispatching_apply, render).await
    }
    .await;

    // Restore the terminal FIRST and unconditionally — regardless of which
    // step above failed, and never gated behind the key-reader thread's
    // join below (that ordering, with an unbounded join, was the shutdown
    // hang described in the module doc comment).
    drop(guard);
    // Signal + join the key-reader thread second. Bounded to roughly
    // `KEY_POLL_INTERVAL` thanks to `spawn_key_reader`'s poll-based loop, so
    // this no longer waits for a keystroke that may never come.
    drop(key_guard);

    if let Err(e) = engine.shutdown().await {
        tracing::warn!(error = %e, "TuiEngine::shutdown returned an error (non-fatal)");
    }

    result.map(|_model| ())
}

#[cfg(test)]
mod tests;
