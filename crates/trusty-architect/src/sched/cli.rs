//! The scheduler verbs, as a function from arguments to output and exit code.
//!
//! Why: the binary stays a thin shell, and every verb is testable with an
//! injected clock, runner and cutover operations against a temp directory.
//! What: [`execute`] parses [`Args`] and runs one verb. `validate` loads the
//! config and prints the job counts; `status` prints liveness and job rows
//! (exit 1 when stale); `run --once` ticks once and waits for the runs it
//! started; `daemon` takes the lock, then loops until SIGTERM; `cutover` and
//! `rollback` move the wake duty ([`super::cutover`]); `parity` prints the
//! shadow-against-poller counts; `plist` prints the launchd plist and writes
//! nothing. The config is `--config`, else `local/meta/registry.json` under
//! `--root`, else `ARCHITECT_PROJECT_DIR`, else the current directory. An
//! empty, unreadable or invalid config is an error and a non-zero exit.
//! Test: `tests/sched_cli.rs` (one test per verb), `tests/sched_daemon.rs`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use super::cli_args::{Args, parse};
use super::config::{Kind, Registry};
use super::cutover::{self, Ops, SystemOps, Thresholds};
use super::daemon::{self, Instance, Reload};
use super::engine::Engine;
use super::plist::PlistSpec;
use super::slot::day_key;
use super::spawn::JobRunner;
use super::state::SchedState;
use super::{Layout, SchedError, parity, status};
use crate::clock::Clock;

/// Everything outside the filesystem a verb touches, injectable.
pub struct Deps<'a> {
    /// Time and sleeping.
    pub clock: &'a dyn Clock,
    /// Starts children.
    pub runner: Arc<dyn JobRunner>,
    /// The cutover side effects; `None` builds [`SystemOps`].
    pub ops: Option<&'a dyn Ops>,
    /// The process environment.
    pub env: Vec<(String, String)>,
    /// The user's home directory.
    pub home: PathBuf,
    /// The current directory.
    pub cwd: PathBuf,
    /// Where agent runs get their scratch directories (outside the root).
    pub scratch: PathBuf,
}

/// A verb's stdout and exit code.
#[derive(Debug)]
pub struct Outcome {
    /// What to print.
    pub stdout: String,
    /// The process exit code.
    pub code: i32,
}

fn ok(stdout: String) -> Result<Outcome, SchedError> {
    Ok(Outcome { stdout, code: 0 })
}

/// Counts of jobs by who runs them: (scheduler-run, meta_due, external, disabled).
fn tally(reg: &Registry) -> (usize, usize, usize, usize) {
    let mut t = (0, 0, 0, 0);
    for j in &reg.jobs {
        match (j.enabled, j.run_kind(), j.managed_by.as_deref()) {
            (false, ..) => t.3 += 1,
            (_, Kind::NotRun, Some("meta_due")) => t.1 += 1,
            (_, Kind::NotRun, _) => t.2 += 1,
            _ => t.0 += 1,
        }
    }
    t
}

/// Runs the verb in `argv` (without the program name).
///
/// # Errors
/// [`SchedError`]; its [`SchedError::exit_code`] is the exit code.
pub fn execute(argv: &[String], d: &Deps<'_>) -> Result<Outcome, SchedError> {
    let args = parse(argv)?;
    let get = |n: &str| d.env.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone());
    let root = crate::verbs::project_root(
        &|n| {
            args.root
                .as_ref()
                .filter(|_| n == "ARCHITECT_PROJECT_DIR")
                .map(|p| p.display().to_string())
                .or_else(|| get(n))
        },
        &d.cwd,
    );
    let layout = Layout::new(&root, &d.home);
    let config = args.config.clone().unwrap_or_else(|| layout.registry());
    let now = d.clock.now_unix().floor() as i64;
    match args.verb.as_str() {
        "validate" => {
            let (reg, _) = daemon::load_initial(&config)?;
            let (run, due, ext, off) = tally(&reg);
            ok(format!(
                "ok: {} jobs ({run} scheduler-run, {due} meta_due, {ext} external, {off} disabled)\n",
                reg.jobs.len()
            ))
        }
        "status" => {
            let (reg, _) = daemon::load_initial(&config)?;
            let st = SchedState::load(&layout.state())?;
            let (stdout, healthy) = status::render(&layout, &reg, &st, now, args.json);
            Ok(Outcome {
                stdout,
                code: i32::from(!healthy),
            })
        }
        "run" => run_once(&layout, &config, d),
        "daemon" => run_daemon(&layout, &config, d),
        "cutover" | "rollback" => {
            let ops_owned;
            let ops: &dyn Ops = match d.ops {
                Some(o) => o,
                None => {
                    ops_owned = system_ops(&root, &get);
                    &ops_owned
                }
            };
            let msg = if args.verb == "cutover" {
                cutover::cutover(&layout, ops, &thresholds(&args), now)?
            } else {
                cutover::rollback(&layout, ops, now)?
            };
            ok(format!("{msg}\n"))
        }
        "parity" => {
            let rep = parity::load(&layout, now - args.since_s.unwrap_or(7200))?;
            ok(parity::render(&rep))
        }
        _ => {
            let exe = args
                .exe
                .clone()
                .unwrap_or_else(|| d.home.join(".cargo/bin/trusty-architect-sched"));
            let spec = PlistSpec {
                exe,
                root: root.clone(),
                log_dir: root.join("local/logs/sched"),
                path_env: PlistSpec::default_path(&d.home),
            };
            ok(spec.render())
        }
    }
}

fn system_ops(root: &Path, get: &dyn Fn(&str) -> Option<String>) -> SystemOps {
    let supervisor = get("ARCHITECT_SESSION")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "tm-architect".to_owned());
    SystemOps {
        root: root.to_owned(),
        socket: get("TMUX_SOCKET").filter(|s| !s.is_empty()),
        session: get("ARCHITECT_POLL_SESSION")
            .filter(|s| !s.is_empty())
            .unwrap_or(format!("{supervisor}-poll")),
    }
}

fn thresholds(a: &Args) -> Thresholds {
    let base = Thresholds::default();
    Thresholds {
        since_s: a.since_s.unwrap_or(base.since_s),
        min_cycles: a.min_cycles.unwrap_or(base.min_cycles),
        min_match_pct: a.min_match_pct.unwrap_or(base.min_match_pct),
        force: a.force,
    }
}

fn engine_for<'a>(
    layout: &Layout,
    config: &Path,
    d: &'a Deps<'_>,
) -> Result<(Engine<'a>, String), SchedError> {
    let (reg, sha) = daemon::load_initial(config)?;
    let state = SchedState::load(&layout.state())?;
    let engine = Engine::new(
        layout.clone(),
        d.clock,
        Arc::clone(&d.runner),
        d.env.clone(),
        reg,
        state,
        d.scratch.clone(),
    );
    Ok((engine, sha))
}

fn run_once(layout: &Layout, config: &Path, d: &Deps<'_>) -> Result<Outcome, SchedError> {
    let (mut engine, _) = engine_for(layout, config, d)?;
    let mut rep = engine.tick()?;
    let longest = engine
        .registry()
        .jobs
        .iter()
        .map(|j| j.timeout())
        .max()
        .unwrap_or(0);
    let waited = engine.wait_idle(Duration::from_secs(longest + 10));
    rep.records.extend(waited.records);
    let mut out = format!(
        "{} run(s) started, {} record(s) written ({})\n",
        rep.started.len(),
        rep.records.len(),
        day_key(engine_now(d))
    );
    for r in &rep.records {
        out += &format!("  {} slot {} {}\n", r.job, r.slot, r.outcome);
    }
    ok(out)
}

fn engine_now(d: &Deps<'_>) -> i64 {
    d.clock.now_unix().floor() as i64
}

fn run_daemon(layout: &Layout, config: &Path, d: &Deps<'_>) -> Result<Outcome, SchedError> {
    // Config first: an empty or unreadable file must fail before the lock.
    let (mut engine, sha) = engine_for(layout, config, d)?;
    let _instance = Instance::acquire(layout)?;
    let stop = crate::serve::install_stop_handlers()
        .map_err(|e| SchedError::io("install signal handlers", e))?;
    let mut reload = Reload::new(config, sha);
    daemon::run_loop(&mut engine, &mut reload, d.clock, stop, None)?;
    ok("stopped\n".to_owned())
}
