//! #9779 review: `tm architect serve`'s cycle schedule on a fake clock.
//!
//! Every instant is `base + offset`; nothing sleeps.

use std::time::{Duration, Instant};

use trusty_architect::serve::{DEBOUNCE, MIN_EVENT_GAP, Pacer, TICK, Trigger};

/// Steps the schedule from `base` to `base + window` in `step`s, with an
/// event at every step when `busy`; returns each cycle's trigger.
fn drive(min_gap: Duration, window: Duration, step: Duration, busy: bool) -> Vec<Trigger> {
    let base = Instant::now();
    let mut pace = Pacer::new(DEBOUNCE, TICK, min_gap, base);
    let mut cycles = Vec::new();
    let mut t = Duration::ZERO;
    while t <= window {
        let now = base + t;
        if busy {
            pace.event(now);
        }
        if let Some(trigger) = pace.due(now) {
            cycles.push(trigger);
            pace.ran(now);
        }
        t += step;
    }
    cycles
}

/// Continuous events for 10 minutes: at most ceil(window / 15 s) + 1 cycles,
/// every one an event cycle, none sooner than 15 s after the last.
#[test]
fn event_cycles_keep_the_least_gap() {
    let window = Duration::from_secs(600);
    let cycles = drive(MIN_EVENT_GAP, window, Duration::from_millis(250), true);
    let bound = window.as_secs().div_ceil(MIN_EVENT_GAP.as_secs()) + 1;
    assert!(cycles.len() as u64 <= bound, "{} > {bound}", cycles.len());
    assert!(
        cycles.len() as u64 >= bound - 2,
        "events still cycle: {}",
        cycles.len()
    );
    assert!(cycles.iter().all(|t| *t == Trigger::Events), "{cycles:?}");
}

/// With no events the tick runs a cycle every 60 s; the gap does not touch it.
#[test]
fn tick_runs_with_no_events() {
    let window = Duration::from_secs(600);
    let cycles = drive(MIN_EVENT_GAP, window, Duration::from_millis(500), false);
    assert_eq!(cycles, vec![Trigger::Tick; 10]);
}

/// One event after a quiet spell runs after the debounce, not the gap.
#[test]
fn a_lone_event_waits_only_the_debounce() {
    let base = Instant::now();
    let mut pace = Pacer::new(DEBOUNCE, TICK, MIN_EVENT_GAP, base);
    let at = base + Duration::from_secs(30);
    pace.event(at);
    assert_eq!(pace.due(at + DEBOUNCE - Duration::from_millis(1)), None);
    assert_eq!(pace.due(at + DEBOUNCE), Some(Trigger::Events));
    assert_eq!(pace.wake(), at + DEBOUNCE);
}
