//! Bind every Architect `claude` a tm launch starts in tmux (#8878, owner
//! ruling 2026-11-08: the Architect is the operator's twin however tm
//! launched it).
//!
//! Why: the trust-anchor floor admits only a `claude` with a launch record.
//! `tm fleet init|relaunch` wrote one; the other paths that start the same
//! supervisor-profile `claude` wrote none, so that Architect was refused its
//! own anchor writes.
//! What: every launch that stamps a profile binds an Architect it started:
//! [`crate::runtime::cli_launch::send_spec_launch`] (`tm connect`, `tm
//! launch`, `tm session start`, the daemon client) calls [`bind_live`]; the
//! daemon's managed spawn and resume bind in `managed_launch::deliver_in`;
//! the GUI spawn binds in `TmuxService::spawn_claude`; `tm fleet` binds
//! through [`crate::core::architect_session::bind_architect`], the one record
//! writer every path reaches. A non-Architect launch is
//! [`Binding::NotArchitect`] and costs nothing. The `claude` is found under
//! the session's pane shell, never named by a caller. A session that was
//! already running is not re-bound ([`Binding::NotStarted`]); re-binding a
//! running Architect is #8878 PR2. The bare-`tm` in-place relaunch refuses a
//! supervisor session before it execs (#9092). [`DEFERRED`] names the one
//! path that does not bind yet. Every failure is a [`Binding`] the caller
//! reports; none grants anything, since no record means the floor denies.
//! Test: `architect_bind_tests.rs`.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::architect_compat::is_architect_stamp;
use crate::core::architect_session::bind_architect;
use crate::core::session_profile::SESSION_PROFILE_ENV;
use crate::core::twin_identity::ArmingRecord;
use crate::runtime::launch_spec::LaunchSpec;

/// How many times [`bind_live`] looks for the `claude` (with [`FIND_DELAY`]
/// between): 10 s, as `tm fleet init` waits.
const FIND_ATTEMPTS: u8 = 20;

/// The pause between two looks for the `claude`.
const FIND_DELAY: Duration = Duration::from_millis(500);

/// The launch paths that can start an Architect `claude` and do not bind it
/// yet, with why; the module doc, the changelog and
/// `every_architect_spawn_site_binds_8878` name the same list.
pub const DEFERRED: &[(&str, &str)] = &[(
    "tm run (core/standalone/run.rs)",
    "it runs `claude` as a direct child in no tmux session, and a launch record \
     carries the session-name sidecar the #8902 pane guard and `tm fleet status` \
     read; a session-less record is #8878 PR2",
)];

/// What binding a launched `claude` as the Architect came to.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub enum Binding {
    /// The spec does not start an Architect; nothing was recorded.
    NotArchitect,
    /// The Architect `claude` was recorded.
    Bound(ArmingRecord),
    /// No `claude` appeared under the session's pane shell.
    NoClaude(String),
    /// A `claude` was found but its record did not land or read back.
    NotRecorded(String),
    /// The session was already running, so its `claude` is not re-bound.
    NotStarted,
}

impl Binding {
    /// The operator warning for an Architect left unbound; `None` otherwise.
    ///
    /// Test: `an_unbound_architect_names_the_relaunch`.
    pub fn warning(&self, session: &str) -> Option<String> {
        let why = match self {
            Self::NotArchitect | Self::Bound(_) => return None,
            Self::NoClaude(why) | Self::NotRecorded(why) => why.as_str(),
            Self::NotStarted => "the session was already running",
        };
        Some(format!(
            "warning: the Architect `claude` in tmux session {session} is NOT bound ({why}); \
             pm-guard denies its trust-anchor writes until `tm fleet relaunch` binds it"
        ))
    }
}

/// Whether `spec` starts an Architect: the last `TRUSTY_MPM_SESSION_PROFILE`
/// it sets (the one the child sees) is the Architect stamp.
///
/// Test: `only_an_architect_stamped_spec_is_bound`.
pub fn spec_is_architect(spec: &LaunchSpec) -> bool {
    let stamp = spec
        .env_set
        .iter()
        .rev()
        .find(|(key, _)| key == SESSION_PROFILE_ENV)
        .map(|(_, value)| OsStr::new(value.as_str()));
    is_architect_stamp(stamp)
}

/// [`bind_profile`] for a launch `spec`: its last stamp and its `cwd`.
///
/// Test: `only_an_architect_stamped_spec_is_bound`.
pub fn bind_launch(
    root: Option<&Path>,
    spec: &LaunchSpec,
    session: &str,
    find: impl FnOnce() -> Option<u32>,
) -> Binding {
    bind_profile(root, spec_is_architect(spec), &spec.cwd, session, find)
}

/// What a launch into a session that was already running binds: nothing.
///
/// Why: only a `claude` this command started may be recorded; the pane's
/// `claude` may be one it found there (#8878 critic). Re-binding a running
/// Architect is #8878 PR2.
/// Test: `a_reused_session_binds_nothing_8878`.
pub fn bind_reused(architect: bool) -> Binding {
    if architect {
        Binding::NotStarted
    } else {
        Binding::NotArchitect
    }
}

/// Bind the `claude` `find` locates for a launch started in `session` in
/// `project`, under `root` (`~/.trusty-mpm`), when `architect` says the launch
/// carries the Architect stamp.
///
/// Why/What: see the module doc. `root` `None` (no home) is
/// [`Binding::NotRecorded`]; `find` runs only for an Architect launch. The
/// record's project is the directory the hook later reads as
/// `CLAUDE_PROJECT_DIR`.
/// Test: `only_an_architect_stamped_spec_is_bound`,
/// `an_architect_spec_with_no_claude_or_no_home_is_unbound`.
pub fn bind_profile(
    root: Option<&Path>,
    architect: bool,
    project: &Path,
    session: &str,
    find: impl FnOnce() -> Option<u32>,
) -> Binding {
    if !architect {
        return Binding::NotArchitect;
    }
    let Some(root) = root else {
        return Binding::NotRecorded("the home directory cannot be resolved".to_owned());
    };
    let Some(pid) = find() else {
        return Binding::NoClaude(format!(
            "no `claude` process appeared in {session} within 10 s"
        ));
    };
    match bind_architect(root, pid, project, session) {
        Ok(record) => Binding::Bound(record),
        Err(why) => Binding::NotRecorded(why),
    }
}

/// [`bind_launch`] on the live system: the home's `~/.trusty-mpm` (the root
/// the pm-guard hook reads) and the `claude` under `session`'s pane shell on
/// tmux `server`.
pub fn bind_live(server: Option<&str>, session: &str, spec: &LaunchSpec) -> Binding {
    bind_launch(home_root().as_deref(), spec, session, || {
        crate::core::process::find_claude_pid_in_tmux_on(server, session, FIND_ATTEMPTS, FIND_DELAY)
    })
}

/// The process home's `~/.trusty-mpm`, the root the pm-guard hook reads.
pub fn home_root() -> Option<PathBuf> {
    dirs::home_dir()
        .filter(|home| home.is_absolute())
        .map(|home| home.join(".trusty-mpm"))
}

/// The `claude` under the pane shell `shell_pid` reads, retried as
/// [`bind_live`] retries; `None` at once when no shell PID reads (no pane).
///
/// Why: the daemon paths read the pane through their own tmux driver.
/// Test: `the_daemon_delivery_binds_an_architect_spec_8878`.
pub fn find_claude_with(mut shell_pid: impl FnMut() -> Option<u32>) -> Option<u32> {
    for attempt in 0..FIND_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(FIND_DELAY);
        }
        if let Some(pid) = crate::core::process::claude_child_of(shell_pid()?) {
            return Some(pid);
        }
    }
    None
}

#[cfg(test)]
#[path = "architect_bind_tests.rs"]
mod tests;
