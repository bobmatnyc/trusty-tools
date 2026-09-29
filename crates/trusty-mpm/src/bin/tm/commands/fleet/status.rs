//! `tm fleet status` — the four facts a working Architect needs, read-only (#8436).
//!
//! Why: the supervisor gate fails closed to the PM profile (#8453), so a
//! half-set-up Architect runs silently as a PM. The operator needs the
//! missing piece named.
//! What: [`status`] reads the allowlist, the profile request, the
//! Architect's session and its launch stamp, and whether that session's
//! `claude` is the bound Architect (#8878 R1). It writes nothing. The
//! session is `--session`, else the recorded name, else `tm-architect`.
//! [`this_session_check`] adds whether the calling session is bound as the
//! Architect, naming the failed identity check as the hook does (#8878 PR-I).
//! Test: `status_reports_incomplete_setup`,
//! `fleet_status_names_why_this_session_is_not_bound`,
//! `status_binding_needs_the_record_and_the_same_session`, `tests/tm_fleet.rs`.

use std::path::{Path, PathBuf};

use serde::Serialize;
use trusty_mpm::core::architect_session::check_session_binding;
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::project_config::PROJECT_CONFIG_FILE;
use trusty_mpm::core::session_profile::{self, SUPERVISOR_PROFILE_ID};

use super::launch::PaneState;
use super::session_name::{self, SessionNames};
use super::{Probe, user_config_path};
use crate::commands::pm_guard_architect_reason::session_binding;
use crate::commands::pm_guard_trust_anchor::HookEnv;

/// One check's verdict.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct Check {
    /// Stable check id.
    pub(crate) name: &'static str,
    /// Whether the check passed.
    pub(crate) ok: bool,
    /// What was found.
    pub(crate) detail: String,
}

/// The whole `tm fleet status` report; its JSON shape is the `--json` output.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct StatusReport {
    /// The Architect directory checked.
    pub(crate) dir: PathBuf,
    /// The tmux session name checked; empty when it could not be read.
    pub(crate) session: String,
    /// `allowlist`, `profile`, `session`, `launch_stamp`, in that order.
    pub(crate) checks: Vec<Check>,
    /// Whether every check passed.
    pub(crate) complete: bool,
    /// Whether the session's `claude` is the Architect tm launched, under
    /// this session name (#8878 R1); informational, never part of `complete`.
    pub(crate) binding: Check,
    /// Whether the calling session is bound as the Architect; informational,
    /// never part of `complete`. `None` when not evaluated.
    pub(crate) this_session: Option<Check>,
}

impl StatusReport {
    /// One line per check, then a verdict line.
    pub(crate) fn render(&self) -> String {
        let mut out = format!("Architect: {}\n", self.dir.display());
        for c in &self.checks {
            let mark = if c.ok { "ok     " } else { "MISSING" };
            out.push_str(&format!("  {mark}  {:<12} {}\n", c.name, c.detail));
        }
        for c in std::iter::once(&self.binding).chain(&self.this_session) {
            let mark = if c.ok { "ok     " } else { "UNBOUND" };
            out.push_str(&format!("  {mark}  {:<12} {}\n", c.name, c.detail));
        }
        out.push_str(if self.complete {
            "complete\n"
        } else {
            "incomplete: run `tm fleet init` to finish the setup\n"
        });
        out
    }
}

/// Read the Architect's setup in `dir` under `home`; see the module doc.
///
/// Why: acceptance of the P2 brief item 2.
/// What: `allowlist` — `dir` is listed in `home`'s `[supervisor] projects`
/// (a malformed config counts as not listed and says so); `profile` — the
/// project's `.trusty-mpm.toml` requests the supervisor; `session` — the
/// Architect's session runs, in `dir`; `launch_stamp` — that session carries
/// the stamp `supervisor`. The session is `session`, else the name recorded
/// in `home`'s config ([`session_name::recorded_names`]); a recorded name
/// that cannot be read fails `session` and `launch_stamp` and is never
/// replaced by `tm-architect`. `binding` comes from [`binding_check`].
/// `probe` reads tmux (see [`Probe`]).
/// Test: `status_reports_incomplete_setup`,
/// `fleet_init_launches_the_architect_and_status_is_complete`,
/// `status_without_the_flag_reads_the_recorded_name`,
/// `a_malformed_recorded_name_refuses_init_and_fails_status`.
pub(crate) fn status(
    dir: &Path,
    home: &Path,
    probe: Probe,
    session: Option<&SessionNames>,
) -> StatusReport {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    // #8878 R1: the flag, else the recorded name; an unreadable one is an error.
    let names = session.map_or_else(|| session_name::recorded_names(home), |n| Ok(n.clone()));
    let checks = vec![
        allowlist_check(&dir, home),
        profile_check(&dir),
        session_check(&dir, probe, &names),
        stamp_check(probe, &names),
    ];
    let complete = checks.iter().all(|c| c.ok);
    StatusReport {
        binding: binding_check(&dir, home, probe, &names),
        session: names
            .as_ref()
            .map(|n| n.architect().to_owned())
            .unwrap_or_default(),
        dir,
        checks,
        complete,
        this_session: None,
    }
}

/// Whether the `claude` in the Architect's session is the Architect tm
/// launched for `dir`, under that session name.
///
/// Why: #8878 R1 Fail-Open Check — an unreadable recorded name, a missing
/// launch record, a tmux lookup error and a record naming another session
/// must each report not bound.
/// What: not bound unless `names` read, the session's pane is live, `probe`
/// finds its `claude`, and [`check_session_binding`] holds under `home`'s
/// `~/.trusty-mpm`. The detail names the failed check, never a PID.
/// Test: `status_binding_needs_the_record_and_the_same_session`.
pub(crate) fn binding_check(
    dir: &Path,
    home: &Path,
    probe: Probe,
    names: &Result<SessionNames, String>,
) -> Check {
    let result = match names {
        Err(err) => Err(err.clone()),
        Ok(names) => bound(dir, home, probe, names.architect()),
    };
    let (ok, detail) = match result {
        Ok(detail) => (true, detail),
        Err(why) => (false, format!("not bound: {why}")),
    };
    Check {
        name: "binding",
        ok,
        detail,
    }
}

fn bound(dir: &Path, home: &Path, probe: Probe, session: &str) -> Result<String, String> {
    match (probe.pane)(session) {
        PaneState::Live(_) => {}
        PaneState::Dead(_) => return Err(format!("the pane of tmux session {session} is dead")),
        PaneState::Absent => return Err(format!("tmux session {session} is not running")),
        PaneState::Unknown(err) => {
            return Err(format!("cannot read tmux session {session}: {err}"));
        }
    }
    let pid = (probe.claude)(session).ok_or_else(|| {
        format!("no `claude` process was found in tmux session {session}, or tmux could not say")
    })?;
    check_session_binding(&home.join(".trusty-mpm"), dir, session, pid)
        .map_err(|why| why.to_string())?;
    Ok(format!(
        "the `claude` in tmux session {session} is the Architect `tm fleet init` launched"
    ))
}

/// Whether the session running `tm fleet status` is bound as the Architect.
///
/// Why: #8878 PR-I — an unbound Architect is denied by the hook, and the
/// operator needs the failed check named here too, in the same words.
/// What: [`session_binding`] over `env` and `config`. A Bash tool call
/// carries no `CLAUDE_PROJECT_DIR`, so when `env` has none, `dir` stands in
/// as the launch directory. The detail names the reason, never a PID.
/// Test: `fleet_status_names_why_this_session_is_not_bound`.
pub(crate) fn this_session_check(
    dir: &Path,
    mut env: HookEnv,
    config: impl FnOnce() -> MpmConfig,
) -> Check {
    if session_profile::hook_project_dir(env.project_dir.clone()).is_none() {
        env.project_dir = Some(dir.as_os_str().to_owned());
    }
    let (ok, detail) = match session_binding(&env, config) {
        Ok(()) => (true, "this session is the Architect".to_owned()),
        Err(why) => (false, format!("this session is not the Architect: {why}")),
    };
    Check {
        name: "this_session",
        ok,
        detail,
    }
}

/// Is `dir` in the user-level allowlist?
fn allowlist_check(dir: &Path, home: &Path) -> Check {
    let path = user_config_path(home);
    let (ok, detail) = match std::fs::read_to_string(&path) {
        Err(_) => (false, format!("{} is absent or unreadable", path.display())),
        Ok(raw) => match super::config::parse_user_config(&raw, &path) {
            Err(err) => (false, format!("{err:#}")),
            Ok((_, cfg)) if session_profile::is_allow_listed(dir, &cfg.supervisor) => (
                true,
                format!("listed in `[supervisor] projects` of {}", path.display()),
            ),
            Ok(_) => (
                false,
                format!("not in `[supervisor] projects` of {}", path.display()),
            ),
        },
    };
    Check {
        name: "allowlist",
        ok,
        detail,
    }
}

/// Does the project request the supervisor profile?
fn profile_check(dir: &Path) -> Check {
    let ok = session_profile::requested(dir).is_supervisor();
    let file = dir.join(PROJECT_CONFIG_FILE);
    let detail = if ok {
        format!(
            "`profile = \"{SUPERVISOR_PROFILE_ID}\"` in {}",
            file.display()
        )
    } else {
        format!(
            "{} does not request `profile = \"{SUPERVISOR_PROFILE_ID}\"`",
            file.display()
        )
    };
    Check {
        name: "profile",
        ok,
        detail,
    }
}

/// Is the Architect's session running, in `dir`?
fn session_check(dir: &Path, probe: Probe, names: &Result<SessionNames, String>) -> Check {
    let session = match names {
        Ok(names) => names.architect(),
        Err(err) => {
            return Check {
                name: "session",
                ok: false,
                detail: err.clone(),
            };
        }
    };
    let (ok, detail) = match (probe.pane)(session).dir() {
        None => (false, format!("tmux session {session} is not running")),
        Some(running) if running == dir => (true, format!("tmux session {session} is running")),
        Some(running) => (
            false,
            format!(
                "tmux session {session} runs in {}, not here",
                running.display()
            ),
        ),
    };
    Check {
        name: "session",
        ok,
        detail,
    }
}

/// Does the running session carry the supervisor stamp?
fn stamp_check(probe: Probe, names: &Result<SessionNames, String>) -> Check {
    let stamp = match names {
        Ok(names) => (probe.stamp)(names.architect()),
        Err(err) => {
            return Check {
                name: "launch_stamp",
                ok: false,
                detail: err.clone(),
            };
        }
    };
    let (ok, detail) = match stamp {
        Some(v) if v == SUPERVISOR_PROFILE_ID => (true, format!("launched as `{v}`")),
        Some(v) => (
            false,
            format!("launched as `{v}`, not `{SUPERVISOR_PROFILE_ID}`"),
        ),
        None => (
            false,
            "no launch stamp: the session was not started by `tm fleet init`".to_owned(),
        ),
    };
    Check {
        name: "launch_stamp",
        ok,
        detail,
    }
}
