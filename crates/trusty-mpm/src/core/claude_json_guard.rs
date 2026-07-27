//! Process-wide mutual exclusion for read-modify-write cycles on the
//! operator's `~/.claude.json`.
//!
//! Why (issue #4072): two independent seeders in this crate perform an
//! unsynchronised load → mutate → store cycle against the SAME
//! `~/.claude.json` file —
//! [`crate::core::home_trust_seed::preseed_home_trust`] (workspace trust +
//! renderer-upsell dismissal) and
//! [`crate::core::session_launch::settings::preseed_workspace_trust`]
//! (workspace trust + `enabledMcpjsonServers` approval). Both run inside the
//! daemon, which provisions and spawns sessions CONCURRENTLY: the workspace
//! provisioner calls the former and then, via `prepare_session`, the latter,
//! once per session, from independent tokio tasks. Two overlapping cycles are
//! a textbook lost update — the slower writer serialises the snapshot it read
//! BEFORE the faster writer's store, so the faster writer's whole
//! `projects.<workspace>` entry silently disappears. The operator's symptom is
//! a session that stalls on the "Do you trust the files in this folder?" or
//! "new MCP servers found" dialog it was supposed to have pre-dismissed, with
//! nothing logged anywhere — both writes "succeeded".
//!
//! This also surfaced as a recurring CI flake: `trusty-mpm`'s lib test binary
//! runs `#[serial]` tests that redirect the process-global `$HOME` to a temp
//! dir alongside non-serial tests that resolve `$HOME` at call time and drive
//! the real provisioner, so a foreign test's seed lands in — and clobbered —
//! the serial test's temp `.claude.json`
//! (`core::session_launch::tests_manifest_toggle_trust_3934` failed this way
//! on PRs #4057 and #4067).
//!
//! What: [`lock`] hands out a guard on one process-wide mutex. Every seeder
//! that read-modify-writes `~/.claude.json` holds it across the WHOLE cycle
//! (read, mutate, write), never just the write. The guard is deliberately not
//! keyed by path: both seeders always target the single `~/.claude.json` of
//! whatever `$HOME` resolves to right now, so one global lock is exactly the
//! granularity needed and is immune to two callers disagreeing about the path.
//!
//! SCOPE: this is an in-process lock. It does not (and is not meant to)
//! serialise two separate `tm` processes racing the same file — that needs an
//! OS file lock and has never been observed; the daemon is the only writer
//! that concurrently seeds. Every write also goes through
//! `trusty_common::claude_config::write_json_atomic`, so even a cross-process
//! race can only ever lose an update, never leave a torn file behind.
//!
//! Test: `concurrent_seeds_preserve_every_workspace_entry` and
//! `concurrent_home_and_workspace_seeds_preserve_both_entries`, in
//! `tests_claude_json_concurrency_4072`.

use std::sync::{Mutex, MutexGuard};

/// The one process-wide `~/.claude.json` read-modify-write mutex.
static CLAUDE_JSON_LOCK: Mutex<()> = Mutex::new(());

/// Acquire the process-wide `~/.claude.json` read-modify-write lock.
///
/// Why: see the module doc — holding this across a seeder's whole load →
/// mutate → store cycle is what makes two concurrent seeds merge instead of
/// clobbering each other.
/// What: locks [`CLAUDE_JSON_LOCK`], recovering from poisoning by taking the
/// inner guard. Poison recovery is correct here because the mutex guards no
/// in-memory invariant — its payload is `()`; a panicking seeder leaves the
/// on-disk file either untouched or atomically replaced, so the next holder
/// starts from a consistent read either way. Panicking instead would turn one
/// unrelated seeder panic into a permanently un-seedable config for the rest
/// of the process's life.
/// Test: `concurrent_seeds_preserve_every_workspace_entry`,
/// `concurrent_home_and_workspace_seeds_preserve_both_entries`.
pub(crate) fn lock() -> MutexGuard<'static, ()> {
    CLAUDE_JSON_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
