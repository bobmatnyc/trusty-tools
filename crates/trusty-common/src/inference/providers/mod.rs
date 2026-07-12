//! Concrete inference provider adapters (issue #2403, epic #2400 Wave 1).
//!
//! Why: the #2402 foundation shipped the [`InferenceAdapter`] trait, the
//! capability registry, and the [`crate::inference::Configurator`] seam but left
//! the construction step to a test-only [`crate::inference::test_support::ScriptedAdapter`].
//! This module realises the seam: a shared OpenAI-compatible HTTP core plus one
//! thin config per OpenAI-dialect provider, and the [`register_default_factories`]
//! entry point that wires them into the configurator so
//! `provider_for("openrouter" | "fireworks" | "openai", &store)` builds a REAL
//! `Box<dyn InferenceAdapter>`.
//! What: [`openai_compat::OpenAiCompatAdapter`] (the core) and the
//! [`openrouter`]/[`fireworks`]/[`openai`] provider configs, plus
//! [`register_default_factories`]. Anthropic-dialect providers (Bedrock #2407,
//! Anthropic-direct #2408) are out of scope here.
//! Test: each submodule's inline `tests` + the offline mock-server round-trip in
//! `crates/trusty-common/tests/inference_adapters.rs`.

pub mod fireworks;
pub mod openai;
pub mod openai_compat;
pub mod openrouter;

pub use openai_compat::{OpenAiCompatAdapter, OpenAiCompatConfig};

use crate::inference::configurator::Configurator;
use crate::inference::registry::ProviderId;

/// Register the built-in OpenAI-dialect provider factories into `cfg`.
///
/// Why: consumers want one call that makes the configurator able to build every
/// adapter this ticket ships, rather than remembering each `register` line. The
/// configurator stays empty by default (no implicit adapters) — this is the
/// explicit opt-in that turns resolution into live adapters.
/// What: registers the OpenRouter, Fireworks, and OpenAI production factories
/// (each pointed at its real base URL) under their [`ProviderId`]. Bedrock and
/// Anthropic-direct are intentionally NOT registered here (later waves).
/// Test: `crates/trusty-common/tests/inference_adapters.rs::default_factories_register_three`.
pub fn register_default_factories(cfg: &mut Configurator) {
    cfg.register(ProviderId::OpenRouter, Box::new(openrouter::factory));
    cfg.register(ProviderId::Fireworks, Box::new(fireworks::factory));
    cfg.register(ProviderId::OpenAI, Box::new(openai::factory));
}
