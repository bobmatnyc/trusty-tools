//! The poller's usage step: `usage.py`'s `poll_step` and `fleet-poll.py`'s
//! `usage_alerts` wrapper (Architect 0d1ef3b; `usage.py` unchanged since
//! 76a5a12).
//!
//! Why: each cycle samples Claude account usage into the ledger and raises
//! `usage` alerts (5h exhaustion within 45 min, weekly 80/90%, cutover
//! recommended), so cutovers can be planned (#9801 slice 1).
//! What: [`poll_step`] samples the statusline cache, decides whether a
//! `/usage` probe is due (on an account change, three tries, else hourly
//! when no fresh reading exists), starts it in the first idle pane through a
//! [`ProbeLauncher`], and returns the alert tuples. [`usage_step`] wraps it
//! for the cycle: alerts become `usage` alerts and a failure is a `poll.log`
//! line, never a stopped cycle. Paths come from [`UsagePaths`], read the way
//! `usage.py` reads them (`USAGE_LEDGER`, `USAGE_STATUSLINE_CACHE`,
//! `USAGE_CLAUDE_JSON`, `CLAUDE_CONFIG_DIR`); no secret is read.
//!
//! Fail-Open Check (#9801): the usage state commits only when the step
//! reaches its probe decision, so a failure before it leaves the stored
//! state as it was. A probe counts (`last_probe`, `tries`) only after the
//! launcher started it; a failed launch is logged and the alerts are kept.
//! The prototype records the probe first and loses the alerts.
//! Test: `tests/parity_usage.rs`, `tests/usage_ported.rs`,
//! `tests/steps_fail_open.rs`, `tests/steps_cycle.rs`.

mod ledger;
mod plan;
mod utc;

use std::fs::OpenOptions;
use std::io;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};

use serde_json::{Map, Value};

pub use self::ledger::{
    CACHE_MAX_AGE, CacheData, CacheReading, LedgerRecord, Pct, Reading, WRITE_EVERY, append_ledger,
    detect_account, read_cache, read_ledger, record_reading, rows_from_values, sample_cache,
};
use self::ledger::{latest, truthy};
pub use self::plan::{
    Burn, ELIGIBLE_5H, Effective, Plan, Projection, Runway, WARN_5H_MIN, WEEK_WARN, Window,
    burn_rate, effective, plan, project, runway, usage_alerts,
};
pub use self::utc::UtcTime;
use crate::classify::{Alert, BoxReading, PaneInfo, alert};
pub use crate::error::UsageError;
use crate::input::InputState;

/// The `/usage` fallback probe runs at most once per this many seconds.
pub const PROBE_EVERY: f64 = 60.0 * 60.0;
/// Probe tries after an account change before falling back to hourly.
const PENDING_TRIES: f64 = 3.0;

/// Where the usage step reads and writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsagePaths {
    /// `USAGE_LEDGER`, else `<root>/local/usage/ledger.jsonl`.
    pub ledger: PathBuf,
    /// `USAGE_STATUSLINE_CACHE`, else `<inbox>/statusline-usage.json`.
    pub cache: PathBuf,
    /// `USAGE_CLAUDE_JSON`, else `<CLAUDE_CONFIG_DIR or
    /// ~/.trusty-tools/trusty-mpm/claude-config>/.claude.json`.
    pub claude_json: PathBuf,
    /// `<root>/scripts/usage-probe.sh`.
    pub probe_script: PathBuf,
    /// `<inbox>/usage-probe.log`: the probe's stdout and stderr.
    pub probe_log: PathBuf,
}

/// Starts a `/usage` probe in one idle pane.
pub trait ProbeLauncher {
    /// Starts the probe for `account` in `session`'s `pane` and returns once
    /// it is running; its reading lands in the ledger by itself.
    ///
    /// # Errors
    /// The probe could not be started.
    fn launch(&self, session: &str, pane: &str, account: &str) -> io::Result<()>;
}

/// The real launcher: `usage-probe.sh <session> <pane> <account>` in a new
/// session, stdin closed, output appended to `usage-probe.log`, reaped by a
/// thread when it exits (the prototype's `Popen(..., start_new_session=True)`).
#[derive(Debug, Clone)]
pub struct ScriptProbe {
    script: PathBuf,
    log: PathBuf,
}

impl ScriptProbe {
    /// The launcher for `paths`' probe script and log.
    pub fn new(paths: &UsagePaths) -> Self {
        Self {
            script: paths.probe_script.clone(),
            log: paths.probe_log.clone(),
        }
    }
}

impl ProbeLauncher for ScriptProbe {
    fn launch(&self, session: &str, pane: &str, account: &str) -> io::Result<()> {
        let log = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&self.log)?;
        let mut cmd = Command::new(&self.script);
        cmd.args([session, pane, account])
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
        // SAFETY: setsid is async-signal-safe and touches no parent memory.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn()?;
        // Reap it when it exits; a failed thread spawn leaves a zombie until
        // the poller exits, never a failed probe.
        let _ = std::thread::Builder::new()
            .name("usage-probe-reaper".to_owned())
            .spawn(move || child.wait());
        Ok(())
    }
}

/// A launcher for tests: records each launch; [`RecordingProbe::failing`]
/// also refuses each one.
#[derive(Debug, Default)]
pub struct RecordingProbe {
    launched: Mutex<Vec<[String; 3]>>,
    fail: bool,
}

impl RecordingProbe {
    /// A launcher whose every launch fails (after being recorded).
    pub fn failing() -> Self {
        Self {
            fail: true,
            ..Self::default()
        }
    }

    /// Every `[session, pane, account]` launched, oldest first.
    pub fn launched(&self) -> Vec<[String; 3]> {
        self.launched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl ProbeLauncher for RecordingProbe {
    fn launch(&self, session: &str, pane: &str, account: &str) -> io::Result<()> {
        self.launched
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push([session, pane, account].map(str::to_owned));
        if self.fail {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "usage-probe.sh: no such file (recording probe)",
            ));
        }
        Ok(())
    }
}

/// A pane a `/usage` probe may type into: an empty or suggestion box, not
/// busy, no dialog, Remote Control up and no usage limit on screen.
pub fn probe_idle(info: &PaneInfo) -> bool {
    matches!(
        info.box_reading,
        BoxReading::Input(InputState::Empty | InputState::Suggestion)
    ) && !info.busy
        && info.dialog.is_none()
        && !info.rc_failed
        && info.limit.is_none()
}

/// What one [`poll_step`] did after its probe decision.
#[derive(Debug)]
pub struct Polled {
    /// The alert tuples, or why they could not be computed.
    pub alerts: Result<Vec<(String, String)>, UsageError>,
    /// The probe launch that failed, when one did.
    pub launch_error: Option<io::Error>,
}

fn num(st: &Map<String, Value>, key: &str) -> Result<f64, UsageError> {
    match st.get(key) {
        None => Ok(0.0),
        Some(Value::Bool(b)) => Ok(f64::from(u8::from(*b))),
        Some(Value::Number(n)) => n.as_f64().ok_or(UsageError::OutOfRange),
        Some(v) => Err(UsageError::WrongType {
            key: format!("usage.{key}"),
            value: v.to_string(),
            want: "a number",
        }),
    }
}

/// `poll_step(state, now, idle_panes, dry_run, launcher)` over the poll
/// state's other keys (`state["usage"]`). See the module docs for the
/// commit rule.
///
/// # Errors
/// A failure before the probe decision (claude.json is read leniently): an
/// unreadable ledger, a cache that raises, a failed ledger append, a
/// malformed `usage` state. `state` is then unchanged apart from an empty
/// `usage` object where there was none.
pub fn poll_step(
    paths: &UsagePaths,
    launcher: &dyn ProbeLauncher,
    state: &mut Map<String, Value>,
    now_ts: f64,
    idle: &[(String, String)],
    dry_run: bool,
) -> Result<Polled, UsageError> {
    let stored = state
        .entry("usage")
        .or_insert_with(|| Value::Object(Map::new()));
    let mut st = match stored {
        Value::Object(m) => m.clone(),
        other => {
            return Err(UsageError::WrongType {
                key: "usage".to_owned(),
                value: other.to_string(),
                want: "an object",
            });
        }
    };
    let now = UtcTime::from_unix(now_ts)?;
    let Some(acct) = detect_account(&paths.claude_json) else {
        return Ok(Polled {
            alerts: Ok(Vec::new()),
            launch_error: None,
        });
    };
    if st.get("account").and_then(Value::as_str) != Some(acct.as_str()) {
        let pending = st.get("account").is_some_and(|v| !v.is_null());
        st.insert("account".to_owned(), Value::String(acct.clone()));
        st.insert("since".to_owned(), now_ts.into());
        st.insert("tries".to_owned(), 0.into());
        st.insert("pending".to_owned(), pending.into());
    }
    let mut rows = read_ledger(&paths.ledger)?;
    if !dry_run {
        sample_cache(paths, now, Some(&rows))?;
        rows = read_ledger(&paths.ledger)?;
    }
    let fresh = match latest(&rows, &acct) {
        Some(last) if last.not_manual() => {
            let ts = last.time()?;
            ts.timestamp() >= num(&st, "since")? - 60.0 && now.seconds_since(ts) < PROBE_EVERY
        }
        _ => false,
    };
    let pending = st.get("pending").is_some_and(truthy);
    if pending && fresh {
        st.insert("pending".to_owned(), false.into());
    }
    let pending = pending && !fresh;
    let tries = num(&st, "tries")?;
    let due = (pending && tries < PENDING_TRIES)
        || (!fresh && now_ts - num(&st, "last_probe")? >= PROBE_EVERY);
    let alerts = usage_alerts(&rows, now, &acct);
    let mut launch_error = None;
    if let (true, Some((session, pane)), false) = (due, idle.first(), dry_run) {
        // #9801: a probe counts only once it started (the prototype counts first).
        match launcher.launch(session, pane, &acct) {
            Ok(()) => {
                let next = match st.get("tries") {
                    Some(Value::Number(n)) if n.is_f64() => Value::from(tries + 1.0),
                    _ => Value::from(tries as i64 + 1),
                };
                st.insert("last_probe".to_owned(), now_ts.into());
                st.insert("tries".to_owned(), next);
            }
            Err(e) => launch_error = Some(e),
        }
    }
    state.insert("usage".to_owned(), Value::Object(st));
    Ok(Polled {
        alerts,
        launch_error,
    })
}

/// `fleet-poll.py`'s `usage_alerts`: one `usage` alert per tuple, and a
/// `poll.log` line (no alert) for each failure. Never fails the cycle.
pub fn usage_step(
    paths: &UsagePaths,
    launcher: &dyn ProbeLauncher,
    state: &mut Map<String, Value>,
    now: f64,
    idle: &[(String, String)],
    dry_run: bool,
) -> (Vec<Alert>, Vec<String>) {
    let mut log = Vec::new();
    let polled = match poll_step(paths, launcher, state, now, idle, dry_run) {
        Ok(p) => p,
        Err(e) => return (Vec::new(), vec![format!("usage sampler ERROR {e}")]),
    };
    if let Some(e) = polled.launch_error {
        log.push(format!("usage probe ERROR {e}"));
    }
    let alerts = match polled.alerts {
        Ok(found) => found
            .iter()
            .map(|(fp, detail)| alert("usage", fp, detail, "", "", now))
            .collect(),
        Err(e) => {
            log.push(format!("usage sampler ERROR {e}"));
            Vec::new()
        }
    };
    (alerts, log)
}
