//! Regression coverage for the `tagent --slack` startup panic (#4825).
//!
//! Why: The Slack gateway died on its first WSS handshake because no
//! process-level rustls `CryptoProvider` was ever installed. A green build
//! proves nothing here — the panic only surfaces when a `rustls::ClientConfig`
//! is actually constructed from the process default, which is what
//! `tokio_tungstenite::connect_async` does for a `wss://` URL.
//!
//! What: Drives the real `tokio-tungstenite` connector — the same one
//! `slack::run_slack_bot` uses — against a local throwaway TCP listener, with
//! no network, no Slack tokens, and no contact with the production Slack app.
//! The listener captures what the client actually puts on the wire and the
//! test asserts it is a well-formed TLS ClientHello, which can only exist if
//! the process-level provider supplied the cipher suites and key share. The
//! connection itself is then expected to fail (the listener speaks no TLS) —
//! as an `Err`, not as a process abort.
//!
//! Verified to fail without the fix: with `install_crypto_provider` absent,
//! this test panics at `rustls-0.23.40/src/crypto/mod.rs:249:14` with the exact
//! message from the owner's production log, and no bytes ever reach the
//! listener.

use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// The connector must emit a real TLS ClientHello and return a transport
/// error, rather than aborting the process.
///
/// Test: this is the test.
#[tokio::test]
async fn wss_handshake_survives_after_install() {
    trusty_agents::tls::install_crypto_provider().expect("crypto provider must install");

    // A listener that accepts, records the client's first flight, and never
    // replies. TCP connect succeeds, so `connect_async` proceeds into TLS setup
    // — where the missing provider used to panic — writes its ClientHello, and
    // then fails on the handshake itself.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 2048];
        let n = match listener.accept().await {
            Ok((mut stream, _)) => stream.read(&mut buf).await.unwrap_or(0),
            Err(_) => 0,
        };
        buf.truncate(n);
        let _ = tx.send(buf);
    });

    let result = tokio_tungstenite::connect_async(format!("wss://127.0.0.1:{port}/")).await;
    assert!(
        result.is_err(),
        "a bare TCP listener cannot complete a TLS handshake; reaching Ok would mean \
         this test stopped exercising the TLS path"
    );

    let first_flight = rx.await.expect("listener task must report");
    // TLS record header: content type 0x16 (handshake), legacy version 0x0301,
    // then handshake type 0x01 (ClientHello) at offset 5. The body carries the
    // provider-supplied cipher-suite list and key share, so a well-formed
    // ClientHello of real size is proof the provider was resolved and used.
    assert!(
        first_flight.len() > 100,
        "expected a full ClientHello, got {} byte(s): {first_flight:02x?}",
        first_flight.len()
    );
    assert_eq!(
        &first_flight[..3],
        &[0x16, 0x03, 0x01],
        "expected a TLS handshake record header"
    );
    assert_eq!(
        first_flight[5], 0x01,
        "expected the handshake message to be a ClientHello"
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
