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
//! [`skills_check`] adds whether the Architect's shipped skills are current
//! in its project tier (#9540); it is never part of `complete`. #10448: an
//! unbound `binding` makes the setup incomplete.
//! It also warns on the pre-#9997 records directory and moves nothing.
//! #9092: it warns, naming the PID, when the session's live `claude` has no
//! launch record.
//! Test: `status_warns_when_the_live_claude_has_no_launch_record_9092`,
//! `status_reports_incomplete_setup`,
//! `status_warns_about_a_stale_records_dir_and_moves_nothing`,
//! `fleet_status_names_why_this_session_is_not_bound`,
//! `the_skills_row_names_a_missing_or_custom_skill`,
//! `status_binding_needs_the_record_and_the_same_session`, `tests/tm_fleet.rs`.

use std::path::{Path, PathBuf};

use serde::Serialize;
use trusty_mpm::core::architect_session::check_session_binding;
use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::project_config::PROJECT_CONFIG_FILE;
use trusty_mpm::core::project_skill_tier::architect_skills;
use trusty_mpm::core::session_profile::{self, ARCHITECT_WRITTEN_PROFILE_ID};
use trusty_mpm::core::twin_arming;

use super::launch::PaneState;
use super::session_name::{self, SessionNames};
use super::{Probe, user_config_path};
use crate::commands::pm_guard_architect_reason::session_binding;
use crate::commands::pm_guard_trust_anchor::{ClaudeLookup, HookEnv};

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
    /// Whether every check passed and `binding` is ok (#10448).
    pub(crate) complete: bool,
    /// Whether the session's `claude` is the Architect tm launched, under
    /// this session name (#8878 R1); #10448: part of `complete`.
    pub(crate) binding: Check,
    /// Whether the Architect's shipped skills are current in its project
    /// tier (#9540); informational, never part of `complete`.
    pub(crate) skills: Check,
    /// Which prototype commit and content the seeded bundle came from
    /// (#10571); informational, never part of `complete`.
    pub(crate) bundle: Check,
    /// Whether the calling session is bound as the Architect; informational,
    /// never part of `complete`. `None` when not evaluated.
    pub(crate) this_session: Option<Check>,
    /// #9092: the warning that the session's live `claude` has no launch
    /// record, naming its PID; `None` when it has one or none was found.
    pub(crate) unrecorded: Option<String>,
}

impl StatusReport {
    /// One line per check, then a verdict line.
    pub(crate) fn render(&self) -> String {
        let mut out = format!("Architect: {}\n", self.dir.display());
        for c in &self.checks {
            let mark = if c.ok { "ok     " } else { "MISSING" };
            out.push_str(&format!("  {mark}  {:<12} {}\n", c.name, c.detail));
        }
        // #9540: a skipped or failed skill deploy is not ok.
        let mark = if self.skills.ok { "ok     " } else { "WARN   " };
        out.push_str(&format!(
            "  {mark}  {:<12} {}\n",
            self.skills.name, self.skills.detail
        ));
        let mark = if self.bundle.ok { "ok     " } else { "WARN   " };
        out.push_str(&format!(
            "  {mark}  {:<12} {}\n",
            self.bundle.name, self.bundle.detail
        ));
        for c in std::iter::once(&self.binding).chain(&self.this_session) {
            let mark = if c.ok { "ok     " } else { "UNBOUND" };
            out.push_str(&format!("  {mark}  {:<12} {}\n", c.name, c.detail));
        }
        if let Some(warning) = &self.unrecorded {
            out.push_str(&format!("WARNING: {warning}\n"));
        }
        if self.complete {
            out.push_str("complete\n");
        } else if self.checks.iter().all(|c| c.ok) {
            // #10448: set up but unbound; `init` refuses a running session.
            out.push_str(&format!(
                "incomplete: the Architect is not bound; run `tm fleet relaunch --dir {}` to \
                 rebind it\n",
                self.dir.display()
            ));
        } else {
            // #8995: a bare `tm fleet init` could set up a second Architect.
            out.push_str(&format!(
                "incomplete: run `tm fleet init --dir {}` to finish the setup\n",
                self.dir.display()
            ));
        }
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
/// replaced by `tm-architect`. `binding` comes from [`binding_check`];
/// `complete` needs all four checks and `binding` (#10448).
/// `probe` reads tmux (see [`Probe`]).
/// Test: `status_reports_incomplete_setup`,
/// `fleet_init_launches_the_architect_and_status_is_complete`,
/// `status_is_complete_only_when_the_architect_is_bound`,
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
    let binding = binding_check(&dir, home, probe, &names);
    // #10448: an unbound Architect is denied every anchor write; not complete.
    let complete = checks.iter().all(|c| c.ok) && binding.ok;
    let unrecorded = names
        .as_ref()
        .ok()
        .and_then(|n| unrecorded_claude(&dir, home, probe, n.architect()));
    StatusReport {
        unrecorded,
        binding,
        skills: skills_check(&dir),
        bundle: bundle_check(&dir),
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

/// The warning that the live `claude` in `session` has no launch record.
///
/// Why: #9092 — a `claude` started outside `tm fleet init|relaunch` (a
/// hand-typed `claude --resume`) runs unbound for hours with every exemption
/// silently denied; the operator needs the PID named, not only `UNBOUND`.
/// What: `Some` only when the pane is live, `probe` finds its `claude`, and
/// `home`'s `architect-launch/` holds no record for that PID (a record that
/// does not read is reported by `binding` instead). Names the PID, the record
/// path and `tm fleet relaunch`.
/// Test: `status_warns_when_the_live_claude_has_no_launch_record_9092`.
fn unrecorded_claude(dir: &Path, home: &Path, probe: Probe, session: &str) -> Option<String> {
    if !matches!((probe.pane)(session), PaneState::Live(_)) {
        return None;
    }
    let pid = (probe.claude)(session)?;
    let root = home.join(".trusty-mpm");
    let records = trusty_mpm::core::architect_launch::ARCHITECT_RECORDS;
    if !matches!(records.read(&root, pid), Ok(None)) {
        return None;
    }
    Some(format!(
        "the live `claude` pid {pid} in tmux session {session} has no Architect launch record \
         ({}); it runs UNBOUND and every Architect exemption is denied. Run `tm fleet relaunch \
         --dir {}` from an operator terminal",
        records.path(&root, pid).display(),
        dir.display()
    ))
}

/// `Ok` when the `claude` in `session` is the bound Architect of `dir`, else
/// why not; shared by [`binding_check`] and `init`'s running-session step.
pub(super) fn bound(
    dir: &Path,
    home: &Path,
    probe: Probe,
    session: &str,
) -> Result<String, String> {
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
/// Production passes [`this_session_env`] (#8938).
/// Test: `fleet_status_names_why_this_session_is_not_bound`,
/// `this_session_reaches_claude_through_the_bash_shell`,
/// `a_nested_claude_under_the_architect_is_not_this_session`.
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

/// The calling process's inputs for [`this_session_check`] (#8938).
///
/// Why: `tm fleet status` runs from the Bash tool, `claude → zsh → tm`, so
/// the hook's one-hop `claude` lookup never reaches the Architect.
/// What: [`HookEnv::ambient`] with the `claude` found by
/// [`twin_arming::nearest_claude_for_status`] (at most three hops, the first
/// `claude` wins). The hook keeps its one-hop lookup; see
/// `twin_arming::nearest_claude_ancestor`.
/// Test: `this_session_reaches_claude_through_the_bash_shell`.
pub(crate) fn this_session_env() -> HookEnv {
    HookEnv {
        claude: ClaudeLookup::new(|| twin_arming::nearest_claude_for_status(std::process::id())),
        ..HookEnv::ambient()
    }
}

/// Are the Architect's shipped skills current in `dir`'s project tier?
///
/// Why: #9540 — the launch deploy is fail-open and keeps a project-custom
/// copy, so a failed or skipped deploy must show here.
/// What: [`architect_skills::health`]; it names each skill that is missing,
/// stale, project-custom or unreadable. #9997: a [`stale_records_warning`]
/// also makes the row not ok and joins its detail. Read-only.
/// Test: `the_skills_row_names_a_missing_or_custom_skill`,
/// `status_warns_about_a_stale_records_dir_and_moves_nothing`.
pub(crate) fn skills_check(dir: &Path) -> Check {
    let (mut ok, mut detail) = architect_skills::health(dir);
    // #9997 (decision 1a): warn about the old layout; never move it.
    if let Some(warning) = stale_records_warning(dir) {
        ok = false;
        detail = format!("{detail}; {warning}");
    }
    Check {
        name: "skills",
        ok,
        detail,
    }
}

/// The seeded bundle's provenance, from `docs/architect/bundle-provenance.json`.
///
/// Why: #10571, an install must say which prototype commit and content it runs.
/// What: ok with the provenance line when the seeded stamp parses; a warning
/// naming `tm fleet init` when it is absent or unreadable. Reads only.
/// Test: `init_and_status_report_the_bundle_provenance`.
pub(crate) fn bundle_check(dir: &Path) -> Check {
    let path = dir.join(super::seed::refresh::PROVENANCE_DEST);
    let line = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| super::seed::refresh::provenance_line(&t));
    Check {
        name: "bundle",
        ok: line.is_some(),
        detail: line.unwrap_or_else(|| {
            format!(
                "no readable {}: run `tm fleet init --dir {}` to seed it",
                super::seed::refresh::PROVENANCE_DEST,
                dir.display()
            )
        }),
    }
}

/// The directory an Architect seeded before #9997 kept its records in.
pub(crate) const STALE_RECORDS_DIR: &str = "records";

/// A warning when `dir` still holds the pre-#9997 records directory.
///
/// Why: #9997 decisions 1a and 2. `tm fleet init` seeds `local/` on a fresh
/// project only, and every launch refreshes the skills, which now name
/// `local/`. An Architect whose records are still in the old directory reads
/// the wrong paths until the owner moves them.
/// What: `Some` when `dir`'s old records path is a directory, or when that
/// cannot be checked (fail closed: a warning, never a silent pass); `None`
/// when it is absent or a file. Reads only and moves nothing.
/// Test: `status_warns_about_a_stale_records_dir_and_moves_nothing`,
/// `an_uncheckable_stale_dir_is_a_warning`.
pub(crate) fn stale_records_warning(dir: &Path) -> Option<String> {
    let path = dir.join(STALE_RECORDS_DIR);
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => Some(format!(
            "stale `{STALE_RECORDS_DIR}/` directory at {}: the refreshed skills now name \
             `local/`; move its files into `local/` yourself (tm moves nothing)",
            path.display()
        )),
        Ok(_) => None,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) => Some(format!(
            "cannot check for a stale `{STALE_RECORDS_DIR}/` directory at {}: {err}",
            path.display()
        )),
    }
}

/// Is `dir` in the user-level allowlist?
fn allowlist_check(dir: &Path, home: &Path) -> Check {
    let path = user_config_path(home);
    let (ok, detail) = match std::fs::read_to_string(&path) {
        Err(_) => (false, format!("{} is absent or unreadable", path.display())),
        Ok(raw) => match super::config::parse_user_config(&raw, &path) {
            Err(err) => (false, format!("{err:#}")),
            Ok((_, cfg)) if session_profile::is_allow_listed(dir, cfg.architect_grant()) => (
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
    let ok = session_profile::requested(dir).is_architect();
    let file = dir.join(PROJECT_CONFIG_FILE);
    let detail = if ok {
        format!(
            "`profile = \"{ARCHITECT_WRITTEN_PROFILE_ID}\"` in {}",
            file.display()
        )
    } else {
        format!(
            "{} does not request `profile = \"{ARCHITECT_WRITTEN_PROFILE_ID}\"`",
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
        // #9755: either stamp value is the Architect.
        Some(v) if trusty_mpm::core::architect_compat::is_architect_stamp(Some(v.as_ref())) => {
            (true, format!("launched as `{v}`"))
        }
        Some(v) => (
            false,
            format!("launched as `{v}`, not `{ARCHITECT_WRITTEN_PROFILE_ID}`"),
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
