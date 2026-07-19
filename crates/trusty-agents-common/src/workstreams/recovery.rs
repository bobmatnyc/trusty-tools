//! [`LedgerRecovery`] — the ledger-recovery seam DOC-44 §8 Phase 2 calls
//! "test registry recovery (load from trusty-memory after restart)" (issue
//! #3008, twin Phase 2).
//!
//! Why: DOC-44 §4.2 describes the registry as "trusty-memory-backed, or a
//! heterogeneous ledger in trusty-agents-common" and §8 Phase 2 lists
//! "recover from trusty-memory on restart" as a deliverable. Wiring a real
//! trusty-memory HTTP round trip needs `trusty-agents`' memory client, which
//! is being reworked in a parallel, in-flight PR — depending on it here
//! would couple this ticket's completion to that unrelated work, and issue
//! #3228 (shared-palace rework) is the tracked follow-up for the query
//! surface a trusty-memory-backed recovery source actually needs (durable
//! cross-session workstream state, not just the per-run chat memory the
//! current client exposes). Rather than block Phase 2 on that stack, this
//! module defines the recovery seam as a trait NOW so a future
//! trusty-memory-backed implementation can be dropped in without changing
//! any caller's code — and ships the working default: the JSON ledger file
//! written by [`super::WorkstreamLedger`] already IS the durable,
//! restart-surviving state DOC-44 asks for, so "recovery" for the default
//! backend is just "read the file", which `WorkstreamLedger::list` already
//! provides.
//! What: [`LedgerRecovery`] — one method, `recover()`. [`JsonFileRecovery`]
//! (the working default) delegates to a wrapped [`super::WorkstreamLedger`].
//! [`TrustyMemoryRecovery`] is a documented stub: it constructs (no live
//! connection held) but `recover()` always returns
//! [`super::LedgerError::NotSupported`] referencing issue #3228 until that
//! work lands.
//! Test: `recovery::tests` covers `JsonFileRecovery` round-tripping a
//! populated ledger through a fresh process (a new `WorkstreamLedger` handle
//! over the same path) and `TrustyMemoryRecovery::recover`'s documented
//! `NotSupported` error.

use std::sync::Arc;

use super::error::LedgerError;
use super::ledger::WorkstreamLedger;
use super::types::Workstream;

/// Seam for recovering the workstream ledger's last-known state, e.g. at
/// process startup.
///
/// Why/What: see module docs.
pub trait LedgerRecovery: Send + Sync {
    /// Return every workstream this backend can recover.
    ///
    /// Why: called once at startup (by a future lead agent, DOC-44 Layer 2)
    /// to rebuild in-memory state before resuming supervision.
    /// What: `Ok(vec![])` is a valid "nothing to recover yet" result — an
    /// empty ledger is not an error. [`super::LedgerError::NotSupported`]
    /// means this backend has no working implementation at all (see
    /// [`TrustyMemoryRecovery`]).
    fn recover(&self) -> Result<Vec<Workstream>, LedgerError>;
}

/// Default recovery source: the JSON ledger file itself.
///
/// Why: the ledger file already survives process restarts (it's on disk);
/// no separate recovery mechanism is needed for the working default.
/// What: thin wrapper delegating to `WorkstreamLedger::list`.
/// Test: `recovery::tests::json_file_recovery_reads_persisted_workstreams`.
pub struct JsonFileRecovery {
    ledger: Arc<WorkstreamLedger>,
}

impl JsonFileRecovery {
    /// Wrap an existing ledger handle as a recovery source.
    pub fn new(ledger: Arc<WorkstreamLedger>) -> Self {
        Self { ledger }
    }
}

impl LedgerRecovery for JsonFileRecovery {
    fn recover(&self) -> Result<Vec<Workstream>, LedgerError> {
        self.ledger.list()
    }
}

/// Documented stub for a future trusty-memory-backed recovery source.
///
/// Why: see module docs — blocked on issue #3228 (shared-palace rework),
/// not implemented in this ticket. Exists now so the seam's shape (and the
/// fact that the lead agent should code against [`LedgerRecovery`], not
/// concretely against [`JsonFileRecovery`]) is settled before Phase 5 (Lead
/// Agent Core) needs it.
/// What: constructing this type is always safe (it holds no connection);
/// `recover()` always returns [`super::LedgerError::NotSupported`].
/// Test: `recovery::tests::trusty_memory_recovery_is_not_supported_yet`.
#[derive(Debug, Default)]
pub struct TrustyMemoryRecovery;

impl LedgerRecovery for TrustyMemoryRecovery {
    fn recover(&self) -> Result<Vec<Workstream>, LedgerError> {
        Err(LedgerError::NotSupported(
            "trusty-memory-backed ledger recovery is not implemented; blocked on issue #3228 \
             (shared-palace rework) — use JsonFileRecovery (the default) until that work lands"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::workstreams::{Harness, NewWorkstream, Priority};

    #[test]
    fn json_file_recovery_reads_persisted_workstreams() {
        let tmp = tempdir().unwrap();
        let ledger = Arc::new(WorkstreamLedger::open(tmp.path()).unwrap());
        ledger
            .create(NewWorkstream {
                ticket_ref: "#3008".into(),
                title: "ledger".into(),
                assigned_harness: Harness::Tcode,
                priority: Priority::P0,
                target_date: None,
            })
            .unwrap();

        // Simulate "restart" with a fresh ledger handle over the same path.
        let reopened = Arc::new(WorkstreamLedger::open(tmp.path()).unwrap());
        let recovery = JsonFileRecovery::new(reopened);
        let recovered = recovery.recover().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].ticket_ref, "#3008");
    }

    #[test]
    fn trusty_memory_recovery_is_not_supported_yet() {
        let recovery = TrustyMemoryRecovery;
        let err = recovery.recover().unwrap_err();
        assert!(matches!(err, LedgerError::NotSupported(msg) if msg.contains("#3228")));
    }
}
