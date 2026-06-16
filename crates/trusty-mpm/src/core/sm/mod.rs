//! Session Manager (SM) agent module (DOC-14 spec).
//!
//! Why: the SM is a PM-like, daemon-side orchestrator that delegates all work by
//! spawning t-mpm sessions (spec §3 prime directive). This module is its home in
//! trusty-mpm-core. SM-1 establishes the skeleton — config schema + an inert
//! agent struct — that SM-2..SM-8 fill in (multi-provider inference, dedicated
//! memory palace, rolling auto-compaction context, goal tracking, and the
//! stdio/HTTP adapters).
//! What: a thin facade that re-exports the SM config types, the agent
//! skeleton, (SM-2) the multi-provider inference abstraction, and (SM-3) the
//! system-prompt assembly + override layering from the `config`, `agent`,
//! `providers`, and `prompt` submodules.
//! Test: submodule `tests` modules (`config::tests`, `agent::tests`,
//! `providers::*::tests`, `prompt::tests`) plus the `MpmConfig`-level coverage
//! in `core/config.rs::tests`.

pub mod agent;
pub mod config;
/// Rolling auto-compaction context engine (SM-5, DOC-14 §7).
///
/// Why: gives the SM effectively infinite conversation context — last-N rounds
/// verbatim plus a growing faithful compressed summary — with a precise §7.5
/// working-prompt assembly. The compaction LLM call is dependency-injected
/// through the [`providers::LlmProvider`] trait so it is unit-testable with a
/// mock; SM-5 is the engine only and is not yet wired into any endpoint/loop.
/// What: re-compiled in every build (no extra feature cost — pure logic + serde
/// + the SM-2 provider trait).
/// Test: `context::*::tests`.
pub mod context;
/// Dedicated SM memory palace + recall/remember wiring (SM-4, DOC-14 §8).
///
/// Why: gated behind the `sm-memory` feature because it turns on
/// `trusty-common/memory-core` — the heavy Memory Palace storage engine
/// (usearch HNSW, redb, bundled-ORT FastEmbedder). Default and
/// `--no-default-features` builds must not pay that cost, so the module (and its
/// dependency) are strictly opt-in.
/// What: re-compiled only under `--features sm-memory`.
/// Test: `memory::tests` (run with `--features sm-memory`).
#[cfg(feature = "sm-memory")]
pub mod memory;
pub mod prompt;
pub mod providers;

pub use agent::SessionManagerAgent;
pub use config::{SessionManagerConfig, SmInferenceConfig, SmMemoryConfig, SmRoundsConfig};
pub use context::{
    ConversationStore, ConversationStoreError, Round, SmContextEngine, SmContextError,
    SmConversation, ToolTrace,
};
#[cfg(feature = "sm-memory")]
pub use memory::{SmMemory, SmMemoryError, SmMemoryResult};
pub use prompt::{
    FILE_SM_INSTRUCTIONS, FILE_SM_TOOLS, FILE_SM_WORKFLOW, SM_OVERRIDE_SUBDIR, assemble_sm_prompt,
    resolve_sm_prompt, resolve_sm_prompt_default, sm_override_dir,
};
pub use providers::{
    AnthropicProvider, LlmProvider, LlmRequest, LlmResponse, OpenRouterProvider, ProviderKind,
    ProviderRegistry, ResolvedCall, SmLlmError, SmModelTier, resolve_provider_and_model,
    resolve_tier_model,
};
