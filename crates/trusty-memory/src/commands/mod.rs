//! Per-subcommand handlers for the `trusty-memory` binary.
//!
//! Why: Keep CLI handlers out of `lib.rs` so the library surface stays focused
//! on MCP server code while the binary stays a thin clap-to-handler shim.
//! What: One submodule per subcommand. `serve` is wired directly to
//! `crate::run_stdio` / `crate::run_http` in `main.rs`; `migrate` rewrites
//! Claude settings to point at trusty-memory; `service` manages the macOS
//! launchd LaunchAgent; `setup` orchestrates first-time install (data dir +
//! launchd + Claude settings patch).
//! Test: Each submodule carries its own unit tests.

pub mod doctor;
pub mod inbox_check;
pub mod migrate;
pub mod monitor;
pub mod prompt_context;
pub mod send_message;
pub mod service;
pub mod setup;
pub mod start;
pub mod stop;
