//! Test-support doubles for the inference layer (issue #2402).
//!
//! Why: exercising the [`super::adapter::InferenceAdapter`] seam and the
//! configurator must not require real HTTP or credentials. These doubles are the
//! only [`super::adapter::InferenceAdapter`] implementation in this foundation
//! ticket; real adapters land in #2403/#2407 and register into the same
//! [`super::configurator::Configurator`] the [`ScriptedAdapter`] validates.
//! Shipped as public support (not `cfg(test)`) so downstream integration tests
//! reuse them, mirroring `embedder-test-support`.
//! What: [`ScriptedAdapter`] (a deterministic in-memory adapter — always
//! available with `inference-client`) and, behind the `axum-server` feature,
//! [`MockInferenceServer`] (a real loopback HTTP mock for the future concrete
//! adapters).
//! Test: each submodule's inline `tests`; cross-cutting use in
//! `crates/trusty-common/tests/inference_foundation.rs`.

mod scripted;

pub use scripted::ScriptedAdapter;

#[cfg(feature = "axum-server")]
mod http_mock;
#[cfg(feature = "axum-server")]
pub use http_mock::MockInferenceServer;
