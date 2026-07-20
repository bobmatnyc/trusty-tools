//! Activation-lock exclusivity layer over [`WorkstreamStore`] (DOC-48 §6,
//! issue #3294).
//!
//! # Spec References
//!
//! - [`SPEC-WS-04~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-04~draft) §4.3
//! - [`SPEC-WS-06~draft`](docs/specs/DOC-48-tcode-workstreams.md#SPEC-WS-06~draft)
//!
//! Why: [`WorkstreamStore::set_active`] is a bare "write the pointer"
//! primitive — issue #3293's foundation deliberately left conflict detection
//! and force-switch semantics for this ticket to build (see that method's
//! own docs). The daemon-enforced singleton invariant (AC-3.1) needs a layer
//! ABOVE the store that decides `ActiveConflict` vs. idempotent re-activation
//! vs. force-switch, and reports which workstream was previously active so
//! RPC/REST callers (and #3297's SSE `WorkstreamActivationChanged` producer)
//! can react to the change.
//!
//! What: [`SharedWorkstreamStore`] (the `Arc<Mutex<...>>` handle every RPC
//! handler closure clones — mirrors `crate::session::SessionRegistry`'s
//! `Arc`-shared-state convention, except `WorkstreamStore`'s mutating methods
//! take `&mut self`, so a `tokio::sync::Mutex` wrapper is required; `tokio`'s
//! (not `std`'s) because every store operation is `async` file I/O and must
//! not block the executor while held), [`ActivationError`] (the typed
//! failure surface), [`ActivateOutcome`] (new + prior ids), and the two free
//! functions [`activate`]/[`deactivate`] that implement DOC-48 §6.1's exact
//! rules:
//!   - `activate{id, force: false}` when `id` is ALREADY the active
//!     workstream: succeeds (idempotent — re-activating yourself is not a
//!     "switch" at all; §6.1's `ActiveConflict` only fires when ANOTHER
//!     workstream is active).
//!   - `activate{id, force: false}` when a DIFFERENT workstream is active:
//!     `ActivationError::ActiveConflict(active_id)`.
//!   - `activate{id, force: true}`: deactivates whichever workstream was
//!     active (if any) and activates `id`, reporting both ids.
//!   - `deactivate{id}`: clears the pointer only if `id` IS the currently
//!     active workstream; otherwise a no-op success (§4.3: "deactivating an
//!     idle or closed workstream is a no-op (idempotent)" — this includes an
//!     unknown or nonexistent id, since the caller cannot distinguish "idle"
//!     from "never existed" without an extra lookup the spec does not ask
//!     for).
//!
//! (issue #3297) [`activate`]/[`deactivate`] are also the sole publishers of
//! [`crate::events::Event::WorkstreamActivationChanged`] (DOC-48 §5.3/§6.3,
//! AC-3.3) — emitted exactly when the persisted active pointer actually
//! changes (a fresh activation, a force-switch, or a real deactivation),
//! never on the idempotent no-op branches. Centralising the emission here
//! (rather than at each RPC/REST call site) means every current AND future
//! caller of these two functions gets the notification for free — see
//! [`crate::events::Event::WorkstreamActivationChanged`]'s own docs for why
//! this reuses the existing global event bus instead of a second channel.
//! They also publish [`crate::events::Event::WorkstreamStateInferred`] for
//! every workstream whose computed state actually transitions alongside the
//! pointer change (the newly-active workstream on every real activation, plus
//! the prior one on a force-switch); [`super::protocol::close`] publishes the
//! same event's `"closed"` state via [`publish_state_inferred`] (`pub(super)`
//! for exactly that one cross-file call).
//!
//! Test: `activation_tests`.

use std::sync::Arc;

use chrono::Utc;
use thiserror::Error;
use tokio::sync::Mutex;

use crate::events::{Event, SessionEventEnvelope};

use super::model::WorkstreamId;
use super::store::{StoreError, WorkstreamStore};

/// Publish `Event::WorkstreamActivationChanged` on the global event bus.
///
/// Why: the one place [`activate`]/[`deactivate`] construct this event, so
/// both always build the same envelope shape.
/// What: `seq` is always `0` — this event is not part of any per-session
/// ring buffer (there is no owning session to sequence it against), unlike
/// every other envelope this daemon publishes. `session_id` is the empty
/// string for the same reason (see the event's own docs).
fn publish_activation_changed(new_active_id: Option<WorkstreamId>, prior_id: Option<WorkstreamId>) {
    let event = Event::WorkstreamActivationChanged {
        new_active_id: new_active_id.map(|id| id.to_string()),
        prior_id: prior_id.map(|id| id.to_string()),
    };
    crate::events::publish(SessionEventEnvelope::new(
        String::new(),
        0,
        Utc::now(),
        event,
    ));
}

/// Publish `Event::WorkstreamStateInferred` on the global event bus.
///
/// Why: shared by [`activate`]/[`deactivate`] (this file) and
/// `super::protocol::close` (the one place outside this module that
/// transitions a workstream's state) — see the module docs for why `close`
/// reaches into this helper rather than duplicating the envelope shape.
/// What: mirrors [`publish_activation_changed`]'s `seq`/`session_id`
/// convention (both are meaningless for a daemon-scoped event).
pub(super) fn publish_state_inferred(workstream_id: WorkstreamId, state: &str, reason: &str) {
    let event = Event::WorkstreamStateInferred {
        workstream_id: workstream_id.to_string(),
        state: state.to_string(),
        reason: reason.to_string(),
    };
    crate::events::publish(SessionEventEnvelope::new(
        String::new(),
        0,
        Utc::now(),
        event,
    ));
}

/// Shared, lock-wrapped handle to a project's workstream store.
///
/// Why: `WorkstreamStore`'s mutating methods take `&mut self` (#3293's
/// single-writer-per-process design — see its own docs), but the daemon's
/// JSON-RPC handlers are `Fn(...) -> Future` closures cloned once per method
/// registration (see `crate::session::protocol::register`'s
/// `Arc<SessionRegistry>` pattern) — they need shared, interior-mutable
/// ownership of the SAME store instance.
/// What: `Arc<tokio::sync::Mutex<WorkstreamStore>>`. `tokio::sync::Mutex`
/// (not `std::sync::Mutex`) because every store operation is `async` (file
/// I/O) and the lock must be held across `.await` points.
pub type SharedWorkstreamStore = Arc<Mutex<WorkstreamStore>>;

/// Typed failure surface for [`activate`]/[`deactivate`].
///
/// Why: callers (the `workstream.activate`/`workstream.deactivate` RPC
/// handlers, `crate::workstreams::protocol`) need to pattern-match the
/// distinct failure modes onto their own JSON-RPC error codes rather than
/// string-sniffing.
/// What: [`Self::NotFound`] — the target `id` names no existing workstream
/// (maps to `-32002 not_found`); [`Self::ActiveConflict`] — DOC-48 §6.1's
/// exclusivity gate (maps to `-32008 active_conflict`, carrying the
/// currently-active id); [`Self::Store`] — a lower-level I/O/serialization
/// failure (maps to `-32603 internal_error`).
/// Test: `activation_tests::activate_unknown_id_returns_not_found`,
/// `activation_tests::activate_without_force_returns_active_conflict`.
#[derive(Debug, Error)]
pub enum ActivationError {
    #[error("workstream not found: {0}")]
    NotFound(WorkstreamId),
    #[error("another workstream is active: {0}")]
    ActiveConflict(WorkstreamId),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The result of a successful [`activate`] call (DOC-48 §5.1's
/// `{active_id, prior_id?}` response shape).
///
/// Why: the RPC/REST layers need both ids to report the switch (and #3297's
/// `WorkstreamActivationChanged` event will need the same pair) — a bare
/// `()` would lose the "what changed" information the spec's response shape
/// requires.
/// What: `active_id` is always `id` (the just-activated workstream);
/// `prior_id` is `None` for a fresh activation or an idempotent
/// re-activation of the already-active workstream, `Some(...)` only when a
/// DIFFERENT workstream was actually deactivated (a force-switch).
/// Test: `activation_tests::*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActivateOutcome {
    pub active_id: WorkstreamId,
    pub prior_id: Option<WorkstreamId>,
}

/// Activate `id` under DOC-48 §6.1's exclusivity rules.
///
/// Why: the single entry point for both `workstream.activate` (RPC) and its
/// REST twin — see the module docs for the exact rule table.
/// What: rejects an unknown `id` with [`ActivationError::NotFound`] BEFORE
/// looking at the active pointer (activating a nonexistent workstream is
/// never valid, force or not). Otherwise: no workstream active -> activate
/// and persist, `prior_id: None`; `id` already active -> idempotent success,
/// `prior_id: None` (no pointer write); a DIFFERENT workstream active and
/// `force: false` -> `ActivationError::ActiveConflict`; a DIFFERENT
/// workstream active and `force: true` -> persist the switch, `prior_id:
/// Some(previous)`.
/// Test: `activation_tests::activate_with_no_prior_active_succeeds`,
/// `activation_tests::activate_already_active_is_idempotent`,
/// `activation_tests::activate_without_force_returns_active_conflict`,
/// `activation_tests::activate_with_force_switches_and_reports_prior`,
/// `activation_tests::activate_unknown_id_returns_not_found`,
/// `activation_tests::activate_with_no_prior_active_publishes_state_inferred`,
/// `activation_tests::activate_with_force_publishes_state_inferred_for_both`.
pub async fn activate(
    store: &SharedWorkstreamStore,
    id: WorkstreamId,
    force: bool,
) -> Result<ActivateOutcome, ActivationError> {
    let mut store = store.lock().await;

    store.get(id).await.map_err(|e| match e {
        StoreError::NotFound(_) => ActivationError::NotFound(id),
        other => ActivationError::Store(other),
    })?;

    match store.active_workstream_id().await? {
        Some(active) if active == id => Ok(ActivateOutcome {
            active_id: id,
            prior_id: None,
        }),
        Some(active) if !force => Err(ActivationError::ActiveConflict(active)),
        Some(active) => {
            store.set_active(Some(id)).await?;
            publish_activation_changed(Some(id), Some(active));
            publish_state_inferred(id, "active", "activated (force-switch)");
            publish_state_inferred(active, "idle", "deactivated (force-switch)");
            Ok(ActivateOutcome {
                active_id: id,
                prior_id: Some(active),
            })
        }
        None => {
            store.set_active(Some(id)).await?;
            publish_activation_changed(Some(id), None);
            publish_state_inferred(id, "active", "activated");
            Ok(ActivateOutcome {
                active_id: id,
                prior_id: None,
            })
        }
    }
}

/// Deactivate `id` under DOC-48 §4.3's idempotent rule.
///
/// Why: the single entry point for both `workstream.deactivate` (RPC) and
/// its REST twin.
/// What: clears the pointer (persisting `None`) and returns `Some(id)` ONLY
/// when `id` is the CURRENTLY active workstream; otherwise a no-op success
/// returning `None` — deactivating an idle, closed, or even nonexistent id
/// is idempotent by design (§4.3), so this never looks the id up in
/// `workstreams[]` at all.
/// Test: `activation_tests::deactivate_active_clears_pointer`,
/// `activation_tests::deactivate_non_active_is_idempotent_noop`,
/// `activation_tests::deactivate_active_publishes_state_inferred_idle`.
pub async fn deactivate(
    store: &SharedWorkstreamStore,
    id: WorkstreamId,
) -> Result<Option<WorkstreamId>, ActivationError> {
    let mut store = store.lock().await;
    if store.active_workstream_id().await? == Some(id) {
        store.set_active(None).await?;
        publish_activation_changed(None, Some(id));
        publish_state_inferred(id, "idle", "deactivated");
        Ok(Some(id))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
#[path = "activation_tests.rs"]
mod activation_tests;
