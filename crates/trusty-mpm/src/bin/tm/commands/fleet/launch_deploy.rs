//! The Architect bundle deploy every `tm architect up` and `restart` runs (#11368).
//!
//! Why: the bundle deployed only on `tm fleet init`, so an Architect that came
//! back through a launch or a restart kept the old scripts, and a running
//! poller or dashboard kept the old code. The scripts stay Python, managed by
//! tm (see #11368).
//! What: [`run`] plans each file with `seed::plan` (Q7: `dry_run` stops here
//! and writes nothing), hashes the [`POLLER_SCRIPTS`] and
//! [`DASHBOARD_SCRIPTS`] (Q5), keeps every file it will replace
//! (`launch_backup::keep`, Q1/Q3), writes through `seed::deploy_with`, whose
//! writes are temp-plus-rename, and hashes again. A changed poller script
//! restarts a running poller once; a changed dashboard script kickstarts a
//! loaded dashboard agent once; neither is started when stopped (Q4). Inside
//! Claude Code nothing is restarted (Q7). A failed plan, backup or write is
//! `Err`; a failed restart is a `Failed` step. The callers launch nothing on
//! either (Q8).
//! Test: `launch_deploy_tests.rs`; the `up` and `restart` wiring in
//! `up_tests.rs` and `restart_tests.rs`.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Context;
use trusty_mpm::core::content_source::Fetch;
use trusty_mpm::core::project_skill_tier::architect_seed::ArchitectSeed;

use super::launch::PaneState;
use super::launch_backup::{self, Kept};
use super::seed::plan::{Disposition, of_live};
use super::seed::{self, ShippedBefore, refresh};
use super::session_name::SessionNames;
use super::{Step, TMUX, dashboard_agent, poller, status};

/// The deployed scripts the poller runs: `fleet-poll.py`, every module it
/// or `fleet-classify.py` loads by path, and its start script (Q5).
pub(crate) const POLLER_SCRIPTS: &[&str] = &[
    "scripts/fleet-poll.py",
    "scripts/fleet-classify.py",
    "scripts/fleet_poll_cfg.py",
    "scripts/fleet_poll_io.py",
    "scripts/fleet_poll_alerts.py",
    "scripts/fleet_poll_wake.py",
    "scripts/fleet_poll_watchdog.py",
    "scripts/fleet_poll_selfrefresh.py",
    "scripts/fleet_poll_deps.py",
    "scripts/fleet_alert_dedupe.py",
    "scripts/input-state.py",
    "scripts/quiet-sessions.py",
    "scripts/self-ctx.py",
    "scripts/owner_answers.py",
    "scripts/architect-watchdog.py",
    "scripts/il-cron.py",
    "scripts/architect_config.py",
    "scripts/start-fleet-poll.sh",
];

/// The deployed scripts the dashboard server runs: `dashboard-serve.py` and
/// the modules it imports by name (Q5).
pub(crate) const DASHBOARD_SCRIPTS: &[&str] = &[
    "scripts/dashboard-serve.py",
    "scripts/dashboard_http.py",
    "scripts/dashboard_page.py",
    "scripts/dashboard_docs.py",
    "scripts/dashboard_il.py",
    "scripts/dashboard_media.py",
    "scripts/dashboard_viz.py",
    "scripts/dashboard_harness.py",
    "scripts/dashboard_harness_base.py",
    "scripts/dashboard_routes.py",
    "scripts/dashboard_fleet.py",
    "scripts/dashboard_mask.py",
    "scripts/dashboard_pane_tail.py",
    "scripts/dashboard_attach.py",
    "scripts/dashboard_projects.py",
    "scripts/dashboard_toml.py",
    "scripts/dashboard_watch.py",
    "scripts/dashboard_keynames.py",
    "scripts/dashboard_archctx.py",
    "scripts/dashboard_store.py",
    "scripts/dashboard_jsonl.py",
    "scripts/dashboard_schema.py",
    "scripts/dashboard_derive.py",
    "scripts/dashboard_ids.py",
    "scripts/dashboard_qual.py",
    "scripts/architect_config.py",
];

/// How one deploy runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Mode {
    /// Report the plan; write and restart nothing (Q7).
    pub(crate) dry_run: bool,
    /// Restart a running poller or dashboard whose scripts changed; `false`
    /// inside Claude Code (Q7).
    pub(crate) restart: bool,
}

/// Runs one deploy in an Architect directory; production passes [`live`].
pub(crate) type DeployFn = fn(&Path, &SessionNames, Mode) -> anyhow::Result<Vec<Step>>;

/// The processes a deploy may restart; [`LiveRestarter`] in production.
pub(crate) trait Restarter {
    /// Whether this Architect's poller session runs; `Err` when tmux cannot
    /// tell, or the session runs for another directory.
    fn poller_running(&self, dir: &Path, names: &SessionNames) -> Result<bool, String>;
    /// Stop the poller session and start it from the deployed scripts.
    fn restart_poller(&self, dir: &Path, names: &SessionNames) -> anyhow::Result<()>;
    /// Whether launchd has the dashboard agent loaded; `Err` when launchctl
    /// cannot tell.
    fn dashboard_loaded(&self) -> Result<bool, String>;
    /// Restart the dashboard under launchd.
    fn kickstart_dashboard(&self) -> anyhow::Result<()>;
    /// #11961: `Some(home)` when this run's `HOME` is not the login user's,
    /// so the user's launchd job must not be read or restarted.
    fn foreign_home(&self) -> Option<String>;
}

/// The live tmux server and launchd.
pub(crate) struct LiveRestarter;

impl Restarter for LiveRestarter {
    fn poller_running(&self, dir: &Path, names: &SessionNames) -> Result<bool, String> {
        pane_running((TMUX.pane)(names.poll()), dir, names.poll())
    }

    fn restart_poller(&self, dir: &Path, names: &SessionNames) -> anyhow::Result<()> {
        let argv = [
            "kill-session".to_owned(),
            "-t".to_owned(),
            trusty_common::tmux::exact_session_target(names.poll()),
        ];
        let out = trusty_mpm::core::tmux::run_tmux_argv(&argv)?;
        anyhow::ensure!(
            out.status.success(),
            "tmux kill-session failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        poller::start(dir, TMUX, names)
    }

    fn dashboard_loaded(&self) -> Result<bool, String> {
        launchd_loaded(&dashboard_agent::label())
    }

    fn kickstart_dashboard(&self) -> anyhow::Result<()> {
        dashboard_agent::kickstart()
    }

    fn foreign_home(&self) -> Option<String> {
        foreign_home_of(
            dirs::home_dir().as_deref(),
            dashboard_agent::passwd_home().as_deref(),
        )
    }
}

/// `Some(home)` unless `home` is the `passwd` home, by the `login_steps` rule.
///
/// Why: #11961 — `tm architect up` and `restart` kickstarted the login
/// user's dashboard job from a test's scratch HOME. An unset HOME is foreign
/// (fail closed).
/// Test: `a_scratch_home_skips_the_dashboard_restart_and_says_so_11961`,
/// `the_login_home_still_kickstarts_the_dashboard_11961`.
pub(crate) fn foreign_home_of(home: Option<&Path>, passwd: Option<&Path>) -> Option<String> {
    match home {
        Some(h) if dashboard_agent::same_home(passwd, h) => None,
        Some(h) => Some(h.display().to_string()),
        None => Some("an unset HOME".to_owned()),
    }
}

/// Whether poller session `session`, read as `state`, runs for `dir`.
///
/// Why: #11368 — as `poller::step_with`, a session that runs for another
/// directory is an error, never "not running" (fail closed).
/// Test: `a_poller_session_of_another_dir_is_an_error_11368`.
pub(crate) fn pane_running(state: PaneState, dir: &Path, session: &str) -> Result<bool, String> {
    match state {
        PaneState::Live(other) | PaneState::Dead(other) if other != dir => Err(format!(
            "tmux session {session} runs in {}, not in this Architect; stop it with \
             `tmux kill-session -t ={session}`",
            other.display()
        )),
        PaneState::Live(_) => Ok(true),
        PaneState::Absent | PaneState::Dead(_) => Ok(false),
        PaneState::Unknown(e) => Err(format!("cannot read tmux session {session}: {e}")),
    }
}

/// Whether launchd has `label` loaded; `Err` when `launchctl` cannot tell.
#[cfg(target_os = "macos")]
fn launchd_loaded(label: &str) -> Result<bool, String> {
    let target = format!("gui/{}/{label}", trusty_common::launchd::current_uid());
    let out = std::process::Command::new("launchctl")
        .args(["print", &target])
        .output()
        .map_err(|e| format!("cannot run `launchctl print {target}`: {e}"))?;
    loaded_from(out.status.code(), &String::from_utf8_lossy(&out.stderr))
}

/// No launchd off macOS, so no dashboard agent is loaded.
#[cfg(not(target_os = "macos"))]
fn launchd_loaded(_label: &str) -> Result<bool, String> {
    Ok(false)
}

/// `launchctl print`'s exit read as loaded or not.
///
/// Why: #11368 — a read error is not "not loaded" (fail closed).
/// What: 0 loaded; 113 (launchctl's "could not find service") not loaded;
/// any other exit, or none, `Err`.
/// Test: `a_launchctl_read_error_is_not_not_loaded_11368`.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn loaded_from(code: Option<i32>, stderr: &str) -> Result<bool, String> {
    match code {
        Some(0) => Ok(true),
        Some(113) => Ok(false),
        Some(c) => Err(format!("launchctl print exited {c}: {}", stderr.trim())),
        None => Err("launchctl print was killed by a signal".to_owned()),
    }
}

/// Everything [`run`] needs besides the directory, the names and the mode.
pub(crate) struct Ctx<'a> {
    /// The bundle to deploy.
    pub(crate) seed: &'a ArchitectSeed,
    /// Whether live bytes are an earlier shipped copy ([`refresh::shipped_before`]).
    pub(crate) shipped_before: ShippedBefore<'a>,
    /// The processes a deploy may restart.
    pub(crate) host: &'a dyn Restarter,
    /// The release the kept copy is filed under.
    pub(crate) version: &'a str,
}

/// Production [`DeployFn`]: this binary's seed, history, version and processes.
///
/// Test: `an_interrupted_deploy_through_up_launches_nothing_11368`.
pub(crate) fn live(dir: &Path, names: &SessionNames, mode: Mode) -> anyhow::Result<Vec<Step>> {
    let seed = ArchitectSeed::resolve_for(dir, Fetch::OnFirstUse)
        .context("the bundle deploy writes nothing without its seed files")?;
    let ctx = Ctx {
        seed: &seed,
        shipped_before: &refresh::shipped_before,
        host: &LiveRestarter,
        version: env!("CARGO_PKG_VERSION"),
    };
    run(dir, names, mode, &ctx)
}

/// Deploy the bundle into `dir`; see the module doc.
///
/// Test: `an_interrupted_deploy_leaves_no_half_written_script_11368`,
/// `a_failed_backup_aborts_and_writes_nothing_11368`,
/// `an_unchanged_deploy_does_not_restart_the_poller_11368`,
/// `a_changed_poller_script_restarts_it_exactly_once_11368`,
/// `a_stopped_poller_is_not_started_by_a_restart_11368`,
/// `dry_run_writes_nothing_11368`.
pub(crate) fn run(
    dir: &Path,
    names: &SessionNames,
    mode: Mode,
    ctx: &Ctx<'_>,
) -> anyhow::Result<Vec<Step>> {
    let planned = plan(dir, ctx)?;
    if mode.dry_run {
        return Ok(render_plan(&planned, names, ctx.version));
    }
    let before = hashes(dir);
    let kept: Vec<Kept> = planned.into_iter().filter_map(|p| p.kept).collect();
    // #11368 Q8: a failed backup returns before the first write.
    let mut steps = launch_backup::keep(dir, ctx.version, &kept)?;
    let deployed = seed::deploy_with(dir, ctx.seed, ctx.shipped_before)?;
    steps.extend(summarize(deployed));
    let after = hashes(dir);
    steps.extend(restarts(dir, names, mode, &before, &after, ctx.host));
    Ok(steps)
}

/// One file's planned write.
struct Planned {
    /// Destination below the Architect directory.
    dest: String,
    /// `None` when absent; else what `seed::deploy` does with it.
    disposition: Option<Disposition>,
    /// The bytes to keep, for a [`Disposition::Replace`].
    kept: Option<Kept>,
}

/// Read-only: what `seed::deploy_with` will do with each file.
fn plan(dir: &Path, ctx: &Ctx<'_>) -> anyhow::Result<Vec<Planned>> {
    let records_era = dir.join(status::STALE_RECORDS_DIR).is_dir();
    let mut out = Vec::new();
    for file in seed::files(ctx.seed) {
        if records_era && file.dest.starts_with("local/") {
            continue;
        }
        let path = dir.join(&file.dest);
        let shipped = file.contents.as_bytes();
        let live = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                out.push(Planned {
                    dest: file.dest,
                    disposition: None,
                    kept: None,
                });
                continue;
            }
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        let disposition = of_live(&file.dest, shipped, &live, ctx.shipped_before);
        let kept = if disposition == Disposition::Replace {
            let meta = std::fs::metadata(&path)
                .with_context(|| format!("cannot read {}", path.display()))?;
            Some(Kept {
                dest: file.dest.clone(),
                bytes: live,
                mode: launch_backup::mode_of(&meta),
                new_sha: refresh::sha256_hex(shipped),
            })
        } else {
            None
        };
        out.push(Planned {
            dest: file.dest,
            disposition: Some(disposition),
            kept,
        });
    }
    Ok(out)
}

/// The `--dry-run` lines: one per file the deploy would change, then a count.
fn render_plan(planned: &[Planned], names: &SessionNames, version: &str) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut same = 0;
    let mut touched: Vec<&str> = Vec::new();
    for p in planned {
        let line = match p.disposition {
            None => format!("would write {}", p.dest),
            Some(Disposition::Replace) => format!(
                "would replace {} and keep its bytes under {}/{version}/",
                p.dest,
                launch_backup::ROOT
            ),
            Some(Disposition::Edited) => format!(
                "would keep the edited {0} and write {0}{1}",
                p.dest,
                seed::SHIPPED_SUFFIX
            ),
            Some(Disposition::Same | Disposition::InstanceKept) => {
                same += 1;
                continue;
            }
        };
        if matches!(p.disposition, None | Some(Disposition::Replace)) {
            touched.push(&p.dest);
        }
        steps.push(Step::Skipped(format!("dry run: {line}")));
    }
    steps.push(Step::Skipped(format!("dry run: {same} file(s) unchanged")));
    if touched.iter().any(|d| POLLER_SCRIPTS.contains(d)) {
        steps.push(Step::Skipped(format!(
            "dry run: would restart poller session {} once if it runs",
            names.poll()
        )));
    }
    if touched.iter().any(|d| DASHBOARD_SCRIPTS.contains(d)) {
        steps.push(Step::Skipped(
            "dry run: would kickstart the dashboard agent once if it is loaded".to_owned(),
        ));
    }
    steps
}

/// sha256 of each [`POLLER_SCRIPTS`] and [`DASHBOARD_SCRIPTS`] file; `None` absent.
fn hashes(dir: &Path) -> BTreeMap<&'static str, Option<String>> {
    POLLER_SCRIPTS
        .iter()
        .chain(DASHBOARD_SCRIPTS)
        .map(|d| {
            (
                *d,
                std::fs::read(dir.join(d))
                    .ok()
                    .map(|b| refresh::sha256_hex(&b)),
            )
        })
        .collect()
}

/// The deploy's lines, each file it left as is counted on one line.
fn summarize(deployed: Vec<Step>) -> Vec<Step> {
    let mut same = 0;
    let mut out: Vec<Step> = deployed
        .into_iter()
        .filter(|s| {
            let keep = !matches!(s, Step::Unchanged(_));
            same += usize::from(!keep);
            keep
        })
        .collect();
    out.push(Step::Unchanged(format!(
        "bundle deploy: {same} file(s) unchanged"
    )));
    out
}

/// The restart steps for the scripts that changed between `before` and `after`.
///
/// Why: Q4 — once, only on a hashed change, never starting a stopped process.
/// Test: `a_changed_poller_script_restarts_it_exactly_once_11368`,
/// `a_changed_dashboard_script_kickstarts_a_loaded_agent_once_11368`,
/// `inside_claude_code_the_deploy_restarts_nothing_11368`,
/// `a_scratch_home_skips_the_dashboard_restart_and_says_so_11961`,
/// `the_login_home_still_kickstarts_the_dashboard_11961`.
fn restarts(
    dir: &Path,
    names: &SessionNames,
    mode: Mode,
    before: &BTreeMap<&'static str, Option<String>>,
    after: &BTreeMap<&'static str, Option<String>>,
    host: &dyn Restarter,
) -> Vec<Step> {
    let changed = |table: &[&'static str]| -> Vec<&'static str> {
        table
            .iter()
            .copied()
            .filter(|d| before.get(d) != after.get(d))
            .collect()
    };
    let (poll, dash) = (changed(POLLER_SCRIPTS), changed(DASHBOARD_SCRIPTS));
    let mut steps = Vec::new();
    if !mode.restart {
        if !poll.is_empty() || !dash.is_empty() {
            steps.push(Step::Skipped(
                "inside Claude Code the deploy restarts nothing; the poller and dashboard keep \
                 the code they loaded until `tm architect restart`"
                    .to_owned(),
            ));
        }
        return steps;
    }
    let session = names.poll();
    if !poll.is_empty() {
        steps.push(match host.poller_running(dir, names) {
            Ok(true) => match host.restart_poller(dir, names) {
                Ok(()) => Step::Changed(format!(
                    "restarted poller session {session} once: changed {}",
                    poll.join(", ")
                )),
                Err(e) => Step::Failed(format!("poller restart: {e:#}")),
            },
            Ok(false) => Step::Unchanged(format!(
                "poller session {session} is not running; a changed script does not start it"
            )),
            Err(e) => Step::Failed(format!("poller restart: {e}")),
        });
    }
    if !dash.is_empty() {
        // #11961: a test, sandbox or scratch HOME never reads or restarts
        // the login user's launchd job; the skip is a reported step.
        if let Some(home) = host.foreign_home() {
            steps.push(Step::Skipped(format!(
                "dashboard restart: skipped, because {home} is not this user's home folder; \
                 the dashboard agent belongs to the real login"
            )));
            return steps;
        }
        steps.push(match host.dashboard_loaded() {
            Ok(false) => Step::Unchanged(
                "the dashboard agent is not loaded; a changed script does not start it".to_owned(),
            ),
            Ok(true) => match host.kickstart_dashboard() {
                Ok(()) => Step::Changed(format!(
                    "kickstarted the dashboard agent once: changed {}",
                    dash.join(", ")
                )),
                Err(e) => Step::Failed(format!("dashboard restart: {e:#}")),
            },
            // #11368: an unreadable launchd state fails, as the poller arm does.
            Err(e) => Step::Failed(format!("dashboard restart: cannot read the agent: {e}")),
        });
    }
    steps
}

/// The text of each `Failed` step, joined by `; `; `None` when none failed.
///
/// Why: #11961 — the verdict after a failed deploy names what failed, not
/// only a fixed line. Test: `a_failed_deploy_names_the_failed_step_11961`.
pub(crate) fn failed_text(steps: &[Step]) -> Option<String> {
    let failed: Vec<&str> = steps
        .iter()
        .filter_map(|s| match s {
            Step::Failed(t) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    (!failed.is_empty()).then(|| failed.join("; "))
}

// The fixtures are shared with `up_tests.rs`.
#[cfg(test)]
#[path = "launch_deploy_tests.rs"]
pub(crate) mod tests;
