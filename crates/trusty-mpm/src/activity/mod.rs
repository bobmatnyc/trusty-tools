//! Activity monitoring subsystem.
//!
//! Why: the operator dashboard and circuit-breaker logic need a real-time view
//! of what each managed session is doing (working, idle, blocked, errored) at
//! a price that scales with session count. This module provides that view with
//! content-hash caching to minimise LLM token spend.
//! What: re-exports the public types from `cache`, `monitor`, and `classifier`
//! so callers import from one stable path. #4427 split the production
//! inference-adapter-backed classifier out of `monitor` into its own module;
//! [`OpenRouterClassifier`] is re-exported here so that move is invisible to
//! callers importing from `crate::activity`.
//! Test: each sub-module carries its own unit tests; the integration path is
//! exercised by the monitor tests against a `MockClassifier`.
//!
//! [`OpenRouterClassifier`]: crate::control::OpenRouterClassifier

pub mod cache;
pub mod classifier;
pub mod monitor;

pub use cache::{ActivityCache, ActivityState, ActivityVerdict, CheckMetrics, CostTally};
pub use classifier::OpenRouterClassifier;
pub use monitor::{ActivityCheckResult, ActivityError, ActivityMonitor, LlmClassifier};
