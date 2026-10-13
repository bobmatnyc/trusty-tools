//! Chat-channel enforcement in the daemon (#8433, #8454).
//!
//! Why: the route policy lives in trusty-channels; the daemon needs one
//! fail-closed view of it that every channel path reads.
//! What: [`crate::channels::snapshot`] — the policy snapshot;
//! [`crate::channels::readiness`] — the egress guard's inline load (#9741);
//! [`crate::channels::question_table`] — the durable question table (#8433 P2).
//! Test: `snapshot_tests.rs`, `bin/tm/commands/egress_guard_policy_tests.rs`,
//! `question_table_tests.rs`.

pub mod question_table;
pub mod readiness;
pub mod snapshot;
