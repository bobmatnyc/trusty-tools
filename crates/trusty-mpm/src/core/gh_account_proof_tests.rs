//! Tests for the proof cache, the host check and the `GET /user` request
//! (#8510 r4).
//!
//! Why: the cache decides whether a token is re-proven, and the request is
//! what sends the token. Every test uses table fakes or a loopback
//! `127.0.0.1` listener: no test runs `gh`, reads a keyring, or leaves the
//! machine. The loopback clients disable proxies, so an ambient `HTTP_PROXY`
//! cannot route a test request off the host.
//! Test: itself.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{
    AccountProver, PROOF_TTL, ProofCache, fetch_user_login, normalize_gh_host, origin_host,
    send_user_request, user_client,
};
use crate::core::gh_account_dir::AccountDirSources;
use crate::core::gh_account_dir::gh_account_dir_tests::{
    ORIGIN, TableCheck, TableProbe, migrated_dir,
};

/// An Enterprise Server repository.
const GHES_ORIGIN: &str = "https://ghe.corp/duettoresearch/jev-matching";

/// A probe and check that prove `tok-bob` for `bob-duetto` under `dir` on
/// github.com, and answer nothing else.
fn proving(dir: &std::path::Path) -> (TableProbe, TableCheck) {
    (
        TableProbe::default().answer(dir, "github.com", "bob-duetto", Ok("tok-bob")),
        TableCheck::default().answer("https://api.github.com", "tok-bob", Ok("bob-duetto")),
    )
}

/// Sources naming only `dir`.
fn only(dir: &std::path::Path) -> AccountDirSources {
    AccountDirSources {
        own_config_dir: Some(dir.to_path_buf()),
        ..AccountDirSources::default()
    }
}

/// 🔴 #8510 r4: a second lookup for the same login and host inside the TTL
/// asks neither `gh` nor GitHub again. A login differing only in case is the
/// same GitHub account.
/// Test: itself.
#[test]
fn a_remembered_proof_is_reused_within_its_ttl() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = migrated_dir(root.path());
    let (probe, check) = proving(&dir);
    let cache = ProofCache::new(PROOF_TTL);
    let sources = only(&dir);
    let prover = AccountProver {
        sources: &sources,
        probe: &probe,
        check: &check,
        cache: Some(&cache),
    };
    let t0 = Instant::now();
    let first = prover.prove_at("bob-duetto", ORIGIN, t0).expect("proves");
    let again = prover
        .prove_at(
            "Bob-Duetto",
            ORIGIN,
            t0 + PROOF_TTL - Duration::from_secs(1),
        )
        .expect("served from the cache");
    assert_eq!(first, again);
    assert_eq!(probe.calls().len(), 1, "gh ran again: {:?}", probe.calls());
    assert_eq!(check.calls().len(), 1, "GitHub was asked again");
}

/// 🔴 #8510 r4: a remembered proof never answers for another login or another
/// host; each is proven on its own.
/// Test: itself.
#[test]
fn a_remembered_proof_never_serves_another_login_or_host() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = migrated_dir(root.path());
    let (probe, check) = proving(&dir);
    let cache = ProofCache::new(PROOF_TTL);
    let sources = only(&dir);
    let prover = AccountProver {
        sources: &sources,
        probe: &probe,
        check: &check,
        cache: Some(&cache),
    };
    let t0 = Instant::now();
    prover.prove_at("bob-duetto", ORIGIN, t0).expect("proves");

    prover
        .prove_at("alice", ORIGIN, t0)
        .expect_err("alice has no token here");
    prover
        .prove_at("bob-duetto", GHES_ORIGIN, t0)
        .expect_err("no token is scripted for ghe.corp");
    let asked: Vec<(String, String)> = probe
        .calls()
        .into_iter()
        .map(|(_, host, login)| (host, login))
        .collect();
    assert_eq!(
        asked,
        [
            ("github.com".to_string(), "bob-duetto".to_string()),
            ("github.com".to_string(), "alice".to_string()),
            ("ghe.corp".to_string(), "bob-duetto".to_string()),
        ]
    );
}

/// 🔴 #8510 r4: an entry as old as the TTL is proven again.
/// Test: itself.
#[test]
fn an_expired_proof_is_proven_again() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = migrated_dir(root.path());
    let (probe, check) = proving(&dir);
    let cache = ProofCache::new(PROOF_TTL);
    let sources = only(&dir);
    let prover = AccountProver {
        sources: &sources,
        probe: &probe,
        check: &check,
        cache: Some(&cache),
    };
    let t0 = Instant::now();
    prover.prove_at("bob-duetto", ORIGIN, t0).expect("proves");
    prover
        .prove_at("bob-duetto", ORIGIN, t0 + PROOF_TTL)
        .expect("proves again");
    assert_eq!(probe.calls().len(), 2);
    assert_eq!(check.calls().len(), 2);
}

/// 🔴 #8510 r4: a refusal is not remembered; the next lookup proves again and
/// is refused again.
/// Test: itself.
#[test]
fn a_refusal_is_never_remembered() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = migrated_dir(root.path());
    let probe = TableProbe::default().answer(&dir, "github.com", "bob-duetto", Ok("tok-g"));
    let check = TableCheck::default().answer("https://api.github.com", "tok-g", Ok("bobmatnyc"));
    let cache = ProofCache::new(PROOF_TTL);
    let sources = only(&dir);
    let prover = AccountProver {
        sources: &sources,
        probe: &probe,
        check: &check,
        cache: Some(&cache),
    };
    let t0 = Instant::now();
    for _ in 0..2 {
        prover
            .prove_at("bob-duetto", ORIGIN, t0)
            .expect_err("another account's token is refused");
    }
    assert_eq!(probe.calls().len(), 2, "a refusal must not be remembered");
}

/// 🔴 #8510 r4: a host outside the hostname set never reaches a URL.
/// Test: itself.
#[test]
fn origin_host_refuses_a_host_outside_the_hostname_set() {
    for origin in [
        "https://evil.com#.foo.ghe.com/org/repo",
        "git@evil.com#.foo.ghe.com:org/repo",
        "https://evil.com?x=.ghe.com/org/repo",
        "https://evil.com:/org/repo",
    ] {
        let err = origin_host(origin).expect_err(origin);
        assert!(err.contains("not a valid gh host name"), "{origin}: {err}");
    }
    assert_eq!(
        origin_host("https://GHE.corp:8443/o/r").as_deref(),
        Ok("ghe.corp:8443")
    );
    assert_eq!(
        normalize_gh_host("api.github.com").as_deref(),
        Ok("github.com")
    );
}

/// 🔴 #8510 r4: production never sends a token over plain http.
/// Test: itself.
#[test]
fn fetch_user_login_refuses_a_non_https_url() {
    let err = fetch_user_login("http://127.0.0.1:9/user", "tok-x").expect_err("http");
    assert!(err.contains("non-https") && !err.contains("tok-x"), "{err}");
}

/// A one-shot loopback server: accepts one connection, returns the request
/// head it read, then runs `respond` on the stream.
fn serve_once(
    respond: impl FnOnce(&mut TcpStream) + Send + 'static,
) -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let url = format!("http://{}/user", listener.local_addr().expect("addr"));
    let handle = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("read timeout");
        let mut head = Vec::new();
        let mut buf = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") {
            match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        respond(&mut stream);
        String::from_utf8_lossy(&head).into_owned()
    });
    (url, handle)
}

/// Write a complete HTTP/1.1 answer.
fn answer(stream: &mut TcpStream, status: &str, headers: &str, body: &str) {
    let text = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(text.as_bytes()).expect("write answer");
}

/// 🔴 #8510 r4: a `302` is refused, and its target is never contacted — the
/// token goes to the one URL derived from the host and nowhere else.
/// Test: itself.
#[test]
fn a_redirect_is_refused_and_never_followed() {
    let target = TcpListener::bind("127.0.0.1:0").expect("bind target");
    target.set_nonblocking(true).expect("nonblocking");
    let location = format!("http://{}/user", target.local_addr().expect("addr"));
    let (url, server) = serve_once(move |stream| {
        answer(
            stream,
            "302 Found",
            &format!("Location: {location}\r\n"),
            "",
        );
    });
    let err = send_user_request(user_client().no_proxy(), &url, "tok-x").expect_err("302");
    server.join().expect("server");
    assert!(err.contains("HTTP 302") && !err.contains("tok-x"), "{err}");
    match target.accept() {
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("the redirect target was contacted: {other:?}"),
    }
}

/// 🔴 #8510 r4: an answer that never comes times out and is refused.
/// Test: itself.
#[test]
fn a_stalled_user_answer_times_out() {
    let (release, wait) = mpsc::channel::<()>();
    let (url, server) = serve_once(move |_stream| {
        let _ = wait.recv_timeout(Duration::from_secs(10));
    });
    let started = Instant::now();
    let client = user_client().no_proxy().timeout(Duration::from_millis(300));
    let err = send_user_request(client, &url, "tok-x").expect_err("a stall is refused");
    let waited = started.elapsed();
    let _ = release.send(());
    server.join().expect("server");
    assert!(waited < Duration::from_secs(5), "waited {waited:?}");
    assert!(err.contains("failed") && !err.contains("tok-x"), "{err}");
}

/// 🔴 #8510 r4: a `200` carrying a login is read, and the request carried the
/// token in the `Authorization` header.
/// Test: itself.
#[test]
fn a_matching_login_is_read_and_the_token_is_sent() {
    let (url, server) = serve_once(|stream| {
        answer(
            stream,
            "200 OK",
            "Content-Type: application/json\r\n",
            r#"{"login":"Bob-Duetto"}"#,
        );
    });
    let login = send_user_request(user_client().no_proxy(), &url, "tok-x").expect("200");
    let head = server.join().expect("server").to_ascii_lowercase();
    assert!(login.eq_ignore_ascii_case("bob-duetto"), "{login}");
    assert!(head.starts_with("get /user http/1.1"), "{head}");
    assert!(head.contains("authorization: token tok-x\r\n"), "{head}");
}
