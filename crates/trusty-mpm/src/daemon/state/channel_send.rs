//! The daemon's hold on the staged-send table (#10423, S1b).
//!
//! Why: the `/api/v1/channels/send` route, the `channel_send` MCP tool and
//! the owner's Approve must share one table per daemon, and a table that
//! cannot load must stop every send rather than start empty (acceptance 5).
//! What: [`SendTableHold`] is a slot loaded from `channels/staged_sends.json`
//! on first use; [`DaemonState::send_table`] hands the locked slot to the
//! caller. A load failure is left uncached, so the next call retries.
//! Test: `daemon/managed_routes/channel_send_tests.rs`.

use std::sync::{Mutex, MutexGuard};

use crate::channels::staged_send::StagedSends;

use super::DaemonState;

/// The slot holding the loaded table, if any.
#[derive(Debug, Default)]
pub(crate) struct SendTableHold {
    slot: Mutex<Option<StagedSends>>,
}

impl DaemonState {
    /// The locked staged-send slot; `None` inside until a load succeeds.
    ///
    /// What: `Err` when the mutex is poisoned (no send proceeds). Blocking:
    /// async callers use `spawn_blocking`.
    pub(crate) fn send_table(&self) -> Result<MutexGuard<'_, Option<StagedSends>>, String> {
        self.channels
            .sends
            .slot
            .lock()
            .map_err(|_| "the staged-send table lock is poisoned".to_owned())
    }
}
