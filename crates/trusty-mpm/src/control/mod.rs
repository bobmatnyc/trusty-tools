//! SESSCTL alpha-1 session control plane.
//!
//! Why: epic #1590 unifies the two divergent session paths (project sessions
//! and managed sessions) behind a single actor+registry system so every session
//! type shares one lifecycle, one ID convention, and one event model. This
//! module is the Phase 1 foundation: the runtime-adapter trait, two concrete
//! backends, the actor skeleton, and the registry. WI-3 adds the §8.2
//! regex/NLP parse layer via the `activity` submodule.
//! What: re-exports the public API from focused sub-modules:
//!   - `activity`   — §8.2 regex/NLP parse layer (`ActivityParser`, `ParseResult`)
//!   - `admission`  — §9.2 concurrency-cap admission decision (WI-5)
//!   - `classifier` — §8.3 optional OpenRouter classifier gate (WI-5)
//!   - `config`     — §9 auth + cost `ControlPlaneConfig` (WI-5)
//!   - `id`         — `ControlSessionId` newtype + `SessionCounter` allocator
//!   - `event`      — `SessionEvent` enum + supporting types
//!   - `state`      — `SessionState` machine + `SessionMetadata` snapshot
//!   - `backend`    — `SessionBackend` trait + `StreamJsonBackend` + `TmuxBackend`
//!   - `actor`      — `SessionActor` task + `SessionActorHandle` + `spawn_actor`
//!   - `registry`   — `SessionRegistry` + `RunParams`
//! Test: each submodule carries inline unit tests; run with
//! `cargo test -p trusty-mpm control::` to scope to this module. The WI-5
//! no-key-leak gating integration test lives in `tests/auth_cost.rs`.

pub mod activity;
pub mod actor;
pub mod admission;
pub mod backend;
pub mod classifier;
pub mod config;
pub mod event;
pub mod id;
pub mod registry;
pub mod state;

// Convenience re-exports so consumers can import from `crate::control::*`.
pub use actor::{
    ActorCommand, DEFAULT_BROADCAST_CAPACITY, SessionActorConfig, SessionActorHandle, spawn_actor,
};
pub use admission::{AdmissionDecision, AdmissionError, check_admission};
pub use backend::{SessionBackend, SessionInput};
pub use classifier::{ActivityClassifier, ClassifierGate, OpenRouterClassifier};
pub use config::{ControlPlaneConfig, ObservabilityConfig};
pub use event::{ActivityKind, BackendKind, SessionEvent, StopReason, StreamJsonEvent};
pub use id::{ControlSessionId, SessionCounter};
pub use registry::{RunParams, SessionRegistry};
pub use state::{SessionMetadata, SessionState};
