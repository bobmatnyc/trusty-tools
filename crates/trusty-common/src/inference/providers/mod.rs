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
//! [`openrouter`]/[`fireworks`]/[`openai`]/[`together`] provider configs, the
//! native [`anthropic`] adapter (its own Messages-API wire format, #2408), plus
//! [`register_default_factories`]. Bedrock (#2407) is still out of scope here.
//! Test: each submodule's inline `tests` + the offline mock-server round-trip in
//! `crates/trusty-common/tests/inference_adapters.rs`.

pub mod anthropic;
pub mod atlascloud;
pub mod fireworks;
pub mod openai;
pub mod openai_compat;
pub mod openrouter;
pub mod together;

pub use openai_compat::{OpenAiCompatAdapter, OpenAiCompatConfig};

use crate::inference::configurator::Configurator;
use crate::inference::registry::ProviderId;

/// Register the built-in OpenAI-dialect provider factories into `cfg`.
///
/// Why: consumers want one call that makes the configurator able to build every
/// adapter this ticket ships, rather than remembering each `register` line. The
/// configurator stays empty by default (no implicit adapters) — this is the
/// explicit opt-in that turns resolution into live adapters.
/// What: registers the OpenRouter, Fireworks, OpenAI, Together (#2488),
/// AtlasCloud (#2536), and Anthropic-direct (#2408) production factories (each
/// pointed at its real base URL) under their [`ProviderId`]. Bedrock is
/// intentionally NOT registered here (later wave, #2407).
/// Test: `crates/trusty-common/tests/inference_adapters.rs::default_factories_register_openai_dialect`
/// and `default_factories_register_anthropic_direct`.
pub fn register_default_factories(cfg: &mut Configurator) {
    cfg.register(ProviderId::OpenRouter, Box::new(openrouter::factory));
    cfg.register(ProviderId::Fireworks, Box::new(fireworks::factory));
    cfg.register(ProviderId::OpenAI, Box::new(openai::factory));
    cfg.register(ProviderId::Together, Box::new(together::factory));
    cfg.register(ProviderId::AtlasCloud, Box::new(atlascloud::factory));
    cfg.register(ProviderId::Anthropic, Box::new(anthropic::factory));
}
