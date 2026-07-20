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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// A render/reducer model the shared [`event_loop`] can drive without
/// knowing its shape.
///
/// Why: the loop needs exactly one piece of information it cannot get from
/// `ReplEvent` alone — whether the model just decided to quit (e.g. the user
/// typed `/quit`, hit `Ctrl-D`, or the engine returned `Ok(false)` from
/// `handle_input`). Everything else about `M` is opaque to this crate; Slice
/// 4 defines the real model.
/// What: a single predicate, checked after every tick redraw and every
/// processed event.
pub trait TuiModel {
    /// Whether the render loop should exit after the current frame.
    fn should_quit(&self) -> bool;
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
pub async fn run<E, M>(
    engine: Arc<E>,
    model: M,
    apply: impl FnMut(&mut M, ReplEvent),
    render: impl FnMut(&mut ratatui::Frame, &M),
) -> anyhow::Result<()>
where
    E: TuiEngine + 'static,
    M: TuiModel,
{
    let (guard, mut terminal) = TerminalGuard::enter()?;
    let (tx, rx) = mpsc::unbounded_channel::<ReplEvent>();
    let key_guard = spawn_key_reader(tx.clone());

    let result: anyhow::Result<M> = async {
        engine.setup(tx.clone()).await?;
        engine.subscribe_workstream_events(tx.clone()).await?;
        event_loop(&mut terminal, model, rx, apply, render).await
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
mod tests {
    use super::*;
    use async_trait::async_trait;
    use crossterm::event::{
        KeyCode as CtKeyCode, KeyEvent, KeyEventState, KeyModifiers as CtKeyModifiers,
    };
    use ratatui::backend::TestBackend;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;
    use tokio::time::{self, Duration as TokioDuration};

    #[derive(Default)]
    struct CountingModel {
        events_seen: usize,
        quit: bool,
    }

    impl TuiModel for CountingModel {
        fn should_quit(&self) -> bool {
            self.quit
        }
    }

    fn render_counts(f: &mut ratatui::Frame, model: &CountingModel) {
        use ratatui::widgets::Paragraph;
        f.render_widget(Paragraph::new(format!("{}", model.events_seen)), f.area());
    }

    /// A dummy `TuiEngine` — these tests exercise [`event_loop`] directly
    /// (not [`run`]), so no engine method is actually invoked; it exists only
    /// to satisfy trait bounds shared with `run`'s signature in case a future
    /// test grows to call it.
    struct NoopEngine;

    #[async_trait]
    impl TuiEngine for NoopEngine {
        async fn handle_input(
            &self,
            _line: String,
            _tx: UnboundedSender<ReplEvent>,
        ) -> anyhow::Result<bool> {
            Ok(true)
        }

        async fn setup(&self, _tx: UnboundedSender<ReplEvent>) -> anyhow::Result<()> {
            Ok(())
        }
    }

    fn key_event(code: CtKeyCode, kind: KeyEventKind) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: CtKeyModifiers::NONE,
            kind,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn classify_maps_key_press_and_repeat() {
        let press = classify(CtEvent::Key(key_event(
            CtKeyCode::Char('a'),
            KeyEventKind::Press,
        )));
        assert!(matches!(press, Some(ReplEvent::Key(_))));

        let repeat = classify(CtEvent::Key(key_event(
            CtKeyCode::Char('a'),
            KeyEventKind::Repeat,
        )));
        assert!(matches!(repeat, Some(ReplEvent::Key(_))));
    }

    #[test]
    fn classify_filters_key_release() {
        let release = classify(CtEvent::Key(key_event(
            CtKeyCode::Char('a'),
            KeyEventKind::Release,
        )));
        assert_eq!(
            release, None,
            "Release must not fire a second ReplEvent::Key"
        );
    }

    #[test]
    fn classify_maps_resize() {
        assert_eq!(
            classify(CtEvent::Resize(120, 40)),
            Some(ReplEvent::Resize(120, 40))
        );
    }

    #[test]
    fn classify_maps_mouse_scroll_up_and_down() {
        let up = classify(CtEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: CtKeyModifiers::NONE,
        }));
        assert_eq!(up, Some(ReplEvent::Scroll(-SCROLL_DELTA)));

        let down = classify(CtEvent::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: CtKeyModifiers::NONE,
        }));
        assert_eq!(down, Some(ReplEvent::Scroll(SCROLL_DELTA)));
    }

    #[test]
    fn classify_ignores_other_mouse_events() {
        let moved = classify(CtEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: CtKeyModifiers::NONE,
        }));
        assert_eq!(moved, None);
    }

    /// The regression test for the shutdown hang: [`spawn_key_reader`]'s
    /// returned guard must join promptly on `Drop` WITHOUT needing a
    /// keystroke to unblock a `read()` call. Bounded to a few multiples of
    /// [`KEY_POLL_INTERVAL`] with generous headroom so this stays reliable
    /// across real TTYs (where `poll` genuinely waits out the interval) and
    /// TTY-less sandboxes (where `poll` errors immediately) alike — the
    /// prior blocking-`read()` design had no such bound at all.
    #[test]
    fn key_reader_guard_drop_completes_promptly_without_a_keypress() {
        let (tx, _rx) = mpsc::unbounded_channel::<ReplEvent>();
        let guard = spawn_key_reader(tx);

        let start = Instant::now();
        drop(guard);
        let elapsed = start.elapsed();

        assert!(
            elapsed < Duration::from_secs(2),
            "KeyReaderGuard::drop must not block on a keystroke; took {:?}",
            elapsed
        );
    }

    #[tokio::test]
    async fn event_loop_applies_events_and_redraws() {
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).expect("construct terminal");
        let (tx, rx) = mpsc::unbounded_channel::<ReplEvent>();

        tx.send(ReplEvent::StatusMessage("one".into())).unwrap();
        tx.send(ReplEvent::StatusMessage("two".into())).unwrap();
        drop(tx); // closes the channel so the loop exits after both events

        let model = event_loop(
            &mut terminal,
            CountingModel::default(),
            rx,
            |m, _ev| {
                m.events_seen += 1;
            },
            render_counts,
        )
        .await
        .expect("event_loop must succeed");

        assert_eq!(model.events_seen, 2);
    }

    #[tokio::test]
    async fn event_loop_stops_when_model_requests_quit() {
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).expect("construct terminal");
        let (tx, rx) = mpsc::unbounded_channel::<ReplEvent>();

        // Send far more events than we expect to be processed — the loop
        // must stop as soon as `should_quit()` flips, not drain the channel.
        for _ in 0..10 {
            tx.send(ReplEvent::Cancel).unwrap();
        }

        let model = event_loop(
            &mut terminal,
            CountingModel::default(),
            rx,
            |m, _ev| {
                m.events_seen += 1;
                if m.events_seen == 1 {
                    m.quit = true;
                }
            },
            render_counts,
        )
        .await
        .expect("event_loop must succeed");

        assert_eq!(
            model.events_seen, 1,
            "must stop at the first quit-triggering event"
        );
    }

    #[tokio::test]
    async fn event_loop_stops_when_channel_closes() {
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).expect("construct terminal");
        let (tx, rx) = mpsc::unbounded_channel::<ReplEvent>();
        drop(tx);

        let model = event_loop(
            &mut terminal,
            CountingModel::default(),
            rx,
            |m, _ev| m.events_seen += 1,
            render_counts,
        )
        .await
        .expect("event_loop must succeed even with zero events");

        assert_eq!(model.events_seen, 0);
        assert!(!model.quit);
    }

    #[tokio::test(start_paused = true)]
    async fn event_loop_redraws_on_tick_even_without_events() {
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).expect("construct terminal");
        let (tx, rx) = mpsc::unbounded_channel::<ReplEvent>();

        let redraw_count = Arc::new(AtomicUsize::new(0));
        let redraw_count_in_render = redraw_count.clone();

        // Quit after the 3rd tick-driven redraw so the loop terminates
        // deterministically under paused time.
        let handle = tokio::spawn(async move {
            event_loop(
                &mut terminal,
                CountingModel::default(),
                rx,
                move |m, _ev| m.events_seen += 1,
                move |f, m| {
                    redraw_count_in_render.fetch_add(1, Ordering::SeqCst);
                    render_counts(f, m);
                },
            )
            .await
        });

        // Let the initial draw happen, then advance paused time past three
        // tick intervals; each tick redraws without needing an event.
        time::sleep(TokioDuration::from_millis(1)).await;
        for _ in 0..3 {
            time::advance(TICK).await;
        }
        // One more advance so the final scheduled tick actually fires before
        // we close the channel and let the loop exit on `None`.
        time::advance(TICK).await;
        drop(tx);

        let _model = handle
            .await
            .expect("task must not panic")
            .expect("loop must succeed");
        // Initial draw (1) + at least the ticks we advanced past (4, since a
        // `Skip`-behavior interval still fires once per elapsed period here
        // because we only advance one tick at a time) — assert a lower bound
        // rather than an exact count to avoid coupling the test to
        // `tokio::time::interval`'s internal scheduling.
        assert!(
            redraw_count.load(Ordering::SeqCst) >= 4,
            "expected at least 4 redraws (1 initial + 3 ticks), got {}",
            redraw_count.load(Ordering::SeqCst)
        );
    }

    /// Compile-time check that `run`'s generic bounds are satisfiable by a
    /// real `TuiEngine` + `TuiModel` pair — exercised only for the type
    /// signature; `run` itself requires a TTY so it is not called (and not
    /// invoked) here, only referenced in a dead-code path the compiler still
    /// has to type-check.
    #[allow(dead_code)]
    async fn run_type_checks(engine: Arc<NoopEngine>) -> anyhow::Result<()> {
        run(
            engine,
            CountingModel::default(),
            |m: &mut CountingModel, _ev| m.events_seen += 1,
            render_counts,
        )
        .await
    }
}
