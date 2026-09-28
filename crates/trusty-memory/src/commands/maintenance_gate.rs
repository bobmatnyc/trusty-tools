//! How a CLI one-shot opens a palace under its data root's maintenance lease
//! (#8733).
//!
//! Why: a Writer-intent `PalaceHandle` open runs the open-time TTL purge,
//! which is maintenance. A one-shot (`palace legacy-kg --apply`, `palace
//! compact`, `rooms backfill --apply`) running beside a daemon that holds the
//! lease would otherwise be a second maintainer on the same root.
//! What: [`open_purging_under_lease`] purges only while this process holds the
//! lease; [`require_lease`] refuses a command whose whole job is maintenance;
//! [`require_state_lease`] does the same for an `AppState`-driven pass
//! (`kg-rebuild --purge-stale-subjects` / `--merge-punctuated-twins`, #8744).
//! None treats an `Unavailable` lease as permission.
//! Test: `import_under_a_lease_held_elsewhere_deletes_no_expired_row`,
//! `compact_under_a_lease_held_elsewhere_refuses_and_deletes_nothing`,
//! `apply_under_a_lease_held_elsewhere_deletes_no_expired_row`,
//! `purge_under_a_lease_held_elsewhere_refuses_and_deletes_nothing`.

use std::sync::Arc;

use anyhow::{bail, Context, Result};
use trusty_common::memory_core::palace::Palace;
use trusty_common::memory_core::retrieval::PalaceHandle;
use trusty_common::memory_core::store::OpenIntent;
use trusty_common::memory_core::{LeaseStatus, MaintenanceLease};

use crate::AppState;

/// Open `palace` under `intent`, deleting expired rows only when this process
/// holds `lease`.
///
/// A non-holder still hides expired drawers from the returned handle; it only
/// skips the delete. The caller keeps `lease` alive for as long as it wants to
/// stay the maintainer.
pub(crate) fn open_purging_under_lease(
    palace: &Palace,
    intent: OpenIntent,
    lease: &MaintenanceLease,
) -> Result<Arc<PalaceHandle>> {
    let purge = lease.try_hold().is_held();
    PalaceHandle::open_with_intent_purging(palace, intent, purge)
        .with_context(|| format!("open palace {}", palace.id))
}

/// Refuse unless this process holds `lease`, naming why it does not.
///
/// Why: a command that exists only to do maintenance (compaction) cannot do
/// its job without breaking the one-maintainer invariant, so it fails loud
/// the same way a daemon's manual dream run answers `Conflict`.
pub(crate) fn require_lease(lease: &MaintenanceLease) -> Result<()> {
    let lock = lease.path().display();
    match lease.try_hold() {
        LeaseStatus::Held => Ok(()),
        LeaseStatus::HeldElsewhere { holder_pid } => {
            let holder = holder_pid.map_or_else(|| "unknown".to_string(), |p| p.to_string());
            bail!(
                "another process (pid {holder}) holds this data root's maintenance lease \
                 ({lock}); stop it and re-run (#8733)"
            )
        }
        LeaseStatus::Unavailable { reason } => bail!(
            "the maintenance lease {lock} is unavailable ({reason}); refusing to run \
             maintenance without it (#8733)"
        ),
    }
}

/// Take `state`'s maintenance lease or refuse, returning it for the caller to
/// hold across its destructive section (#8744).
///
/// Why: a writer-intent registry already owns this data root's lease. A second
/// `MaintenanceLease` in the same process holds its own `flock` descriptor and
/// would lose to that one, so a pass must reuse the registry's lease.
/// What: the registry's lease when it has one, otherwise a fresh lease on
/// `state.data_root`, then [`require_lease`]. A fresh lease is released when
/// the last clone of the returned `Arc` drops; the registry's is held until the
/// registry drops. A crashed holder's `flock` is dropped by the kernel.
/// Test: `purge_under_a_lease_held_elsewhere_refuses_and_deletes_nothing`,
/// `merge_under_a_lease_held_elsewhere_refuses_and_repoints_nothing`,
/// `destructive_passes_fail_closed_on_an_unavailable_lease`.
pub(crate) fn require_state_lease(state: &AppState) -> Result<Arc<MaintenanceLease>> {
    let lease = state
        .registry
        .maintenance_lease()
        .cloned()
        .unwrap_or_else(|| Arc::new(MaintenanceLease::new(&state.data_root)));
    require_lease(&lease)?;
    Ok(lease)
}
