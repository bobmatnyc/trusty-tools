//! Refuse a bare-`tm` in-place relaunch of a supervisor session (#9092).
//!
//! Why: the in-place relaunch execs a new `claude` from the daemon's session
//! record and never writes an Architect launch record, so the relaunched
//! Architect's anchor writes are denied (#9092). Owner ruling 2026-10-17
//! (option c with b as guard): `tm fleet relaunch` is the supported path,
//! and the in-place one refuses a supervisor session.
//! What: [`refusal`] names the first signal that marks `session` as a
//! supervisor session, in a message pointing at `tm fleet relaunch`. Any one
//! signal refuses (plan §2, ruling Q4): the project is in the operator's
//! `[supervisor] projects` allowlist (which every supervisor-profile
//! project is, so this covers the profile check too); the tmux session
//! carries a supervisor launch stamp; or an `*.architect-session` sidecar
//! records the session's name. A config or sidecar directory that cannot be
//! read refuses too (fail closed).
//! Test: `guided_supervisor_guard_tests.rs`;
//! `run_inplace_relaunch_refuses_a_supervisor_session_before_any_daemon_call`.

use std::ffi::OsStr;
use std::path::Path;

use trusty_mpm::core::architect_compat::is_architect_stamp;
use trusty_mpm::core::architect_session::recorded_session_names;
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::session_profile::is_allow_listed;

/// Reads tmux session `name`'s profile stamp; production passes
/// `fleet::launch::launch_stamp`.
pub(crate) type StampReader = fn(&str) -> Option<String>;

/// The refusal for an in-place relaunch of `session`, or `None` to proceed.
///
/// Why: see the module doc.
/// What: `cwd` is the project the relaunch would run in, `root` the
/// `~/.trusty-mpm` directory whose `config.toml` and `architect-launch/`
/// are read, and `stamp` reads the session's launch stamp. `Some` names the
/// signal and `tm fleet relaunch`.
/// Test: `an_allow_listed_project_is_refused`,
/// `a_supervisor_stamp_alone_is_refused`,
/// `a_recorded_session_name_alone_is_refused`,
/// `an_unreadable_config_or_sidecar_refuses`,
/// `a_pm_session_is_not_refused`.
pub(crate) fn refusal(
    session: &str,
    cwd: Option<&Path>,
    root: &Path,
    stamp: StampReader,
) -> Option<String> {
    let why = supervisor_signal(session, cwd, root, stamp)?;
    Some(format!(
        "refusing an in-place relaunch of supervisor session {session} ({why}): the relaunched \
         claude would carry no Architect launch record. Run `tm fleet relaunch` instead"
    ))
}

/// The first signal that marks `session` as a supervisor session.
fn supervisor_signal(
    session: &str,
    cwd: Option<&Path>,
    root: &Path,
    stamp: StampReader,
) -> Option<String> {
    let config = match user_config(root) {
        Ok(config) => config,
        Err(why) => return Some(format!("the user config cannot be read: {why}")),
    };
    if let Some(cwd) = cwd
        && is_allow_listed(cwd, config.architect_grant())
    {
        return Some(format!(
            "{} is in the operator's Architect allowlist",
            cwd.display()
        ));
    }
    if is_architect_stamp(stamp(session).as_deref().map(OsStr::new)) {
        return Some("the session carries the supervisor launch stamp".to_owned());
    }
    match recorded_session_names(root) {
        Err(why) => Some(format!(
            "the Architect launch records cannot be read: {why}"
        )),
        Ok(names) if names.iter().any(|n| n == session) => {
            Some("an Architect launch record names the session".to_owned())
        }
        Ok(_) => None,
    }
}

/// `root/config.toml`, parsed; absent is the default config, and a file that
/// cannot be read or parsed is `Err` (unlike `MpmConfig::load`).
fn user_config(root: &Path) -> Result<MpmConfig, String> {
    let path = root.join("config.toml");
    match std::fs::read_to_string(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(MpmConfig::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
        Ok(raw) => toml::from_str(&raw).map_err(|e| format!("{}: {e}", path.display())),
    }
}

#[cfg(test)]
#[path = "guided_supervisor_guard_tests.rs"]
mod tests;
