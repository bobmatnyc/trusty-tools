//! `tm fleet relaunch` — restart the Architect's `claude` and bind it (#9092).
//!
//! Why: a bare `tm` in the Architect's pane execs a new `claude` that no
//! launch record names, so the pm-guard denies the Architect's own anchor
//! writes and `session_send` loses its tmux identity (#9092). Owner ruling
//! D6f: `relaunch` stops the `claude` in the pane, starts a new one with the
//! flags `init` uses, and records it. `init` still starts an absent session.
//! What: [`relaunch`] refuses, before it changes anything, a directory that
//! is not an Architect, a session that is absent (naming `tm fleet init`),
//! unreadable, dead, in another directory or with more than one pane, a pane
//! whose `claude` it cannot locate, and a launch that cannot be prepared
//! ([`launch::prepare_restart`], the launch path `init` uses). Then it stops
//! the pane's `claude` ([`stop_claude`]), proves the pane holds none, and
//! starts and records the new one. A new `claude` that is
//! not recorded is a [`Step::Failed`]. Last it removes this session's replaced
//! launch records ([`prune_replaced`]); `fleet::run` then re-registers the
//! Architect with the daemon (#8942).
//! Test: `relaunch_tests.rs`; `tests/tm_fleet.rs` (`fleet_relaunch_*`).

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow, bail};
use trusty_mpm::core::architect_conversation::ConversationStart;
use trusty_mpm::core::architect_launch::{ARCHITECT_DIR, ARCHITECT_RECORDS};
use trusty_mpm::core::architect_session::{SESSION_EXT, read_sidecar};
use trusty_mpm::core::architect_sidecar_prune as prune;
use trusty_mpm::core::process::{self, PaneClaude};
use trusty_mpm::core::tmux;
use trusty_mpm::core::twin_arming::process_facts;
use trusty_mpm::session_manager::ArchitectRegistration;

use super::launch::{self, PaneState};
use super::session_name::SessionNames;
use super::{Probe, Step, TMUX, register, status};

/// How long a stopped `claude` gets after SIGTERM before SIGKILL.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// How long a `claude` gets after SIGKILL before the stop fails.
const KILL_GRACE: Duration = Duration::from_secs(2);

/// The pause between two "is it gone" reads.
const STOP_POLL: Duration = Duration::from_millis(100);

/// What [`relaunch`] reads and does; production passes [`LIVE`].
///
/// Why: the unit tests must reach no tmux server and stop no process.
#[derive(Clone, Copy)]
pub(crate) struct Ops {
    /// The session's pane, stamp and `claude` PID.
    pub(crate) probe: Probe,
    /// `Err` unless a launch in the directory runs the supervisor profile.
    pub(crate) architect: fn(&Path, &str) -> anyhow::Result<()>,
    /// How many panes the session has over all its windows; see [`pane_count`].
    pub(crate) panes: fn(&str) -> Result<usize, String>,
    /// Whether a `claude` runs in the session's pane, three-state.
    pub(crate) pane_claude: fn(&str) -> PaneClaude,
    /// Stop the `claude` at a PID; see [`stop_claude`].
    pub(crate) stop: fn(u32) -> Result<(), String>,
    /// Prepare a new `claude`, touching no tmux session; the returned
    /// [`launch::Pending`] starts and records it. See [`launch::prepare_restart`].
    pub(crate) prepare: fn(&Path, &Path, &str) -> anyhow::Result<launch::Pending>,
}

/// The real tmux server, process table and launch path.
pub(crate) const LIVE: Ops = Ops {
    probe: TMUX,
    architect: launch::require_architect,
    panes: pane_count,
    pane_claude: process::pane_claude,
    stop: stop_claude,
    prepare: launch::prepare_restart,
};

/// The steps one `relaunch` run took.
#[derive(Debug)]
pub(crate) struct RelaunchReport {
    /// One entry per step, in order.
    pub(crate) steps: Vec<Step>,
    /// The Architect's session.
    pub(crate) session: String,
    /// The registration [`super::run`] sends; set only once the new `claude`
    /// is recorded.
    pub(crate) registration: Option<ArchitectRegistration>,
}

impl RelaunchReport {
    /// Whether any step failed; the command then exits 1.
    pub(crate) fn failed(&self) -> bool {
        self.steps.iter().any(|s| matches!(s, Step::Failed(_)))
    }

    /// One line per step, then a summary line.
    pub(crate) fn render(&self) -> String {
        let mut out: String = self.steps.iter().map(Step::render_line).collect();
        out.push_str(&if self.failed() {
            "The Architect is NOT fully relaunched: fix the FAILED step and run `tm fleet \
             relaunch` again.\n"
                .to_owned()
        } else {
            format!(
                "Architect relaunched in tmux session {}. Check it with `tm fleet status`.\n",
                self.session
            )
        });
        out
    }
}

/// Restart the Architect's `claude` in its running session and record it.
///
/// Why: see the module doc; the plan of record is #9092 §3 as revised by
/// owner ruling D6f (relaunch restarts `claude`, then re-records).
/// What: in order — the supervisor check; the session must run live in
/// `dir` with exactly one pane, so every session-addressed read, signal and
/// keystroke reaches the Architect's own pane; the pane's `claude`, when one
/// runs, is located under the pane shell; the new launch is prepared
/// (`ops.prepare`), so each launch-path refusal comes before any stop; the
/// old `claude` is stopped, and the pane must then still be the only one and
/// hold no `claude`; the new `claude` is started and recorded; this
/// session's replaced launch records are removed. Every refusal before the
/// stop says nothing was stopped; every failure after it names the stopped
/// PID. A failed record is a [`Step::Failed`] with no registration.
/// Test: `relaunch_refuses_an_absent_session_and_names_init`,
/// `relaunch_refuses_before_any_change_when_the_pane_cannot_be_read`,
/// `relaunch_refuses_a_session_with_more_than_one_pane`,
/// `relaunch_stops_nothing_when_the_launch_cannot_be_prepared`,
/// `relaunch_names_the_stopped_pid_when_the_new_claude_fails`,
/// `relaunch_stops_the_old_claude_before_the_new_one_starts`,
/// `relaunch_never_types_into_a_pane_that_still_holds_a_claude`,
/// `relaunch_reports_an_unrecorded_claude_as_failed`,
/// `fleet_relaunch_restarts_the_architect_and_rebinds_it`.
pub(crate) fn relaunch(
    dir: &Path,
    home: &Path,
    names: &SessionNames,
    ops: Ops,
) -> anyhow::Result<RelaunchReport> {
    let session = names.architect();
    let dir = std::fs::canonicalize(dir).with_context(|| {
        format!(
            "{} is not an Architect directory; set one up with `tm fleet init`",
            dir.display()
        )
    })?;
    (ops.architect)(&dir, session)?;
    require_live_session(&dir, session, ops.probe)?;
    require_one_pane(session, ops)?;
    let old = match (ops.pane_claude)(session) {
        PaneClaude::Absent => None,
        PaneClaude::Unknown => bail!(
            "cannot tell whether a `claude` runs in tmux session {session}: tmux or the process \
             table could not be read; nothing was changed"
        ),
        PaneClaude::Present => Some(pane_claude_pid(&dir, session, ops)?),
    };
    // #9092 critic HIGH: every launch-path refusal comes before the stop.
    let pending = (ops.prepare)(&dir, home, session)
        .with_context(|| format!("nothing was stopped in tmux session {session}"))?;
    let mut steps = Vec::new();
    match old {
        None => steps.push(Step::Unchanged(format!(
            "no `claude` ran in tmux session {session}"
        ))),
        Some(pid) => steps.push(stop_pane_claude(&dir, home, session, pid, ops)?),
    }
    // From here on the old `claude` is gone; each error names it.
    let stopped = || match old {
        Some(pid) => format!(
            "stopped claude pid {pid} in tmux session {session}, but no new `claude` runs \
             there; run `tm fleet relaunch` again"
        ),
        None => format!("no new `claude` runs in tmux session {session}"),
    };
    require_one_pane(session, ops).with_context(stopped)?;
    let (binding, conversation) = pending().with_context(stopped)?;
    if let ConversationStart::Fresh {
        reason: Some(_), ..
    } = &conversation
    {
        eprintln!("warning: {}", conversation.describe());
    }
    let mut report = RelaunchReport {
        steps,
        session: session.to_owned(),
        registration: None,
    };
    let record = match binding {
        Ok(record) => record,
        Err(err) => {
            report.steps.push(Step::Failed(format!(
                "started a new `claude` in tmux session {session}, but it is NOT bound ({err}), \
                 so its anchor writes will be denied"
            )));
            return Ok(report);
        }
    };
    report.steps.push(Step::Changed(format!(
        "started a new `claude` in tmux session {session} on the `opus` alias, bound to claude \
         pid {}, {} with remote control on",
        record.pid,
        conversation.describe()
    )));
    let root = home.join(".trusty-mpm");
    report.steps.extend(prune_replaced(
        &root,
        &dir,
        session,
        record.pid,
        &prune::start_time,
    ));
    report.registration = Some(register::registration(&dir, names));
    Ok(report)
}

/// `Ok` when `session` runs with a live pane in `dir`; else why not.
fn require_live_session(dir: &Path, session: &str, probe: Probe) -> anyhow::Result<()> {
    match (probe.pane)(session) {
        PaneState::Live(running) if running == dir => Ok(()),
        PaneState::Live(running) => bail!(
            "tmux session {session} runs in {}, not in {}; not relaunching it",
            running.display(),
            dir.display()
        ),
        // #9092 ruling Q3: relaunch never starts a session; init does.
        PaneState::Absent => bail!(
            "no tmux session {session} runs, so there is no Architect to relaunch; start it with \
             `tm fleet init --dir {}`",
            dir.display()
        ),
        PaneState::Dead(_) => bail!(
            "the pane of tmux session {session} is dead; stop it with `tmux kill-session -t \
             ={session}`, then run `tm fleet init --dir {}`",
            dir.display()
        ),
        PaneState::Unknown(err) => {
            bail!("cannot read tmux session {session}: {err}; nothing was changed")
        }
    }
}

/// `Ok` when `session` has exactly one pane over all its windows (#9092).
///
/// Why: critic HIGH — the pane reads, the stop and the typed `claude` line
/// all address the session's active pane, which a split or a second window
/// makes another pane than the Architect's.
fn require_one_pane(session: &str, ops: Ops) -> anyhow::Result<()> {
    match (ops.panes)(session) {
        Ok(1) => Ok(()),
        Ok(n) => bail!(
            "tmux session {session} has {n} panes; `tm fleet relaunch` acts only on a session \
             whose one pane is the Architect's. Close the other panes and windows, then run it \
             again; nothing was changed by this check"
        ),
        Err(why) => bail!("cannot count the panes of tmux session {session}: {why}"),
    }
}

/// How many panes tmux session `name` has, over all its windows (#9092).
///
/// What: `list-panes -s -t =<name> -F '#{pane_id}'`, one line per pane.
/// Test: `fleet_relaunch_refuses_a_split_architect_session`.
pub(crate) fn pane_count(name: &str) -> Result<usize, String> {
    let argv = [
        "list-panes",
        "-s",
        "-t",
        &tmux::exact_session_target(name),
        "-F",
        "#{pane_id}",
    ]
    .map(str::to_owned);
    let out = tmux::run_tmux_argv(&argv).map_err(|e| format!("cannot run tmux: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "tmux list-panes failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count())
}

/// The PID of the `claude` under `session`'s pane shell (`ops.probe.claude`),
/// never one the caller names; none found is `Err`.
fn pane_claude_pid(dir: &Path, session: &str, ops: Ops) -> anyhow::Result<u32> {
    (ops.probe.claude)(session).ok_or_else(|| {
        anyhow!(
            "a `claude` runs in tmux session {session} but not under its shell, so it cannot be \
             stopped safely; stop it with `tmux kill-session -t ={session}`, then run `tm fleet \
             init --dir {}`",
            dir.display()
        )
    })
}

/// Stop the `claude` at `pid` in `session`'s pane and prove the pane holds
/// none; the step says whether it was bound.
fn stop_pane_claude(
    dir: &Path,
    home: &Path,
    session: &str,
    pid: u32,
    ops: Ops,
) -> anyhow::Result<Step> {
    let was = match status::bound(dir, home, ops.probe, session) {
        Ok(_) => "the bound",
        Err(_) => "the unbound",
    };
    (ops.stop)(pid).map_err(|why| {
        anyhow!(
            "could not stop claude pid {pid} in tmux session {session} ({why}); nothing new was \
             started"
        )
    })?;
    // #9092: never type the launch line into a pane where a `claude` reads it.
    if (ops.pane_claude)(session) != PaneClaude::Absent {
        bail!(
            "stopped claude pid {pid}, but tmux session {session} still holds a `claude`, or \
             cannot say; nothing new was started"
        );
    }
    Ok(Step::Changed(format!(
        "stopped {was} `claude` pid {pid} in tmux session {session}"
    )))
}

/// Stop the `claude` at `pid`: SIGTERM, then SIGKILL, waiting for it to go.
///
/// Why: #9092 ruling D6f — relaunch replaces the running `claude`.
/// What: reads `pid`'s start time and refuses a process not named `claude`.
/// Sends SIGTERM and waits up to [`STOP_GRACE`], then SIGKILL and up to
/// [`KILL_GRACE`]. Gone means the process table holds no `pid`, or holds it
/// with another start time. Still running after both is `Err`.
/// #9092 critic MEDIUM: first refuses a `pid` that is this process or one of
/// its ancestors — a relaunch run from inside the Architect would stop the
/// `claude` running it.
/// Test: `stop_claude_ends_a_claude_process`,
/// `stop_claude_refuses_a_process_not_named_claude`,
/// `stop_claude_refuses_an_ancestor_of_this_process`.
pub(crate) fn stop_claude(pid: u32) -> Result<(), String> {
    if is_self_or_ancestor(pid, std::process::id()) {
        return Err(format!(
            "pid {pid} runs this `tm` (it is this process or an ancestor of it); run `tm fleet \
             relaunch` from an operator terminal outside the Architect's session"
        ));
    }
    let start = process_facts(pid)?.start_time;
    if !process::process_name_is_claude(pid) {
        return Err(format!("pid {pid} is not a `claude` process"));
    }
    let gone = || process_facts(pid).map_or(true, |f| f.start_time != start);
    for (signal, grace) in [(Signal::Term, STOP_GRACE), (Signal::Kill, KILL_GRACE)] {
        signal.send(pid)?;
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if gone() {
                return Ok(());
            }
            std::thread::sleep(STOP_POLL);
        }
    }
    Err(format!(
        "pid {pid} still runs {} s after SIGTERM and SIGKILL",
        (STOP_GRACE + KILL_GRACE).as_secs()
    ))
}

/// Whether `pid` is `from` or on its parent chain, read from the process
/// table. A chain that cannot be read further ends the walk; it is bounded
/// at 128 hops against a cycle.
fn is_self_or_ancestor(pid: u32, from: u32) -> bool {
    let mut current = from;
    for _ in 0..128 {
        if current == pid {
            return true;
        }
        match process_facts(current).ok().and_then(|f| f.parent) {
            Some(parent) if parent != current && parent != 0 => current = parent,
            _ => return false,
        }
    }
    false
}

/// The two signals [`stop_claude`] sends.
#[derive(Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

impl Signal {
    #[cfg(unix)]
    fn send(self, pid: u32) -> Result<(), String> {
        use nix::sys::signal::{Signal as Nix, kill};
        let raw = i32::try_from(pid).map_err(|_| format!("pid {pid} is out of range"))?;
        let signal = match self {
            Self::Term => Nix::SIGTERM,
            Self::Kill => Nix::SIGKILL,
        };
        match kill(nix::unistd::Pid::from_raw(raw), signal) {
            // Gone already: the wait loop sees it.
            Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
            Err(e) => Err(format!("cannot signal pid {pid}: {e}")),
        }
    }

    #[cfg(not(unix))]
    fn send(self, pid: u32) -> Result<(), String> {
        Err(format!("cannot signal pid {pid} on this platform"))
    }
}

/// Remove `session`'s launch records that a relaunch replaced (#9092 Q6).
///
/// Why: each relaunch leaves the old `<pid>.architect` pair behind, and
/// `init`'s prune keeps every sidecar whose session still exists.
/// What: for each `*.architect-session` sidecar under `root` that names
/// `session` and a PID other than `keep`, whose `<pid>.architect` record is
/// absent or names `dir`, and whose launch is proven gone (`start_time` says
/// no such process, or another start time): removes the record, then the
/// sidecar. An unreadable sidecar, another session's or project's, and one
/// whose process may live are kept. `None` when nothing was removed.
/// Test: `prune_replaced_removes_only_this_sessions_dead_records`.
pub(crate) fn prune_replaced(
    root: &Path,
    dir: &Path,
    session: &str,
    keep: u32,
    start_time: &dyn Fn(u32) -> Result<Option<u64>, String>,
) -> Option<Step> {
    let records = root.join(ARCHITECT_DIR);
    let entries = match std::fs::read_dir(&records) {
        Ok(entries) => entries,
        Err(e) => {
            return Some(Step::Unchanged(format!(
                "replaced launch records kept: cannot list {} ({e})",
                records.display()
            )));
        }
    };
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
        if path.extension().and_then(|e| e.to_str()) != Some(SESSION_EXT) {
            continue;
        }
        let Ok((pid, recorded, name)) = read_sidecar(&path) else {
            continue;
        };
        if pid == keep || name != session {
            continue;
        }
        let this_project = match ARCHITECT_RECORDS.read(root, pid) {
            Ok(None) => true,
            Ok(Some(record)) => std::fs::canonicalize(&record.project_dir).is_ok_and(|p| p == dir),
            Err(_) => false,
        };
        let launch_gone = match start_time(pid) {
            Ok(None) => true,
            Ok(Some(t)) => t != recorded,
            Err(_) => false,
        };
        if !(this_project && launch_gone) {
            continue;
        }
        let record = ARCHITECT_RECORDS.path(root, pid);
        match remove_if_present(&record).and_then(|()| remove_if_present(&path)) {
            Ok(()) => removed.push(pid.to_string()),
            Err(e) => kept.push(format!("pid {pid}: {e}")),
        }
    }
    let kept = (!kept.is_empty()).then(|| format!("could not remove: {}", kept.join("; ")));
    if removed.is_empty() {
        return kept.map(|k| Step::Unchanged(format!("replaced launch records kept; {k}")));
    }
    let mut text = format!(
        "removed the replaced launch record of claude pid {}",
        removed.join(", ")
    );
    if let Some(k) = kept {
        text.push_str(&format!("; {k}"));
    }
    Some(Step::Changed(text))
}

/// Remove `path`; one that is already gone counts as removed.
fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
#[path = "relaunch_tests.rs"]
mod tests;
