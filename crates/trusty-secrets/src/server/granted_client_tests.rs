//! The tier-3 client against the in-process server (#9071).
//!
//! A child of `wire_tests`, so it shares that module's fixture, fake process
//! table and grant helpers. This test process plays tm (it grants) and the
//! granted child (it resolves): the socket's peer is this pid, and the fake
//! table puts it under the grant.
//!
//! Test: itself.

use std::io::Write;
use std::os::fd::OwnedFd;
use std::path::PathBuf;

use super::*;
use crate::granted::{self, GrantedClient, GrantedError, RESOLVE_WAIT};

/// A pipe holding `token`, its write end closed; the read end.
fn token_fd(token: &str) -> OwnedFd {
    let (reader, mut writer) = std::io::pipe().unwrap();
    writer.write_all(token.as_bytes()).unwrap();
    drop(writer);
    OwnedFd::from(reader)
}

/// One blocking resolve of `name` under `token`, off the runtime.
async fn resolve_blocking(
    socket: PathBuf,
    token: String,
    name: &str,
) -> Result<SecretValue, GrantedError> {
    let name = key(name);
    tokio::task::spawn_blocking(move || {
        GrantedClient::from_token_fd(token_fd(&token))
            .and_then(|client| client.with_socket(socket).resolve(&name))
    })
    .await
    .unwrap()
}

/// Why: AC1 — inside a live grant, the client returns the stored value
/// from a running server, twice on a reusable grant.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn granted_client_resolves_a_granted_key_from_a_running_server() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let token = granted(&fx, me(), &["API_KEY"]).await;
    let socket = fx.settings.socket.clone();
    let first = resolve_blocking(socket.clone(), token.clone(), "API_KEY").await;
    let second = resolve_blocking(socket, token, "API_KEY").await;
    server.stop().await;
    assert_eq!(first.unwrap().expose(), SENTINEL_VALUE);
    assert_eq!(second.unwrap().expose(), SENTINEL_VALUE);
}

/// Why: AC4 — a forged token and a key outside the grant are both
/// `Refused` with the wire code and kind of `grant_refused`; no value.
/// Test: itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn granted_client_refusal_is_an_error_with_the_wire_kind() {
    let fx = fixture();
    seed(&fx, "API_KEY", SENTINEL_VALUE, false);
    seed(&fx, "OTHER_KEY", SENTINEL_VALUE, false);
    let (state, _grants) = state_with(&fx, procs_with_me());
    let server = fx.start_state(state).await;
    let token = granted(&fx, me(), &["API_KEY"]).await;
    let socket = fx.settings.socket.clone();
    let forged = resolve_blocking(socket.clone(), "0".repeat(64), "API_KEY").await;
    let outside = resolve_blocking(socket, token, "OTHER_KEY").await;
    server.stop().await;
    for outcome in [forged, outside] {
        match outcome {
            Err(GrantedError::Refused(refusal)) => {
                assert_eq!(refusal.code, ErrorKind::GrantRefused.code());
                assert_eq!(refusal.code, -32082);
                assert_eq!(refusal.kind.as_deref(), Some("grant_refused"));
                assert!(!refusal.to_string().contains(SENTINEL_VALUE));
            }
            other => panic!("{other:?}"),
        }
    }
}

/// Why: AC7 — the client waits exactly as long as the server's table says
/// a `secrets.resolve` client waits, so it never gives up on a call that
/// may still spend a one-shot grant.
/// Test: itself.
#[test]
fn granted_resolve_wait_matches_the_server_deadline() {
    assert_eq!(
        RESOLVE_WAIT,
        crate::server::deadline::client_wait(method::RESOLVE)
    );
    assert_eq!(RESOLVE_WAIT, Duration::from_secs(135));
}

/// Why: the client dials the server's default socket path; a drift fails.
/// Test: itself.
#[test]
fn granted_socket_subpath_matches_server_settings() {
    assert_eq!(
        granted::SOCKET_SUBPATH,
        crate::server::settings::SOCKET_SUBPATH
    );
}
