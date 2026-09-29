//! `tm fleet status` — the four facts a working Architect needs, read-only (#8436).
//!
//! Why: the supervisor gate fails closed to the PM profile (#8453), so a
//! half-set-up Architect runs silently as a PM. The operator needs the
//! missing piece named.
//! What: [`status`] reads the allowlist, the profile request, the
//! `tm-architect` session and its launch stamp. It writes nothing.
//! [`this_session_check`] adds whether the calling session is bound as the
//! Architect, naming the failed identity check as the hook does (#8878 PR-I).
//! Test: `status_reports_incomplete_setup`,
//! `fleet_status_names_why_this_session_is_not_bound`, `tests/tm_fleet.rs`.

use std::path::{Path, PathBuf};

use serde::Serialize;
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::project_config::PROJECT_CONFIG_FILE;
use trusty_mpm::core::session_profile::{self, SUPERVISOR_PROFILE_ID};

use super::{ARCHITECT_SESSION, Probe, user_config_path};
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
    /// The tmux session name checked.
    pub(crate) session: &'static str,
    /// `allowlist`, `profile`, `session`, `launch_stamp`, in that order.
    pub(crate) checks: Vec<Check>,
    /// Whether every check passed.
    pub(crate) complete: bool,
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
        if let Some(c) = &self.this_session {
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
/// project's `.trusty-mpm.toml` requests the supervisor; `session` —
/// `tm-architect` runs, in `dir`; `launch_stamp` — that session carries the
/// stamp `supervisor`. `probe` reads tmux (see [`Probe`]).
/// Test: `status_reports_incomplete_setup`,
/// `fleet_init_launches_the_architect_and_status_is_complete`.
pub(crate) fn status(dir: &Path, home: &Path, probe: Probe) -> StatusReport {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let checks = vec![
        allowlist_check(&dir, home),
        profile_check(&dir),
        session_check(&dir, probe),
        stamp_check(probe),
    ];
    let complete = checks.iter().all(|c| c.ok);
    StatusReport {
        dir,
        session: ARCHITECT_SESSION,
        checks,
        complete,
        this_session: None,
    }
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

/// Is `tm-architect` running, in `dir`?
fn session_check(dir: &Path, probe: Probe) -> Check {
    let (ok, detail) = match (probe.pane)(ARCHITECT_SESSION).dir() {
        None => (
            false,
            format!("tmux session {ARCHITECT_SESSION} is not running"),
        ),
        Some(running) if running == dir => {
            (true, format!("tmux session {ARCHITECT_SESSION} is running"))
        }
        Some(running) => (
            false,
            format!(
                "tmux session {ARCHITECT_SESSION} runs in {}, not here",
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
fn stamp_check(probe: Probe) -> Check {
    let (ok, detail) = match (probe.stamp)() {
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
