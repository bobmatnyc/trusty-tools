//! `tm fleet init|status` — set up and inspect the Architect (#8436, P2 and P4).
//!
//! Why: the Architect is the one fleet supervisor per user. Its session runs
//! the supervisor profile only when three things agree (#8453): the user-level
//! `[supervisor] projects` allowlist, the project's `profile = "supervisor"`,
//! and the launch stamp. Setting that up by hand meant editing the operator's
//! config file; `tm fleet init` does it in one idempotent command.
//! What: [`init`] creates the project (local git repo, no remote), writes the
//! profile request and the allowlist entry — each an atomic write that keeps
//! the rest of the file — seeds the fleet files ([`seed`]), and starts the
//! `tm-architect` session and its poller ([`poller`]). [`status()`] reads the
//! four session facts back and exits 1 when any is missing. Twin mode (#8878)
//! is out of scope (ruling Q7). `add` and `remove` are phase P3.
//! Test: `commands::fleet::tests`, `commands::fleet::preflight::tests`,
//! `tests/tm_fleet.rs`.

mod config;
mod launch;
mod poller;
mod preflight;
mod seed;
mod status;

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use trusty_mpm::core::project_config::PROJECT_CONFIG_FILE;

use self::config::Edit;
use crate::cli::FleetAction;

pub(crate) use self::launch::ARCHITECT_SESSION;
pub(crate) use self::status::status;

/// The Architect's directory under the user home (ruling Q4).
pub(crate) const DEFAULT_DIR: &str = "trusty-mpm-projects/architect";

/// Dispatch `tm fleet <action>` against the process home.
///
/// Why: the CLI entry point; every function below takes `home` so a test
/// never touches the operator's `~/.trusty-mpm`.
/// What: resolves the home and the directory, runs [`init`] or [`status()`],
/// and prints the report. `init` returns an error, so exit 1, when a step
/// failed; `status` does when the setup is incomplete.
/// Test: `tests/tm_fleet.rs`.
pub(crate) async fn run(action: FleetAction) -> anyhow::Result<()> {
    let home = dirs::home_dir().context("cannot resolve the home directory")?;
    match action {
        FleetAction::Init { dir, no_launch } => {
            let dir = resolve_dir(dir.as_deref(), &home)?;
            let report = init(&dir, &home, !no_launch, TMUX)?;
            print!("{}", report.render());
            if report.failed() {
                bail!("tm fleet init failed; see the FAILED step above");
            }
            Ok(())
        }
        FleetAction::Status { dir, json } => {
            let dir = resolve_dir(dir.as_deref(), &home)?;
            let report = status(&dir, &home, TMUX);
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", report.render());
            }
            if !report.complete {
                bail!("the Architect is not fully set up in {}", dir.display());
            }
            Ok(())
        }
    }
}

/// The Architect directory: `--dir` made absolute, else [`DEFAULT_DIR`] under `home`.
///
/// Test: `dir_overrides_the_default`.
pub(crate) fn resolve_dir(dir: Option<&str>, home: &Path) -> anyhow::Result<PathBuf> {
    match dir {
        Some(dir) => std::path::absolute(dir).with_context(|| format!("invalid --dir {dir:?}")),
        None => Ok(home.join(DEFAULT_DIR)),
    }
}

/// How `init` and `status` read tmux (#8436 P4 fix).
///
/// Why: the unit tests must never reach a real tmux server, where a running
/// `tm-architect` made every fixture fail with "already runs".
/// What: production passes [`TMUX`]; every unit test passes a stub.
/// Test: `a_live_architect_on_the_test_server_does_not_reach_a_fixture`.
#[derive(Clone, Copy)]
pub(crate) struct Probe {
    /// Session `name` and its first pane; see [`launch::pane_state`].
    pub(crate) pane: fn(&str) -> launch::PaneState,
    /// The launch stamp on `tm-architect`; see [`launch::launch_stamp`].
    pub(crate) stamp: fn() -> Option<String>,
}

/// The real tmux server.
pub(crate) const TMUX: Probe = Probe {
    pane: launch::pane_state,
    stamp: launch::launch_stamp,
};

/// What one `init` step did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// The step wrote something.
    Changed(String),
    /// The step found its work already done.
    Unchanged(String),
    /// The step did not run.
    Skipped(String),
    /// The step ran and failed; `init` reports it and the command exits 1.
    Failed(String),
}

/// The steps one `init` run took, in order.
#[derive(Debug, Default)]
pub(crate) struct InitReport {
    /// One entry per step.
    pub(crate) steps: Vec<Step>,
    /// This run started the Architect but could not bind it to its `claude`
    /// (#8878 ruling A); a warning, never a failure.
    pub(crate) unbound: bool,
}

impl InitReport {
    /// Whether any step wrote anything.
    pub(crate) fn changed(&self) -> bool {
        self.steps.iter().any(|s| matches!(s, Step::Changed(_)))
    }

    /// Whether any step failed.
    pub(crate) fn failed(&self) -> bool {
        self.steps.iter().any(|s| matches!(s, Step::Failed(_)))
    }

    /// One line per step, then a summary line.
    pub(crate) fn render(&self) -> String {
        let mut out = String::new();
        for step in &self.steps {
            let (mark, text) = match step {
                Step::Changed(t) => ("done     ", t),
                Step::Unchanged(t) => ("unchanged", t),
                Step::Skipped(t) => ("skipped  ", t),
                Step::Failed(t) => ("FAILED   ", t),
            };
            out.push_str(&format!("  {mark}  {text}\n"));
        }
        out.push_str(if self.failed() {
            "The Architect is not fully set up: fix the FAILED step and run `tm fleet init` again.\n"
        } else if self.unbound {
            // #8436 P4 fix: an unbound Architect is set up, but not fully.
            "Architect set up (NOT bound: anchor writes will be denied; see the warning above). \
             Check it with `tm fleet status`.\n"
        } else if self.changed() {
            "Architect set up. Check it with `tm fleet status`.\n"
        } else {
            "Nothing changed: the Architect is already set up.\n"
        });
        out
    }
}

/// Set up the Architect project in `dir`; see the module doc.
///
/// Why: acceptance 1-3 of #8436, under the P2 brief: idempotent, atomic, and
/// fail-closed on a file it cannot parse.
/// What: first refuses a directory fleet init must never own (see
/// [`preflight::check`]) and continues with its canonical path. Reads and
/// parses both config files BEFORE writing anything, then refuses when
/// another Architect exists (an allow-listed supervisor project
/// elsewhere, or `tm-architect` running in another directory). Then, in
/// order: create `dir`; `git init` when `dir/.git` is absent (no remote is
/// ever added); request the profile; add the allowlist entry; write the
/// fleet files ([`seed::deploy`], P4); start the session when `launch` and it
/// is not already running; start the poller ([`poller::step`], P4). A poller
/// that cannot start is a [`Step::Failed`], never a warning.
/// Test: `a_second_run_changes_nothing`,
/// `a_malformed_config_fails_and_is_left_byte_identical`,
/// `an_unrelated_key_and_comment_survive_the_allowlist_write`,
/// `a_second_architect_elsewhere_is_refused`,
/// `the_home_directory_is_refused_however_it_is_spelled`,
/// `a_first_run_seeds_the_architect_project`,
/// `fleet_init_fails_closed_when_the_poller_does_not_start`.
pub(crate) fn init(
    dir: &Path,
    home: &Path,
    launch: bool,
    probe: Probe,
) -> anyhow::Result<InitReport> {
    // #8436: the preflight runs before any read or write, and every later step
    // uses the path it checked (code-critic BLOCK: `--dir $HOME` was accepted).
    let checked = preflight::check(dir, home)?;
    let dir = checked.as_path();
    let config_path = user_config_path(home);
    let config_raw = read_or_empty(&config_path)?;
    let (_, user_config) = config::parse_user_config(&config_raw, &config_path)?;
    let project_path = dir.join(PROJECT_CONFIG_FILE);
    let project_raw = read_or_empty(&project_path)?;
    config::request_supervisor_profile(&project_raw, &project_path)?;
    refuse_second_architect(&user_config, dir, probe)?;

    let mut report = InitReport::default();
    report.steps.push(if dir.is_dir() {
        Step::Unchanged(format!("project directory {}", dir.display()))
    } else {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        Step::Changed(format!("created project directory {}", dir.display()))
    });
    report.steps.push(git_init(dir)?);

    let edit = config::request_supervisor_profile(&project_raw, &project_path)?;
    report.steps.push(write_edit(
        &project_path,
        edit,
        "`profile = \"supervisor\"` in",
    )?);
    // #3981: the allowlist is the operator's grant, kept outside every
    // project's write boundary. #8436 ruling Q6 departs from that: `tm fleet
    // init` always writes it, also when a PM session runs the command.
    let edit = config::add_allowlist_entry(&config_raw, dir, &config_path)?;
    let label = format!("`[supervisor] projects` entry {} in", dir.display());
    report.steps.push(write_edit(&config_path, edit, &label)?);

    // #8436 P4: the seeded CLAUDE.md must exist before the launch, whose
    // instruction pipeline creates a stub CLAUDE.md when none is there.
    report.steps.extend(seed::deploy(dir)?);
    let (step, unbound) = launch_step(dir, home, launch, probe)?;
    report.steps.push(step);
    report.unbound = unbound;
    report.steps.push(poller::step(dir, launch, probe));
    Ok(report)
}

/// `~/.trusty-mpm/config.toml` under `home`.
pub(crate) fn user_config_path(home: &Path) -> PathBuf {
    home.join(".trusty-mpm").join("config.toml")
}

/// A file's text, or `""` when it does not exist.
fn read_or_empty(path: &Path) -> anyhow::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(raw) => Ok(raw),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Refuse when an Architect other than `dir` already exists.
fn refuse_second_architect(
    user_config: &trusty_mpm::core::config::MpmConfig,
    dir: &Path,
    probe: Probe,
) -> anyhow::Result<()> {
    if let Some(other) = config::other_architects(user_config, dir).first() {
        bail!(
            "an Architect is already set up at {}; there is one per user. Run `tm fleet status \
             --dir {}`, or remove that entry from `[supervisor] projects` first",
            other.display(),
            other.display()
        );
    }
    let wanted = std::fs::canonicalize(dir).ok();
    match (probe.pane)(ARCHITECT_SESSION).dir() {
        Some(running) if Some(&running) != wanted.as_ref() => bail!(
            "tmux session {ARCHITECT_SESSION} already runs in {}; there is one Architect per user",
            running.display()
        ),
        _ => Ok(()),
    }
}

/// `git init` in `dir` unless it already holds a repository. Adds no remote.
fn git_init(dir: &Path) -> anyhow::Result<Step> {
    if dir.join(".git").exists() {
        return Ok(Step::Unchanged(
            "git repository (no remote added)".to_owned(),
        ));
    }
    let out = preflight::git()
        .args(["init", "--quiet"])
        .arg(dir)
        .output()
        .context("failed to run `git init`")?;
    if !out.status.success() {
        bail!(
            "`git init` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(Step::Changed(
        "initialised a local git repository with no remote".to_owned(),
    ))
}

/// Write `edit` to `path` atomically, or report it unchanged.
fn write_edit(path: &Path, edit: Edit, label: &str) -> anyhow::Result<Step> {
    let what = format!("{label} {}", path.display());
    match edit {
        Edit::Unchanged => Ok(Step::Unchanged(what)),
        Edit::Changed(text) => {
            trusty_common::atomic_file::write_atomic(path, text.as_bytes())
                .with_context(|| format!("cannot write {}", path.display()))?;
            Ok(Step::Changed(format!("wrote {what}")))
        }
    }
}

/// Start the session unless told not to or already running; the flag is
/// whether a started Architect is unbound (see [`InitReport::unbound`]).
fn launch_step(
    dir: &Path,
    home: &Path,
    launch: bool,
    probe: Probe,
) -> anyhow::Result<(Step, bool)> {
    if (probe.pane)(ARCHITECT_SESSION).dir().is_some() {
        return Ok((
            Step::Unchanged(format!("tmux session {ARCHITECT_SESSION} is running")),
            false,
        ));
    }
    if !launch {
        return Ok((
            Step::Skipped(format!(
                "session start (--no-launch); start it with `tm fleet init --dir {}`",
                dir.display()
            )),
            false,
        ));
    }
    // #8878 ruling A: the record binds the Architect identity to this claude.
    let (bound, unbound) = match launch::start(dir, home)? {
        Ok(record) => (format!("bound to claude pid {}", record.pid), false),
        Err(err) => {
            eprintln!("warning: the Architect process was NOT recorded: {err}");
            let text = format!(
                "NOT bound to its claude ({err}), so it cannot write the trust anchors; stop \
                 the session and re-run `tm fleet init` to bind it"
            );
            (text, true)
        }
    };
    let step = Step::Changed(format!(
        "started tmux session {ARCHITECT_SESSION} on the `opus` alias, {bound}; attach with \
         `tmux attach -t ={ARCHITECT_SESSION}`"
    ));
    Ok((step, unbound))
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
