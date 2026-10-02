//! End-to-end regression suite for `trusty-mpm`.
//!
//! Why: the per-module `#[cfg(test)]` unit tests drive handlers and core
//! functions directly. This suite instead exercises the daemon as a black box —
//! a real axum server on a loopback port, driven over HTTP with `reqwest` — so
//! routing, status codes, JSON shapes, and the framework-config boot path are
//! all verified the way a real client (CLI, TUI, Telegram bot) sees them.
//! What: the `e2e` module of the parallel `integration` target (#8345).
//! [`harness`] spawns a temp-scoped daemon; each `test_*` module is one
//! scenario area. libtest runs these tests concurrently with every other
//! `integration` test, which is safe because each spawns its own isolated
//! daemon and temp directory and none mutates the process environment.
//! Test: `cargo test -p trusty-mpm --test integration e2e::`.

mod harness;

mod test_agent_deploy;
mod test_events;
mod test_health;
mod test_instruction_pipeline;
mod test_optimizer;
mod test_overseer;
mod test_projects;
mod test_sessions;
