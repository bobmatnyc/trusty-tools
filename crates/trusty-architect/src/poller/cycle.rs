//! One poll cycle up to dedup: `fleet-poll.py`'s `cycle` (988152c lines
//! 1482-1591), core steps only.
//!
//! Why: the cycle turns the fleet's panes, hook events and host sensors into
//! the new alerts of this tick; the run around it writes them, wakes the
//! Architect and saves the state (#9171 A3b-2).
//! What: [`collect`] lists the panes, renames `tm-*` windows, and for each
//! watched pane either raises `exited` (a shell pane) or captures, parses,
//! fills ctx from the pane's own transcript when the status line has none,
//! and evaluates it. It then raises `exited` for each vanished session,
//! adds the fleet plan, events, host and self-ctx alerts, drops `idle` alerts
//! of quiet sessions, and dedups. Test-fixture sessions never raise `exited`.
//! Steps owned elsewhere are left out: usage, effort, token, pm_intake,
//! dashboard, owner_*, auto_sleep, user_state, exited_step and triage rules
//! (#9801); the watchdog and Remote Control (#9779).
//! Test: `tests/cycle_ported.rs` (the prototype's cycle tests, by name),
//! `cycle_falls_back_to_transcript_when_status_ctx_is_none`,
//! `failed_cycle_commits_nothing`.

use std::collections::BTreeMap;

use super::architect::is_shell;
use super::config::Config;
use super::ctx::TranscriptCtx;
use super::events::read_events;
use super::host::{HostProbe, host_alerts};
use super::listing::{Pane, capture, list_panes};
use super::self_ctx_alerts::self_ctx_alerts;
use super::state::PollState;
use super::windows::rename_windows;
use crate::classify::{Alert, PaneInfo, alert, dedup, evaluate, fleet_plan_alerts, parse_pane};
use crate::error::{CycleError, TmuxError};
use crate::quiet::{is_test_fixture_session, read_quiet_sessions};
use crate::tmux::Tmux;

/// What one cycle found.
#[derive(Debug, Default)]
pub struct Collected {
    /// Every listed pane (the wake picks the Architect's from these).
    pub panes: Vec<Pane>,
    /// How many panes were watched.
    pub watched: usize,
    /// Alerts not seen within their re-emit interval.
    pub new: Vec<Alert>,
    /// Alerts dropped as repeats.
    pub suppressed: usize,
    /// `poll.log` lines; a dry run prints them instead.
    pub log: Vec<String>,
}

/// Why [`collect`] did not finish, with the log lines written so far.
#[derive(Debug)]
pub enum Stopped {
    /// `list-panes` failed: the prototype logs it and skips the cycle.
    ListPanes(TmuxError),
    /// A sensor failed: the prototype raises and the cycle is lost.
    Failed(CycleError, Vec<String>),
}

/// The parts of the poller one cycle reads.
#[derive(Clone, Copy)]
pub struct Sensors<'a, T: Tmux + ?Sized> {
    /// The tmux server holding the fleet.
    pub tmux: &'a T,
    /// The host load, disk and memory.
    pub probe: &'a dyn HostProbe,
    /// Paths and thresholds.
    pub config: &'a Config,
    /// When true, rename nothing.
    pub dry_run: bool,
}

/// `cycle()`'s dry-run line for one watched pane (988152c line 1527), with
/// Python's `str()` of each value: `True`/`False`, `None`, and the list of
/// alert kinds as its `repr`, or `-` when empty.
///
/// Test: `dry_run_pane_lines_match_python` (tests/verbs_poll.rs).
pub(crate) fn dry_run_line(
    sess: &str,
    pid: &str,
    cmd: &str,
    info: &PaneInfo,
    found: &[Alert],
) -> String {
    let py_bool = |b: bool| if b { "True" } else { "False" };
    let py_opt = |v: Option<u64>| v.map_or_else(|| "None".to_owned(), |n| n.to_string());
    let alerts = if found.is_empty() {
        "-".to_owned()
    } else {
        let kinds: Vec<String> = found.iter().map(|a| format!("'{}'", a.kind)).collect();
        format!("[{}]", kinds.join(", "))
    };
    format!(
        "  {sess} {pid} cmd={cmd} box={} agents={} busy={} dialog={} ctx={} plan={}/{} q={} alerts={alerts}",
        info.box_reading.as_str(),
        info.agents,
        py_bool(info.busy),
        py_bool(info.dialog.is_some()),
        py_opt(info.ctx),
        py_opt(info.plan5h),
        py_opt(info.planwk),
        py_bool(info.question.as_deref().is_some_and(|q| !q.is_empty())),
    )
}

/// Runs one cycle's sensing and dedup against `state` at time `now`.
///
/// # Errors
/// [`Stopped::ListPanes`] when tmux cannot list panes (nothing in `state`
/// changes); [`Stopped::Failed`] when `events.jsonl` or the host cannot be
/// read, after `state` may have changed: the caller must discard it.
pub fn collect<T: Tmux + ?Sized>(
    s: Sensors<'_, T>,
    transcripts: &mut TranscriptCtx,
    state: &mut PollState,
    now: f64,
) -> Result<Collected, Stopped> {
    let listing = list_panes(s.tmux).map_err(Stopped::ListPanes)?;
    let mut log: Vec<String> = listing
        .skipped
        .iter()
        .map(|row| format!("list-panes row without a %N pane id skipped: {row:?}"))
        .collect();
    log.extend(rename_windows(s.tmux, s.dry_run).1);
    let watched: Vec<&Pane> = listing
        .panes
        .iter()
        .filter(|p| p.session.starts_with("tm-") && !s.config.skips(&p.session))
        .collect();
    let mut candidates = Vec::new();
    let mut infos = Vec::new();
    let mut current = BTreeMap::new();
    for p in &watched {
        let (sess, pid) = (p.session.as_str(), p.pane.as_target());
        current.insert(sess.to_owned(), pid.to_owned());
        let ps = state.panes.entry(format!("{sess}|{pid}")).or_default();
        if is_shell(&p.cmd) {
            ps.idle_since = None;
            if !is_test_fixture_session(sess) {
                let detail = format!("pane runs {}, not Claude", p.cmd);
                let fp = format!("shell-{}", p.cmd);
                candidates.push(alert("exited", &fp, &detail, sess, pid, now));
            }
            continue;
        }
        let raw = match capture(s.tmux, &p.pane) {
            Ok(raw) => raw,
            Err(e) => {
                log.push(format!("{sess} {pid} capture ERROR {e}"));
                continue;
            }
        };
        let mut info = parse_pane(&raw);
        if let Err(e) = transcripts.fill_ctx(s.config, &mut info, pid) {
            log.push(format!("transcript ctx ERROR {e}"));
        }
        let found = evaluate(&info, ps, now, sess, pid);
        if s.dry_run {
            // #9822: the prototype's per-pane dry-run line, for `tm architect poll --dry-run`.
            log.push(dry_run_line(sess, pid, &p.cmd, &info, &found));
        }
        candidates.extend(found);
        infos.push((sess.to_owned(), info));
    }
    for (sess, pid) in &state.sessions {
        if !current.contains_key(sess) && !is_test_fixture_session(sess) {
            candidates.push(alert(
                "exited",
                "gone",
                "session disappeared",
                sess,
                pid,
                now,
            ));
        }
    }
    let live: Vec<String> = current.iter().map(|(k, v)| format!("{k}|{v}")).collect();
    state.panes.retain(|k, _| live.contains(k));
    state.sessions = current;
    candidates.extend(fleet_plan_alerts(&infos, now));
    let failed = |e, log| Stopped::Failed(e, log);
    match read_events(&s.config.events(), &mut state.events_offset, now) {
        // #9822: a configured supervisor's own events drop too.
        Ok(found) => candidates.extend(found.into_iter().filter(|a| !s.config.skips(&a.session))),
        Err(e) => return Err(failed(CycleError::Events(e), log)),
    }
    match host_alerts(s.probe, s.config, now, Some(&mut state.band)) {
        Ok(found) => candidates.extend(found),
        Err(e) => return Err(failed(CycleError::Host(e), log)),
    }
    match self_ctx_alerts(s.config, now) {
        Ok(found) => candidates.extend(found),
        Err(e) => log.push(format!("self_ctx ERROR {e}")),
    }
    let quiet = read_quiet_sessions(&s.config.quiet_sessions);
    candidates.retain(|a| !(a.kind == "idle" && quiet.contains(&a.session)));
    let (new, suppressed) = dedup(&mut state.alerts, &candidates, now);
    Ok(Collected {
        watched: watched.len(),
        panes: listing.panes,
        new,
        suppressed,
        log,
    })
}
