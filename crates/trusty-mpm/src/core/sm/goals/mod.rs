//! Goal-tracking model + dual persistence for the Session Manager (DOC-14 §9).
//!
//! Why: the SM tracks operator intent as durable [`Goal`]s, each fanned out to
//! one-or-many delegated t-mpm sessions, with progress and a BLOCKING
//! verification gate (§3.5) DERIVED from per-session verification state. Goals are
//! the central SM artifact and must outlive daemon restarts, so they use a DUAL
//! persistence design (§9.4): the dedicated SM palace (SM-4) is the source of
//! truth, and `~/.trusty-mpm/sm/goals.json` is a hot cache rebuilt from the palace
//! on startup. SM-6 delivers the model + store in ISOLATION; it deliberately does
//! NOT wire into any endpoint or the agent loop (those are SM-7 / SM-8). The
//! palace seam and the data root are injectable, so the whole feature is
//! unit-testable with a mock palace + a tempdir (no ONNX).
//! What: a thin facade re-exporting the four concerns — [`model`] (§9.1 data
//! types + derived progress + the gate predicate), [`error`] (the typed errors),
//! [`memory`] (the [`GoalMemory`] palace seam + the `SmMemory` impl, gated), and
//! [`cache`] (the atomic `goals.json` store) — plus [`store::SmGoalStore`], the
//! lifecycle + dual-persistence owner.
//! Test: each submodule carries its own `*_tests.rs`; together they cover id
//! stability, progress recompute, the verification gate, palace→cache rebuild
//! equality, and palace-unavailable fallback.

pub mod cache;
pub mod error;
pub mod memory;
pub mod model;
pub mod store;

pub use cache::{GOALS_CACHE_FILE, GoalCache};
pub use error::{SmGoalError, SmGoalResult};
pub use memory::{GOAL_TAG, GoalMemory};
pub use model::{Goal, GoalStatus, SessionLink, SessionTaskState};
pub use store::{SessionUpdate, SmGoalStore};

#[cfg(test)]
#[path = "model_tests.rs"]
mod model_tests;

#[cfg(test)]
#[path = "cache_tests.rs"]
mod cache_tests;

#[cfg(test)]
#[path = "store_tests.rs"]
mod store_tests;
