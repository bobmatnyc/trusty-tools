//! Whether the in-project hygiene sweep runs, and the lane it runs in (#7965).
//!
//! Why a module of its own: the decision used to be fifteen lines inline in
//! `daemon/mod.rs`'s boot sequence — the env gate, the #5784 host-state refusal
//! and the `spawn_blocking` call — which is the shape that made it invisible.
//! `merged_pr_reclaim::spawn_if_enabled` already reads as one line at the call
//! site; this is the same split for the other sweep, and it is where the #7965
//! maintenance lane and the pass timing are applied so neither can be forgotten
//! by a future edit to the boot sequence.
//!
//! What: [`spawn_if_enabled`] reads [`ENV_ENABLED`], refuses a host state that
//! must not be swept, then takes the single background-maintenance permit
//! ([`crate::daemon::services::sweep_status::lane`]) before handing the
//! synchronous pass to the blocking pool. Holding the permit for the pass is what
//! stops this sweep and the worktree reclaim from running their subprocess loops
//! at the same time.
//!
//! Test: `parse_enabled_defaults_on_and_honours_the_off_switch`,
//! `spawn_if_enabled_is_a_noop_when_disabled` in
//! `inproject_hygiene_sweep_tests.rs`.

use std::sync::Arc;

use tracing::{info, warn};

use crate::daemon::services::sweep_status;
use crate::daemon::state::DaemonState;

/// Environment variable that disables the in-project hygiene sweep entirely.
///
/// Why it stays a kill switch: #7965's mitigation on the reporting host was
/// exactly `TRUSTY_MPM_INPROJECT_HYGIENE=0`, and an operator must keep that lever
/// even after the sweep is bounded.
/// What: `0`/`false`/`off`/`no` (trimmed, case-insensitive) disables; anything
/// else, including unset, enables.
/// Test: `parse_enabled_defaults_on_and_honours_the_off_switch`.
pub(crate) const ENV_ENABLED: &str = "TRUSTY_MPM_INPROJECT_HYGIENE";

/// Pure parse of [`ENV_ENABLED`] into an on/off decision.
///
/// Why pure: env vars are process-global and ~20 tests in this binary write them,
/// so the policy is tested on a value rather than by mutating the process.
/// Test: `parse_enabled_defaults_on_and_honours_the_off_switch`.
pub(crate) fn parse_enabled(raw: Option<&str>) -> bool {
    match raw {
        Some(v) => !matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "no"
        ),
        None => true,
    }
}

/// Whether the hygiene sweep should run at all.
///
/// Test: the policy is `parse_enabled_defaults_on_and_honours_the_off_switch`.
pub(crate) fn enabled() -> bool {
    parse_enabled(std::env::var(ENV_ENABLED).ok().as_deref())
}

/// Spawn the startup hygiene sweep unless an operator switched it off (#1709,
/// #7965).
///
/// Why `state` is taken but only its host-state gate used: the refusal is the
/// #5784 one — a daemon whose `$HOME` was reassigned must not fetch and
/// fast-forward the operator's real clones — and keeping the argument makes the
/// call site identical in shape to the reclaim sweep's.
/// What: gate, refuse, then `tokio::spawn` a task that acquires the single
/// maintenance permit and runs the synchronous pass on the blocking pool with a
/// [`sweep_status::HYGIENE`] timing guard. The task is detached: hygiene is
/// best-effort freshness and must never delay the listener coming up.
/// Test: `spawn_if_enabled_is_a_noop_when_disabled`.
pub(crate) fn spawn_if_enabled(state: Arc<DaemonState>) {
    // #8059: the pass is recorded under this daemon's framework root so the
    // daemonless `tm doctor` can read it; the host-state gate below is the
    // other use of `state`.
    let framework_root = state.framework_root().to_path_buf();
    if !enabled() {
        info!("inproject-hygiene disabled via {ENV_ENABLED}");
        return;
    }
    let repos_root = super::inproject::repos_root();
    // #5784: `repos_root()` is only $HOME-scoped when nothing overrode it.
    // Precedence is TRUSTY_MPM_REPOS_ROOT > TRUSTY_MPM_WORKSPACE_ROOT / config >
    // $HOME-derived, and a scratch daemon launched from the operator's shell
    // inherits those exports — so a reassigned $HOME can still point this sweep,
    // which fetches and fast-forwards real clones, at the operator's real repos
    // root. Skip when the two disagree.
    if let Some(reason) = crate::core::host_state_gate::host_state_access().skip_reason() {
        warn!(
            repos_root = %repos_root.display(),
            "inproject-hygiene skipped — {reason}"
        );
        return;
    }
    tokio::spawn(async move {
        // #7965: one sweep at a time. `acquire` cannot fail — nothing closes this
        // semaphore — but a closed one must not silently skip the pass either, so
        // the error arm logs and returns rather than running unserialised.
        let Ok(_permit) = sweep_status::lane().acquire().await else {
            warn!("inproject-hygiene: maintenance lane closed; sweep not run");
            return;
        };
        let _ = tokio::task::spawn_blocking(move || {
            let _timing = sweep_status::HYGIENE.begin(Some(&framework_root));
            super::inproject_hygiene::run_hygiene_for_all_bases(&repos_root);
        })
        .await;
    });
}

#[cfg(test)]
#[path = "inproject_hygiene_sweep_tests.rs"]
mod inproject_hygiene_sweep_tests;
