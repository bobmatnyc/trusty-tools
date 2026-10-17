//! The python intent behind the embedder `build_embedder` installs (#9970).
//!
//! Why: #9970 AC2 and ruling Q5 — a host that intended python but serves ort
//! is degraded and says so. The eager python path recorded its ort fallback
//! as plain `NotApplicable`, so `meta.embedder` reported `intended: "ort"`
//! with no note and `/health` stayed `ok`.
//! What: [`PythonIntent`] names what python means for the built backend, and
//! [`wrap_switchable`] turns it into the `SwitchableEmbedder` snapshot and
//! bootstrap report the agent note and `/health` read.
//! Test: `eager_python_failure_reports_python_intended_and_degraded`,
//! `eager_python_latch_trip_reports_ort_with_a_note` in `start/tests_9970.rs`.

use std::sync::{Arc, OnceLock};

use super::embedder::{backend_respects_quantized_env, quantized_from_env, EMBEDDER_MODEL_NAME};
use super::embedder_fallback::TripListener;
use crate::service::embedder_supervisor::bootstrap_control::BootstrapReport;
use crate::service::embedder_supervisor::{
    ActiveBackend, BackendKind, BootstrapState, SwitchableEmbedder,
};

/// What the python embedder means for the backend `build_embedder_raw` built.
pub(super) enum PythonIntent {
    /// Python is not intended: the backend is what the host asked for.
    None,
    /// The graceful default: ort serves while the background bootstrap runs.
    Graceful,
    /// The eager path armed python behind the fallback latch; the slot hears
    /// a latch trip (#9970).
    EagerArmed(Arc<OnceLock<TripListener>>),
    /// The eager path failed at startup and ort serves; carries the cause
    /// (#9970).
    EagerFailed(String),
}

/// Wrap `raw` in a [`SwitchableEmbedder`] whose snapshot states the python
/// intent, and say whether the graceful bootstrap must be spawned.
///
/// Why: #9970 AC2 and ruling Q5 — an intended-python host that serves ort is
/// degraded and says so; recording the eager path's ort fallback as plain
/// `NotApplicable` reported `intended: "ort"` with no note.
/// What: `Graceful` → `Bootstrapping`. `EagerFailed` → `Failed`, with the
/// cause as the report's `last_error`. `EagerArmed` → `NotApplicable` while
/// python serves, plus a latch listener that hot-swaps the snapshot to
/// `Ort` / `FellBackToOrt` with the trip reason. `None` → `NotApplicable`.
/// Test: `eager_python_failure_reports_python_intended_and_degraded`,
/// `eager_python_latch_trip_reports_ort_with_a_note` in `start/tests_9970.rs`;
/// `/health` on both states:
/// `health_reports_an_eager_python_fallback_as_degraded_with_the_cause`.
pub(super) fn wrap_switchable(
    raw: Arc<dyn crate::core::Embedder>,
    kind: BackendKind,
    intent: PythonIntent,
) -> (Arc<SwitchableEmbedder>, bool) {
    let graceful = matches!(intent, PythonIntent::Graceful);
    let bootstrap = match intent {
        // Epic #3524 slice 6 PR-3: `/health` reports `Bootstrapping` from
        // the very first read of the graceful arm.
        PythonIntent::Graceful => BootstrapState::Bootstrapping,
        // #9970: python was intended and its setup failed; ort serves.
        PythonIntent::EagerFailed(_) => BootstrapState::Failed,
        PythonIntent::None | PythonIntent::EagerArmed(_) => BootstrapState::NotApplicable,
    };
    let active = ActiveBackend {
        kind,
        provider: raw.provider(),
        model: EMBEDDER_MODEL_NAME.to_string(),
        // Issue #3530 / #3493 P1: only the ort/in-process `FastEmbedder`
        // path actually honours `TRUSTY_EMBEDDER_MODEL` — see
        // `backend_respects_quantized_env`'s doc for why.
        quantized: backend_respects_quantized_env(kind) && quantized_from_env(),
        bootstrap,
    };
    let switchable = Arc::new(SwitchableEmbedder::new(raw, active));
    match intent {
        PythonIntent::EagerFailed(cause) => {
            switchable
                .bootstrap_control()
                .update_report(|r| BootstrapReport {
                    last_error: Some(cause.clone()),
                    ..r.clone()
                });
        }
        PythonIntent::EagerArmed(slot) => {
            // #9970: weak, so the adapter inside `switchable` does not keep
            // `switchable` alive.
            let weak = Arc::downgrade(&switchable);
            let listener: TripListener = Box::new(move |reason, fallback| {
                if let Some(switchable) = weak.upgrade() {
                    record_latch_trip(&switchable, reason, fallback);
                }
            });
            if slot.set(listener).is_err() {
                tracing::warn!("python fallback latch already had a listener; keeping it");
            }
        }
        PythonIntent::None | PythonIntent::Graceful => {}
    }
    (switchable, graceful)
}

/// Report the eager path's latch trip: ort serves for the rest of the run.
/// The cause lands in the report before the state changes, so a reader that
/// sees `FellBackToOrt` also sees why (#9970).
fn record_latch_trip(
    switchable: &SwitchableEmbedder,
    reason: &str,
    fallback: &Arc<dyn crate::core::Embedder>,
) {
    switchable
        .bootstrap_control()
        .update_report(|r| BootstrapReport {
            last_error: Some(reason.to_string()),
            ..r.clone()
        });
    switchable.swap_to(
        Arc::clone(fallback),
        ActiveBackend {
            kind: BackendKind::Ort,
            provider: fallback.provider(),
            model: EMBEDDER_MODEL_NAME.to_string(),
            quantized: backend_respects_quantized_env(BackendKind::Ort) && quantized_from_env(),
            bootstrap: BootstrapState::FellBackToOrt,
        },
    );
}
