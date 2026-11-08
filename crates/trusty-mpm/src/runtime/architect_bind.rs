//! Bind every Architect `claude` a CLI launch starts (#8878, owner ruling
//! 2026-11-08: the Architect is the operator's twin however tm launched it).
//!
//! Why: the trust-anchor floor admits only a `claude` with a launch record.
//! `tm fleet init|relaunch` wrote one; `tm connect`, `tm launch`, `tm session
//! start` and the daemon client's launch and connect start the same
//! supervisor-profile `claude` from the same kind of spec and wrote none, so
//! that Architect was refused its own anchor writes.
//! What: [`crate::runtime::cli_launch::send_spec_launch`] — the one function
//! that types a launch line into a pane — calls [`bind_live`] after the line is
//! typed. A spec whose effective `TRUSTY_MPM_SESSION_PROFILE` is not the
//! Architect's is [`Binding::NotArchitect`] and costs nothing. For an Architect
//! spec the `claude` is found under the session's pane shell, never named by a
//! caller, and recorded through
//! [`crate::core::architect_session::bind_architect`], the one record writer
//! the fleet path calls too. Every failure is a [`Binding`] the caller reports;
//! none grants anything, since no record means the floor denies.
//! Test: `architect_bind_tests.rs`.

use std::ffi::OsStr;
use std::path::Path;
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
}

impl Binding {
    /// The operator warning for an Architect left unbound; `None` otherwise.
    ///
    /// Test: `an_unbound_architect_names_the_relaunch`.
    pub fn warning(&self, session: &str) -> Option<String> {
        let why = match self {
            Self::NotArchitect | Self::Bound(_) => return None,
            Self::NoClaude(why) | Self::NotRecorded(why) => why,
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

/// Bind the `claude` `find` locates for an Architect `spec` started in
/// `session`, under `root` (`~/.trusty-mpm`).
///
/// Why/What: see the module doc. `root` `None` (no home) is
/// [`Binding::NotRecorded`]; `find` runs only for an Architect spec. The
/// record's project is the spec's `cwd`, the directory the hook later reads
/// as `CLAUDE_PROJECT_DIR`.
/// Test: `only_an_architect_stamped_spec_is_bound`,
/// `an_architect_spec_with_no_claude_or_no_home_is_unbound`.
pub fn bind_launch(
    root: Option<&Path>,
    spec: &LaunchSpec,
    session: &str,
    find: impl FnOnce() -> Option<u32>,
) -> Binding {
    if !spec_is_architect(spec) {
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
    match bind_architect(root, pid, &spec.cwd, session) {
        Ok(record) => Binding::Bound(record),
        Err(why) => Binding::NotRecorded(why),
    }
}

/// [`bind_launch`] on the live system: the home's `~/.trusty-mpm` (the root
/// the pm-guard hook reads) and the `claude` under `session`'s pane shell on
/// tmux `server`.
pub fn bind_live(server: Option<&str>, session: &str, spec: &LaunchSpec) -> Binding {
    let root = dirs::home_dir()
        .filter(|home| home.is_absolute())
        .map(|home| home.join(".trusty-mpm"));
    bind_launch(root.as_deref(), spec, session, || {
        crate::core::process::find_claude_pid_in_tmux_on(server, session, FIND_ATTEMPTS, FIND_DELAY)
    })
}

#[cfg(test)]
#[path = "architect_bind_tests.rs"]
mod tests;
