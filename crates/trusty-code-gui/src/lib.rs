//! trusty-code (tcode) desktop shell (Tauri 2).
//!
//! Why: a native window over the `tcode` daemon (issue #2983, DOC-39
//! `docs/specs/trusty-code-harness-ui.md`). #6637 changed what sits between the
//! two: the daemon speaks framed JSON-RPC over a Unix socket, and a webview
//! cannot dial one, so this shell now runs the HTTP+SSE bridge the webview talks
//! to instead of pointing it at the daemon's own TCP listener — the listener PR
//! 2c deletes.
//!
//! What: binds the bridge (`crate::bridge::serve::start`), manages the
//! [`crate::state::GuiState`] carrying its URL and per-launch token, registers
//! the two IPC commands that hand both to the webview, then runs the Tauri
//! event loop. DOC-39 §2.1's thin-client rule still holds — the Rust side
//! computes no daemon data, it only translates transport.
//! Test: `cargo test -p trusty-code-gui`, and `tests/bridge_uds.rs` for the
//! bridge itself; launching the app shows the session list fetched through it.

pub mod bridge;
mod commands;
mod state;

use state::GuiState;

/// Build and run the Tauri application.
///
/// Why the bridge is bound BEFORE the builder rather than in a `setup` hook: the
/// URL and token are constructor arguments to the state the webview reads on its
/// first paint, and a window that opened while the port was still being assigned
/// would `fetch()` an address that did not exist yet.
/// What: `block_on` the bind on Tauri's own runtime (rather than standing up a
/// second one), manage the resulting state, register the two commands, run.
///
/// # Panics
///
/// When the loopback bridge cannot bind, or the Tauri event loop fails to start.
/// Both are launch-time failures with no degraded mode worth offering: a shell
/// whose webview has nothing to talk to is a blank window, and a panic at least
/// says why in the console the operator launched from.
/// Test: exercised by launching the app; the bridge itself is covered by
/// `crate::bridge::serve::tests` and `tests/bridge_uds.rs`.
pub fn run() {
    let bridge = tauri::async_runtime::block_on(bridge::serve::start(None))
        .expect("trusty-code-gui could not bind its loopback bridge");
    tracing::info!("trusty-code-gui bridge listening on {}", bridge.addr);

    tauri::Builder::default()
        .manage(GuiState::new(bridge.url, bridge.token))
        .invoke_handler(tauri::generate_handler![
            commands::get_daemon_url,
            commands::get_daemon_token
        ])
        .run(tauri::generate_context!())
        .expect("error while running trusty-code-gui");
}
