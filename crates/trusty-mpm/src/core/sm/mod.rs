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
/// The session-control seam the SM drives its fleet through (SM-8, DOC-14 §2.6).
///
/// Why: both the SM-8 delegation loop and the SM-STDIO adapter must drive managed
/// sessions through one narrow, mockable trait. Relocating it here (from
/// `daemon::sm_stdio`) lets the agent-side loop depend on it while `sm_stdio`
/// re-exports it unchanged.
/// What: re-compiled in every build (pure trait + serde value shapes).
/// Test: `agent::delegate` tests + `daemon::sm_stdio::tests`.
pub mod control;
/// Goal-tracking model + dual (palace + cache) persistence (SM-6, DOC-14 §9).
///
/// Why: the SM tracks operator intent as durable goals fanned out to delegated
/// sessions, with progress and a BLOCKING verification gate (§3.5) derived from
/// per-session verification state. Goals persist with a dual design (§9.4): the
/// SM palace (SM-4) is the source of truth, `goals.json` is a hot cache rebuilt
/// from it on startup. The module is feature-INDEPENDENT (always compiled): the
/// store depends on the `GoalMemory` abstraction, and only the production
/// `SmMemory` impl of that trait is gated behind `sm-memory` (internally). SM-6 is
/// the store only — not yet wired into any endpoint/loop (SM-7 / SM-8).
/// What: re-compiled in every build (pure logic + serde + uuid/chrono).
/// Test: `goals::{model_tests, cache_tests, store_tests}`.
pub mod goals;
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
/// Idle-session prune policy (issue #1313).
///
/// Why: the `tm sessions prune-idle` command needs a pure, testable mapping from
/// a session's latest activity-monitor verdict to a durability-respecting
/// teardown action (stop idle, decommission done, skip everything else). Keeping
/// it in core (not the binary) lets it be unit-tested without a live daemon and
/// reused by any future caller.
/// What: re-compiled in every build (pure logic, no extra deps).
/// Test: `prune::tests`.
pub mod prune;

pub use agent::{
    SessionManagerAgent, SmAgentError, SmChatActionOutcome, SmChatOutcome, SmHealth, SmModelTiers,
};
pub use config::{SessionManagerConfig, SmInferenceConfig, SmMemoryConfig, SmRoundsConfig};
pub use context::{
    ConversationStore, ConversationStoreError, Round, SmContextEngine, SmContextError,
    SmConversation, ToolTrace,
};
pub use control::{
    LaunchParams, RawObservation, SessionControl, SessionControlError, Submit, Summary,
};
pub use goals::{
    GOAL_TAG, Goal, GoalCache, GoalMemory, GoalStatus, SessionLink, SessionTaskState,
    SessionUpdate, SmGoalError, SmGoalResult, SmGoalStore,
};
#[cfg(feature = "sm-memory")]
pub use memory::{SmMemory, SmMemoryError, SmMemoryResult};
pub use prompt::{
    FILE_SM_INSTRUCTIONS, FILE_SM_TOOLS, FILE_SM_WORKFLOW, SM_OVERRIDE_SUBDIR, assemble_sm_prompt,
    resolve_sm_prompt, resolve_sm_prompt_default, sm_override_dir,
};
pub use providers::{
    AnthropicProvider, LlmProvider, LlmRequest, LlmResponse, OpenRouterProvider, ProviderKind,
    ProviderRegistry, ResolvedCall, SmLlmError, SmModelTier, TierResolver,
    resolve_provider_and_model, resolve_tier_model,
};
pub use prune::{PruneAction, decide, normalize_verdict};
