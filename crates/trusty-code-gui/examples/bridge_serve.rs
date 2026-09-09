//! Run the webview bridge alone, without the desktop shell around it (#6637).
//!
//! Why: the bridge is the whole of what the webview talks to, and proving it
//! against a REAL `tcode serve` needs something curl-able. Opening the packaged
//! app does not give you that — the port and token go to the webview over IPC
//! and never reach a terminal.
//!
//! What: binds the bridge exactly as `crate::run` does, prints its URL and token
//! on stdout, and serves until interrupted.
//!
//! ```text
//! SKIP_UI_BUILD=1 cargo run -p trusty-code-gui --example bridge_serve
//! curl -H "Authorization: Bearer $TOKEN" $URL/sessions
//! ```
//!
//! Not a shipped binary: an example is built by `--all-targets` and by nothing
//! an operator installs.

/// Bind the bridge, announce it, and serve until interrupted.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bridge = trusty_code_gui::bridge::serve::start(None).await?;
    println!("URL={}", bridge.url);
    println!("TOKEN={}", bridge.token);
    trusty_common::shutdown_signal().await;
    Ok(())
}
