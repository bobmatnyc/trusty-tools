//! Tests for `crate::run` (`event_loop`/`run`/`classify`/`spawn_key_reader`/
//! `dispatch_pending`), split into its own file to satisfy the 500-SLOC
//! production-file cap (`scripts/check_line_cap.sh`) — mirrors the precedent
//! set by `crate::app::reduce`/`crate::app::reduce::tests`. Reachable via
//! `crate::run::tests` exactly as an inline `mod tests { ... }` would have
//! been; only the file boundary changed.

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

// ─────────────────────────────────────────────────────────────────────────
// `dispatch_pending` — Slice 5 (DOC-50 §5): the Ctrl-C → engine.cancel_session
// / Submit → engine.handle_input wiring. Uses `crate::app::ReplApp` (the
// real `TuiModel` these methods exist for) plus a `MockEngine` that records
// which methods were called, rather than `CountingModel` above (which never
// overrides `take_pending_submit`/`take_pending_cancel`, so it can't exercise
// this path at all).
// ─────────────────────────────────────────────────────────────────────────

use crate::app::{ReplApp, apply as app_apply};
use crate::event::{KeyCode, KeyInput, KeyModifiers};

#[derive(Default)]
struct MockEngine {
    cancel_calls: AtomicUsize,
    handle_input_calls: AtomicUsize,
    /// When set, `handle_input` returns this instead of `Ok(true)` — lets a
    /// test exercise the `Ok(false)` → `ReplEvent::Quit` relay.
    handle_input_returns_quit: AtomicBool,
}

#[async_trait]
impl TuiEngine for MockEngine {
    async fn handle_input(
        &self,
        _line: String,
        _tx: UnboundedSender<ReplEvent>,
    ) -> anyhow::Result<bool> {
        self.handle_input_calls.fetch_add(1, Ordering::SeqCst);
        Ok(!self.handle_input_returns_quit.load(Ordering::SeqCst))
    }

    async fn setup(&self, _tx: UnboundedSender<ReplEvent>) -> anyhow::Result<()> {
        Ok(())
    }

    async fn cancel_session(&self) -> anyhow::Result<()> {
        self.cancel_calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn ctrl_c() -> ReplEvent {
    ReplEvent::Key(KeyInput {
        code: KeyCode::Char('c'),
        modifiers: KeyModifiers {
            ctrl: true,
            alt: false,
            shift: false,
        },
    })
}

/// The acceptance criterion this whole slice exists for: pressing Ctrl-C
/// must reach `TuiEngine::cancel_session`, not just clear local UI state
/// (thin-client axiom, DOC-50 §5 Slice 5). Drives the real reducer
/// (`crate::app::apply`) so this proves the FULL path — key press to
/// `pending_cancel` to `dispatch_pending` to the engine — not just
/// `dispatch_pending` in isolation.
#[tokio::test]
async fn dispatch_pending_cancel_reaches_cancel_session() {
    let engine = Arc::new(MockEngine::default());
    let mut app = ReplApp::new("demo", "u");
    app.busy = true;

    app_apply(&mut app, ctrl_c());
    assert!(app.pending_cancel, "reducer must stage pending_cancel");

    let (tx, _rx) = mpsc::unbounded_channel::<ReplEvent>();
    let current_task: CurrentTask = Arc::new(StdMutex::new(None));
    dispatch_pending(&mut app, &engine, &tx, &current_task);

    assert!(!app.pending_cancel, "must be drained synchronously");
    assert!(!app.busy, "on_cancelled must clear busy synchronously");

    // The RPC itself runs on a spawned task; wait for it to actually run
    // rather than asserting immediately after `dispatch_pending` returns.
    for _ in 0..200 {
        if engine.cancel_calls.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        engine.cancel_calls.load(Ordering::SeqCst),
        1,
        "Ctrl-C must reach engine.cancel_session() exactly once"
    );
}

/// `ReplEvent::Submit`/Enter staging a line must reach
/// `TuiEngine::handle_input`.
#[tokio::test]
async fn dispatch_pending_submit_reaches_handle_input() {
    let engine = Arc::new(MockEngine::default());
    let mut app = ReplApp::new("demo", "u");
    app_apply(&mut app, ReplEvent::Submit("hello".to_string()));
    assert_eq!(app.pending_submit.as_deref(), Some("hello"));

    let (tx, _rx) = mpsc::unbounded_channel::<ReplEvent>();
    let current_task: CurrentTask = Arc::new(StdMutex::new(None));
    dispatch_pending(&mut app, &engine, &tx, &current_task);
    assert!(
        app.pending_submit.is_none(),
        "must be drained synchronously"
    );

    for _ in 0..200 {
        if engine.handle_input_calls.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(engine.handle_input_calls.load(Ordering::SeqCst), 1);
}

/// `handle_input` returning `Ok(false)` must relay `ReplEvent::Quit` onto
/// the channel so the render loop's next `apply` sets `ReplApp::quit` —
/// the only way a spawned task can reach model state it doesn't own.
#[tokio::test]
async fn dispatch_pending_submit_ok_false_relays_quit_event() {
    let engine = Arc::new(MockEngine::default());
    engine
        .handle_input_returns_quit
        .store(true, Ordering::SeqCst);
    let mut app = ReplApp::new("demo", "u");
    app_apply(&mut app, ReplEvent::Submit("bye".to_string()));

    let (tx, mut rx) = mpsc::unbounded_channel::<ReplEvent>();
    let current_task: CurrentTask = Arc::new(StdMutex::new(None));
    dispatch_pending(&mut app, &engine, &tx, &current_task);

    let ev = rx.recv().await.expect("Quit event must be sent");
    assert_eq!(ev, ReplEvent::Quit);
}

/// A cancel dispatched while a submit task is already in flight must abort
/// that task's `JoinHandle` — mirrors tagent's `current_task` abort-on-cancel
/// precedent (`crates/trusty-agents/src/repl/tui/events.rs`).
#[tokio::test]
async fn dispatch_pending_cancel_aborts_in_flight_submit_task() {
    let engine = Arc::new(MockEngine::default());
    let mut app = ReplApp::new("demo", "u");
    let (tx, _rx) = mpsc::unbounded_channel::<ReplEvent>();
    let current_task: CurrentTask = Arc::new(StdMutex::new(None));

    app_apply(&mut app, ReplEvent::Submit("long task".to_string()));
    dispatch_pending(&mut app, &engine, &tx, &current_task);
    assert!(
        current_task.lock().unwrap().is_some(),
        "submit must stash a JoinHandle in current_task"
    );

    app.busy = true;
    app_apply(&mut app, ctrl_c());
    dispatch_pending(&mut app, &engine, &tx, &current_task);

    assert!(
        current_task.lock().unwrap().is_none(),
        "cancel must take (and abort) the stashed handle"
    );
}

/// Neither `handle_input` nor `cancel_session` is called when nothing was
/// staged — `dispatch_pending` must be a true no-op on an idle model,
/// matching every plain keystroke (insert/backspace/cursor-move) that
/// doesn't set either pending flag.
#[tokio::test]
async fn dispatch_pending_noop_when_nothing_pending() {
    let engine = Arc::new(MockEngine::default());
    let mut app = ReplApp::new("demo", "u");
    app_apply(
        &mut app,
        ReplEvent::Key(KeyInput {
            code: KeyCode::Char('h'),
            modifiers: KeyModifiers::default(),
        }),
    );

    let (tx, _rx) = mpsc::unbounded_channel::<ReplEvent>();
    let current_task: CurrentTask = Arc::new(StdMutex::new(None));
    dispatch_pending(&mut app, &engine, &tx, &current_task);

    // Give any wrongly-spawned task a chance to run before asserting.
    tokio::task::yield_now().await;
    assert_eq!(engine.cancel_calls.load(Ordering::SeqCst), 0);
    assert_eq!(engine.handle_input_calls.load(Ordering::SeqCst), 0);
}
