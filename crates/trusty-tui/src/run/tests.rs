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
use std::sync::atomic::AtomicU64;
use tokio::sync::Notify;

#[derive(Default)]
struct MockEngine {
    cancel_calls: AtomicUsize,
    handle_input_calls: AtomicUsize,
    /// When set, `handle_input` returns this instead of `Ok(true)` — lets a
    /// test exercise the `Ok(false)` → `ReplEvent::Quit` relay.
    handle_input_returns_quit: AtomicBool,
    /// When `Some`, `handle_input` sends one `AssistantOutput` chunk, sets
    /// [`Self::chunk_sent`], and then blocks on this `Notify` before
    /// returning — a controllable await point so a test can prove the task
    /// is genuinely mid-poll (parked at an `.await`, not merely spawned-and-
    /// never-scheduled) at the moment it gets aborted. `None` (the default)
    /// keeps every other test's fire-and-return-immediately behavior
    /// unchanged. See issue raised on PR #3477: the original
    /// `dispatch_pending_cancel_aborts_in_flight_submit_task` proved nothing
    /// because a `MockEngine` with no internal `.await` is never actually
    /// polled by a current-thread runtime before `abort()` lands, making
    /// "genuinely interrupted" indistinguishable from "never started".
    hold_until: Option<Arc<Notify>>,
    /// Set by `handle_input` immediately after sending its chunk and before
    /// waiting on [`Self::hold_until`] — a test polls this (not a fixed
    /// sleep) to know the task has reached its await point.
    chunk_sent: AtomicBool,
    /// Set by `handle_input` only AFTER `hold_until`'s wait resolves — never
    /// set at all if the task is aborted while parked there. A test that
    /// observes `chunk_sent == true` (task genuinely reached the await
    /// point) followed by `completed` staying `false` forever (even after
    /// further scheduling opportunities, with `notify` never released) has
    /// proven the task was truly interrupted mid-flight, not merely
    /// unpolled.
    completed: AtomicBool,
}

#[async_trait]
impl TuiEngine for MockEngine {
    async fn handle_input(
        &self,
        _line: String,
        tx: UnboundedSender<ReplEvent>,
    ) -> anyhow::Result<bool> {
        self.handle_input_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(notify) = &self.hold_until {
            let _ = tx.send(ReplEvent::AssistantOutput {
                chunk: "partial".to_string(),
                done: false,
                is_error: false,
            });
            self.chunk_sent.store(true, Ordering::SeqCst);
            notify.notified().await;
            self.completed.store(true, Ordering::SeqCst);
        }
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

/// Shorthand for a fresh generation counter — every dispatch-layer test
/// starts its own `run()`-equivalent state from scratch.
fn new_generation() -> Generation {
    Arc::new(AtomicU64::new(0))
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
    let generation = new_generation();
    dispatch_pending(&mut app, &engine, &tx, &current_task, &generation);

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
    let generation = new_generation();
    dispatch_pending(&mut app, &engine, &tx, &current_task, &generation);
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
    let generation = new_generation();
    dispatch_pending(&mut app, &engine, &tx, &current_task, &generation);

    let ev = rx.recv().await.expect("Quit event must be sent");
    assert_eq!(ev, ReplEvent::Quit);
}

/// A cancel dispatched while a submit task is GENUINELY in flight (parked
/// mid-`.await`, proven via `MockEngine::chunk_sent` — not merely spawned
/// and never polled, the gap a code-review pass on PR #3477 caught in an
/// earlier revision of this test) must abort that task's `JoinHandle`.
/// Proof of genuine interruption: `chunk_sent` becoming `true` shows the
/// task reached its await point; `completed` staying `false` — even after
/// further scheduling opportunities and WITHOUT ever releasing `notify` —
/// shows it never resumed past that point, i.e. it was truly cancelled, not
/// merely "replaced in the slot" while still running to completion
/// unobserved. Mirrors tagent's `current_task` abort-on-cancel precedent
/// (`crates/trusty-agents/src/repl/tui/events.rs`).
#[tokio::test]
async fn dispatch_pending_cancel_aborts_genuinely_in_flight_submit_task() {
    let notify = Arc::new(Notify::new());
    let engine = Arc::new(MockEngine {
        hold_until: Some(Arc::clone(&notify)),
        ..Default::default()
    });
    let mut app = ReplApp::new("demo", "u");
    let (tx, _rx) = mpsc::unbounded_channel::<ReplEvent>();
    let current_task: CurrentTask = Arc::new(StdMutex::new(None));
    let generation = new_generation();

    app_apply(&mut app, ReplEvent::Submit("long task".to_string()));
    dispatch_pending(&mut app, &engine, &tx, &current_task, &generation);
    assert!(
        current_task.lock().unwrap().is_some(),
        "submit must stash a JoinHandle in current_task"
    );

    // Wait for genuine proof the task is parked at its await point (sent its
    // chunk, now blocked on `notify`) — not a fixed sleep, and not merely
    // "spawned".
    for _ in 0..500 {
        if engine.chunk_sent.load(Ordering::SeqCst) {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        engine.chunk_sent.load(Ordering::SeqCst),
        "task must reach its await point before we cancel it, or this test proves nothing"
    );

    app.busy = true;
    app_apply(&mut app, ctrl_c());
    dispatch_pending(&mut app, &engine, &tx, &current_task, &generation);

    assert!(
        current_task.lock().unwrap().is_none(),
        "cancel must take (and abort) the stashed handle"
    );
    assert!(!app.busy, "on_cancelled must clear busy synchronously");

    // The decisive check: release the gate the (supposedly aborted) task
    // was parked on. If `abort()` genuinely worked, the task is already gone
    // and this is a no-op — `completed` can never become `true`. If abort
    // were silently ineffective (the bug this test guards against), the
    // task would still be alive, wake up, and complete.
    notify.notify_one();
    for _ in 0..500 {
        tokio::task::yield_now().await;
    }
    assert!(
        !engine.completed.load(Ordering::SeqCst),
        "an aborted task must never resume past its await point, even when its wait condition is satisfied afterward"
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
    let generation = new_generation();
    dispatch_pending(&mut app, &engine, &tx, &current_task, &generation);

    // Give any wrongly-spawned task a chance to run before asserting.
    tokio::task::yield_now().await;
    assert_eq!(engine.cancel_calls.load(Ordering::SeqCst), 0);
    assert_eq!(engine.handle_input_calls.load(Ordering::SeqCst), 0);
}

/// FIX 2 (generation tagging, PR #3477 review): a message already sitting in
/// a turn's private channel — sent before a cancel bumped the live
/// generation past that turn's own number, exactly the race `abort()` only
/// taking effect at the next `.await` creates (an already-queued chunk
/// survives the abort itself) — must be dropped by
/// [`forward_while_current_generation`], never relayed onto the real
/// channel where it would render as a fresh reply to a request the user
/// already cancelled.
///
/// Deliberately tests [`forward_while_current_generation`] directly rather
/// than through `dispatch_pending`'s spawned tasks: whether a real spawned
/// forwarder drains a message before or after a concurrent generation bump
/// is itself a scheduling race (forwarding it BEFORE the bump is correct —
/// the chunk was legitimately live when sent), so asserting "never forwarded"
/// against the full spawned pipeline is nondeterministic by construction. The
/// actual invariant this crate guarantees is narrower and fully
/// deterministic: ANY message the forwarder has not yet drained by the time
/// `generation` no longer matches its turn gets dropped when the forwarder
/// finally gets to it — which is exactly what direct, un-raced construction
/// below proves.
#[tokio::test]
async fn forward_while_current_generation_drops_stale_generation_message() {
    let (turn_tx, turn_rx) = mpsc::unbounded_channel::<ReplEvent>();
    let (real_tx, mut real_rx) = mpsc::unbounded_channel::<ReplEvent>();
    let generation: Generation = Arc::new(AtomicU64::new(5));

    // A message sent while generation 5 was still live...
    turn_tx
        .send(ReplEvent::AssistantOutput {
            chunk: "partial".to_string(),
            done: false,
            is_error: false,
        })
        .unwrap();
    // ...but by the time anyone drains it, a cancel has already bumped the
    // live generation to 6 — turn 5 is now stale, exactly what
    // `dispatch_pending`'s cancel branch does before `abort()` runs.
    generation.store(6, Ordering::SeqCst);
    drop(turn_tx); // no more messages; the forwarder runs to completion

    forward_while_current_generation(turn_rx, real_tx, generation, 5).await;

    assert!(
        real_rx.try_recv().is_err(),
        "a message tagged for a superseded generation must never reach the real channel"
    );
}

/// Symmetric companion to the drop test above: a message whose generation
/// still matches the live one must be forwarded normally — the mechanism
/// must not become a black hole for ordinary, un-cancelled turns.
#[tokio::test]
async fn forward_while_current_generation_forwards_matching_generation_message() {
    let (turn_tx, turn_rx) = mpsc::unbounded_channel::<ReplEvent>();
    let (real_tx, mut real_rx) = mpsc::unbounded_channel::<ReplEvent>();
    let generation: Generation = Arc::new(AtomicU64::new(1));

    turn_tx
        .send(ReplEvent::AssistantOutput {
            chunk: "hello".to_string(),
            done: true,
            is_error: false,
        })
        .unwrap();
    drop(turn_tx);

    forward_while_current_generation(turn_rx, real_tx, generation, 1).await;

    let ev = real_rx
        .try_recv()
        .expect("matching-generation message must forward");
    assert_eq!(
        ev,
        ReplEvent::AssistantOutput {
            chunk: "hello".to_string(),
            done: true,
            is_error: false,
        }
    );
}

/// FIX 1 (busy-gating) at the dispatch layer: while a turn is in flight, a
/// second `Submit` must never reach `TuiEngine::handle_input` a second time.
/// `ReplApp::submit_line`'s own busy guard means `pending_submit` never gets
/// staged for the second attempt, so `dispatch_pending` has nothing to
/// dispatch — this is the direct fix for the double-submit corruption
/// (task B's chunks splicing into task A's orphaned `streaming_idx` entry)
/// a code-review pass caught on PR #3477.
#[tokio::test]
async fn dispatch_pending_second_submit_while_busy_does_not_start_second_task() {
    let engine = Arc::new(MockEngine::default());
    let mut app = ReplApp::new("demo", "u");
    let (tx, _rx) = mpsc::unbounded_channel::<ReplEvent>();
    let current_task: CurrentTask = Arc::new(StdMutex::new(None));
    let generation = new_generation();

    app_apply(&mut app, ReplEvent::Submit("explain X".to_string()));
    dispatch_pending(&mut app, &engine, &tx, &current_task, &generation);
    for _ in 0..200 {
        if engine.handle_input_calls.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(engine.handle_input_calls.load(Ordering::SeqCst), 1);
    assert!(app.busy, "first submit must leave the app busy");

    // Second submit attempt while still busy: the reducer refuses to stage
    // it at all (`ReplApp::submit_line`'s guard), so there is nothing for
    // `dispatch_pending` to dispatch.
    app_apply(&mut app, ReplEvent::Submit("explain Y".to_string()));
    assert!(app.pending_submit.is_none(), "must not stage a second turn");
    dispatch_pending(&mut app, &engine, &tx, &current_task, &generation);

    tokio::task::yield_now().await;
    assert_eq!(
        engine.handle_input_calls.load(Ordering::SeqCst),
        1,
        "a second submit while busy must never reach handle_input"
    );
}
