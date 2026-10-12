//! Chat-channel enforcement in the daemon (#8433, #8454).
//!
//! Why: the route policy lives in trusty-channels; the daemon needs one
//! fail-closed view of it that every channel path reads.
//! What: [`crate::channels::snapshot`] — the policy snapshot;
//! [`crate::channels::readiness`] — the egress guard's inline load (#9741).
//! Test: `snapshot_tests.rs`, `bin/tm/commands/egress_guard_policy_tests.rs`.

pub mod readiness;
pub mod snapshot;
