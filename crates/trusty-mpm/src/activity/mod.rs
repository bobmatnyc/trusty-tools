//! Activity monitoring subsystem.
//!
//! Why: the operator dashboard and circuit-breaker logic need a real-time view
//! of what each managed session is doing (working, idle, blocked, errored) at
//! a price that scales with session count. This module provides that view with
//! content-hash caching to minimise LLM token spend.
//! What: re-exports the public types from `cache` and `monitor` so callers
//! import from one stable path.
//! Test: each sub-module carries its own unit tests; the integration path is
//! exercised by the monitor tests against a `MockClassifier`.

pub mod cache;
pub mod monitor;

pub use cache::{ActivityCache, ActivityState, ActivityVerdict, CheckMetrics, CostTally};
pub use monitor::{
    ActivityCheckResult, ActivityError, ActivityMonitor, LlmClassifier, OpenRouterClassifier,
};
