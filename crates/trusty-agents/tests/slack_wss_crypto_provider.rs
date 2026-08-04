//! Regression coverage for the `tagent --slack` startup panic (#4825).
//!
//! Why: The Slack gateway died on its first WSS handshake because no
//! process-level rustls `CryptoProvider` was ever installed. A green build
//! proves nothing here — the panic only surfaces when a `rustls::ClientConfig`
//! is actually constructed from the process default, which is what
//! `tokio_tungstenite::connect_async` does for a `wss://` URL.
//!
//! What: Drives the real `tokio-tungstenite` connector — the same one
//! `slack::run_slack_bot` uses — against a local throwaway TCP listener, so
//! the TLS setup path is exercised with no network, no Slack tokens, and no
//! contact with the production Slack app. The handshake is expected to FAIL
//! (the listener speaks no TLS); the assertion is that it fails as an `Err`
//! rather than aborting the process.
//!
//! Verified to fail without the fix: with `install_crypto_provider` absent,
//! this test panics at `rustls-0.23.40/src/crypto/mod.rs:249:14` with the exact
//! message from the owner's production log.

use tokio::net::TcpListener;

/// The connector must return a transport error, not abort the process.
///
/// Test: this is the test.
#[tokio::test]
async fn wss_handshake_survives_after_install() {
    trusty_agents::tls::install_crypto_provider().expect("crypto provider must install");

    // A listener that accepts and then says nothing. TCP connect succeeds, so
    // `connect_async` proceeds into TLS setup — where the missing provider used
    // to panic — and then fails on the handshake itself.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        if let Ok((stream, _)) = listener.accept().await {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            drop(stream);
        }
    });

    let result = tokio_tungstenite::connect_async(format!("wss://127.0.0.1:{port}/")).await;
    assert!(
        result.is_err(),
        "a bare TCP listener cannot complete a TLS handshake; reaching Ok would mean \
         this test stopped exercising the TLS path"
    );
}

/// Calling the installer twice must stay `Ok`, because both `tagent` and
/// `trusty-agents-local` route through `run()` and a launcher may have
/// installed a provider first.
#[test]
fn install_is_idempotent_and_leaves_a_default_provider() {
    trusty_agents::tls::install_crypto_provider().expect("first install");
    trusty_agents::tls::install_crypto_provider().expect("second install must be a no-op");
    assert!(
        rustls::crypto::CryptoProvider::get_default().is_some(),
        "a process-level provider must be installed once this returns Ok"
    );
}
