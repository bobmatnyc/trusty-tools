//! The tick engine: which jobs are due, and what happens to each slot.
//!
//! Why: this is the scheduler's rule set, kept separate from process spawning
//! ([`super::spawn`]) and I/O seams so a fixed [`Clock`] and a scripted
//! [`JobRunner`] test every rule: schedule, jitter, catch-up, overlap, daily
//! cap, timeout outcome, failure backoff and freshness escalation.
//! What: [`Engine::tick`] first drains finished runs, then for each job the
//! scheduler runs (enabled, no `managed_by`) compares `now` with the slot's
//! fire time (slot plus jitter). A due slot always advances `next_due` to the
//! first slot after now, then becomes one of: a run, `overrun` (the previous
//! run is still alive), `skipped` (catch-up `skip` and the slot is more than
//! [`LATE_GRACE_S`] late; the daily cap is used up; or the job is backing
//! off). Each run goes to its own thread; one job never blocks another. A
//! first sight of a job sets `next_due` to its next slot: nothing runs for a
//! slot that predates the scheduler. Failures back off 30 s, 60 s, ... capped
//! at one hour, with one `meta_miss` per streak (at the third failure, or at
//! once for a spawn error). Delivery is at-least-once: the state is saved at
//! the end of a tick, so a crash between a run's spawn and that save reruns
//! the slot after restart.
//! Test: `tests/sched_engine.rs`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use super::agent;
use super::config::{Job, Kind, Registry};
use super::engine_run::{Done, Origin};
use super::fresh::{self, meta_alert};
use super::runlog::{RunRecord, append_jsonl};
use super::shadow;
use super::slot::{Sched, day_key, jitter, missed_slots};
use super::spawn::{JobRunner, RunResult};
use super::state::{JobState, SchedState};
use super::{Layout, SchedError};
use crate::classify::alerts::Alert;
use crate::clock::Clock;
use crate::poller::append_alerts;

/// A slot later than this (seconds) past its fire time counts as missed.
pub const LATE_GRACE_S: i64 = 120;
/// The `next_due` of a schedule with no further slot.
pub const NEVER: i64 = i64::MAX / 2;
/// Failure streak length that raises the one `meta_miss`.
pub const STREAK_ALERT_AT: u32 = 3;
/// The longest failure backoff.
pub const BACKOFF_CAP_S: i64 = 3600;

/// What one tick did.
#[derive(Debug, Default)]
pub struct TickReport {
    /// Ids of runs started.
    pub started: Vec<String>,
    /// `runs.jsonl` lines written: finished runs, skips and overruns.
    pub records: Vec<RunRecord>,
}

/// The scheduler's rules over a clock, a runner and the state.
pub struct Engine<'a> {
    pub(super) layout: Layout,
    pub(super) reg: Registry,
    pub(super) state: SchedState,
    pub(super) clock: &'a dyn Clock,
    pub(super) runner: Arc<dyn JobRunner>,
    pub(super) env: Vec<(String, String)>,
    pub(super) scratch_root: PathBuf,
    pub(super) running: HashSet<String>,
    pub(super) tx: Sender<Done>,
    rx: Receiver<Done>,
    dirty: bool,
}

fn backoff(streak: u32) -> i64 {
    (30i64 << streak.saturating_sub(1).min(8)).min(BACKOFF_CAP_S)
}

fn outcome_of(r: &RunResult) -> &'static str {
    if r.timed_out {
        "timeout"
    } else if r.error.is_none() && r.exit == Some(0) {
        "ok"
    } else {
        "fail"
    }
}

impl<'a> Engine<'a> {
    /// An engine over `reg` and `state`. `env` is the process environment
    /// snapshot the allowlist filters; `scratch_root` holds agent scratch dirs.
    pub fn new(
        layout: Layout,
        clock: &'a dyn Clock,
        runner: Arc<dyn JobRunner>,
        env: Vec<(String, String)>,
        reg: Registry,
        state: SchedState,
        scratch_root: PathBuf,
    ) -> Self {
        let (tx, rx) = channel();
        Self {
            layout,
            reg,
            state,
            clock,
            runner,
            env,
            scratch_root,
            running: HashSet::new(),
            tx,
            rx,
            dirty: false,
        }
    }

    /// The current config.
    pub fn registry(&self) -> &Registry {
        &self.reg
    }

    /// The current state.
    pub fn state(&self) -> &SchedState {
        &self.state
    }

    /// Replaces the config (a reload); state is kept.
    pub fn set_registry(&mut self, reg: Registry) {
        self.reg = reg;
    }

    /// Runs still alive.
    pub fn running_count(&self) -> usize {
        self.running.len()
    }

    pub(super) fn now(&self) -> i64 {
        self.clock.now_unix().floor() as i64
    }

    /// One pass: finish, decide and start. Saves the state when it changed.
    ///
    /// # Errors
    /// [`SchedError::Io`] when the state cannot be saved; a failing job is
    /// never an error here.
    pub fn tick(&mut self) -> Result<TickReport, SchedError> {
        let now = self.now();
        let mut rep = TickReport::default();
        self.drain(&mut rep);
        let jobs = self.reg.jobs.clone();
        for job in jobs.iter().filter(|j| j.run_kind() != Kind::NotRun) {
            self.process(job, now, &mut rep);
        }
        self.poll_tick(now, &mut rep);
        if std::mem::take(&mut self.dirty) {
            self.state.save(&self.layout.state())?;
        }
        Ok(rep)
    }

    /// Waits until no run is alive or `max` passes; finished runs are applied.
    pub fn wait_idle(&mut self, max: Duration) -> TickReport {
        let mut rep = TickReport::default();
        let deadline = std::time::Instant::now() + max;
        while !self.running.is_empty() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            match self.rx.recv_timeout(left) {
                Ok(done) => self.finish(done, &mut rep),
                Err(_) => break,
            }
        }
        if std::mem::take(&mut self.dirty)
            && let Err(e) = self.state.save(&self.layout.state())
        {
            tracing::error!(error = %e, "scheduler state not saved");
        }
        rep
    }

    /// The stop path: waits `wait` for running children, then terminates the
    /// ones still alive through the runner and collects their records.
    pub fn shutdown(&mut self, wait: Duration) -> TickReport {
        let mut rep = self.wait_idle(wait);
        if !self.running.is_empty() {
            tracing::warn!(
                running = self.running.len(),
                "stop: killing children still running"
            );
            self.runner.terminate_all(Duration::from_secs(2));
            let more = self.wait_idle(Duration::from_secs(10));
            rep.records.extend(more.records);
        }
        rep
    }

    fn drain(&mut self, rep: &mut TickReport) {
        while let Ok(done) = self.rx.try_recv() {
            self.finish(done, rep);
        }
    }

    fn process(&mut self, job: &Job, now: i64, rep: &mut TickReport) {
        let sched = match Sched::from_config(&job.schedule) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(job = %job.id, error = %e, "unschedulable job");
                return;
            }
        };
        let mut js = self.state.jobs.get(&job.id).cloned().unwrap_or_default();
        let before = js.clone();
        if js.next_due == 0 {
            js.next_due = sched.next_after(now, now).unwrap_or(NEVER);
        } else if now >= js.next_due + jitter(&job.id, js.next_due, job.schedule.jitter_s) {
            self.fire(job, &sched, &mut js, now, rep);
        }
        if let Some(alert) = fresh::evaluate(job, &mut js, &self.layout, now) {
            self.send_alert(&alert);
        }
        if js != before {
            self.state.jobs.insert(job.id.clone(), js);
            self.dirty = true;
        }
    }

    fn fire(
        &mut self,
        job: &Job,
        sched: &Sched,
        js: &mut JobState,
        now: i64,
        rep: &mut TickReport,
    ) {
        let slot = js.next_due;
        let fire_at = slot + jitter(&job.id, slot, job.schedule.jitter_s);
        let missed = missed_slots(sched, slot, now);
        js.next_due = sched.next_after(slot, now).unwrap_or(NEVER);
        js.last_slot = slot;
        let today = day_key(now);
        if js.day != today {
            js.day = today;
            js.runs_today = 0;
        }
        let late = now - fire_at > LATE_GRACE_S;
        let skip = if self.running.contains(&job.id) {
            js.overruns += 1;
            Some("overrun")
        } else if late && job.schedule.catch_up.as_deref() == Some("skip") {
            js.misses += missed + 1;
            Some("skipped")
        } else if (job.daily_cap > 0 && js.runs_today >= job.daily_cap) || now < js.backoff_until {
            // The daily cap is used up, or the job is backing off a failure.
            Some("skipped")
        } else {
            None
        };
        if let Some(outcome) = skip {
            js.last_result = Some(outcome.to_owned());
            self.log_run(RunRecord::new(&job.id, slot, now, now, None, outcome), rep);
            return;
        }
        js.misses += missed;
        js.runs_today += 1;
        js.last_run = Some(now);
        js.last_result = Some("running".to_owned());
        match self.launch(job, slot, now) {
            Ok(()) => rep.started.push(job.id.clone()),
            Err(e) => {
                tracing::error!(job = %job.id, error = %e, "run could not start");
                js.last_result = Some("fail".to_owned());
                js.fail_streak += 1;
                self.send_alert(&meta_alert(
                    now,
                    &job.id,
                    &format!("{}: run could not start", job.id),
                ));
                self.log_run(RunRecord::new(&job.id, slot, now, now, None, "fail"), rep);
            }
        }
    }

    fn finish(&mut self, d: Done, rep: &mut TickReport) {
        self.running.remove(&d.id);
        let end = d.start + d.result.elapsed_s;
        let outcome = outcome_of(&d.result);
        if let Some(err) = &d.result.error {
            tracing::warn!(job = %d.id, error = %err, "run failed");
        }
        let mut alert = None;
        if let Some(js) = self.state.jobs.get_mut(&d.id) {
            js.last_result = Some(outcome.to_owned());
            if outcome == "ok" {
                js.fail_streak = 0;
                js.backoff_until = 0;
            } else {
                js.fail_streak += 1;
                js.backoff_until = end + backoff(js.fail_streak);
                if js.fail_streak == STREAK_ALERT_AT
                    || (d.result.error.is_some() && js.fail_streak == 1)
                {
                    alert = Some(meta_alert(
                        end,
                        &d.id,
                        &format!("{}: {outcome} (streak {})", d.id, js.fail_streak),
                    ));
                }
            }
            self.dirty = true;
        }
        if let Some(a) = alert {
            self.send_alert(&a);
        }
        match d.origin {
            Origin::Agent(plan) => {
                if let Err(e) = agent::finish(&plan, &d.id, &d.result, &self.layout, end) {
                    tracing::error!(job = %d.id, error = %e, "agent result not recorded");
                }
            }
            Origin::Shadow(ctx) => {
                if let Err(e) = shadow::record(&self.layout, &ctx, &d.result, d.slot, d.start) {
                    tracing::error!(error = %e, "shadow record not written");
                }
            }
            Origin::Plain => {}
        }
        self.log_run(
            RunRecord::new(&d.id, d.slot, d.start, end, d.result.exit, outcome),
            rep,
        );
    }

    pub(super) fn log_run(&mut self, rec: RunRecord, rep: &mut TickReport) {
        if let Err(e) = append_jsonl(&self.layout.runs(), &rec) {
            tracing::error!(error = %e, "runs.jsonl not written");
        }
        rep.records.push(rec);
    }

    pub(super) fn send_alert(&self, alert: &Alert) {
        let path = self.layout.alerts();
        let wrote = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| append_alerts(&path, std::slice::from_ref(alert)));
        if let Err(e) = wrote {
            tracing::error!(error = %e, "alert not written");
        }
    }

    pub(super) fn mark_dirty(&mut self) {
        self.dirty = true;
    }
}
