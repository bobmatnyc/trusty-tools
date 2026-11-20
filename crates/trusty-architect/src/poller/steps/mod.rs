//! Poller steps ported from the prototype's features that no #9171 phase
//! owns, one module per feature (#9801).
//!
//! Why: `fleet-poll.py` cannot be removed until each of its by-path modules
//! is ported or ruled out; keeping each step in its own module keeps
//! `cycle.rs` to one call per step.
//! What: slice 1 ports [`usage`] (sampling and `usage` alerts, run by the
//! cycle) and [`notify_macos`] (the notification for `architect_*` alerts,
//! run after the alerts are written). df-T7i P1b adds [`scripts`]; #11540
//! adds [`ledger_shim`].
//! Test: see each module.

// #11540: the ledger-bound steps, a Python shim run each cycle.
pub mod ledger_shim;
pub mod notify_macos;
// df-T7i P1b: the prototype's run_* scripts, started on its cadence.
pub mod scripts;
pub mod usage;
