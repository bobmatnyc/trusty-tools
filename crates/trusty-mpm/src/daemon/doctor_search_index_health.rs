//! `tm doctor` rows fed by trusty-search's `search.project.health` (#10535).
//!
//! Why: the `search_project_*` rows answer per-project questions; nothing
//! reported index hygiene across the whole daemon, such as parked indexes
//! whose root is gone but whose corpus is still on disk. The counts come from
//! one read-only call that loads no corpus.
//! What: [`check_search_index_health`] makes one call bounded by
//! [`PROBE_TIMEOUT`] and renders [`ORPHAN_PARKED`]. A daemon that does not
//! answer, predates the method, or answers a body that does not decode is
//! Warn "not checked", never `Ok`. `search_index_stale`,
//! `search_index_hand_named` and `search_index_nested_root` need #10534's
//! classification and land with its PR 2.
//! Test: `doctor_search_index_health_tests.rs`.

use std::path::Path;

use trusty_common::search_rpc::{ProjectHealthCounts, SearchRpcError, project_health_at};
use trusty_common::uds::server::CODE_METHOD_NOT_FOUND;

use crate::core::doctor::{CheckStatus, DoctorCheck};
use crate::daemon::search_rpc;

use super::PROBE_TIMEOUT;

/// Parked indexes whose stored park reason is `orphaned`.
pub(super) const ORPHAN_PARKED: &str = "search_index_orphan_parked";

/// The `search_index_*` rows for the daemon at the resolved socket.
///
/// Why: `run_doctor` calls this beside `check_search_project`.
/// What: resolves the socket, then [`check_search_index_health_at`].
/// Test: `a_down_daemon_leaves_orphan_parked_not_checked`.
pub(super) async fn check_search_index_health() -> Vec<DoctorCheck> {
    match search_rpc::search_socket() {
        Ok(socket) => check_search_index_health_at(&socket).await,
        Err(e) => vec![not_checked(&format!(
            "cannot resolve the trusty-search socket: {e:#}"
        ))],
    }
}

/// [`check_search_index_health`] against an explicit socket.
///
/// Test: `orphaned_parks_are_warn_with_the_count`,
/// `no_orphaned_park_is_ok`, `a_down_daemon_leaves_orphan_parked_not_checked`.
pub(super) async fn check_search_index_health_at(socket: &Path) -> Vec<DoctorCheck> {
    vec![build_orphan_parked(
        socket,
        project_health_at(socket, PROBE_TIMEOUT).await,
    )]
}

/// The [`ORPHAN_PARKED`] row for one call's outcome (pure).
///
/// What: `Ok` with zero orphaned parks; `Warn` naming the count and the
/// listing command otherwise; `Warn` "not checked" for a missing method (a
/// daemon older than #10535), any other refusal, no answer, or a body that
/// does not decode.
/// Test: `orphaned_parks_are_warn_with_the_count`, `no_orphaned_park_is_ok`,
/// `a_daemon_without_the_method_is_not_checked`,
/// `a_down_daemon_leaves_orphan_parked_not_checked`.
pub(super) fn build_orphan_parked(
    socket: &Path,
    outcome: anyhow::Result<ProjectHealthCounts>,
) -> DoctorCheck {
    let counts = match outcome {
        Ok(counts) => counts,
        Err(e) => {
            let reason = match e.downcast_ref::<SearchRpcError>() {
                Some(rpc) if rpc.code == CODE_METHOD_NOT_FOUND => {
                    "the trusty-search daemon predates search.project.health".to_string()
                }
                Some(rpc) => format!("search.project.health was refused: {}", rpc.message),
                None => format!(
                    "no usable answer from trusty-search at {} ({e:#})",
                    socket.display()
                ),
            };
            return not_checked(&reason);
        }
    };
    if counts.orphaned == 0 {
        return DoctorCheck::new(
            ORPHAN_PARKED,
            CheckStatus::Ok,
            format!(
                "no parked index was orphaned ({} registered, {} parked)",
                counts.registered, counts.parked
            ),
        );
    }
    DoctorCheck::new(
        ORPHAN_PARKED,
        CheckStatus::Warn,
        format!(
            "{} parked index(es) were parked because their root was gone; each keeps its \
             corpus on disk. List them with `trusty-search project map` (reason `orphaned`)",
            counts.orphaned
        ),
    )
}

/// The row as Warn "not checked" for `reason`.
fn not_checked(reason: &str) -> DoctorCheck {
    DoctorCheck::new(
        ORPHAN_PARKED,
        CheckStatus::Warn,
        format!("not checked: {reason}"),
    )
}

#[cfg(test)]
#[path = "doctor_search_index_health_tests.rs"]
mod tests;
