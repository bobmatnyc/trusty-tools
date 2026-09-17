//! What the Telegram gateway is doing, as a status surface rather than a log
//! line (#8190).
//!
//! Why (code-critic MEDIUM 5): every gateway degradation — a binding whose
//! token will not resolve, a roster that cannot be read, a bot whose lock
//! another process holds — was reported only by `tracing`, and izzie's host
//! captures no stderr. So the whole failure class was invisible: Telegram was
//! simply dead, with nothing anywhere saying why. This module keeps the same
//! facts in a process-global status that `tagent system status` and the
//! `system_status` tool both render.
//!
//! What: [`record_scan`] replaces the per-bot roster and the scan warnings
//! wholesale on every rescan, so a fixed binding stops being reported the
//! moment the next scan sees it fixed. [`record_state`] updates one bot's live
//! state between scans. [`snapshot`] merges the stored state with a LIVE probe
//! of the per-bot lock files, which is what lets the separate `tagent system
//! status` process — which has no gateway of its own — still report that
//! something on this machine is polling.
//!
//! Nothing here renders a bot token or its digest. A bot is identified by the
//! credential REFERENCES an operator wrote into the bindings, which are config
//! names; the lock probe contributes PIDs only.
//!
//! Test: `crate::system_status::telegram_gateway_tests`.

use std::sync::{LazyLock, Mutex};

use serde::Serialize;

/// One bot's gateway state, as reported.
///
/// Why: an operator asking "why is Telegram not answering" needs the bot, who
/// it delivers to, and what it is doing right now — in one row.
/// What: `credential_refs` is the operator's own reference text. `state` is one
/// of `polling`, `waiting-for-lock`, `restarting`, `skipped`, `stopped`.
/// `detail` carries the reason for anything other than `polling`.
/// Test: `telegram_gateway_status_records_a_skipped_binding`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TelegramBotStatus {
    /// Operator-authored credential reference text naming this bot.
    pub credential_refs: String,
    /// Assistants this bot delivers to.
    pub assistants: Vec<String>,
    /// `polling` | `waiting-for-lock` | `restarting` | `skipped` | `stopped`.
    pub state: String,
    /// Why, for any state other than `polling`.
    pub detail: Option<String>,
}

/// The whole gateway's reported state.
///
/// Test: `telegram_gateway_status_records_a_skipped_binding`,
/// `telegram_gateway_status_snapshot_reports_a_live_lock_holder`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TelegramGatewayStatus {
    /// One row per distinct bot the last scan found.
    pub bots: Vec<TelegramBotStatus>,
    /// Scan-level degradations, replaced wholesale by each scan.
    pub warnings: Vec<String>,
    /// PIDs holding a Telegram gateway lock on this machine, probed live.
    pub lock_holders: Vec<i32>,
}

/// The process-global gateway state the API host writes and the tool reads.
///
/// Why: the gateway runs in a spawned task with no handle the status path
/// holds, so the two meet at a process global. A poisoned lock is recovered
/// rather than propagated — a status surface must never be the thing that
/// panics a host.
static STATE: LazyLock<Mutex<TelegramGatewayStatus>> =
    LazyLock::new(|| Mutex::new(TelegramGatewayStatus::default()));

/// Replace the bot roster and the scan warnings with this scan's answer.
///
/// Why (#8190 finding 4): the scan re-runs on a timer, so a binding an operator
/// enables or fixes must be able to REMOVE its own warning. Appending would
/// grow an unbounded log of stale complaints.
/// What: overwrites `bots` and `warnings`; `lock_holders` is never stored,
/// because it is probed at read time.
/// Test: `telegram_gateway_status_records_a_skipped_binding`,
/// `telegram_gateway_status_scan_replaces_the_previous_warnings`.
pub(crate) fn record_scan(bots: Vec<TelegramBotStatus>, warnings: Vec<String>) {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state.bots = bots;
    state.warnings = warnings;
}

/// Update one bot's live state between scans.
///
/// Why: a poller that starts waiting on another process's lock, or enters a
/// restart backoff, changes state without the binding set changing at all.
/// What: matches on `credential_refs`, which is the scan's own row key. A bot
/// the last scan did not report is ignored rather than invented — the scan owns
/// the roster.
/// Test: `telegram_gateway_status_records_a_live_state_change`.
pub(crate) fn record_state(credential_refs: &str, state: &str, detail: Option<String>) {
    let mut guard = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(row) = guard
        .bots
        .iter_mut()
        .find(|b| b.credential_refs == credential_refs)
    {
        row.state = state.to_string();
        row.detail = detail;
    }
}

/// Drop every stored row — the host is no longer running a gateway.
///
/// Test: `telegram_gateway_status_scan_replaces_the_previous_warnings`.
pub(crate) fn clear() {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    *state = TelegramGatewayStatus::default();
}

/// The stored state plus a live probe of this machine's gateway locks.
///
/// Why: `tagent system status` usually runs in a DIFFERENT process from the API
/// host, where the stored rows are empty. The lock probe is the one signal that
/// crosses that boundary, so "a poller is alive on this machine" is reportable
/// either way.
/// Test: `telegram_gateway_status_snapshot_reports_a_live_lock_holder`.
pub fn snapshot() -> TelegramGatewayStatus {
    snapshot_at(&crate::telegram::gateway_state_dir())
}

/// [`snapshot`] against an explicit state directory.
///
/// Why: the real directory is `$HOME`-derived, which a test must not depend on.
/// Test: `telegram_gateway_status_snapshot_reports_a_live_lock_holder`.
pub(crate) fn snapshot_at(state_dir: &std::path::Path) -> TelegramGatewayStatus {
    let mut status = STATE.lock().unwrap_or_else(|e| e.into_inner()).clone();
    status.lock_holders = crate::telegram::live_gateway_lock_holders_in(state_dir);
    status
}

#[cfg(test)]
#[path = "telegram_gateway_tests.rs"]
mod telegram_gateway_tests;
