//! Dial the running `tcode` daemon for a one-shot thin-client command (#10169).
//!
//! Why: `session list|status`, `attach`, `cancel` and `transcript` each spawned
//! a fresh `serve --stdio` child, whose in-memory registry knew no session but
//! its own, so `session list` against a live daemon printed `(no sessions)`.
//! Owner ruling, 2026-09-08: trusty-code's transport is UDS, natively.
//! What: [`connect`] resolves the daemon serving `project` with the same
//! attach-or-start policy `tcode tui` uses
//! ([`super::daemon_autospawn::ensure_daemon`]) and returns a client for its
//! socket. A resolution failure is returned as an error; there is no stdio
//! fallback and no empty default.
//! Test: `thin_clients_uds_10169::session_list_refuses_an_unverifiable_daemon_instead_of_listing_nothing`,
//! plus the per-command cases in `tests/thin_clients_uds_10169.rs`.

use std::path::Path;

use anyhow::Result;
use trusty_code::tui_client::uds_rpc::UdsRpcClient;

use super::daemon_autospawn;

/// A client for the daemon serving `project`, starting one if none answers.
///
/// `project` is already canonicalized by `main.rs`, which is what the
/// daemon's reported binding is compared against.
pub async fn connect(project: &Path) -> Result<UdsRpcClient> {
    // #10169: the daemon's socket, never a `serve --stdio` child.
    let socket = daemon_autospawn::ensure_daemon(Some(project)).await?;
    Ok(UdsRpcClient::new(socket))
}
