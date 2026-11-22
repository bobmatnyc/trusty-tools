//! The `token` alert kind: `token_watch.py`'s `step` and `fleet-poll.py`'s
//! `token_alerts`, ported (MB1595 fix round, class E).
//!
//! Why: the deployed poller raises `token` alerts from a deterministic
//! transcript watcher (no LLM); the port never ran it, so every live `token`
//! alert was poller-only in the parity report.
//! What: [`step`] is one scan: load the state, read each discovered
//! transcript, apply the per-turn and fleet rules, then flush completed
//! hours, prune and save as [`Mode`] allows. [`token_alerts`] wraps the
//! result as `token` alerts (`session` the label, `fingerprint` the rule
//! fingerprint, display `[<session>]: token <detail>`); a failure is one
//! `token watcher ERROR` log line and no alerts. Re-emit is the dedup
//! table's `token` 3600 s; triage is `triage_rules`' `token` rule.
//! [`mode_for`] picks the state file: live runs share
//! `<inbox>/token-watch-state.json` with the Python watcher; a plain dry run
//! reads it and writes nothing; a shadow run (`--shadow` or
//! `ARCHITECT_DRY_RUN_STATE`) keeps its own copy beside its poll state,
//! seeded once from the live file, so it dedupes across cycles without
//! racing the live watcher's offsets (class D).
//! Test: token/tests.rs (the deployed `test_token_watch.py` cases, by name),
//! `token_alerts_shape_and_failure_contract`, `shadow_mode_keeps_its_own_token_state`.

mod clock;
mod discover;
mod fleet;
mod scan;
mod state;
mod units;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub use state::{StateError, Thresholds, TokenState};

use super::config::Config;
use crate::classify::{Alert, alert};
use discover::{agent_type, discover, session_names};
use fleet::{degraded, fleet_rules, strip_user_prefix};
use scan::Scan;
use state::FileState;

/// `BUDGET_S`: a scan stops after this long and resumes next cycle.
const BUDGET_S: f64 = 20.0;
/// The state file's name.
pub const STATE_FILE: &str = "token-watch-state.json";

/// One raised rule: `{"rule","session","fingerprint","detail"}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenAlert {
    /// The rule.
    pub rule: String,
    /// The session label (tmux session, `<project>:<sid>`, or a scope).
    pub session: String,
    /// The fingerprint.
    pub fingerprint: String,
    /// `<rule>: <detail>`.
    pub detail: String,
}

/// Where the scan reads and what it may write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mode {
    /// The state file read.
    pub state: PathBuf,
    /// Copied to `state` once when `state` is missing.
    pub seed: Option<PathBuf>,
    /// Save the state after the scan.
    pub save: bool,
    /// The hourly rollup file; `None` writes none.
    pub hourly: Option<PathBuf>,
}

/// The paths one scan uses.
#[derive(Debug, Clone)]
pub struct Paths<'a> {
    /// The transcript base.
    pub base: &'a Path,
    /// The live-session registry.
    pub sessions: &'a Path,
    /// `local/usage/token-thresholds.json`.
    pub thresholds: PathBuf,
    /// `local/il-schedule.json`.
    pub schedule: &'a Path,
}

/// The [`Mode`] for `config`; `hands_off` is a dry or shadow run.
pub fn mode_for(config: &Config, hands_off: bool) -> Mode {
    let live = config.read_inbox.join(STATE_FILE);
    let own = match (&config.dry_run_state, config.shadow) {
        (Some(p), _) => p.parent().map(|d| d.join(STATE_FILE)),
        (None, true) => Some(config.shadow_dir().join(STATE_FILE)),
        (None, false) => None,
    };
    match own.filter(|_| hands_off) {
        Some(state) => Mode {
            seed: (state != live).then_some(live),
            state,
            save: true,
            hourly: None,
        },
        None => Mode {
            state: config.inbox.join(STATE_FILE),
            seed: None,
            save: !hands_off,
            hourly: (!hands_off).then(|| config.root.join("local/usage/token-hourly.jsonl")),
        },
    }
}

/// `SESSION_DIR_RE.sub('', project)[:24]`.
pub(crate) fn project_label(project: &str) -> String {
    strip_user_prefix(project).chars().take(24).collect()
}

fn word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `POLL_RE.search(cmd)`: `^\s*tm wait\b` or `\bsleep \d+`.
pub(crate) fn is_poll_command(cmd: &str) -> bool {
    if let Some(rest) = cmd.trim_start().strip_prefix("tm wait")
        && !rest.chars().next().is_some_and(word)
    {
        return true;
    }
    cmd.match_indices("sleep ").any(|(i, _)| {
        let before = cmd[..i].chars().next_back();
        let after = cmd[i + 6..].chars().next();
        !before.is_some_and(word) && after.is_some_and(|c| c.is_ascii_digit())
    })
}

/// `step(now, dry_run)`: one scan; see the module doc.
///
/// # Errors
/// [`StateError`] when the state has the wrong shape or cannot be saved, or
/// the hourly file cannot be appended.
pub fn step(
    now: f64,
    mode: &Mode,
    paths: &Paths<'_>,
    names: Option<BTreeMap<String, String>>,
) -> Result<Vec<TokenAlert>, StateError> {
    if let Some(seed) = mode.seed.as_ref().filter(|_| !mode.state.exists()) {
        let st = state::load(seed)?;
        state::save(&mode.state, &st)?;
    }
    let mut st = state::load(&mode.state)?;
    let names = names.unwrap_or_else(|| session_names(paths.sessions));
    let th = Thresholds::load(&paths.thresholds);
    let base = paths.base.to_string_lossy().into_owned();
    let mut sc = Scan::new(&mut st, th, now, base, names, paths.schedule);
    let t0 = Instant::now();
    for found in discover(paths.base, now) {
        if t0.elapsed().as_secs_f64() > BUDGET_S {
            tracing::warn!("token_watch: time budget reached, resuming next cycle");
            break;
        }
        let mut f = sc
            .st
            .files
            .remove(&found.path)
            .unwrap_or_else(|| FileState {
                kind: found.kind.to_owned(),
                project: found.project.clone(),
                session: found.session.clone(),
                agent_id: found.agent_id.clone(),
                ..FileState::default()
            });
        if found.kind == "sub" && f.agent_type.is_none() {
            f.agent_type = Some(agent_type(&found.path));
        }
        sc.read_file(&found.path, &mut f);
        sc.st.files.insert(found.path, f);
    }
    degraded(&mut sc);
    fleet_rules(&mut sc);
    let (lines, errors, alerts) = (sc.lines, sc.errors, std::mem::take(&mut sc.alerts));
    (st.lines, st.parse_errors) = (lines, errors);
    match &mode.hourly {
        Some(h) => {
            state::flush_hours(&mut st, now, h)?;
        }
        None if mode.save => state::drop_done_hours(&mut st, now),
        None => {}
    }
    state::prune(&mut st, now);
    if mode.save {
        state::save(&mode.state, &st)?;
    }
    Ok(alerts)
}

/// The `token` alert for one raised rule (`fleet-poll.py` `token_alerts`).
pub fn to_alert(r: &TokenAlert, now: f64) -> Alert {
    let mut a = alert("token", &r.fingerprint, &r.detail, &r.session, "", now);
    a.display = format!("[{}]: token {}", r.session, a.detail);
    a
}

/// The cycle's token step: alerts, plus a log line on failure.
pub fn token_alerts(config: &Config, now: f64, hands_off: bool) -> (Vec<Alert>, Vec<String>) {
    let mode = mode_for(config, hands_off);
    let paths = Paths {
        base: &config.transcript_base,
        sessions: &config.sessions_dir,
        thresholds: config.root.join("local/usage/token-thresholds.json"),
        schedule: &config.il_schedule,
    };
    match step(now, &mode, &paths, None) {
        Ok(found) => (found.iter().map(|r| to_alert(r, now)).collect(), Vec::new()),
        Err(e) => (Vec::new(), vec![format!("token watcher ERROR {e}")]),
    }
}
