//! `tm architect restart`: restart the Architect behind the checkpoint gate (#11365).
//!
//! Why: the never-restart-without-a-verified-checkpoint rule lived only in
//! `self-clear.sh`; `tm fleet relaunch` stops `claude` without it. Owner
//! ruling Dpg: restart is its own verb, refused unless a fresh Intent Ledger
//! checkpoint exists and the box is idle. No bypass flag: the owner overrides
//! with `tm fleet relaunch`.
//! What: [`restart`] refuses inside Claude Code (#10033, exit 2); takes the
//! `up` lock ([`super::up::UP_LOCK`]) without waiting (exit 1 when held) and
//! keeps it to the end; gates on
//! [`trusty_architect::ledger::fresh::ready`] over `<dir>/local/il` (exit 4,
//! nothing sent, a read error included); gates on 3 idle reads [`IDLE_GAP`]
//! apart (exit 2, or keep trying until `--wait`); re-checks the ledger and
//! reads idle once more; then
//! runs [`super::relaunch::relaunch`] (stop, bound launch with resume and
//! remote control, poller), types [`RESUME`] through `send_guarded`, and
//! appends one line to `inbox/restart.log`.
//! Test: `restart_tests.rs`.

use std::path::Path;
use std::time::Duration;

use trusty_architect::classify::parse_pane;
use trusty_architect::ledger::fresh::{ready, state_dir};
use trusty_architect::poller::capture;
use trusty_architect::{PaneId, Tmux, input_state_for_send, send_line_guarded};
use trusty_mpm::session_manager::ArchitectRegistration;

use super::relaunch::RelaunchReport;
use super::session_name::SessionNames;

/// The pause between two idle reads (`self-clear.sh`'s `SELF_CLEAR_POLL`).
pub(crate) const IDLE_GAP: Duration = Duration::from_secs(5);

/// Consecutive idle reads the gate needs.
const IDLE_READS: usize = 3;

/// How long the new `claude` gets to show an empty box before the resume line.
const BOX_WAIT: Duration = Duration::from_secs(90);

/// The one resume line, as `self-clear.sh` sends it.
pub(crate) const RESUME: &str = "Resume from the Architect IL: run `python3 scripts/datg.py \
     --project local/il digest` and `python3 scripts/dashboard-db.py summary`; do not run \
     session_context_catchup.";

/// What [`restart`] reads and runs; production passes the live tmux and relaunch.
pub(crate) struct Ops<'a> {
    /// The tmux server holding the Architect.
    pub(crate) tmux: &'a dyn Tmux,
    /// Stop and relaunch bound; see [`super::relaunch::relaunch`].
    pub(crate) relaunch: &'a dyn Fn() -> anyhow::Result<RelaunchReport>,
    /// Wall clock, epoch seconds.
    pub(crate) now: fn() -> f64,
    /// Sleeps between reads.
    pub(crate) sleep: fn(Duration),
}

/// One `restart` run.
#[derive(Debug, Default)]
pub(crate) struct RestartReport {
    /// One line per step or refusal reason.
    pub(crate) lines: Vec<String>,
    /// 0 restarted, 1 a step failed, 2 refused or busy, 4 checkpoint not fresh.
    pub(crate) exit: i32,
    /// Sent by the caller once the new `claude` is recorded.
    pub(crate) registration: Option<ArchitectRegistration>,
}

impl RestartReport {
    fn done(exit: i32, lines: Vec<String>) -> Self {
        Self {
            lines,
            exit,
            registration: None,
        }
    }
}

/// Restart the Architect in `dir`; see the module doc.
///
/// Why: #11365 closure — a stale IL sends nothing and exits 4; a typed draft
/// sends nothing; the relaunched `claude` is bound.
/// What: every refusal returns before the first `send-keys` and before the
/// relaunch. `wait` keeps reading for idle until it passes; `None` gives up
/// after one run of [`IDLE_READS`] reads.
/// Test: `a_stale_checkpoint_exits_4_and_touches_nothing`,
/// `a_checkpoint_read_error_refuses_the_restart`,
/// `a_busy_box_exits_2_and_touches_nothing`,
/// `inside_claude_code_restart_is_refused`,
/// `an_idle_box_and_a_fresh_checkpoint_restart_and_send_one_resume_line`,
/// `a_held_up_lock_refuses_the_restart_before_any_tmux_call`,
/// `a_box_busy_just_before_relaunch_refuses`.
pub(crate) fn restart(
    dir: &Path,
    names: &SessionNames,
    inside: bool,
    wait: Option<Duration>,
    ops: &Ops<'_>,
) -> RestartReport {
    let session = names.architect();
    if inside {
        return RestartReport::done(
            2,
            vec![
                "refused: `tm architect restart` runs inside a Claude Code session (#10033); \
                  run it from an operator terminal"
                    .into(),
            ],
        );
    }
    // #11365: hold `up`'s lock for the whole live path, so `up` (which sees a
    // shell during the relaunch) and a second `restart` never act on the pane.
    let _lock = match super::up::lock_within(dir, Duration::ZERO) {
        Ok(f) => f,
        Err(e) => {
            return RestartReport::done(1, vec![format!("FAILED  {e}; nothing sent")]);
        }
    };
    if let Some(r) = il_refusal(dir, session, ops) {
        return r;
    }
    let pane = match pane_id(ops.tmux, session) {
        Ok(p) => p,
        Err(e) => return RestartReport::done(1, vec![format!("FAILED  {e}")]),
    };
    let deadline = (ops.now)() + wait.map_or(0.0, |w| w.as_secs_f64());
    let mut run = 0;
    loop {
        run = if idle(ops.tmux, &pane) { run + 1 } else { 0 };
        if run >= IDLE_READS {
            break;
        }
        let gave_up = wait.is_none() && run == 0 || wait.is_some() && (ops.now)() >= deadline;
        if gave_up {
            return RestartReport::done(
                2,
                vec![format!(
                    "busy: tmux session {session} is not idle (a turn runs, or the input box is \
                     not empty); nothing sent. Retry, or pass --wait <seconds>"
                )],
            );
        }
        (ops.sleep)(IDLE_GAP);
    }
    // As `self-clear.sh`: the ledger may have changed during the idle wait.
    if let Some(r) = il_refusal(dir, session, ops) {
        return r;
    }
    if !idle(ops.tmux, &pane) {
        return RestartReport::done(
            2,
            vec![format!(
                "busy: tmux session {session} turned busy before the relaunch; nothing sent"
            )],
        );
    }
    let mut report = match (ops.relaunch)() {
        Ok(r) => RestartReport {
            lines: r
                .steps
                .iter()
                .map(|s| s.render_line().trim().to_owned())
                .collect(),
            exit: if r.failed() { 1 } else { 0 },
            registration: r.registration,
        },
        Err(e) => return RestartReport::done(1, vec![format!("FAILED  {e:#}")]),
    };
    let sent = send_resume(ops, session);
    report.lines.push(match &sent {
        Ok(()) => "typed the resume line".to_owned(),
        Err(e) => {
            report.exit = 1;
            format!("FAILED  relaunched, but the resume line was not sent: {e}")
        }
    });
    let entry = format!(
        "{} restart session={session} exit={} resume={}\n",
        iso((ops.now)()),
        report.exit,
        if sent.is_ok() { "sent" } else { "not-sent" }
    );
    if let Err(e) = append_log(dir, &entry) {
        report
            .lines
            .push(format!("warning: inbox/restart.log not written: {e}"));
    }
    report
}

/// [`restart`] against the live tmux server and [`super::relaunch::LIVE`].
pub(crate) fn restart_live(
    dir: &Path,
    home: &Path,
    names: &SessionNames,
    inside: bool,
    wait: Option<Duration>,
) -> RestartReport {
    let tmux = trusty_architect::SystemTmux::new();
    let relaunch = || super::relaunch::relaunch(dir, home, names, super::relaunch::LIVE);
    let ops = Ops {
        tmux: &tmux,
        relaunch: &relaunch,
        now: || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0.0, |d| d.as_secs_f64())
        },
        sleep: std::thread::sleep,
    };
    restart(dir, names, inside, wait, &ops)
}

/// `Some(exit 4)` unless the Architect's ledger is ready; a read error refuses too.
fn il_refusal(dir: &Path, session: &str, ops: &Ops<'_>) -> Option<RestartReport> {
    let verdict = ready(
        &state_dir(&dir.join("local/il"), Some(session)),
        None,
        None,
        (ops.now)(),
    );
    if verdict.ok() {
        return None;
    }
    let mut lines = vec!["the Intent Ledger checkpoint is not fresh; nothing sent".to_owned()];
    lines.extend(verdict.reasons.into_iter().map(|r| format!("  {r}")));
    Some(RestartReport::done(4, lines))
}

/// The `%N` id of `session`'s pane.
fn pane_id(tmux: &dyn Tmux, session: &str) -> Result<PaneId, String> {
    let target = trusty_mpm::core::tmux::exact_window_target(session);
    let out = tmux
        .run(&["display-message", "-p", "-t", &target, "#{pane_id}"])
        .map_err(|e| format!("cannot read tmux session {session}: {e}"))?;
    PaneId::parse(out.trim()).map_err(|e| format!("tmux session {session}: {e}"))
}

/// One idle read: no turn or agent runs, and the box accepts input.
fn idle(tmux: &dyn Tmux, pane: &PaneId) -> bool {
    capture(tmux, pane).is_ok_and(|cap| {
        let info = parse_pane(&cap);
        !info.busy && !info.turn_active && input_state_for_send(&cap).accepts_input()
    })
}

/// Waits for the new `claude`'s box, then types [`RESUME`] and Enter, each guarded.
fn send_resume(ops: &Ops<'_>, session: &str) -> Result<(), String> {
    let pane = pane_id(ops.tmux, session)?;
    let start = (ops.now)();
    while !idle(ops.tmux, &pane) {
        if (ops.now)() - start > BOX_WAIT.as_secs_f64() {
            return Err(format!(
                "the new `claude` showed no empty box in {}s",
                BOX_WAIT.as_secs()
            ));
        }
        (ops.sleep)(Duration::from_secs(1));
    }
    let reading = capture(ops.tmux, &pane)
        .map(|c| input_state_for_send(&c))
        .map_err(|e| e.to_string())?;
    let settle = || (ops.sleep)(Duration::from_millis(1500));
    send_line_guarded(ops.tmux, &pane, reading, RESUME, settle).map_err(|e| e.to_string())
}

/// Appends `entry` to `<dir>/inbox/restart.log`.
fn append_log(dir: &Path, entry: &str) -> std::io::Result<()> {
    use std::io::Write;
    let path = dir.join("inbox/restart.log");
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(entry.as_bytes())
}

/// `YYYY-MM-DDTHH:MM:SSZ` for epoch `t`.
fn iso(t: f64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(t as i64, 0)
        .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "restart_tests.rs"]
mod tests;
