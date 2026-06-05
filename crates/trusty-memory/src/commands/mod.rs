//! Per-subcommand handlers for the `trusty-memory` binary.
//!
//! Why: Keep CLI handlers out of `lib.rs` so the library surface stays focused
//! on MCP server code while the binary stays a thin clap-to-handler shim.
//! What: One submodule per subcommand. `serve` is wired directly to
//! `crate::run_http` / `crate::run_http_dynamic` in `main.rs` (the legacy
//! `crate::run_stdio` entry point was removed in issue #150 — Claude Code
//! now talks to the daemon via the `trusty-memory-mcp-bridge` stdio-to-UDS
//! pipe from PR #149); `migrate` rewrites Claude settings to point at
//! trusty-memory; `service` manages the macOS launchd LaunchAgent; `setup`
//! orchestrates first-time install (data dir + launchd + Claude settings
//! patch).
//! Test: Each submodule carries its own unit tests.

pub mod daemon_guard;
pub mod daemon_lock;
pub mod doctor;
pub mod inbox_check;
pub mod kg_rebuild;
pub mod kuzu_migrate;
pub mod link;
pub mod migrate;
pub mod migrations;
pub mod monitor;
pub mod note;
pub mod port;
pub mod prompt_context;
pub mod send_message;
pub mod service;
pub mod setup;
pub mod single_instance;
pub mod start;
pub mod stop;
pub mod upgrade;

/// Process-wide lock for tests that mutate `TRUSTY_DATA_DIR_OVERRIDE` and
/// related env vars.
///
/// Why: Rust's default test runner executes tests in the same process with
/// thread parallelism, but `std::env::set_var` / `remove_var` mutate
/// process-wide state. Tests that pin the data dir override to a tempdir
/// can otherwise observe each other's writes mid-run, with the symptom that
/// the handler resolves the *real* user data dir and the test asserts the
/// wrong directory. Acquiring this lock at the top of each env-touching
/// test forces them to run one-at-a-time without pulling in `serial_test`.
/// What: a `tokio::sync::Mutex<()>` (chosen over `std::sync::Mutex` so the
/// guard can be held across `.await` points without tripping clippy's
/// `await_holding_lock` lint). Tests hold the guard for the duration of
/// the env-sensitive section.
/// Test: indirectly via the integration tests in `prompt_context` and
/// `inbox_check`.
#[cfg(test)]
pub(crate) fn env_test_lock() -> &'static tokio::sync::Mutex<()> {
    use std::sync::OnceLock;
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}
