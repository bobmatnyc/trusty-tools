//! The poll loop: `fleet-poll.py`'s `main` and the write half of `cycle`
//! (988152c lines 1572-1636).
//!
//! Why: the loop must keep every alert it found and every wake it owes
//! across a crash, an unreadable file or a failed write, and never spin
//! (#9171 A3b-2, plan AC3 and AC4).
//! What: [`Runner::run_once`] runs one cycle on a copy of the state: collect
//! and dedup, append the new alerts to `alerts.jsonl`, triage, wake, log,
//! then commit the copy and save it. Any failure before the commit (a
//! sensor, the append, a phase hook) commits nothing, so the same alerts
//! come out again next cycle; dedup keys repeat identically. A `list-panes`
//! failure logs `panes=? ERROR ...; cycle skipped`. [`Runner::run_loop`]
//! checks the interval before any tmux call, takes `poll.lock`, loads the
//! state ([`PollSession::load`]) and sleeps the interval exactly once after
//! every cycle, whatever the cycle did. Every cycle also logs each bad
//! `Config::problems` entry with the sensor it turned off. A dry run takes no lock, writes no
//! file, sends no keys and runs one cycle. No signal handler (A3 ruling Q8).
//! [`Runner::run_single`] is `--once`: the same start, one cycle, no sleep,
//! and an error for a skipped or unsaved cycle, or for any `Config::problems`
//! entry, which the loop only logs (#9822).
//! Test: `tests/cycle_loop.rs`, `tests/state_session.rs`, `tests/verbs_poll.rs`.

use std::fs;
use std::io;
use std::time::Duration;

use super::config::Config;
use super::cycle::{Sensors, Stopped, collect};
use super::host::HostProbe;
use super::lock::PollLock;
use super::pyfmt::py_int;
use super::session::PollSession;
use super::sink::{append_alerts, append_log};
use super::state::save_state;
use super::triage::Triage;
use super::wake::Waker;
use crate::classify::Alert;
use crate::classify::alerts::iso;
use crate::clock::Clock;
use crate::error::{ConfigError, RunError};
use crate::tmux::Tmux;

/// A point in [`Runner::run_once_with`] where a test can stop the run, as a
/// kill would.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// The new alerts are in `alerts.jsonl`; nothing is committed or saved.
    AlertsAppended,
    /// The state is committed and saved.
    StateSaved,
}

/// What one cycle did.
#[derive(Debug, Default)]
pub struct CycleSummary {
    /// The cycle's new alerts.
    pub new: Vec<Alert>,
    /// Alerts dropped as repeats.
    pub suppressed: usize,
    /// How many new alerts triage sent to the Architect.
    pub woken: usize,
    /// How many new alerts triage only logged.
    pub logged: u64,
    /// The wake phrase; `None` when the cycle was skipped.
    pub wake: Option<String>,
    /// Why the cycle was skipped (the `list-panes` error), when it was.
    pub skipped: Option<String>,
}

/// `--interval`: whole seconds, at least 1.
///
/// # Errors
/// [`ConfigError`] for zero, a negative number, or text Python's `int()`
/// rejects. The prototype accepts 0 and then spins.
pub fn parse_interval(text: &str) -> Result<Duration, ConfigError> {
    match py_int(text) {
        Some(n) if n > 0 => Ok(Duration::from_secs(n.unsigned_abs())),
        _ => Err(ConfigError {
            name: "interval",
            value: text.to_owned(),
            want: "positive whole number of seconds",
        }),
    }
}

/// What a bad environment variable turns off (see [`Config::problems`]).
fn sensor_off(name: &str) -> &'static str {
    match name {
        "LOAD_FACTOR" => "host alerts (load, disk, swap, band)",
        "PANE_CTX_WINDOW" => "pane ctx",
        _ => "self_ctx",
    }
}

/// Everything the loop runs on.
pub struct Runner<'a, T: Tmux + ?Sized, C: Clock + ?Sized> {
    /// The tmux server holding the fleet.
    pub tmux: &'a T,
    /// Time, the pointer's stamp and every sleep.
    pub clock: &'a C,
    /// The host sensors.
    pub probe: &'a dyn HostProbe,
    /// Which new alerts wake the Architect.
    pub triage: &'a dyn Triage,
    /// Paths and thresholds.
    pub config: &'a Config,
    /// One cycle that writes nothing and sends nothing.
    pub dry_run: bool,
}

impl<T: Tmux + ?Sized, C: Clock + ?Sized> Runner<'_, T, C> {
    /// One cycle; see the module docs.
    ///
    /// # Errors
    /// As [`Runner::run_once_with`].
    pub fn run_once(&self, session: &mut PollSession) -> Result<CycleSummary, RunError> {
        self.run_once_with(session, &mut |_| Ok(()))
    }

    /// One cycle, calling `hook` at each [`Phase`]; an error from `hook`
    /// stops the run there. Every error is also logged.
    ///
    /// # Errors
    /// [`RunError::Cycle`] (a sensor failed), [`RunError::Io`] (the append
    /// or a hook failed): nothing was committed. [`RunError::State`]: the
    /// alerts were written and the state is committed in memory, not saved.
    pub fn run_once_with(
        &self,
        session: &mut PollSession,
        hook: &mut dyn FnMut(Phase) -> io::Result<()>,
    ) -> Result<CycleSummary, RunError> {
        let now = self.clock.now_unix();
        let mut log = Vec::new();
        session.retry_load(self.config, self.dry_run, now, &mut log);
        // #9171: a bad variable is reported by every cycle, not once at loop
        // start, so a quiet summary never reads as a healthy sensor.
        log.extend(
            self.config
                .problems
                .iter()
                .map(|p| format!("config ERROR {p}; {} off", sensor_off(p.name))),
        );
        let result = self.cycle(session, now, &mut log, hook);
        if let Err(e) = &result {
            log.push(format!("ERROR {e}"));
        }
        self.write_log(&log);
        result
    }

    fn cycle(
        &self,
        session: &mut PollSession,
        now: f64,
        log: &mut Vec<String>,
        hook: &mut dyn FnMut(Phase) -> io::Result<()>,
    ) -> Result<CycleSummary, RunError> {
        let sensors = Sensors {
            tmux: self.tmux,
            probe: self.probe,
            config: self.config,
            dry_run: self.dry_run,
        };
        // #9171 A3b-2: work on a copy; nothing is committed before the append.
        let mut next = session.state.clone();
        let found = match collect(sensors, &mut session.transcripts, &mut next, now) {
            Ok(found) => found,
            Err(Stopped::ListPanes(e)) => {
                log.push(format!("panes=? ERROR {e}; cycle skipped"));
                return Ok(CycleSummary {
                    skipped: Some(e.to_string()),
                    ..CycleSummary::default()
                });
            }
            Err(Stopped::Failed(e, lines)) => {
                log.extend(lines);
                return Err(e.into());
            }
        };
        log.extend(found.log);
        let phase = |p: Phase| {
            move |source| RunError::Io {
                what: format!("phase {p:?}"),
                source,
            }
        };
        if self.dry_run {
            log.extend(
                found
                    .new
                    .iter()
                    .map(|a| format!("  would alert: {}", a.display)),
            );
        } else {
            append_alerts(&self.config.alerts(), &found.new).map_err(|source| RunError::Io {
                what: "append alerts".to_owned(),
                source,
            })?;
        }
        hook(Phase::AlertsAppended).map_err(phase(Phase::AlertsAppended))?;
        let (woken, logged) = self.triage.triage(&found.new, now, &mut next, self.dry_run);
        let alerts = self.config.alerts();
        let waker = Waker {
            tmux: self.tmux,
            clock: self.clock,
            alerts: &alerts,
            dry_run: self.dry_run,
        };
        let wake = waker.dispatch_as(
            &self.config.supervisor,
            &mut next,
            &found.panes,
            woken.len() as u64,
            Some(logged),
        );
        log.push(format!(
            "panes={} new={} woken={} logged={logged} suppressed={} wake={wake}",
            found.watched,
            found.new.len(),
            woken.len(),
            found.suppressed
        ));
        session.state = next;
        if !self.dry_run && !session.save_blocked() {
            save_state(&self.config.state(), &session.state)?;
            hook(Phase::StateSaved).map_err(phase(Phase::StateSaved))?;
        }
        Ok(CycleSummary {
            new: found.new,
            suppressed: found.suppressed,
            woken: woken.len(),
            logged,
            wake: Some(wake),
            skipped: None,
        })
    }

    /// Writes `lines` to `poll.log`, or prints them in a dry run.
    fn write_log(&self, lines: &[String]) {
        let now = self.clock.now_unix();
        for line in lines {
            if self.dry_run {
                if line.starts_with("  ") {
                    println!("{line}");
                } else {
                    println!("{} {line}", iso(now));
                }
            } else if let Err(e) = append_log(&self.config.log(), now, line) {
                tracing::warn!(error = %e, line, "poll.log append failed");
            }
        }
    }

    /// The loop: `interval` is checked first, then the lock is taken and the
    /// state loaded; each cycle is followed by one sleep of the interval.
    /// `cycles` bounds the loop (`None` runs until the process ends). A dry
    /// run returns after one cycle and no sleep.
    ///
    /// # Errors
    /// [`RunError::Config`] for a bad interval, [`RunError::AlreadyRunning`]
    /// when another poller holds the lock, [`RunError::Io`] when the inbox or
    /// lock cannot be made. A failed cycle is logged, never returned.
    pub fn run_loop(&self, interval: &str, cycles: Option<u64>) -> Result<(), RunError> {
        let every = parse_interval(interval)?;
        let (_lock, mut session) = self.start(every)?;
        let mut done = 0u64;
        loop {
            // #9171 AC4: a failed cycle is already logged; it never skips the sleep.
            let _ = self.run_once(&mut session);
            if self.dry_run {
                return Ok(());
            }
            self.clock.sleep(every);
            done += 1;
            if cycles.is_some_and(|n| done >= n) {
                return Ok(());
            }
        }
    }

    /// `fleet-poll.py --once` / `--dry-run`: the loop's start, one cycle and no
    /// sleep, failing closed where the prototype logs and exits 0 (#9822).
    ///
    /// # Errors
    /// As [`Runner::run_loop`] before the cycle; then the cycle's own error,
    /// or [`RunError::Incomplete`] when tmux could not list panes or the
    /// state could not be read and so was not saved. [`RunError::BadConfig`]
    /// (before the lock or any file) when `Config::problems` is not empty.
    pub fn run_single(&self, interval: &str) -> Result<CycleSummary, RunError> {
        let every = parse_interval(interval)?;
        // #9822: a bad variable turns a sensor off; a single run refuses to
        // report that as success. The loop keeps running with the sensor off.
        if !self.config.problems.is_empty() {
            let list: Vec<String> = self
                .config
                .problems
                .iter()
                .map(|p| format!("{p}; {} would be off", sensor_off(p.name)))
                .collect();
            return Err(RunError::BadConfig(list.join("; ")));
        }
        let (_lock, mut session) = self.start(every)?;
        let summary = self.run_once(&mut session)?;
        if let Some(why) = &summary.skipped {
            return Err(RunError::Incomplete(format!(
                "tmux could not list panes: {why}"
            )));
        }
        if !self.dry_run && session.save_blocked() {
            return Err(RunError::Incomplete(format!(
                "{} could not be read, so it was not saved",
                self.config.state().display()
            )));
        }
        Ok(summary)
    }

    /// Takes the lock (not in a dry run), loads the state and logs the start.
    fn start(&self, every: Duration) -> Result<(Option<PollLock>, PollSession), RunError> {
        let lock = if self.dry_run {
            None
        } else {
            fs::create_dir_all(&self.config.inbox).map_err(|source| RunError::Io {
                what: format!("create {}", self.config.inbox.display()),
                source,
            })?;
            Some(PollLock::acquire(&self.config.lock())?)
        };
        let (session, mut lines) =
            PollSession::load(self.config, self.dry_run, self.clock.now_unix());
        if !self.dry_run {
            lines.push(format!("poll start interval={}s", every.as_secs()));
        }
        self.write_log(&lines);
        Ok((lock, session))
    }
}
