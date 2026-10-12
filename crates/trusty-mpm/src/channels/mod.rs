//! Chat-channel enforcement in the daemon (#8433, #8454).
//!
//! Why: the route policy lives in trusty-channels; the daemon needs one
//! fail-closed view of it that every channel path reads.
//! What: [`snapshot`] — the policy snapshot.
//! Test: `snapshot_tests.rs`.

pub mod snapshot;
