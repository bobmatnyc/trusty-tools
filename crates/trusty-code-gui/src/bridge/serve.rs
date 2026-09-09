//! Binding the bridge listener at app launch (#6637).
//!
//! Why an ephemeral port and an in-memory token: the daemon's listener was a
//! FIXED port with a `0600` token file, and both facts existed because two
//! processes that never speak to each other had to agree on them. Here they do
//! speak — the shell mints the credential and hands the webview both halves over
//! Tauri IPC — so neither the port nor the token needs to be discoverable, and
//! there is nothing on disk for anything else on this machine to read. A port
//! that changes per launch also removes the class of bug where a stale process
//! holds the number the app expects.
//!
//! What: [`start`] binds `127.0.0.1:0`, mints a token, spawns the server, and
//! hands back the [`Bridge`] the shell keeps for the app's life.
//!
//! Test: `crate::bridge::serve::tests::a_fresh_start_binds_loopback_with_a_strong_token`,
//! and `tests/bridge_uds.rs`, which drives the same router the shell serves.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use trusty_common::server::DaemonAuth;

use super::routes::{BridgeState, build_router};

/// A running bridge: where it listens and what opens it.
///
/// Why both halves are owned here rather than read back from the listener: the
/// shell hands exactly these two strings to the webview, and deriving either one
/// a second time is how the two ends stop agreeing.
pub struct Bridge {
    /// Base URL the webview `fetch()`es, with no trailing slash.
    pub url: String,
    /// Credential the webview attaches to every request.
    pub token: String,
    /// The address actually bound — the port is assigned, never chosen.
    pub addr: SocketAddr,
}

/// Bind the bridge on loopback and start serving.
///
/// Why `127.0.0.1:0`: loopback-only is ADR-0018's doctrine and the port is the
/// OS's to pick, since nothing discovers this listener by number. Binding fails
/// loudly rather than degrading — a shell whose webview has nothing to talk to
/// is a blank window, and an error at launch says why.
///
/// What: binds, mints a credential with `trusty_common::daemon_token::mint_token`
/// (the same minter the daemon used, so the strength floor
/// `DaemonAuth::new` enforces is met by construction), spawns the server onto
/// the ambient runtime, and returns the URL and token. `socket_override` points
/// the bridge at a stub daemon socket; production passes `None`.
///
/// **No public paths.** The daemon carved `/health` out because trusty-console's
/// gateway polls it holding no credential. Nothing polls this listener from
/// outside the app, so every route requires the token.
///
/// # Errors
///
/// The loopback port could not be bound, or the minted credential was refused as
/// too weak (unreachable — `mint_token` is well over the floor — but not
/// `unwrap`ed for it).
///
/// Test: `a_fresh_start_binds_loopback_with_a_strong_token`.
pub async fn start(socket_override: Option<PathBuf>) -> Result<Bridge> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("bind the trusty-code-gui bridge on loopback")?;
    let addr = listener
        .local_addr()
        .context("read the bridge's bound address")?;

    let token = trusty_common::daemon_token::mint_token();
    let auth = DaemonAuth::new(token.clone(), Vec::<String>::new())
        .map_err(|e| anyhow::anyhow!("the bridge credential is unusable: {e}"))?;

    let mut state = BridgeState::new(auth);
    if let Some(socket) = socket_override {
        state = state.with_socket(socket);
    }
    let router = build_router(state);

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!("trusty-code-gui bridge stopped serving: {e}");
        }
    });

    Ok(Bridge {
        url: format!("http://{addr}"),
        token,
        addr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the two facts the webview is handed are the whole of its access, and
    /// each has a failure mode worth pinning — a non-loopback bind would expose
    /// the daemon's whole surface to the network, and a token under
    /// `DaemonAuth`'s floor would be refused at construction rather than guard
    /// anything.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_fresh_start_binds_loopback_with_a_strong_token() {
        let bridge = start(None).await.expect("the bridge must bind");
        assert!(bridge.addr.ip().is_loopback(), "{}", bridge.addr);
        assert_ne!(bridge.addr.port(), 0, "the OS must assign a real port");
        assert!(
            bridge.token.len() >= trusty_common::daemon_token::MIN_TOKEN_LEN,
            "a token under the floor is refused by DaemonAuth::new"
        );
        assert_eq!(bridge.url, format!("http://{}", bridge.addr));
    }

    /// Why: two launches must not share a port or a credential — a token that
    /// outlived a launch would be worth capturing, and a fixed port is the stale
    /// -holder bug this design removes.
    /// Test: this is the test.
    #[tokio::test]
    async fn two_launches_share_neither_port_nor_token() {
        let a = start(None).await.expect("bind a");
        let b = start(None).await.expect("bind b");
        assert_ne!(a.addr.port(), b.addr.port());
        assert_ne!(a.token, b.token);
    }
}
