//! `tm architect serve`: the poller's cycle, driven by local events.
//!
//! Why: `tm architect poll` runs a full cycle every 60 s whether anything
//! changed or not, so a new inbox line or a pane prompt waits up to a minute
//! (#9779, plan section 2). The service runs the SAME [`Runner`] cycle when a
//! local source changes, and keeps a 60 s tick for the time-based sensors
//! (idle, host load, watchdog clocks, routines). GitHub stays on the cycle's
//! own fixed-cadence steps: no webhook, no listener port.
//! What: [`ServeLoop::run`] runs one cycle at start, then waits on three
//! sources, both read every [`SCAN_EVERY`]: [`FileWatch`] (mtime polling of
//! the inbox and the hot Claude transcripts) and [`TmuxActivity`] (one
//! `tmux list-panes -a` read of each pane's activity time; no tmux client is
//! attached), and the tick. The first event opens a debounce window
//! ([`DEBOUNCE`]); every event inside it joins the same cycle, and an event
//! cycle starts no sooner than `min_gap` ([`MIN_EVENT_GAP`]) after the last
//! cycle ([`Pacer`]). After a cycle both sources re-read their baseline, so
//! the cycle's own writes start no cycle. A source that fails is a fault ([`Faults`]):
//! logged, stored under [`SERVE_FAULT`] in the poll state and raised by the
//! cycle as a `serve` alert in its failing set ([`serve_fault_alerts`]),
//! while the tick keeps cycling. A panicking cycle is caught, logged and
//! raised the same way (`cycle-panic`), and the loop goes on. The stop flag
//! ([`install_stop_handlers`]: SIGTERM, SIGINT) is read between cycles, so a
//! running cycle always finishes. [`serve_runner`] takes `poll.lock` through
//! [`Runner::start`] (none in a shadow run) and holds it for the service's
//! life, so a second `serve` or a `poll` cannot cycle beside it.
//! Test: `tests/serve_loop.rs`, `tests/serve_runner.rs`,
//! `tests/serve_activity.rs`, `tests/serve_pace.rs`, `tests/serve_signal.rs`.

mod activity;
mod files;
mod pace;
mod signal;

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Map, Value};

pub use activity::{ACTIVITY_FORMAT, TMUX_FAULT, TmuxActivity};
pub use files::{FileWatch, WatchRoot};
pub use pace::Pacer;
pub use signal::install_stop_handlers;

use crate::classify::Alert;
use crate::classify::alerts::alert;
use crate::clock::Clock;
use crate::error::RunError;
use crate::poller::{Config, Runner};
use crate::tmux::{SystemTmux, Tmux};

/// Events closer together than this join one cycle.
pub const DEBOUNCE: Duration = Duration::from_secs(2);
/// An event cycle starts at least this long after the last cycle.
pub const MIN_EVENT_GAP: Duration = Duration::from_secs(15);
/// A cycle runs at least this often, events or not.
pub const TICK: Duration = Duration::from_secs(60);
/// How often the file watch re-reads its mtimes.
pub const SCAN_EVERY: Duration = Duration::from_secs(1);
/// `poll-state.json` key of the service's open faults: `{kind: detail}`.
pub const SERVE_FAULT: &str = "serve_fault";
/// The fault kind of a cycle that panicked.
pub const CYCLE_PANIC: &str = "cycle-panic";
/// Longest single wait, so the stop flag is read promptly.
const STOP_POLL: Duration = Duration::from_millis(200);

/// What a source tells the loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Something changed in the named source; a cycle is due.
    Changed(String),
    /// The source `kind` failed; the detail is one line.
    Fault {
        /// Fault kind: the alert fingerprint.
        kind: String,
        /// What failed.
        detail: String,
    },
    /// The source `kind` works again.
    Clear(String),
}

/// What started a cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The service started (and `--once`).
    Start,
    /// A debounced burst of events.
    Events,
    /// The tick, with no event due.
    Tick,
}

/// The loop's timings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServeOptions {
    /// [`DEBOUNCE`] in production.
    pub debounce: Duration,
    /// [`TICK`] in production.
    pub tick: Duration,
    /// [`MIN_EVENT_GAP`] by default; the config's `SERVE_MIN_EVENT_GAP`.
    pub min_gap: Duration,
    /// [`SCAN_EVERY`] in production.
    pub scan_every: Duration,
    /// Run the start cycle only, then return.
    pub once: bool,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            debounce: DEBOUNCE,
            tick: TICK,
            min_gap: MIN_EVENT_GAP,
            scan_every: SCAN_EVERY,
            once: false,
        }
    }
}

/// The service's open faults, `kind -> detail`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Faults(BTreeMap<String, String>);

impl Faults {
    /// Records a fault; true when it is new or its detail changed.
    pub fn set(&mut self, kind: &str, detail: &str) -> bool {
        self.0.insert(kind.to_owned(), detail.to_owned()).as_deref() != Some(detail)
    }

    /// Drops a fault; true when it was open.
    pub fn clear(&mut self, kind: &str) -> bool {
        self.0.remove(kind).is_some()
    }

    /// The detail of an open fault.
    pub fn get(&self, kind: &str) -> Option<&str> {
        self.0.get(kind).map(String::as_str)
    }

    /// True when no fault is open.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Writes the faults under [`SERVE_FAULT`] in `other`, or removes the key.
    pub fn store(&self, other: &mut Map<String, Value>) {
        if self.0.is_empty() {
            other.remove(SERVE_FAULT);
            return;
        }
        let map = self
            .0
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        other.insert(SERVE_FAULT.to_owned(), Value::Object(map));
    }
}

/// The stored service faults as `serve` alerts on `supervisor`, one per kind.
///
/// Test: `watcher_start_failure_is_a_serve_alert` (tests/serve_runner.rs).
pub fn serve_fault_alerts(other: &Map<String, Value>, supervisor: &str, now: f64) -> Vec<Alert> {
    let Some(Value::Object(map)) = other.get(SERVE_FAULT) else {
        return Vec::new();
    };
    map.iter()
        .map(|(kind, detail)| {
            let detail = format!("serve {kind}: {}", detail.as_str().unwrap_or("unknown"));
            alert("serve", kind, &detail, supervisor, "", now)
        })
        .collect()
}

/// How many cycles each trigger ran.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServeStats {
    /// Start cycles (one).
    pub start: u64,
    /// Event cycles.
    pub events: u64,
    /// Tick cycles.
    pub ticks: u64,
    /// Cycles that panicked.
    pub panics: u64,
}

/// One cycle: the trigger and the faults to raise; `Err` is a failed cycle.
pub type CycleFn<'f> = dyn FnMut(Trigger, &Faults) -> Result<(), String> + 'f;

/// The event loop; see the module docs.
pub struct ServeLoop<'a> {
    /// Timings.
    pub opts: ServeOptions,
    /// Set by a signal (or a test) to stop after the current cycle.
    pub stop: &'a AtomicBool,
    /// The inbox and transcript watch; `None` runs on the tick only.
    pub files: Option<FileWatch>,
    /// The pane activity read, run with each file scan.
    pub tmux: Option<TmuxActivity>,
    /// Where tests (and any future source) send [`Event`]s.
    pub events: Option<Receiver<Event>>,
    /// Appends one line to the service log (`poll.log` / `shadow.log`).
    pub log: &'a dyn Fn(&str),
}

impl<'a> ServeLoop<'a> {
    /// The production sources for `config`: the inbox (its shadow dir,
    /// `poll.log` and `poll.lock` excluded) and transcript watch, and the
    /// pane activity of every session on `tmux`'s server but the poll session.
    pub fn for_config(
        config: &Config,
        tmux: SystemTmux,
        stop: &'a AtomicBool,
        opts: ServeOptions,
        log: &'a dyn Fn(&str),
    ) -> Self {
        let exclude = vec![
            config.shadow_dir(),
            config.inbox.join("poll.log"),
            config.lock(),
        ];
        let files = FileWatch::new(vec![
            WatchRoot::inbox(&config.inbox, exclude),
            WatchRoot::transcripts(&config.transcript_base),
        ]);
        let tmux = TmuxActivity::new(tmux, vec![config.poll_session.clone()]);
        Self {
            opts,
            stop,
            files: Some(files),
            tmux: Some(tmux),
            events: None,
            log,
        }
    }

    /// Runs until the stop flag is set (or one cycle with `once`).
    ///
    /// # Errors
    /// With `once`, the start cycle's error. Otherwise none: a failed or
    /// panicking cycle is logged and the loop continues.
    pub fn run(&mut self, cycle: &mut CycleFn<'_>) -> Result<ServeStats, String> {
        let mut faults = Faults::default();
        let mut stats = ServeStats::default();
        self.scan(&mut faults, true);
        let first = self.run_cycle(Trigger::Start, &mut faults, &mut stats, cycle);
        if self.opts.once {
            return first.map(|()| stats);
        }
        self.scan(&mut faults, false);
        let o = self.opts;
        let mut pace = Pacer::new(o.debounce, o.tick, o.min_gap, Instant::now());
        // A busy fleet can keep events due forever; the full scan still runs.
        let mut next_refresh = Instant::now() + o.tick;
        let mut next_scan = Instant::now() + o.scan_every;
        while !self.stop.load(Ordering::SeqCst) {
            let now = Instant::now();
            if now >= next_scan {
                if self.scan(&mut faults, false) {
                    pace.event(now);
                }
                next_scan = now + o.scan_every;
            }
            if now >= next_refresh {
                self.scan(&mut faults, true);
                next_refresh = now + o.tick;
            }
            if let Some(trigger) = pace.due(now) {
                let _ = self.run_cycle(trigger, &mut faults, &mut stats, cycle);
                // The cycle's own writes are the new baseline, not an event.
                self.scan(&mut faults, false);
                pace.ran(Instant::now());
                next_scan = Instant::now() + o.scan_every;
                continue;
            }
            let wake = pace.wake().min(next_scan);
            let wait = wake.saturating_duration_since(now).min(STOP_POLL);
            match self.events.as_ref().map(|rx| rx.recv_timeout(wait)) {
                Some(Ok(event)) => self.handle(event, &mut faults, &mut pace),
                Some(Err(RecvTimeoutError::Timeout)) => {}
                Some(Err(RecvTimeoutError::Disconnected)) => self.events = None,
                None => std::thread::sleep(wait),
            }
        }
        (self.log)("serve stop: signal received; last cycle finished");
        Ok(stats)
    }

    fn handle(&self, event: Event, faults: &mut Faults, pace: &mut Pacer) {
        match event {
            Event::Changed(_) => pace.event(Instant::now()),
            Event::Fault { kind, detail } => {
                if self.fault(faults, &kind, &detail) {
                    pace.event(Instant::now());
                }
            }
            Event::Clear(kind) => self.recovered(faults, &kind),
        }
    }

    /// Opens a fault; logs it when new. True when new.
    fn fault(&self, faults: &mut Faults, kind: &str, detail: &str) -> bool {
        let new = faults.set(kind, detail);
        if new {
            tracing::error!(kind, detail, "serve source failed");
            (self.log)(&format!(
                "serve ERROR {kind}: {detail}; the {}s tick keeps cycling",
                self.opts.tick.as_secs()
            ));
        }
        new
    }

    fn recovered(&self, faults: &mut Faults, kind: &str) {
        if faults.clear(kind) {
            (self.log)(&format!("serve {kind} recovered"));
        }
    }

    /// Re-reads the file watch (every file when `full`) and the tmux pane
    /// activity; true when either changed.
    fn scan(&mut self, faults: &mut Faults, full: bool) -> bool {
        let mut changed = false;
        if let Some(files) = self.files.as_mut() {
            for (kind, result) in files.scan(full, SystemTime::now()) {
                match result {
                    Ok(c) => {
                        changed |= c;
                        self.recovered(faults, &kind);
                    }
                    Err(e) => {
                        self.fault(faults, &kind, &e);
                    }
                }
            }
        }
        // #9779 review: a failed tmux read fails closed as a `serve` fault.
        match self.tmux.as_mut().map(TmuxActivity::poll) {
            Some(Ok(c)) => {
                changed |= c;
                self.recovered(faults, TMUX_FAULT);
            }
            Some(Err(e)) => {
                if self.fault(faults, TMUX_FAULT, &format!("list-panes: {e}")) {
                    changed = true;
                }
            }
            None => {}
        }
        changed
    }

    fn run_cycle(
        &self,
        trigger: Trigger,
        faults: &mut Faults,
        stats: &mut ServeStats,
        cycle: &mut CycleFn<'_>,
    ) -> Result<(), String> {
        let carried = faults.clone();
        (self.log)(&format!("serve cycle trigger={trigger:?}"));
        match trigger {
            Trigger::Start => stats.start += 1,
            Trigger::Events => stats.events += 1,
            Trigger::Tick => stats.ticks += 1,
        }
        match catch_unwind(AssertUnwindSafe(|| cycle(trigger, &carried))) {
            Ok(result) => {
                // The panic alert went out with this cycle; it is closed.
                if carried.get(CYCLE_PANIC).is_some() {
                    self.recovered(faults, CYCLE_PANIC);
                }
                result
            }
            Err(payload) => {
                stats.panics += 1;
                let text = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "non-string panic".to_owned());
                self.fault(faults, CYCLE_PANIC, &format!("cycle panicked: {text}"));
                Err(format!("cycle panicked: {text}"))
            }
        }
    }
}

/// Runs `lp` over `runner`: takes the lock and loads the state as `poll`
/// does, then each cycle stores the faults and calls [`Runner::run_once`].
///
/// # Errors
/// [`RunError::AlreadyRunning`] when another poller holds `poll.lock` (no
/// cycle runs); [`RunError::Io`] when the inbox or lock cannot be made;
/// [`RunError::Incomplete`] for a failed `--once` cycle.
pub fn serve_runner<T: Tmux + ?Sized, C: Clock + ?Sized>(
    runner: &Runner<'_, T, C>,
    lp: &mut ServeLoop<'_>,
) -> Result<ServeStats, RunError> {
    let (_lock, mut session) = runner.start(lp.opts.tick)?;
    (lp.log)(&format!(
        "serve start debounce={}ms tick={}s shadow={}",
        lp.opts.debounce.as_millis(),
        lp.opts.tick.as_secs(),
        runner.config.shadow
    ));
    lp.run(&mut |_, faults| {
        faults.store(&mut session.state.other);
        runner
            .run_once(&mut session)
            .map(drop)
            .map_err(|e| e.to_string())
    })
    .map_err(RunError::Incomplete)
}
