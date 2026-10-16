//! The index-count gauge (issue #41 Phase 1).
//!
//! Why: emit sites across the crate use the `metrics` facade. #9535 deleted
//! `GET /metrics`, the request middleware and the Prometheus recorder that
//! route rendered; nothing read the recorder after #9214 removed the TCP
//! listener. With no recorder installed, every facade call is a no-op.
//!
//! What: [`set_index_count`], called after each registry mutation.
//!
//! Test: none — the gauge has no observable effect without a recorder.

/// Set the static `trusty_index_count` gauge. Call after each index registry
/// mutation (register / unregister) so the gauge reflects the current value.
///
/// Why: a periodic ticker would be acceptable, but having handlers call this
/// directly means dashboards stay in sync within microseconds of a mutation
/// rather than seconds.
/// What: writes the count to the named gauge.
/// Test: indirect — exercised by `create_index` / `delete_index` flows.
pub fn set_index_count(count: usize) {
    metrics::gauge!("trusty_index_count").set(count as f64);
}
