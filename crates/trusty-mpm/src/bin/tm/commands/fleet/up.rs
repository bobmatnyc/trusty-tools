//! `tm architect up` and `attach`: bring the Architect up from any state (#11364).
//!
//! Why: reaching the Architect took `tm fleet status`, then `init`,
//! `relaunch` or `bind`, then `tmux attach`, and nothing covered a dead pane.
//! Owner ruling Dpg: bare `tm architect` is an idempotent `up` plus attach,
//! runnable from any prompt, and it never stops or restarts a live `claude`.
//! What: [`up`] takes `<dir>/.trusty-mpm/architect-up.lock`, reads the state
//! ([`observe`]), maps it to one [`Action`] (plan section 1.4) and runs the
//! existing fleet path for it: [`super::init_full`] for no session and for
//! ensure, [`super::relaunch::relaunch`] for a pane at a shell (after
//! `respawn-pane` for a dead one). A live unbound `claude` is reported, never
//! killed. Inside Claude Code it only ensures a bound Architect.
//! Test: `up_tests.rs`; `tests/tm_architect_up.rs`.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;
use trusty_mpm::core::process::{self, PaneClaude};
use trusty_mpm::session_manager::ArchitectRegistration;

use super::launch::PaneState;
use super::session_name::SessionNames;
use super::{Probe, Step, TMUX, config, relaunch, status};

/// The lock that makes two concurrent `up` calls one Architect.
pub(crate) const UP_LOCK: &str = ".trusty-mpm/architect-up.lock";

/// How long `up` waits for another `up` to release [`UP_LOCK`].
const LOCK_WAIT: Duration = Duration::from_secs(180);

/// What `up` found, one per row of the plan's state table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state", content = "detail")]
pub(crate) enum Observed {
    /// No tmux server, or no Architect session.
    NoSession,
    /// The session exists but its pane's process exited.
    DeadPane,
    /// The pane runs, with no `claude` in it.
    Shell,
    /// A `claude` runs and its launch record binds it.
    LiveBound,
    /// A `claude` runs that no launch record binds; the reason.
    LiveUnbound(String),
    /// Another Architect exists, or the session runs elsewhere.
    Elsewhere(String),
    /// tmux, the config or the process table could not be read.
    Unreadable(String),
}

/// What `up` does about an [`Observed`] state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Action {
    /// [`super::init_full`] with a launch.
    Init,
    /// `respawn-pane`, then launch bound into it.
    RespawnAndLaunch,
    /// Launch bound into the pane's shell.
    Launch,
    /// [`super::init_full`] over a bound Architect: poller and files only.
    Ensure,
    /// Report the state; change nothing.
    Report,
    /// Refuse: exit 2.
    Refuse,
    /// Cannot tell: exit 1.
    Fail,
}

/// What one `up` reads and runs; production passes [`LIVE`].
#[derive(Clone, Copy)]
pub(crate) struct Ops {
    /// The session's pane, stamp and `claude` PID.
    pub(crate) probe: Probe,
    /// Whether a `claude` runs in the session's pane.
    pub(crate) pane_claude: fn(&str) -> PaneClaude,
    /// `Ok` when the session's `claude` is the bound Architect; see [`status::bound`].
    pub(crate) bound: fn(&Path, &Path, Probe, &str) -> Result<String, String>,
    /// Runs an action that changes something.
    pub(crate) run: fn(Action, &Path, &Path, &SessionNames) -> anyhow::Result<Outcome>,
}

/// The real tmux server and fleet paths.
pub(crate) const LIVE: Ops = Ops {
    probe: TMUX,
    pane_claude: process::pane_claude,
    bound: status::bound,
    run: run_action,
};

/// The steps an action took and the registration to send.
#[derive(Debug, Default)]
pub(crate) struct Outcome {
    /// One entry per step.
    pub(crate) steps: Vec<Step>,
    /// Sent by the caller once the Architect runs bound.
    pub(crate) registration: Option<ArchitectRegistration>,
}

/// One `up` run, as printed or as `--json`.
#[derive(Debug, Serialize)]
pub(crate) struct UpReport {
    /// The Architect directory.
    pub(crate) dir: PathBuf,
    /// The Architect's tmux session.
    pub(crate) session: String,
    /// What was found.
    pub(crate) observed: Observed,
    /// What was (or, with `--dry-run`, would be) done.
    pub(crate) action: Action,
    /// Whether this was a `--dry-run`.
    pub(crate) dry_run: bool,
    /// One line per step.
    pub(crate) steps: Vec<String>,
    /// 0 up, 1 a step failed or not healthy, 2 refused.
    pub(crate) exit: i32,
    /// Sent by the caller; never serialized.
    #[serde(skip)]
    pub(crate) registration: Option<ArchitectRegistration>,
}

impl UpReport {
    /// The step lines, then one summary line.
    pub(crate) fn render(&self) -> String {
        let mut out: String = self.steps.iter().map(|s| format!("  {s}\n")).collect();
        let verb = if self.dry_run { "would" } else { "did" };
        out.push_str(&format!(
            "tm architect up: {} ({verb} {:?}); exit {}\n",
            describe(&self.observed),
            self.action,
            self.exit
        ));
        out
    }
}

/// A one-line description of `observed`.
fn describe(observed: &Observed) -> String {
    match observed {
        Observed::NoSession => "no Architect session runs".into(),
        Observed::DeadPane => "the Architect pane is dead".into(),
        Observed::Shell => "the Architect pane is at a shell".into(),
        Observed::LiveBound => "the Architect runs bound".into(),
        Observed::LiveUnbound(why) => format!("unbound: guard exemptions are off ({why})"),
        Observed::Elsewhere(why) => format!("refused: {why}"),
        Observed::Unreadable(why) => format!("cannot tell: {why}"),
    }
}

/// Read the Architect's state in `dir` (canonical).
///
/// Test: `each_state_table_row_maps_to_its_action`.
pub(crate) fn observe(dir: &Path, home: &Path, names: &SessionNames, ops: &Ops) -> Observed {
    let path = super::user_config_path(home);
    let others = super::read_or_empty(&path)
        .and_then(|raw| config::parse_user_config(&raw, &path))
        .map(|(_, typed)| config::other_architects(&typed, dir));
    match others {
        Err(e) => return Observed::Unreadable(format!("{e:#}")),
        Ok(o) if !o.is_empty() => {
            return Observed::Elsewhere(format!(
                "an Architect is already set up at {}; there is one per user",
                o[0].display()
            ));
        }
        Ok(_) => {}
    }
    let session = names.architect();
    match (ops.probe.pane)(session) {
        PaneState::Absent => Observed::NoSession,
        PaneState::Unknown(e) => Observed::Unreadable(e),
        PaneState::Live(d) | PaneState::Dead(d) if d != dir => Observed::Elsewhere(format!(
            "tmux session {session} runs in {}; there is one Architect per user",
            d.display()
        )),
        PaneState::Dead(_) => Observed::DeadPane,
        PaneState::Live(_) => match (ops.pane_claude)(session) {
            PaneClaude::Absent => Observed::Shell,
            PaneClaude::Unknown => {
                Observed::Unreadable(format!("cannot tell whether a `claude` runs in {session}"))
            }
            PaneClaude::Present => match (ops.bound)(dir, home, ops.probe, session) {
                Ok(_) => Observed::LiveBound,
                Err(why) => Observed::LiveUnbound(why),
            },
        },
    }
}

/// The action for `observed`; `inside` is a caller inside Claude Code.
///
/// Inside Claude Code, `up` never starts or stops `claude` (#10033): only a
/// bound Architect is ensured, every other state is reported.
pub(crate) fn action_for(observed: &Observed, inside: bool) -> Action {
    match observed {
        Observed::Elsewhere(_) => Action::Refuse,
        Observed::Unreadable(_) => Action::Fail,
        Observed::LiveBound => Action::Ensure,
        Observed::LiveUnbound(_) => Action::Report,
        _ if inside => Action::Report,
        Observed::NoSession => Action::Init,
        Observed::DeadPane => Action::RespawnAndLaunch,
        Observed::Shell => Action::Launch,
    }
}

/// Bring the Architect in `dir` up; see the module doc.
///
/// Why: plan section 1.4, the `up` state table; #11364 closure conditions.
/// What: refuses a directory [`super::preflight::check`] refuses (exit 2).
/// Without `dry_run`, holds [`UP_LOCK`] for the whole run, so a second `up`
/// waits and then finds the first one's Architect. Reads the state after the
/// lock, then runs its action through `ops.run`. Exit: 0 up; 1 a step failed,
/// the state cannot be read, or (inside Claude Code) not healthy; 2 refused.
/// Test: `each_state_table_row_maps_to_its_action`,
/// `two_concurrent_ups_start_one_architect`,
/// `up_never_kills_a_live_unbound_claude`,
/// `inside_claude_code_up_never_starts_or_stops_claude`,
/// `dry_run_changes_nothing`.
pub(crate) fn up(
    dir: &Path,
    home: &Path,
    names: &SessionNames,
    inside: bool,
    dry_run: bool,
    ops: Ops,
) -> UpReport {
    let mut report = UpReport {
        dir: dir.to_owned(),
        session: names.architect().to_owned(),
        observed: Observed::NoSession,
        action: Action::Refuse,
        dry_run,
        steps: Vec::new(),
        exit: 2,
        registration: None,
    };
    let dir = match super::preflight::check(dir, home) {
        Ok(d) => d,
        Err(e) => {
            report.observed = Observed::Elsewhere(e.to_string());
            report.steps.push(format!("refused: {e}"));
            return report;
        }
    };
    report.dir = dir.clone();
    let _lock = if dry_run {
        None
    } else {
        match lock(&dir) {
            Ok(f) => Some(f),
            Err(e) => {
                report.observed = Observed::Unreadable(e.clone());
                report.action = Action::Fail;
                report.steps.push(format!("FAILED  {e}"));
                report.exit = 1;
                return report;
            }
        }
    };
    report.observed = observe(&dir, home, names, &ops);
    report.action = action_for(&report.observed, inside);
    report.exit = match report.action {
        Action::Refuse => 2,
        Action::Fail => 1,
        Action::Report if inside => 1,
        _ => 0,
    };
    if dry_run
        || matches!(
            report.action,
            Action::Report | Action::Refuse | Action::Fail
        )
    {
        if let Observed::LiveUnbound(_) = report.observed {
            report.steps.push(
                "unbound: guard exemptions are off; `up` never kills it. Fix it with `tm \
                 architect restart` or `tm fleet bind`"
                    .into(),
            );
        }
        return report;
    }
    match (ops.run)(report.action, &dir, home, names) {
        Ok(outcome) => {
            if outcome.steps.iter().any(|s| matches!(s, Step::Failed(_))) {
                report.exit = 1;
            }
            report.steps.extend(
                outcome
                    .steps
                    .iter()
                    .map(|s| s.render_line().trim().to_owned()),
            );
            report.registration = outcome.registration;
        }
        Err(e) => {
            report.steps.push(format!("FAILED     {e:#}"));
            report.exit = 1;
        }
    }
    report
}

/// Hold an exclusive `flock` on `dir/`[`UP_LOCK`], waiting up to [`LOCK_WAIT`].
fn lock(dir: &Path) -> Result<File, String> {
    lock_within(dir, LOCK_WAIT)
}

/// Hold an exclusive `flock` on `dir/`[`UP_LOCK`], waiting up to `wait`.
/// `restart` passes zero, so it refuses at once while `up` or another
/// `restart` acts on the pane (#11365).
pub(crate) fn lock_within(dir: &Path, wait: Duration) -> Result<File, String> {
    let path = dir.join(UP_LOCK);
    let parent = path.parent().unwrap_or(dir);
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let deadline = Instant::now() + wait;
    loop {
        match try_flock(&file) {
            Ok(true) => return Ok(file),
            Ok(false) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(200))
            }
            Ok(false) => {
                return Err(format!(
                    "another `tm architect up` or `restart` holds {}",
                    path.display()
                ));
            }
            Err(e) => return Err(format!("cannot lock {}: {e}", path.display())),
        }
    }
}

/// `flock(LOCK_EX | LOCK_NB)`: `Ok(true)` held, `Ok(false)` would block.
fn try_flock(file: &File) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    // SAFETY: `file` owns a valid open descriptor for the duration of the call.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EWOULDBLOCK) => Ok(false),
        _ => Err(err),
    }
}

/// Production [`Ops::run`]: the existing fleet path for each action.
fn run_action(
    action: Action,
    dir: &Path,
    home: &Path,
    names: &SessionNames,
) -> anyhow::Result<Outcome> {
    use trusty_mpm::core::content_source::Fetch;
    use trusty_mpm::core::project_skill_tier::architect_seed::ArchitectSeed;
    match action {
        Action::Init | Action::Ensure => {
            let seed = |d: &Path| ArchitectSeed::resolve_for(d, Fetch::OnFirstUse);
            let r = super::init_full(dir, home, true, TMUX, None, seed, None)?;
            Ok(Outcome {
                steps: r.steps,
                registration: r.registration,
            })
        }
        Action::RespawnAndLaunch | Action::Launch => {
            let mut steps = Vec::new();
            if action == Action::RespawnAndLaunch {
                steps.push(respawn(names.architect(), dir)?);
            }
            let r = relaunch::relaunch(dir, home, names, relaunch::LIVE)?;
            steps.extend(r.steps);
            Ok(Outcome {
                steps,
                registration: r.registration,
            })
        }
        Action::Report | Action::Refuse | Action::Fail => Ok(Outcome::default()),
    }
}

/// `respawn-pane` on `session`'s dead pane, in `dir`, so a launch can type into it.
fn respawn(session: &str, dir: &Path) -> anyhow::Result<Step> {
    let argv = [
        "respawn-pane".to_owned(),
        "-t".to_owned(),
        trusty_mpm::core::tmux::exact_window_target(session),
        "-c".to_owned(),
        dir.display().to_string(),
    ];
    let out = trusty_mpm::core::tmux::run_tmux_argv(&argv)?;
    if !out.status.success() {
        anyhow::bail!(
            "tmux respawn-pane failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    // The new shell needs a moment before a launch line is typed into it.
    std::thread::sleep(Duration::from_secs(1));
    Ok(Step::Changed(format!(
        "respawned the dead pane of tmux session {session}"
    )))
}

#[cfg(test)]
#[path = "up_tests.rs"]
mod tests;
