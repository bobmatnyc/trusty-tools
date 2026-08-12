//! Loopback release-server fixtures shared by the download-path test suites.
//!
//! Why: `pinned/tests.rs` built a path-routing fixture server so its fail-closed
//! arms could be proved without a live network call. #5518 needs the same thing
//! for `try_install_prebuilt`'s verify step, and a second copy would be two
//! fixture servers to keep in step — the drift risk the crate's duplicate rule
//! exists to avoid. This is that server, extracted, with both suites calling it.
//!
//! What: [`serve_fixture`] answers fixed bodies per request path on loopback;
//! [`fake_tarball`] builds an archive shaped like a real release asset; and
//! [`sha256_hex`] computes the digest a `.sha256` sidecar would carry.
//!
//! Test: Every test in `pinned::tests` and `download::mod_tests`.

use std::collections::HashMap;
use std::sync::Arc;

/// Routes a fixture server answers: path → (status code, body bytes).
pub(crate) type Routes = HashMap<String, (u16, Vec<u8>)>;

/// Serve fixed responses per PATH on loopback, for as long as the test runs.
///
/// Why: Every fail-closed arm must be provable WITHOUT a live network call.
/// The crate's `commands::test_support` stubs answer a fixed SEQUENCE with JSON
/// bodies; these pipelines issue three requests and one of them is a binary
/// tarball, so this needs path routing and byte bodies. Same raw-`TcpListener`
/// vehicle, no new dev-dependency.
///
/// What: Binds an ephemeral loopback port, spawns an accept loop, and answers
/// each request from `routes` (404 for anything unrouted). Returns the base URL.
pub(crate) async fn serve_fixture(routes: Routes) -> String {
    let routes = Arc::new(routes);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let routes = Arc::clone(&routes);
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                // Drain the header block before replying — the split-request
                // flake shape `test_support::serve_fixed` documents.
                let mut acc = Vec::with_capacity(2048);
                let mut chunk = [0u8; 2048];
                loop {
                    match sock.read(&mut chunk).await {
                        Ok(0) => break,
                        Ok(n) => {
                            acc.extend_from_slice(&chunk[..n]);
                            if acc.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => return,
                    }
                }
                let head = String::from_utf8_lossy(&acc);
                let path = head
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("/")
                    .to_owned();
                let (status, body) = match routes.get(&path) {
                    Some((s, b)) => (*s, b.clone()),
                    None => (404, b"not found".to_vec()),
                };
                let reason = if status == 200 { "OK" } else { "Not Found" };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

/// A `.tar.gz` holding one executable that prints `version_line`.
///
/// Why: The pinned path probes the staged binary with `--version`, so the
/// fixture must be genuinely executable — a byte blob would prove nothing about
/// the version gate.
///
/// What: One 0755 shell script named `binary`, plus the non-executable
/// `LICENSE` every real release tarball in this workspace also ships (#5495 —
/// without it the suite was green while a multi-tool set collided on
/// `tools/LICENSE` and could never install).
pub(crate) fn fake_tarball(binary: &str, version_line: &str) -> Vec<u8> {
    use flate2::{write::GzEncoder, Compression};
    let script = format!("#!/bin/sh\necho '{version_line}'\n");
    let data = script.as_bytes();
    let enc = GzEncoder::new(Vec::new(), Compression::fast());
    let mut ar = tar::Builder::new(enc);
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    ar.append_data(&mut header, binary, data).unwrap();

    let license = b"MIT\n";
    let mut doc = tar::Header::new_gnu();
    doc.set_size(license.len() as u64);
    doc.set_mode(0o644);
    doc.set_cksum();
    ar.append_data(&mut doc, "LICENSE", &license[..]).unwrap();

    ar.into_inner().unwrap().finish().unwrap()
}

/// The lowercase hex SHA-256 of `bytes` — what a `.sha256` sidecar carries.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    format!("{:x}", h.finalize())
}
