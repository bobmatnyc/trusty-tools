//! The cutover switch and its rollback.
//!
//! Why: wake duty lives in `poll-supervise.sh` today. The spike must show one
//! command that moves it to the scheduler and one that moves it back, and must
//! never double-wake or leave the fleet with no poller. The Architect throws
//! the switch after reading the parity report; nothing here runs on its own.
//! What: the mode lives in `local/meta/cutover.json` (absent = `shadow`); the
//! scheduler reads it every tick, so the marker is what moves the poll job
//! between the shadow dry-run and the live `poll --once`. [`cutover`] refuses
//! unless the parity report meets [`Thresholds`] (or `force`), stops the
//! supervisor session through [`Ops`], then writes the marker; if the marker
//! write fails it restarts the supervisor. [`rollback`] writes the `shadow`
//! marker first (the scheduler stops waking at once), then runs
//! `scripts/start-fleet-poll.sh` through [`Ops`]. Both are idempotent and each
//! appends a line to `inbox/cutover.log`. [`SystemOps`] is the real stop and
//! start; tests inject a fake and use temp directories.
//! Test: `cutover_refuses_below_the_parity_threshold`, `cutover_flips_to_live_after_stopping_the_supervisor`,
//! `cutover_is_idempotent_when_already_live`, `rollback_flips_to_shadow_then_starts_the_supervisor`,
//! `cutover_restores_the_supervisor_when_the_marker_cannot_be_written`.

use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::parity;
use super::state::write_atomic;
use super::{Layout, SchedError};
use crate::classify::alerts::iso;
use crate::process::{RunFailure, run_bounded};

/// Who owns the wake duty.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The Python poller wakes; the scheduler only shadows it.
    Shadow,
    /// The scheduler wakes (`poll --once` per cycle).
    Live,
}

#[derive(Serialize, Deserialize)]
struct Marker {
    mode: String,
    at: String,
}

/// The mode in `cutover.json`; absent is [`Mode::Shadow`].
///
/// # Errors
/// [`SchedError::Io`] / [`SchedError::Parse`] when the marker is unreadable or
/// names an unknown mode: the mode is never guessed.
pub fn read_mode(layout: &Layout) -> Result<Mode, SchedError> {
    let path = layout.cutover();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Mode::Shadow),
        Err(e) => return Err(SchedError::io(format!("read {}", path.display()), e)),
    };
    let parse = |detail: String| SchedError::Parse {
        what: path.display().to_string(),
        detail,
    };
    let m: Marker = serde_json::from_str(&text).map_err(|e| parse(e.to_string()))?;
    match m.mode.as_str() {
        "shadow" => Ok(Mode::Shadow),
        "live" => Ok(Mode::Live),
        other => Err(parse(format!("unknown mode {other:?}"))),
    }
}

fn write_mode(layout: &Layout, mode: &str, now: i64) -> Result<(), SchedError> {
    let marker = Marker {
        mode: mode.to_owned(),
        at: iso(now as f64),
    };
    let text =
        serde_json::to_string_pretty(&marker).map_err(|e| SchedError::Cutover(e.to_string()))?;
    write_atomic(&layout.cutover(), text.as_bytes())
}

fn log_line(layout: &Layout, now: i64, msg: &str) {
    let path = layout.cutover_log();
    let line = format!("{} {msg}\n", iso(now as f64));
    let wrote = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)
        })
        .and_then(|mut f| f.write_all(line.as_bytes()));
    if let Err(e) = wrote {
        tracing::error!(error = %e, "cutover.log not written");
    }
}

/// The two side effects outside the scheduler's own files.
pub trait Ops {
    /// Stops the `tm-architect-poll` session (and so `poll-supervise.sh` and
    /// its poller). Success when it was already stopped.
    ///
    /// # Errors
    /// A message when tmux could not be asked or refused.
    fn stop_supervisor(&self) -> Result<String, String>;
    /// Runs `scripts/start-fleet-poll.sh` (idempotent).
    ///
    /// # Errors
    /// A message when the script could not run or exited non-zero.
    fn start_supervisor(&self) -> Result<String, String>;
}

/// The real [`Ops`]: tmux and the start script.
#[derive(Debug, Clone)]
pub struct SystemOps {
    /// The Architect root.
    pub root: PathBuf,
    /// `TMUX_SOCKET`, passed as `-S`.
    pub socket: Option<String>,
    /// The supervisor's tmux session.
    pub session: String,
}

const OPS_TIMEOUT: Duration = Duration::from_secs(20);

fn bounded(cmd: Command, what: &str) -> Result<(bool, String), String> {
    match run_bounded(cmd, OPS_TIMEOUT) {
        Ok(f) => Ok((
            f.status.success(),
            String::from_utf8_lossy(&[f.stdout, f.stderr].concat())
                .trim()
                .to_owned(),
        )),
        Err(RunFailure::Timeout) => Err(format!("{what} timed out")),
        Err(RunFailure::Spawn(e) | RunFailure::Wait(e)) => Err(format!("{what}: {e}")),
    }
}

impl Ops for SystemOps {
    fn stop_supervisor(&self) -> Result<String, String> {
        let mut cmd = Command::new("tmux");
        if let Some(s) = &self.socket {
            cmd.args(["-S", s]);
        }
        cmd.args(["kill-session", "-t", &format!("={}", self.session)]);
        let (ok, text) = bounded(cmd, "tmux kill-session")?;
        // A missing session is the stopped state: tmux says so on stderr.
        if ok || text.contains("can't find session") || text.contains("no server running") {
            Ok(format!("stopped {}", self.session))
        } else {
            Err(format!("tmux kill-session: {text}"))
        }
    }

    fn start_supervisor(&self) -> Result<String, String> {
        let mut cmd = Command::new("bash");
        cmd.arg(self.root.join("scripts/start-fleet-poll.sh"))
            .current_dir(&self.root);
        let (ok, text) = bounded(cmd, "start-fleet-poll.sh")?;
        if ok {
            Ok(text)
        } else {
            Err(format!("start-fleet-poll.sh failed: {text}"))
        }
    }
}

/// The parity a cutover needs.
#[derive(Debug, Clone, Copy)]
pub struct Thresholds {
    /// Look back this many seconds.
    pub since_s: i64,
    /// Joined cycles at least (120 is two hours at 60 s).
    pub min_cycles: u64,
    /// Matched share of compared items, percent.
    pub min_match_pct: f64,
    /// Skip the parity check (named loudly in the log).
    pub force: bool,
}

impl Default for Thresholds {
    fn default() -> Self {
        Self {
            since_s: 7200,
            min_cycles: 120,
            min_match_pct: 95.0,
            force: false,
        }
    }
}

/// Moves the wake duty to the scheduler.
///
/// # Errors
/// [`SchedError::Cutover`] when parity is below the thresholds or the
/// supervisor cannot be stopped (nothing changed); parity read errors as
/// [`parity::load`].
pub fn cutover(
    layout: &Layout,
    ops: &dyn Ops,
    th: &Thresholds,
    now: i64,
) -> Result<String, SchedError> {
    if read_mode(layout)? == Mode::Live {
        return Ok("already live: nothing to do".to_owned());
    }
    if !th.force {
        let rep = parity::load(layout, now - th.since_s)?;
        if rep.cycles < th.min_cycles || rep.match_pct() < th.min_match_pct {
            return Err(SchedError::Cutover(format!(
                "parity below the threshold ({} cycles of {} needed, {:.1}% matched of {:.1}% needed); \
                 pass --force to override",
                rep.cycles,
                th.min_cycles,
                rep.match_pct(),
                th.min_match_pct
            )));
        }
    }
    let stopped = ops.stop_supervisor().map_err(SchedError::Cutover)?;
    if let Err(e) = write_mode(layout, "live", now) {
        let restored = ops.start_supervisor();
        return Err(SchedError::Cutover(format!(
            "marker not written ({e}); supervisor restart: {}",
            restored.unwrap_or_else(|m| m)
        )));
    }
    let how = if th.force {
        "FORCED, parity not checked"
    } else {
        "parity ok"
    };
    log_line(
        layout,
        now,
        &format!("cutover: wake duty moved to the scheduler ({how}); {stopped}"),
    );
    Ok(format!("live: the scheduler now wakes ({how}); {stopped}"))
}

/// Moves the wake duty back to `poll-supervise.sh`.
///
/// # Errors
/// [`SchedError::Cutover`] when the supervisor cannot be started (the mode is
/// already `shadow`, so the scheduler is not waking either).
pub fn rollback(layout: &Layout, ops: &dyn Ops, now: i64) -> Result<String, SchedError> {
    write_mode(layout, "shadow", now)?;
    let started = ops.start_supervisor().map_err(|e| {
        log_line(
            layout,
            now,
            &format!("rollback: mode is shadow but the supervisor did not start: {e}"),
        );
        SchedError::Cutover(format!("mode is shadow, supervisor did not start: {e}"))
    })?;
    log_line(
        layout,
        now,
        &format!("rollback: wake duty back on poll-supervise.sh; {started}"),
    );
    Ok(format!(
        "shadow: poll-supervise.sh owns the wake duty; {started}"
    ))
}
