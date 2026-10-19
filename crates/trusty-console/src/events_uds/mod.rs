//! Console's client to the `trusty-events` bus (#9076, ADR-0065).
//!
//! Why: the bus runs as its own daemon (ADR-0065 option A), and console has
//! no bus of its own since #9075. Console reads events the way every reader
//! does, as an `events.subscribe` client on `events_socket_path()`, over UDS
//! only (ADR-0032). The bus being down must not stop console (ruling 2a,
//! condition 4), and must not look like a quiet bus either (DOC-73 §8.5).
//! What:
//! - [`client`]: the reconnecting subscription. It resumes with `since_seq`
//!   and `since_id`, resubscribes from 0 and drops duplicates by `EventId`
//!   when a resume is refused, passes gaps on, backs off between attempts,
//!   and does not time out a quiet subscription.
//! - [`token`]: the `<seq>:<uuid>` resume token an SSE `id:` carries.
//! - [`relay`]: the status the client publishes, its route, and the
//!   in-process fan-out the event routes (#6850, #6851) will read.
//!
//! [`start`] is the one call `run_serve` makes.
//! Test: `tests` (stub daemon, every error arm) and
//! `tests/events_uds_live.rs` (the real `trusty-events` binary).

pub mod client;
mod decode;
pub mod relay;
pub mod token;

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;

pub use client::{ClientConfig, Start, spawn};
pub use relay::{BusState, BusStatus, EventsRelay, FailureKind, RelayItem, status_handler};
pub use token::{ResumeToken, ResumeTokenError};

/// Start console's bus client in the background.
///
/// Why: `run_serve` must not fail on a bus problem; this keeps its part to
/// one call.
/// What: resolves `events_socket_path()` and hands it to [`start_with`].
/// Test: `tests::an_unresolvable_socket_is_reported_unavailable`.
pub fn start(relay: Arc<EventsRelay>) -> Option<tokio::task::JoinHandle<()>> {
    start_with(trusty_common::control_bus::events_socket_path(), relay)
}

/// [`start`] on an already-resolved socket path.
///
/// What: spawns [`client::run`] on the path, live from now. A path that
/// could not be resolved leaves the relay [`BusState::Unavailable`] with the
/// reason, and nothing is spawned.
/// Test: `tests::an_unresolvable_socket_is_reported_unavailable`.
pub(crate) fn start_with(
    socket: anyhow::Result<PathBuf>,
    relay: Arc<EventsRelay>,
) -> Option<tokio::task::JoinHandle<()>> {
    match socket {
        Ok(socket) => Some(spawn(ClientConfig::new(socket), relay)),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "event bus client disabled: cannot resolve the trusty-events socket");
            relay.note_unavailable();
            None
        }
    }
}
