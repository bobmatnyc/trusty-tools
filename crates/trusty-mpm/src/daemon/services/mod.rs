//! Daemon domain services.
//!
//! Why: the HTTP handlers in `api.rs` had grown to embed session lifecycle
//! rules, the hook overseer pipeline, tmux I/O policy, and bot pairing logic
//! directly in the request functions, leaving a 2300-line file where business
//! logic and transport were inseparable. Extracting that logic into focused,
//! HTTP-agnostic services makes each rule unit-testable without spinning up an
//! axum request, and shrinks every handler to deserialize-call-serialize.
//! What: this module groups the four domain services and re-exports their
//! public types so call sites use `crate::daemon::services::SessionService` etc.
//! Test: each submodule carries its own `#[cfg(test)]` suite; run them with
//! `cargo test -p trusty-mpm-daemon services`.

pub mod hook_service;
pub mod pairing_service;
pub mod session_service;
pub mod tmux_service;

pub use hook_service::{HookDecision, HookService};
pub use pairing_service::{PairCode, PairStatus, PairingService};
pub use session_service::{PauseResult, ReapResult, SessionService};
pub use tmux_service::TmuxService;
