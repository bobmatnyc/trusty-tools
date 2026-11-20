//! The Architect's self-refresh at 50% context: when to start
//! `self-clear.sh` (D22, Bob 2026-10-21; df-T7i P1b).
//!
//! Why: an Architect far past its bar loses the fleet's thread; the poller
//! starts `scripts/self-clear.sh --live`, which owns the freshness gate and
//! the idle checks. This only decides when to start it.
//! What: [`self_refresh_step`] returns the log phrase. It does nothing below
//! `SELF_REFRESH_PCT` (default 50) of the window or with no transcript; it
//! skips while a self-clear runs (the stored pid, or any `self-clear.sh`
//! process), within [`SELF_REFRESH_DEBOUNCE`] of the last start, with no
//! Architect pane, or on a pane that is busy, shows a dialog or holds typed
//! text. A dry or shadow run reports what it would start. A start stores
//! `started_at` and `pid` in `state["self_refresh"]`. A missing window or
//! percent setting, a zero window, unreadable context, a failed capture or a
//! failed start is `self-refresh=error`, a log line, and a fault record in
//! `state["self_refresh_fault"]`; any other outcome clears the record. The
//! next cycle's `collect` raises the record as a `self_refresh` alert (one
//! fingerprint per kind) into `failing`, so it is due a push like any other
//! failure (#11541 review). The caller runs it only in a cycle whose wake
//! typed nothing.
//! Test: `test_self_refresh_starts_once_at_50_percent_when_idle`,
//! `test_self_refresh_skips_below_threshold_running_and_busy`,
//! `a_failed_self_clear_start_is_an_error_phrase` (tests/p1b_ported.rs),
//! `self_refresh_config_fault_is_an_alert`, `self_refresh_zero_window_is_an_alert`,
//! `self_refresh_unreadable_tokens_is_an_alert`,
//! `self_refresh_capture_failure_is_an_alert`,
//! `self_refresh_failed_start_is_an_alert` (tests/self_refresh_faults.rs).

use std::path::Path;
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::config::Config;
use super::listing::{Pane, capture};
use super::steps::scripts::Subprocess;
use crate::classify::{Alert, BoxReading, alert, parse_pane};
use crate::input::InputState;
use crate::self_ctx::self_ctx_tokens;
use crate::tmux::Tmux;

/// Seconds before the poller starts `self-clear.sh` again.
pub const SELF_REFRESH_DEBOUNCE: f64 = 1800.0;
/// Own context percent at which the poller starts `self-clear.sh`.
pub const SELF_REFRESH_PCT_DEFAULT: f64 = 50.0;

/// `state` key of the step's last fault: `{"kind": .., "detail": ..}`.
pub const SELF_REFRESH_FAULT: &str = "self_refresh_fault";

/// A self-refresh failure: the alert fingerprint and a one-line detail.
type Fault = (&'static str, String);

/// The stored fault as a `self_refresh` alert on the supervisor; empty when
/// the last run of the step had none.
///
/// Test: `self_refresh_config_fault_is_an_alert` and its siblings.
pub fn self_refresh_fault_alerts(
    other: &Map<String, Value>,
    supervisor: &str,
    now: f64,
) -> Vec<Alert> {
    let Some(rec) = other.get(SELF_REFRESH_FAULT) else {
        return Vec::new();
    };
    let text = |k: &str| rec.get(k).and_then(Value::as_str).unwrap_or("unknown");
    let detail = format!("self-refresh {}: {}", text("kind"), text("detail"));
    vec![alert(
        "self_refresh",
        text("kind"),
        &detail,
        supervisor,
        "",
        now,
    )]
}

/// `self_clear_running(state)`: the stored pid is a live `self-clear.sh`, or
/// `pgrep -f scripts/self-clear.sh` finds one. A failed check is "not running".
pub fn self_clear_running(runner: &dyn Subprocess, pid: Option<i64>) -> bool {
    let run = |argv: &[String]| runner.output(argv, Duration::from_secs(5)).ok();
    if let Some(pid) = pid {
        let argv = ["ps", "-p", &pid.to_string(), "-o", "command="].map(str::to_owned);
        if run(&argv).is_some_and(|o| o.stdout.contains("self-clear.sh")) {
            return true;
        }
    }
    let argv = ["pgrep", "-f", "scripts/self-clear.sh"].map(str::to_owned);
    run(&argv).is_some_and(|o| o.code == Some(0))
}

/// `self_refresh_step(state, panes, now, dry_run)`; see the module docs.
#[allow(clippy::too_many_arguments)] // the prototype's step, plus its seams
pub fn self_refresh_step<T: Tmux + ?Sized>(
    other: &mut Map<String, Value>,
    cfg: &Config,
    tmux: &T,
    runner: &dyn Subprocess,
    panes: &[Pane],
    now: f64,
    dry_run: bool,
    log: &mut Vec<String>,
) -> String {
    match step(other, cfg, tmux, runner, panes, now, dry_run, log) {
        Ok(phrase) => {
            other.remove(SELF_REFRESH_FAULT);
            phrase
        }
        Err((kind, detail)) => {
            // #11541: a fault is an alert next cycle, not only a log line.
            log.push(format!("self_refresh {kind} ERROR {detail}"));
            let rec = json!({"kind": kind, "detail": detail});
            other.insert(SELF_REFRESH_FAULT.to_owned(), rec);
            "self-refresh=error".to_owned()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn step<T: Tmux + ?Sized>(
    other: &mut Map<String, Value>,
    cfg: &Config,
    tmux: &T,
    runner: &dyn Subprocess,
    panes: &[Pane],
    now: f64,
    dry_run: bool,
    log: &mut Vec<String>,
) -> Result<String, Fault> {
    let (Some(window), Some(pct_bar)) = (cfg.self_ctx_window, cfg.self_refresh_pct) else {
        let why = "SELF_CTX_WINDOW or SELF_REFRESH_PCT missing or invalid";
        return Err(("config", why.to_owned()));
    };
    if window == 0 {
        return Err(("window", "SELF_CTX_WINDOW is 0".to_owned()));
    }
    let tokens = self_ctx_tokens(&cfg.self_ctx_dir).map_err(|e| ("tokens", e.to_string()))?;
    let Some(tokens) = tokens else {
        return Ok(String::new());
    };
    let pct = 100.0 * tokens as f64 / window as f64;
    if pct < pct_bar {
        return Ok(String::new());
    }
    if !other.get("self_refresh").is_some_and(Value::is_object) {
        other.insert("self_refresh".to_owned(), json!({}));
    }
    let book = other.get("self_refresh").and_then(Value::as_object);
    let pid = book.and_then(|b| b.get("pid")).and_then(Value::as_i64);
    let started = book
        .and_then(|b| b.get("started_at"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if self_clear_running(runner, pid) {
        return Ok("self-refresh=running".to_owned());
    }
    if now - started < SELF_REFRESH_DEBOUNCE {
        return Ok("self-refresh=debounced".to_owned());
    }
    let mut sup: Vec<&Pane> = panes
        .iter()
        .filter(|p| p.session == cfg.supervisor)
        .collect();
    sup.sort_by_key(|p| p.active != "11");
    let Some(pane) = sup.first() else {
        return Ok("self-refresh=no pane".to_owned());
    };
    let raw = capture(tmux, &pane.pane).map_err(|e| ("capture", e.to_string()))?;
    let info = parse_pane(&raw);
    let idle_box = matches!(
        info.box_reading,
        BoxReading::Input(InputState::Empty | InputState::Suggestion)
    );
    if info.dialog.is_some() || info.busy || !info.has_box || !idle_box {
        return Ok("self-refresh=not idle".to_owned());
    }
    if dry_run {
        return Ok(format!("self-refresh=dry-run would start at {pct:.0}%"));
    }
    let script = cfg.root.join("scripts/self-clear.sh");
    let argv = [
        "bash".to_owned(),
        script.to_string_lossy().into_owned(),
        "--live".to_owned(),
    ];
    let out = cfg.inbox.join("self-clear.log");
    match runner.spawn_detached(&argv, Some(Path::new(&out))) {
        Ok(pid) => {
            if let Some(Value::Object(b)) = other.get_mut("self_refresh") {
                b.insert("started_at".to_owned(), json!(now));
                b.insert("pid".to_owned(), json!(pid));
            }
            log.push(format!(
                "self_refresh started self-clear.sh pid={pid} ctx={pct:.0}%"
            ));
            Ok(format!("self-refresh=started ctx={pct:.0}%"))
        }
        Err(e) => Err(("start", e.to_string())),
    }
}
