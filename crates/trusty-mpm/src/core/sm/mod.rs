//! Session Manager (SM) agent module (DOC-14 spec).
//!
//! Why: the SM is a PM-like, daemon-side orchestrator that delegates all work by
//! spawning t-mpm sessions (spec §3 prime directive). This module is its home in
//! trusty-mpm-core. SM-1 establishes the skeleton — config schema + an inert
//! agent struct — that SM-2..SM-8 fill in (multi-provider inference, dedicated
//! memory palace, rolling auto-compaction context, goal tracking, and the
//! stdio/HTTP adapters).
//! What: a thin facade that re-exports the SM config types and the agent
//! skeleton from the `config` and `agent` submodules.
//! Test: submodule `tests` modules (`config::tests`, `agent::tests`) plus the
//! `MpmConfig`-level coverage in `core/config.rs::tests`.

pub mod agent;
pub mod config;

pub use agent::SessionManagerAgent;
pub use config::{SessionManagerConfig, SmInferenceConfig, SmMemoryConfig, SmRoundsConfig};
