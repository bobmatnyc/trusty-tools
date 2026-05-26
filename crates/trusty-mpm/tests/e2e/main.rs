//! End-to-end regression suite for `trusty-mpm`.
//!
//! Why: the per-module `#[cfg(test)]` unit tests drive handlers and core
//! functions directly. This suite instead exercises the daemon as a black box —
//! a real axum server on a loopback port, driven over HTTP with `reqwest` — so
//! routing, status codes, JSON shapes, and the framework-config boot path are
//! all verified the way a real client (CLI, TUI, Telegram bot) sees them.
//! What: a single integration-test binary. [`harness`] spawns a temp-scoped
//! daemon; each `test_*` module is one scenario area. Rust runs the `#[test]`
//! functions across these modules concurrently, and that is safe here because
//! every test spawns its own isolated daemon and temp directory.
//! Test: `cargo test -p trusty-mpm-daemon --test e2e`.

mod harness;

mod test_agent_deploy;
mod test_events;
mod test_health;
mod test_instruction_pipeline;
mod test_optimizer;
mod test_overseer;
mod test_projects;
mod test_sessions;
