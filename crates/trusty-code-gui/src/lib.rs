//! trusty-code (tcode) desktop shell (Tauri 2).
//!
//! Why: a native window over the `tcode` daemon (issue #2983, DOC-39
//! `docs/specs/trusty-code-harness-ui.md`). The daemon speaks framed JSON-RPC
//! over a Unix socket (#6637) and a webview cannot dial one, so this shell runs
//! the HTTP+SSE bridge the webview talks to. Since #10166 that bridge opens no
//! socket of its own: the webview reaches it through the `tcode-bridge://`
//! custom URI scheme and an IPC `Channel`, both in-process.
//!
//! What: builds the bridge (`crate::bridge::serve::start`), registers the
//! scheme handler over its router, manages the [`crate::state::GuiState`]
//! carrying its URL, token, router and streams, registers the four IPC
//! commands, then runs the Tauri event loop. DOC-39 §2.1's thin-client rule
//! still holds — the Rust side computes no daemon data, it only translates
//! transport. macOS only (owner ruling Q5, 2026-10-23).
//! Test: `cargo test -p trusty-code-gui`: `tests/no_tcp_listener_10166.rs` for
//! the no-listener property, `tests/bridge_uds.rs` for the bridge itself;
//! launching the app shows the session list fetched through it.

pub mod bridge;
mod commands;
mod state;

use state::GuiState;
use tauri::Manager as _;

/// Build and run the Tauri application.
///
/// Why the bridge is built BEFORE the builder rather than in a `setup` hook:
/// the scheme handler and the managed state both need its router, and the
/// handler must be registered on the builder.
/// What: `block_on` the build on Tauri's own runtime, register the
/// `tcode-bridge` scheme — each request is answered by
/// [`bridge::scheme::dispatch`] on a spawned task — manage the state, register
/// the commands, run.
///
/// # Panics
///
/// When the bridge cannot be built, or the Tauri event loop fails to start.
/// Both are launch-time failures with no degraded mode worth offering: a shell
/// whose webview has nothing to talk to is a blank window, and a panic at least
/// says why in the console the operator launched from.
/// Test: `tests/no_tcp_listener_10166.rs` starts the bridge the same way;
/// launching the app covers the scheme registration.
pub fn run() {
    let bridge = tauri::async_runtime::block_on(bridge::serve::start(None))
        .expect("trusty-code-gui could not build its in-process bridge");
    tracing::info!("trusty-code-gui bridge serving on {}", bridge.url);

    let scheme_router = bridge.router.clone();
    tauri::Builder::default()
        // #10166: unary REST calls arrive here instead of over a TCP port.
        .register_asynchronous_uri_scheme_protocol(
            bridge::scheme::SCHEME,
            move |_ctx, request, responder| {
                let router = scheme_router.clone();
                tauri::async_runtime::spawn(async move {
                    responder.respond(bridge::scheme::dispatch(router, request).await);
                });
            },
        )
        .manage(GuiState::new(bridge.url, bridge.token, bridge.router))
        // #10166: a reload leaves the old page's streams with no owner and no
        // failing send (heartbeats emit nothing), so release them all when the
        // new page starts loading, before its scripts open their own.
        .on_page_load(|webview, payload| {
            if payload.event() == tauri::webview::PageLoadEvent::Started {
                webview.state::<GuiState>().streams.close_all();
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_daemon_url,
            commands::get_daemon_token,
            commands::open_daemon_stream,
            commands::close_daemon_stream
        ])
        .run(tauri::generate_context!())
        .expect("error while running trusty-code-gui");
}
