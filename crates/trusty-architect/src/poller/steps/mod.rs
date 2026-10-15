//! Poller steps ported from the prototype's features that no #9171 phase
//! owns, one module per feature (#9801).
//!
//! Why: `fleet-poll.py` cannot be removed until each of its by-path modules
//! is ported or ruled out; keeping each step in its own module keeps
//! `cycle.rs` to one call per step.
//! What: slice 1 ports [`usage`] (sampling and `usage` alerts, run by the
//! cycle) and [`notify_macos`] (the notification for `architect_*` alerts,
//! run after the alerts are written).
//! Test: see each module.

pub mod notify_macos;
pub mod usage;
