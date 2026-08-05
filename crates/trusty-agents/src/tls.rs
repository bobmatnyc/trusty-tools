//! Process-level rustls `CryptoProvider` installation (#4825).
//!
//! Why: `tagent --slack` reached its first WSS handshake and aborted the whole
//! process inside rustls 0.23 — "Could not automatically determine the
//! process-level CryptoProvider from Rustls crate features". A single
//! `rustls 0.23` instance in this workspace's graph has BOTH provider features
//! enabled (`ring`, pulled by reqwest's `rustls-tls` and `ureq`; `aws-lc-rs`,
//! pulled by the `aws-smithy-http-client` stack behind `aws-sdk-bedrockruntime`),
//! so rustls refuses to guess and panics. Dependencies that configure their own
//! provider (reqwest, aws-smithy) are unaffected, which is why the Slack gateway
//! could log "Socket Mode connected" and only then die: `tokio-tungstenite`'s
//! connector builds a `ClientConfig` from the process default.
//!
//! Deduplicating the providers instead was rejected: `ring` and `aws-lc-rs`
//! arrive from independent subtrees (HTTP client vs. AWS SDK), so removing
//! either means changing reqwest's TLS backend workspace-wide or dropping the
//! Bedrock SDK. An explicit install is one call, is immune to a future
//! dependency re-enabling the other feature, and makes the choice auditable.
//!
//! What: `install_crypto_provider`, called once at the top of
//! `runtime::run()` before any TLS-capable code path.
//! Test: `tests/slack_wss_crypto_provider.rs`.

use anyhow::{Result, bail};

/// Install the process-level rustls `CryptoProvider` if none is installed yet.
///
/// Why: See the module docs — without this, the first `rustls::ClientConfig`
/// built from the process default (the Socket Mode WebSocket connector)
/// panics and takes the gateway down.
/// What: Installs the `aws-lc-rs` provider, then asserts the postcondition
/// that the process now HAS a default provider. `install_default` returns
/// `Err` for exactly one reason — a provider was already installed, by a
/// launcher or an earlier call — which is benign and why the error value
/// itself is discarded rather than propagated. The postcondition check is
/// what makes that discard safe: a swallowed failure that left the process
/// without a provider would re-hide the original panic until the first
/// handshake, so it is turned into a loud startup error instead.
///
/// Idempotent; safe to call from every launcher.
/// Test: `wss_handshake_survives_after_install`,
/// `install_is_idempotent_and_leaves_a_default_provider`.
pub fn install_crypto_provider() -> Result<()> {
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        // Err here means "someone else installed one first" — a race we are
        // happy to lose. The check below is the real gate.
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        bail!(
            "failed to install a process-level rustls CryptoProvider; \
             TLS connections (Slack Socket Mode, HTTPS) would panic at the \
             first handshake"
        );
    }
    Ok(())
}
