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
//! It runs the usage step after the fleet plan (#9801 slice 1).
//! df-T7i P1b adds the completion alerts (each pane, and the Architect's
//! own outside a dry run), the queued ctx-cycle failures, the `run_*`
//! scripts and the zone watch (outside a dry run) and the selftest alert.
//! #11540 adds the ledger shim (live and shadow; its failures are `failing`).
//! #11541 review: the last self-refresh fault and the Architect pane's own
//! capture or self-ctx read failures are `self_refresh` alerts in `failing`.
//! Steps owned elsewhere are left out: effort, token, pm_intake,
//! dashboard, owner_*, auto_sleep, user_state and triage rules
//! (#9801); the watchdog and Remote Control (#9779).
//! Test: `tests/cycle_ported.rs` (the prototype's cycle tests, by name),
//! `cycle_falls_back_to_transcript_when_status_ctx_is_none`,
//! `failed_cycle_commits_nothing`.

use std::collections::BTreeMap;

use super::architect::is_shell;
use super::completion::{completion_alerts, il_task_complete_for};
use super::config::Config;
use super::ctx::TranscriptCtx;
use super::events::read_events;
use super::host::{HostProbe, host_alerts};
use super::inbox_alerts::{ctx_cycle_alerts, selftest_alerts};
use super::listing::{Pane, capture, list_panes};
use super::push::{PushRoute, push_step};
use super::routines::routine_alerts;
use super::self_ctx_alerts::self_ctx_alerts;
use super::self_refresh::self_refresh_fault_alerts;
use super::slack::PushSender;
use super::state::PollState;
use super::steps::ledger_shim::run_ledger_shim;
use super::steps::scripts::ScriptSteps;
use super::steps::usage::{ProbeLauncher, probe_idle, usage_step};
use super::watchdog::Relauncher;
use super::watchdog::wiring::watchdog_alerts;
use super::windows::rename_windows;
use super::zone::{read_zone, zone_alerts};
use crate::classify::{Alert, PaneInfo, alert, dedup, evaluate, fleet_plan_alerts, parse_pane};
use crate::error::{CycleError, TmuxError};
use crate::quiet::{is_test_fixture_session, read_quiet_sessions};
use crate::self_ctx::self_ctx_tokens;
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
    /// The watchdog's `architect=...` phrase; empty when it is off (#9779).
    pub watch_phrase: String,
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
    /// Starts the usage step's `/usage` probe (#9801).
    pub usage_probe: &'a dyn ProbeLauncher,
    /// When true, rename nothing, relaunch nothing and start no probe (a dry
    /// run or a shadow run).
    pub dry_run: bool,
    /// The watchdog's relaunch path; `None` turns the watchdog off (#9779).
    pub relauncher: Option<&'a dyn Relauncher>,
    /// The script steps and the subprocess seam the zone watch reads
    /// through; `None` turns both off (df-T7i P1b; tests).
    pub steps: Option<&'a ScriptSteps<'a>>,
    /// Sends the owner push (#9779 D5t); `None` makes a due push with a
    /// channel set a `push_failed` alert.
    pub pusher: Option<&'a dyn PushSender>,
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
    // #9779: the prototype runs the watchdog before the pane loop.
    let mut watch_found = Vec::new();
    let mut watch_phrase = String::new();
    if let Some(r) = s.relauncher {
        let (found, phrase, lines) =
            watchdog_alerts(s.tmux, &listing.panes, state, s.config, now, r, s.dry_run);
        (watch_found, watch_phrase) = (found, phrase);
        log.extend(lines);
    }
    // df-T7i P1b: the Architect's own task-complete refresh (the prototype's
    // architect_rc tail; its Remote Control half is not ported).
    let mut candidates = Vec::new();
    let mut sup_faults = Vec::new();
    if !s.dry_run {
        let (found, faults, lines) = supervisor_completion(&s, &listing.panes, state, now);
        candidates.extend(found);
        sup_faults = faults;
        log.extend(lines);
    }
    let watched: Vec<&Pane> = listing
        .panes
        .iter()
        .filter(|p| p.session.starts_with("tm-") && !s.config.skips(&p.session))
        .collect();
    let mut infos = Vec::new();
    let mut current = BTreeMap::new();
    let mut idle_panes = Vec::new();
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
        if probe_idle(&info) {
            idle_panes.push((sess.to_owned(), pid.to_owned())); // #9801: a /usage probe may type here
        }
        let mut found = evaluate(&info, ps, now, sess, pid);
        // df-T7i P1b: a completed task asks for a refresh below the bar.
        let il = |name: &str| il_task_complete_for(&s.config.il_schedule, name);
        found.extend(completion_alerts(
            &mut state.other,
            sess,
            pid,
            &info,
            now,
            &il,
        ));
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
    // #9801: a usage failure is a log line, never a stopped cycle.
    let usage = &s.config.usage;
    let (found, lines) = usage_step(
        usage,
        s.usage_probe,
        &mut state.other,
        now,
        &idle_panes,
        s.dry_run,
    );
    candidates.extend(found);
    log.extend(lines);
    // df-T7i P1b: queued ctx-cycle failures; the run_* scripts; selftest.
    let queue = &s.config.ctx_cycle_alerts;
    candidates.extend(ctx_cycle_alerts(&mut state.other, queue, now, s.dry_run));
    if let Some(steps) = s.steps.filter(|_| !s.dry_run) {
        log.extend(steps.run(now));
    }
    // #11540: the ledger-bound steps run in Python, live or shadow (never in
    // a dry run); a shim failure joins `failing` below.
    let mut shim_failing = Vec::new();
    if let Some(steps) = s.steps.filter(|_| !s.dry_run || s.config.shadow) {
        let shim = run_ledger_shim(steps, s.config, now);
        candidates.extend(shim.alerts);
        state.other.extend(shim.state);
        log.extend(shim.log);
        shim_failing = shim.failing;
    }
    candidates.extend(selftest_alerts(&s.config.selftest_state, now));
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
    if let Some(steps) = s.steps.filter(|_| !s.dry_run) {
        let read = || read_zone(steps.runner);
        let (found, lines) = zone_alerts(&mut state.other, now, read, &s.config.zone_log);
        candidates.extend(found);
        log.extend(lines);
    }
    match self_ctx_alerts(s.config, now) {
        Ok(found) => candidates.extend(found),
        Err(e) => log.push(format!("self_ctx ERROR {e}")),
    }
    // #9779 D5r: routines ran; an overdue failure is due a push (never sent).
    let mut failing = watch_found.clone();
    failing.extend(shim_failing);
    // #11541: self-refresh faults (last run's record, this cycle's reads).
    let sup = &s.config.supervisor;
    for a in self_refresh_fault_alerts(&state.other, sup, now)
        .into_iter()
        .chain(sup_faults)
    {
        if !failing
            .iter()
            .any(|f| f.kind == a.kind && f.fingerprint == a.fingerprint)
        {
            failing.push(a);
        }
    }
    match routine_alerts(&s.config.routines, &s.config.supervisor, now) {
        Ok(found) => failing.extend(found),
        Err(e) => {
            // #9779: a bad routines config is a failure, not a silent off.
            log.push(format!("routines ERROR {e}"));
            let detail = format!("routines config: {e}");
            let sup = &s.config.supervisor;
            failing.push(alert("routine_stale", "config", &detail, sup, "", now));
        }
    }
    // #9779 D5t: an overdue failure is a Slack DM; a failed send alerts.
    let route = PushRoute {
        channel: s.config.push_channel.as_deref(),
        recipient: s.config.push_recipient.as_deref(),
        sender: s.pusher,
        hands_off: s.dry_run,
        supervisor: &s.config.supervisor,
    };
    let (push_failed, lines) = push_step(&mut state.other, &failing, now, &route);
    log.extend(lines);
    candidates.extend(push_failed);
    candidates.extend(failing);
    let quiet = read_quiet_sessions(&s.config.quiet_sessions);
    candidates.retain(|a| !(a.kind == "idle" && quiet.contains(&a.session)));
    let (new, suppressed) = dedup(&mut state.alerts, &candidates, now);
    Ok(Collected {
        watched: watched.len(),
        panes: listing.panes,
        new,
        suppressed,
        log,
        watch_phrase,
    })
}

/// The Architect pane's completion alert: its status line has no `ctx N%`,
/// so its own transcript gives the percent (`None` when unreadable). A failed
/// capture or self-ctx read is also a `self_refresh` fault alert (#11541).
fn supervisor_completion<T: Tmux + ?Sized>(
    s: &Sensors<'_, T>,
    panes: &[Pane],
    state: &mut PollState,
    now: f64,
) -> (Vec<Alert>, Vec<Alert>, Vec<String>) {
    let mut sup: Vec<&Pane> = panes
        .iter()
        .filter(|p| p.session == s.config.supervisor && !is_shell(&p.cmd))
        .collect();
    sup.sort_by_key(|p| p.active != "11");
    let Some(p) = sup.first() else {
        return (Vec::new(), Vec::new(), Vec::new());
    };
    let sess = s.config.supervisor.clone();
    let fault = |fp: &str, e: &dyn std::fmt::Display| {
        let detail = format!("self-refresh {fp}: {e}");
        alert("self_refresh", fp, &detail, &sess, "", now)
    };
    let mut info = match capture(s.tmux, &p.pane) {
        Ok(raw) => parse_pane(&raw),
        Err(e) => {
            let line = format!("architect rc capture ERROR {e}");
            return (Vec::new(), vec![fault("architect-capture", &e)], vec![line]);
        }
    };
    let (mut faults, mut lines) = (Vec::new(), Vec::new());
    let tokens = match self_ctx_tokens(&s.config.self_ctx_dir) {
        Ok(t) => t,
        Err(e) => {
            lines.push(format!("architect rc self_ctx ERROR {e}"));
            faults.push(fault("tokens", &e));
            None
        }
    };
    info.ctx = match (tokens, s.config.self_ctx_window) {
        (Some(t), Some(w)) if w > 0 => u64::try_from(100 * i128::from(t) / i128::from(w)).ok(),
        _ => None,
    };
    let il = |name: &str| il_task_complete_for(&s.config.il_schedule, name);
    let found = completion_alerts(&mut state.other, &sess, p.pane.as_target(), &info, now, &il);
    (found, faults, lines)
}
