//! `CodeEngine` — the `trusty-tui` engine adapter for `tcode tui` (issue
//! #3415, DOC-50 §3.3/§3.4, epic #3411 Slice 3).
//!
//! Why: see `crate::tui_client`'s doc comment on the `pub mod tui_client;`
//! declaration in `lib.rs` for the full rationale (ephemeral `--stdio` CLI
//! client vs. long-lived `--http` TUI client).
//! What: [`discovery`] (daemon lookup), [`rpc`] (pooled `POST /rpc` client),
//! [`sse`] (SSE line pump), [`error::EngineError`] (this module's unified
//! error type), and [`engine::CodeEngine`] (the `trusty_tui::TuiEngine`
//! impl itself — the module most callers want).
//! Test: see each submodule's own docs; the full engine flow against a mock
//! HTTP daemon lives in `tests/tui_client_engine.rs`.

pub mod discovery;
pub mod engine;
mod engine_state;
pub mod error;
pub mod rpc;
mod session_events;
pub mod sse;
mod workstream_subscription;

pub use engine::CodeEngine;
pub use error::EngineError;
