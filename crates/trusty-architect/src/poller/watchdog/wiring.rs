//! One cycle's watchdog: `fleet-poll.py`'s `watchdog_alerts`.
//!
//! Why: the cycle needs the watchdog's alerts and its log phrase, read from
//! the live pane and the Architect's transcript (#9779).
//! What: [`watchdog_alerts`] reads `(busy, spinner)` from the Architect's
//! pane (read-only; a failed capture is a log line and reads idle, as in the
//! prototype), the newest transcript's mtime, and runs [`super::step`] on
//! `state.other["watchdog"]`. A reported relaunch clears `wake_sent_at`, so
//! the new session starts a fresh silent clock.
//! Test: `watchdog_alerts_dead_architect_alerts_and_relaunches`,
//! `watchdog_alerts_capture_failure_reads_idle_and_logs`,
//! `watchdog_alerts_shadow_never_relaunches`.
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde_json::{Map, Value};

use super::{Observed, Relauncher, step};
use crate::classify::Alert;
use crate::classify::alerts::alert;
use crate::poller::architect::architect_busy_in;
use crate::poller::config::Config;
use crate::poller::listing::Pane;
use crate::poller::state::PollState;
use crate::self_ctx::{CANDIDATE_LIMIT, newest_transcript};
use crate::tmux::Tmux;

/// mtime of the newest interactive transcript in `dir`, or `None`.
fn transcript_mtime(dir: &Path) -> Option<f64> {
    let path = newest_transcript(dir, CANDIDATE_LIMIT)?;
    let modified = path.metadata().ok()?.modified().ok()?;
    Some(modified.duration_since(UNIX_EPOCH).ok()?.as_secs_f64())
}

/// `(alerts, log phrase, log lines)` for the watchdog this cycle. `dry_run`
/// covers a dry run and a shadow run: no relaunch is attempted.
pub fn watchdog_alerts<T: Tmux + ?Sized>(
    tmux: &T,
    panes: &[Pane],
    state: &mut PollState,
    config: &Config,
    now: f64,
    relauncher: &dyn Relauncher,
    dry_run: bool,
) -> (Vec<Alert>, String, Vec<String>) {
    let mut log = Vec::new();
    let session = config.supervisor.as_str();
    let (busy, spinner) = architect_busy_in(tmux, panes, session).unwrap_or_else(|e| {
        log.push(format!("architect capture ERROR {e}"));
        (false, String::new())
    });
    let observed = Observed {
        panes,
        session,
        wake_at: state.wake_sent_at,
        transcript_mtime: transcript_mtime(&config.self_ctx_dir),
        now,
        silent_min: config.silent_minutes,
        frozen_min: config.frozen_minutes,
        busy,
        spinner: &spinner,
    };
    let entry = state
        .other
        .entry("watchdog".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        log.push("watchdog state was not an object; reset".to_owned());
        *entry = Value::Object(Map::new());
    }
    let Value::Object(book) = entry else {
        return (Vec::new(), String::new(), log);
    };
    let result = step(book, &observed, relauncher, dry_run);
    if result.relaunched {
        state.wake_sent_at = None;
    }
    let alerts = result
        .found
        .iter()
        .map(|f| alert(f.kind, &f.fingerprint, &f.detail, session, "", now))
        .collect();
    (alerts, result.phrase, log)
}
