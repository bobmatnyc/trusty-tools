//! `tm architect poll`: the port of `fleet-poll.py`'s command line.
//!
//! Why: `start-fleet-poll.sh` runs the poller in its own tmux session with
//! `fleet-poll.py [--once] [--interval N] [--dry-run]` (#9822).
//! What: [`poll_config`] builds the poller [`Config`] from the environment,
//! with the shipped scripts' names: `ARCHITECT_INBOX_DIR` (which outranks
//! `SUPERVISOR_INBOX_DIR`, as in the script), `ARCHITECT_QUIET_SESSIONS_FILE` for the quiet
//! list, `ARCHITECT_SESSION` (default `tm-architect`) for the one pane the
//! poller wakes and `ARCHITECT_POLL_SESSION` (default `<that>-poll`) for its
//! own session; `tm fleet init --session` sets both. [`poll`] runs the
//! loop on the real tmux, clock and host: `--once` and `--dry-run` run one
//! cycle through [`Runner::run_single`], which fails where the script logs
//! and exits 0. #9167: each submitted wake is recorded through the caller's
//! [`MessageSink`] (`tm` passes its trusty-events producer). #9779: the
//! watchdog runs every cycle (relaunch via `session-relaunch.sh`), and
//! `--shadow` redirects every write to `<inbox>/shadow/` and acts on nothing.
//! Test: `poll_config_reads_the_shipped_names`,
//! `run_single_fails_closed_when_tmux_cannot_list_panes` (tests/verbs_poll.rs).

use std::path::{Path, PathBuf};

use super::VerbError;
use crate::clock::SystemClock;
use crate::error::RunError;
use crate::poller::slack::{SEND_TIMEOUT, SLACK_POST_URL, SlackDm, SystemCurl, env_token};
use crate::poller::steps::notify_macos::Osascript;
use crate::poller::steps::scripts::{ScriptSteps, SystemSubprocess};
use crate::poller::steps::usage::ScriptProbe;
use crate::poller::watchdog::relaunch::{RELAUNCH_TIMEOUT, ScriptRelauncher, SystemScriptRunner};
use crate::poller::{Config, MessageSink, RuleTriage, Runner, SUPERVISOR, SystemHostProbe};
use crate::tmux::SystemTmux;

/// `fleet-poll.py`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PollArgs {
    /// `--once`: one cycle, then exit.
    pub once: bool,
    /// `--interval N`, as typed: seconds between cycles; callers default it
    /// to [`configured_interval`] (#11369).
    pub interval: String,
    /// `--dry-run`: one cycle that prints, writes no file and sends no keys.
    pub dry_run: bool,
    /// `--shadow`: writes only `<inbox>/shadow/`, takes no lock, sends no
    /// keys and relaunches nothing (#9779).
    pub shadow: bool,
}

/// The poller configuration for the Architect in `root`; see the module doc.
///
/// # Errors
/// [`VerbError::Usage`] when `ARCHITECT_SESSION` or `ARCHITECT_POLL_SESSION`
/// is not a tmux session name (it holds `:`, `.` or whitespace).
pub fn poll_config(
    root: &Path,
    home: &Path,
    get: &dyn Fn(&str) -> Option<String>,
) -> Result<Config, VerbError> {
    let set = |name: &str| get(name).filter(|v| !v.is_empty());
    let supervisor = set("ARCHITECT_SESSION").unwrap_or_else(|| SUPERVISOR.to_owned());
    let poll_session =
        set("ARCHITECT_POLL_SESSION").unwrap_or_else(|| format!("{supervisor}-poll"));
    for (name, value) in [
        ("ARCHITECT_SESSION", &supervisor),
        ("ARCHITECT_POLL_SESSION", &poll_session),
    ] {
        if value.contains(|c: char| c == ':' || c == '.' || c.is_whitespace()) {
            return Err(VerbError::Usage(format!(
                "{name}={value:?} is not a tmux session name"
            )));
        }
    }
    let lookup = |name: &str| match name {
        // #9822: fleet-poll.py reads only ARCHITECT_INBOX_DIR, so it wins.
        "SUPERVISOR_INBOX_DIR" => set("ARCHITECT_INBOX_DIR").or_else(|| set(name)),
        _ => get(name),
    };
    let mut config =
        Config::from_env(root, home, &lookup).map_err(|e| VerbError::Usage(e.to_string()))?;
    if let Some(file) = set("ARCHITECT_QUIET_SESSIONS_FILE") {
        config.quiet_sessions = PathBuf::from(file);
    }
    config.supervisor = supervisor;
    config.poll_session = poll_session;
    Ok(config)
}

/// Runs the poller as `fleet-poll.py` would with `args`, recording each
/// submitted wake through `messages`; see the module doc.
///
/// # Errors
/// [`VerbError::Usage`] for an interval that is not a positive whole number;
/// [`VerbError::Failed`] when another poller holds the lock, the inbox cannot
/// be written, or a single run's cycle fails or is incomplete.
pub fn poll(args: &PollArgs, config: &Config, messages: &dyn MessageSink) -> Result<(), VerbError> {
    let mut config = config.clone();
    config.shadow = args.shadow;
    let result = with_live_runner(&config, args.dry_run, messages, &mut |runner| {
        if args.once || args.dry_run {
            runner.run_single(&args.interval).map(drop)
        } else {
            runner.run_loop(&args.interval, None)
        }
    });
    result.map_err(run_error)
}

/// What `poll` or `serve` does with the production runner.
pub(crate) type LiveCycle<'f> =
    dyn FnMut(&Runner<'_, SystemTmux, SystemClock>) -> Result<(), RunError> + 'f;

/// The production [`Runner`] for `config` handed to `f`: the real tmux,
/// clock, host, usage probe, notifier, relaunch script, script steps and
/// Slack push. `poll` and `serve` (#9779) share it, so they run one cycle.
pub(crate) fn with_live_runner(
    config: &Config,
    dry_run: bool,
    messages: &dyn MessageSink,
    f: &mut LiveCycle<'_>,
) -> Result<(), RunError> {
    let tmux = config
        .tmux_socket
        .as_ref()
        .map_or_else(SystemTmux::new, SystemTmux::with_socket);
    // #9801: the real usage probe (usage-probe.sh) and macOS notifier.
    let usage_probe = ScriptProbe::new(&config.usage);
    // #9779: the watchdog relaunches through the Python session-relaunch.sh.
    let relauncher = ScriptRelauncher {
        tmux: &tmux,
        runner: &SystemScriptRunner,
        script: config.relaunch_script.clone(),
        session: config.supervisor.clone(),
        root: config.root.clone(),
        timeout: RELAUNCH_TIMEOUT,
    };
    // df-T7i P1b: the run_* scripts, zone watch and self-refresh start.
    let steps = ScriptSteps::new(&SystemSubprocess, config);
    // #9779 D5t: the owner push; sends only with ARCHITECT_PUSH_CHANNEL=slack.
    let slack = SlackDm {
        curl: &SystemCurl,
        token: &env_token,
        url: SLACK_POST_URL.to_owned(),
        timeout: SEND_TIMEOUT,
    };
    // See MB1595: alert_triage.py's rules, not wake-for-all.
    let triage = RuleTriage {
        rules: config.triage_rules.clone(),
        schedule: config.il_schedule.clone(),
    };
    let runner = Runner {
        tmux: &tmux,
        clock: &SystemClock,
        probe: &SystemHostProbe,
        triage: &triage,
        config,
        usage_probe: &usage_probe,
        notifier: &Osascript::default(),
        messages,
        relauncher: Some(&relauncher),
        steps: Some(&steps),
        pusher: Some(&slack),
        dry_run,
    };
    f(&runner)
}

/// The `local/architect.json` key holding the poll cadence in whole seconds.
pub const POLL_INTERVAL_KEY: &str = "poll_interval_s";

/// The cadence when `local/architect.json` names none; `architect_config.py`'s
/// `DEFAULT_POLL_INTERVAL_S`.
pub const DEFAULT_POLL_INTERVAL_S: u64 = 60;

/// The poll interval, as `--interval` text, every start path shares.
///
/// Why: #11369 — the shell start scripts, `fleet-poll.py` and `tm architect
/// poll` each carried their own default, so the running cadence depended on
/// who started the poller.
/// What: reads [`POLL_INTERVAL_KEY`] from the file `ARCHITECT_CONFIG` names
/// (which must exist), else `<root>/local/architect.json`, the same lookup as
/// `architect_config.py`. A missing or blank default file, or an absent key,
/// gives [`DEFAULT_POLL_INTERVAL_S`].
/// Test: `configured_interval_reads_architect_json` (tests/verbs_poll.rs);
/// the cross-path parity test lives in `tm`'s `commands/architect.rs`.
///
/// # Errors
/// [`VerbError::Usage`] for an unreadable file, malformed JSON, a top level
/// that is not an object, or a key that is not a whole number of at least 1;
/// the message names the path and key, never the value.
pub fn configured_interval(
    root: &Path,
    get: &dyn Fn(&str) -> Option<String>,
) -> Result<String, VerbError> {
    let named = get("ARCHITECT_CONFIG").filter(|v| !v.is_empty());
    let path = named
        .as_ref()
        .map_or_else(|| root.join("local/architect.json"), PathBuf::from);
    let shown = path.display();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && named.is_none() => {
            return Ok(DEFAULT_POLL_INTERVAL_S.to_string());
        }
        // #11369: a named or unreadable file never falls back to the default.
        Err(e) => return Err(VerbError::Usage(format!("{shown}: cannot read: {e}"))),
    };
    if text.trim().is_empty() {
        return Ok(DEFAULT_POLL_INTERVAL_S.to_string());
    }
    let data: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| VerbError::Usage(format!("{shown}: not valid JSON ({e})")))?;
    let object = data
        .as_object()
        .ok_or_else(|| VerbError::Usage(format!("{shown}: the top level must be a JSON object")))?;
    match object.get(POLL_INTERVAL_KEY) {
        None => Ok(DEFAULT_POLL_INTERVAL_S.to_string()),
        Some(value) => match value.as_u64() {
            Some(secs) if secs >= 1 => Ok(secs.to_string()),
            _ => Err(VerbError::Usage(format!(
                "{shown}: {POLL_INTERVAL_KEY}: must be a whole number of seconds, at least 1"
            ))),
        },
    }
}

/// A bad interval is a usage error, as argparse's `type=int` makes it.
pub(crate) fn run_error(e: RunError) -> VerbError {
    match e {
        RunError::Config(c) => VerbError::Usage(c.to_string()),
        other => VerbError::Failed(other.to_string()),
    }
}
