//! The ledger shim: `scripts/fleet-poll-ledger.py`, run every [`SHIM_EVERY`].
//!
//! Why: ten prototype steps read and write the Intent Ledger and the
//! dashboard store (il_cron, share_request, reminder, ctx_watch,
//! morning_brief, dream, stale_tasks, factory_overdue, run_dashboard_updates,
//! exited_step). They stay Python until the ledger port (#11540, Architect
//! ruling 2026-11-20 01:41Z); the poller runs them and must surface a failure
//! as an alert, never only a log line.
//! What: [`run_ledger_shim`] runs the script through the [`Subprocess`](super::scripts::Subprocess) seam,
//! bounded by [`SHIM_TIMEOUT`] (killed on expiry), with `--state` (the run's
//! own `poll-state.json`, read only), `--now`, and `--shadow <inbox>/shadow`
//! in a shadow run. Its stdout object gives the step alerts and the changed
//! state keys ([`SHIM_STATE_KEYS`] only). A missing script, a child that
//! cannot start, a timeout, a non-zero exit or unreadable output each yield
//! one `ledger_shim` alert in [`ShimRun::failing`], fingerprinted by the
//! failure kind, so dedup and `push_due` see it.
//! [`ledger_shim_step`] is the cycle's call: it runs the shim at most once per
//! [`SHIM_EVERY`] on [`ScriptSteps::due`]'s clock (a `serve` cycle can come
//! every 15 s, #9779 review) and, on a skipped cycle, returns the last run's
//! failures so an open failure stays in the `failing` set.
//! Test: `ledger_shim_runs_once_per_cadence_across_fast_cycles`,
//! `ledger_shim_success_raises_no_alert`,
//! `ledger_shim_each_failure_kind_raises_an_alert`,
//! `ledger_shim_shadow_passes_the_shadow_flag`,
//! `ledger_shim_failure_reaches_the_cycle_failing_set`.

use std::io;
use std::time::Duration;

use serde_json::{Map, Value};

use super::scripts::{ChildOutput, ScriptSteps};
use crate::classify::{Alert, alert};
use crate::poller::Config;

/// The shim script under `<root>/scripts`.
pub const SHIM_SCRIPT: &str = "fleet-poll-ledger.py";
/// How long one shim run may take before it is killed.
pub const SHIM_TIMEOUT: Duration = Duration::from_secs(240);
/// The alert kind of a shim failure.
pub const SHIM_ALERT: &str = "ledger_shim";
/// The shim's cadence, in seconds: the prototype's per-cycle run at 60 s.
pub const SHIM_EVERY: f64 = 60.0;
/// The shim's name on the [`ScriptSteps::due`] clock.
const SHIM_STEP: &str = "ledger shim";
/// The `poll-state.json` keys the shim's steps own.
pub const SHIM_STATE_KEYS: [&str; 3] = ["morning_brief", "dream", "exited"];

/// One shim run's result.
#[derive(Debug, Default)]
pub struct ShimRun {
    /// The steps' own alerts.
    pub alerts: Vec<Alert>,
    /// One `ledger_shim` alert per failure.
    pub failing: Vec<Alert>,
    /// Changed state keys, limited to [`SHIM_STATE_KEYS`].
    pub state: Map<String, Value>,
    /// `poll.log` lines.
    pub log: Vec<String>,
}

/// The shim's argv for this config at `now`.
pub fn shim_argv(steps: &ScriptSteps<'_>, cfg: &Config, now: f64) -> Vec<String> {
    let path = |p: std::path::PathBuf| p.to_string_lossy().into_owned();
    let mut v = vec![
        steps.python.clone(),
        path(steps.scripts.join(SHIM_SCRIPT)),
        "--state".to_owned(),
        path(cfg.state()),
        "--now".to_owned(),
        now.to_string(),
    ];
    if cfg.shadow {
        v.push("--shadow".to_owned());
        v.push(path(cfg.shadow_dir()));
    }
    v
}

/// The cycle's shim step at `now`: a run when due, else the last failures.
pub fn ledger_shim_step(steps: &ScriptSteps<'_>, cfg: &Config, now: f64) -> ShimRun {
    if !steps.due(SHIM_STEP, SHIM_EVERY, now) {
        return ShimRun {
            failing: steps.shim_failing(),
            ..ShimRun::default()
        };
    }
    let run = run_ledger_shim(steps, cfg, now);
    steps.set_shim_failing(&run.failing);
    run
}

/// Runs the shim once; see the module docs.
pub fn run_ledger_shim(steps: &ScriptSteps<'_>, cfg: &Config, now: f64) -> ShimRun {
    let mut run = ShimRun::default();
    let sup = cfg.supervisor.clone();
    let fail = |run: &mut ShimRun, kind: &str, detail: String| {
        run.log.push(format!("ledger shim ERROR {detail}"));
        let detail = format!("{SHIM_SCRIPT} {detail}");
        run.failing
            .push(alert(SHIM_ALERT, kind, &detail, &sup, "", now));
    };
    let script = steps.scripts.join(SHIM_SCRIPT);
    if !script.is_file() {
        fail(
            &mut run,
            "missing",
            format!("missing: {}", script.display()),
        );
        return run;
    }
    let argv = shim_argv(steps, cfg, now);
    let out = match steps.runner.output(&argv, SHIM_TIMEOUT) {
        Ok(out) => out,
        Err(e) if e.kind() == io::ErrorKind::TimedOut => {
            fail(&mut run, "timeout", format!("killed: {e}"));
            return run;
        }
        Err(e) => {
            fail(&mut run, "start", format!("cannot start: {e}"));
            return run;
        }
    };
    if let Err(why) = read_output(&out, &mut run) {
        fail(&mut run, "output", format!("unreadable output: {why}"));
    }
    if out.code != Some(0) {
        let code = out
            .code
            .map_or_else(|| "signal".to_owned(), |c| c.to_string());
        let lines: Vec<&str> = out.stderr.lines().filter(|l| !l.is_empty()).collect();
        fail(
            &mut run,
            "exit",
            format!("exit {code}: {}", lines.join("; ")),
        );
    }
    run
}

/// Fills `run.alerts` and `run.state` from the shim's stdout object.
fn read_output(out: &ChildOutput, run: &mut ShimRun) -> Result<(), String> {
    let line = out
        .stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or("no output")?;
    let v: Value = serde_json::from_str(line).map_err(|e| e.to_string())?;
    let rows = v
        .get("alerts")
        .and_then(Value::as_array)
        .ok_or("no alerts list")?;
    for row in rows {
        run.alerts
            .push(to_alert(row).ok_or("an alert row lacks a field")?);
    }
    if let Some(state) = v.get("state").and_then(Value::as_object) {
        for (k, val) in state {
            if SHIM_STATE_KEYS.contains(&k.as_str()) {
                run.state.insert(k.clone(), val.clone());
            }
        }
    }
    Ok(())
}

/// One `fleet-poll.alert` dict as an [`Alert`], its display kept.
fn to_alert(row: &Value) -> Option<Alert> {
    let s = |k: &str| row.get(k).and_then(Value::as_str).map(str::to_owned);
    Some(Alert {
        ts: s("ts")?,
        session: s("session")?,
        pane: s("pane")?,
        kind: s("kind")?,
        fingerprint: s("fingerprint")?,
        detail: s("detail")?,
        display: s("display")?,
    })
}
