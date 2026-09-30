//! The kill loop of the shutdown legacy-registry reap (#8942).
//!
//! Why: boot discovery fills the legacy registry from ANY claude pane, the
//! Architect's included, so the graceful-shutdown reap reached the Architect
//! by name with no record at all. Split out of `daemon/mod.rs` so a test can
//! drive the loop with a scripted tmux and a scratch kill floor.
//! What: [`reap_legacy_names`] kills each name through
//! [`TmuxDriver::kill_session`], whose #8942 floor refuses the Architect's
//! names; a refusal or failure is logged and the loop moves on.
//! Test: `reap_all_live_sessions_never_kills_a_sidecar_named_session`.

use tracing::warn;

use super::tmux::TmuxDriver;

/// Kill every tmux session in `names`; returns how many kills ran.
///
/// Why: see the module doc. One failure must not stop the rest.
/// What: best-effort `kill_session` per name; a floor refusal is an `Err`
/// like any other and is logged, never retried.
/// Test: `reap_all_live_sessions_never_kills_a_sidecar_named_session`.
pub(crate) fn reap_legacy_names(
    driver: &TmuxDriver,
    names: impl IntoIterator<Item = String>,
) -> usize {
    let mut reaped = 0usize;
    for name in names {
        match driver.kill_session(&name) {
            Ok(()) => reaped += 1,
            // Fail-open: a kill failing (already gone, never had a host, or
            // refused by the #8942 floor) must not stop us reaping the rest.
            Err(e) => warn!("graceful shutdown: kill_session({name}) failed or was refused: {e}"),
        }
    }
    reaped
}

#[cfg(test)]
#[path = "legacy_reap_tests.rs"]
mod tests;
