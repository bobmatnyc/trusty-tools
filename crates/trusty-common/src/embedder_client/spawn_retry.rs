//! `trusty-embedderd` sidecar command construction, spawned through the
//! crate's shared ETXTBSY retry.
//!
//! Why: isolated in a sibling file (rather than inline in `supervisor.rs`) to
//! keep `supervisor.rs` under its 500-SLOC production-file cap while still
//! sharing this crate's usual per-module split (`error.rs`, `stdio.rs`, …)
//! rather than a test-only `#[path]` trick. The retry policy itself moved to
//! [`crate::spawn_retry`] in #5446; this file now owns only the command.
//!
//! Test: exercised indirectly via every test in
//! `supervisor::tests::shutdown_tests` (all of which call
//! `EmbedderSupervisor::spawn_stdio` -> `spawn_child` -> `spawn_embedderd`
//! before exercising shutdown), especially
//! `supervisor_dropped_handle_does_not_busy_spin` (#3570); the retry contract
//! itself by `crate::spawn_retry::tests`.

use std::path::Path;
use std::process::Stdio;

use tokio::process::{Child, Command};

use super::supervisor::SupervisorConfig;

/// Build the `trusty-embedderd --stdio` command and spawn it with a bounded
/// ETXTBSY retry.
///
/// Why: extracted out of `supervisor::spawn_child` so command construction
/// and the retry live together — `Command` is not `Clone`, so retrying a
/// spawn means rebuilding it from scratch on every attempt anyway.
/// What: `Command::new(binary_path).arg("--stdio")` with piped stdin/stdout,
/// inherited stderr, `kill_on_drop(true)`, and (when
/// `config.sidecar_batch_size` is `Some(n)`) `TRUSTY_EMBED_BATCH_SIZE=n`
/// (issue #747 Fix C) — then spawns it through
/// [`crate::spawn_retry::retry_on_etxtbsy_async`], which wraps the `.spawn()`
/// call and nothing else.
/// Test: `supervisor_dropped_handle_does_not_busy_spin`,
/// `supervisor_shutdown_kills_child`.
pub(super) async fn spawn_embedderd(
    binary_path: &Path,
    config: &SupervisorConfig,
) -> std::io::Result<Child> {
    let build = || {
        let mut cmd = Command::new(binary_path);
        cmd.arg("--stdio")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true);
        if let Some(bs) = config.sidecar_batch_size {
            cmd.env("TRUSTY_EMBED_BATCH_SIZE", bs.to_string());
        }
        cmd
    };
    // #5446: retry the spawn — and only the spawn — through the workspace's
    // single ETXTBSY policy, replacing this file's own copy of it.
    crate::spawn_retry::retry_on_etxtbsy_async(|| build().spawn()).await
}
