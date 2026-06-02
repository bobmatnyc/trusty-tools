//! Persistence and concurrency-guard layer for trusty-review (issue #582).
//!
//! Why: live posting needs two coordination mechanisms beyond the review
//! pipeline itself — a durable cross-process dedup claim store (so retries and
//! restarts do not re-review the same head SHA) and an in-process in-flight
//! guard (so concurrent webhook deliveries for the same PR do not race).
//! Grouping them under one module keeps the storage concerns out of the
//! pipeline modules.
//!
//! What: re-exports the `dedup` SHA-keyed claim store and the `in_flight`
//! RAII guard registry.
//!
//! Test: each submodule carries its own unit tests.

pub mod dedup;
pub mod in_flight;

pub use dedup::{ClaimOutcome, DedupError, DedupStore};
pub use in_flight::{InFlightGuard, InFlightRegistry};
