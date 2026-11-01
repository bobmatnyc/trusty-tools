//! The project view's draw → key → poll loop, with the daemon poll off the
//! UI loop (#10476).
//!
//! Why: the loop awaited each refresh inline, so a ~1 s fleet call blocked
//! drawing and key input for every tick; with a slow or unreachable daemon a
//! keypress waited for the round trip.
//! What: [`drive`] draws, reads at most one event for [`KEY_POLL`], dispatches
//! a key or mouse event, then collects a finished poll task and applies it, then starts the
//! next poll as a spawned task when the interval has passed or a mutation asked
//! for a repoll. At most one poll is in flight. A [`HealthCadence`] decides
//! which polls probe `/health`. The draw, the event source and the poll are
//! parameters, so a test can inject a slow poller.
//! Test: `keypress_redraws_while_poll_in_flight_10476`,
//! `only_one_poll_in_flight_10476` (`super::tests`);
//! `health_checked_every_nth_tick` (`poll::cadence_tests`).

use std::future::Future;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, MouseEventKind};
use tokio::task::JoinHandle;

use super::actions;
use super::events::{handle_key, handle_mouse};
use super::poll::tick::{HealthCadence, PollOutcome, PollRequest, apply_tick, request_for};
use super::state::ProjectCtlState;
use super::trace::TuiTrace;
use crate::client::DaemonClient;

/// How long [`drive`] blocks waiting for an event before redrawing.
///
/// Why: matches the coordinator/dashboard screens' input cadence — short
/// enough to feel responsive, independent of the daemon refresh cadence.
pub(crate) const KEY_POLL: Duration = Duration::from_millis(50);

/// A source of terminal events; the seam a test replaces.
pub(crate) trait EventSource {
    /// Wait up to `timeout` for one event.
    fn next_event(&mut self, timeout: Duration) -> std::io::Result<Option<Event>>;
}

/// The real terminal's events, through crossterm.
pub(crate) struct TerminalEvents;

/// Is this event pointer motion, which nothing in the projects view acts on?
///
/// Why: crossterm's capture mode reports every motion, and the loop draws once
/// per event, so a moving mouse would otherwise burn a redraw per cell (#10476).
/// Test: `pointer_motion_is_dropped_before_the_loop`.
fn is_pointer_motion(event: &Event) -> bool {
    matches!(event, Event::Mouse(m) if matches!(m.kind, MouseEventKind::Moved | MouseEventKind::Drag(_)))
}

impl EventSource for TerminalEvents {
    fn next_event(&mut self, timeout: Duration) -> std::io::Result<Option<Event>> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if !event::poll(left)? {
                return Ok(None);
            }
            let event = event::read()?;
            if !is_pointer_motion(&event) {
                return Ok(Some(event));
            }
            if left.is_zero() {
                return Ok(None);
            }
        }
    }
}

/// Run the loop until `q` / Ctrl-C, or until a draw or event read fails.
///
/// Why: see the module doc; the poll must never hold up a frame.
/// What: `poll` is started with a clone of `client` and the tick's
/// [`PollRequest`]; its outcome replaces `client` (a re-discovered URL rides
/// back with it) and is applied with [`apply_tick`]. The interval restarts when
/// a poll completes, so the daemon sees the same request rate as before. A
/// repoll asked for while a poll runs waits for it to finish. A poll still
/// running at exit is aborted. The request's `check_health` comes from a
/// [`HealthCadence`] the loop owns.
/// Test: `keypress_redraws_while_poll_in_flight_10476`,
/// `only_one_poll_in_flight_10476`, `health_checked_every_nth_tick`.
pub(crate) async fn drive<D, E, P, F>(
    draw: &mut D,
    events: &mut E,
    poll: P,
    client: &mut DaemonClient,
    mut state: ProjectCtlState,
    interval: Duration,
    trace: &mut TuiTrace,
) -> anyhow::Result<()>
where
    D: FnMut(&mut ProjectCtlState) -> anyhow::Result<()>,
    E: EventSource,
    P: Fn(DaemonClient, PollRequest) -> F,
    F: Future<Output = PollOutcome> + Send + 'static,
{
    let mut last_poll = Instant::now();
    let mut in_flight: Option<JoinHandle<PollOutcome>> = None;
    let mut key_at: Option<Instant> = None;
    let mut health = HealthCadence::default();

    let result = loop {
        let frame_start = Instant::now();
        if let Err(e) = draw(&mut state) {
            break Err(e);
        }
        trace.draw(frame_start.elapsed());
        if let Some(at) = key_at.take() {
            trace.key_to_frame(at.elapsed());
        }

        match events.next_event(KEY_POLL) {
            Err(e) => break Err(e.into()),
            Ok(Some(Event::Key(key))) => {
                key_at = Some(Instant::now());
                if let Some(action) = handle_key(&mut state, key) {
                    actions::dispatch(&mut state, client, action).await;
                }
                if state.should_exit {
                    break Ok(());
                }
            }
            // #10476: a click can request the same actions a key can.
            Ok(Some(Event::Mouse(mouse))) => {
                key_at = Some(Instant::now());
                if let Some(action) = handle_mouse(&mut state, mouse) {
                    actions::dispatch(&mut state, client, action).await;
                }
            }
            Ok(_) => {}
        }

        if in_flight.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(handle) = in_flight.take()
        {
            match handle.await {
                Ok(outcome) => {
                    *client = outcome.client;
                    let t = outcome.timing;
                    trace.tick(t.started, &t.calls, t.total);
                    apply_tick(&mut state, outcome.result);
                }
                Err(e) => state.set_notice(format!("daemon poll failed: {e}")),
            }
            last_poll = Instant::now();
        }

        // A requested repoll (post-mutation) or the timer elapsing both start
        // a poll; the flag is only taken when no poll is running.
        if in_flight.is_none() && (state.take_repoll() || last_poll.elapsed() >= interval) {
            let mut req = request_for(&mut state, Instant::now());
            // #10476: probe `/health` only on the cadence's polls.
            req.check_health = health.next_due(state.daemon_reachable);
            in_flight = Some(tokio::spawn(poll(client.clone(), req)));
        }
    };

    if let Some(handle) = in_flight {
        handle.abort();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent};

    #[test]
    fn pointer_motion_is_dropped_before_the_loop() {
        let mouse = |kind| {
            Event::Mouse(MouseEvent {
                kind,
                column: 1,
                row: 1,
                modifiers: KeyModifiers::NONE,
            })
        };
        assert!(is_pointer_motion(&mouse(MouseEventKind::Moved)));
        assert!(is_pointer_motion(&mouse(MouseEventKind::Drag(
            MouseButton::Left
        ))));
        assert!(!is_pointer_motion(&mouse(MouseEventKind::Down(
            MouseButton::Left
        ))));
        assert!(!is_pointer_motion(&mouse(MouseEventKind::ScrollDown)));
        let key = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        assert!(!is_pointer_motion(&Event::Key(key)));
    }
}
