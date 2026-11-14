//! Building the in-process bridge at app launch (#6637, #10166).
//!
//! Why: the bridge used to bind `127.0.0.1:0` and serve the router over TCP,
//! the last listener on the trusty-code side. #10166 removes it: the webview
//! reaches the same router through the `tcode-bridge://` custom URI scheme
//! ([`crate::bridge::scheme`]) and the two event streams through a Tauri IPC
//! `Channel` ([`crate::bridge::channel`]). Both run the router in this process,
//! so nothing listens on any socket and there is no port to discover.
//!
//! The per-launch token stays (owner ruling Q3, 2026-10-23): it costs nothing
//! and keeps the guard stack, the ticket flow and every UI fetch site as they
//! were.
//!
//! What: [`start`] mints a token, builds the guarded router, and hands back the
//! [`Bridge`] the shell keeps for the app's life.
//!
//! Test: `crate::bridge::serve::tests::a_fresh_start_names_the_scheme_with_a_strong_token`,
//! `tests/no_tcp_listener_10166.rs`, and `tests/bridge_uds.rs`, which drives
//! the same router.

use std::path::PathBuf;

use anyhow::Result;
use trusty_common::server::DaemonAuth;

use super::routes::{BridgeState, build_router};
use super::scheme::SCHEME_ORIGIN;

/// The bridge: the router the webview reaches and what opens it.
///
/// Why the router is owned here: the scheme handler and the stream command
/// each need a handle on the SAME router, and so the same ticket table — a
/// ticket minted over the scheme must be redeemable by the stream command.
pub struct Bridge {
    /// Base URL the webview `fetch()`es — the scheme origin, no trailing slash.
    pub url: String,
    /// Credential the webview attaches to every request.
    pub token: String,
    /// The guarded router both transports dispatch into.
    pub router: axum::Router,
}

/// Build the bridge. Opens no socket of any kind.
///
/// Why `async`: `crate::run` and the #10166 regression test call it under
/// `block_on`, the shape it had when it bound a port; keeping the shape keeps
/// that test meaningful against both the old and the new code.
///
/// What: mints a credential with `trusty_common::daemon_token::mint_token`
/// (the strength floor `DaemonAuth::new` enforces is met by construction),
/// builds the guarded router, and returns it with the scheme origin and the
/// token. `socket_override` points the bridge at a stub daemon socket;
/// production passes `None`.
///
/// **No public paths.** Every route requires the token.
///
/// # Errors
///
/// The minted credential was refused as too weak (unreachable — `mint_token`
/// is well over the floor — but not `unwrap`ed for it).
///
/// Test: `a_fresh_start_names_the_scheme_with_a_strong_token`.
pub async fn start(socket_override: Option<PathBuf>) -> Result<Bridge> {
    let token = trusty_common::daemon_token::mint_token();
    let auth = DaemonAuth::new(token.clone(), Vec::<String>::new())
        .map_err(|e| anyhow::anyhow!("the bridge credential is unusable: {e}"))?;

    let mut state = BridgeState::new(auth);
    if let Some(socket) = socket_override {
        state = state.with_socket(socket);
    }

    Ok(Bridge {
        url: SCHEME_ORIGIN.to_string(),
        token,
        router: build_router(state),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the two facts the webview is handed are the whole of its access. A
    /// URL that is not the scheme origin sends every `fetch()` nowhere, and a
    /// token under `DaemonAuth`'s floor is refused at construction.
    /// Test: this is the test.
    #[tokio::test]
    async fn a_fresh_start_names_the_scheme_with_a_strong_token() {
        let bridge = start(None).await.expect("the bridge must build");
        assert_eq!(bridge.url, "tcode-bridge://localhost");
        assert!(
            bridge.token.len() >= trusty_common::daemon_token::MIN_TOKEN_LEN,
            "a token under the floor is refused by DaemonAuth::new"
        );
    }

    /// Why: two launches must not share a credential — a token that outlived a
    /// launch would be worth capturing.
    /// Test: this is the test.
    #[tokio::test]
    async fn two_launches_do_not_share_a_token() {
        let a = start(None).await.expect("build a");
        let b = start(None).await.expect("build b");
        assert_ne!(a.token, b.token);
    }
}
