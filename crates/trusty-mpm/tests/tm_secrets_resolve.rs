//! `tm secrets resolve <key>` against a real trusty-secrets server (#9072).
//!
//! Why: the verb is the tier-3 grant client for every language (owner ruling
//! 2026-10-21), so its success line and each refusal must hold end to end:
//! the built `tm` binary, the real server, the OS process table.
//! What: [`Live`] serves trusty-secrets under a scratch `$HOME`. This test
//! process grants its own pid, so the `tm` it spawns resolves as a
//! descendant. Every key is seeded with "agents may use" on, so the run
//! passes the server's agent check with or without Claude Code above it. The
//! token reaches `tm` the way the exec stub hands it down: a read-only fd on
//! an unlinked file, its number in `TRUSTY_SECRETS_TOKEN_FD`.
//! Test: this file IS the test module.

use crate::common;
use crate::tm_secrets_exec_live::Live;

use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Output, Stdio};
use std::sync::Arc;

use trusty_secrets::api::TOKEN_FD_ENV;
use trusty_secrets::api::methods::{ExecGrantRequest, ExecGrantResponse};
use trusty_secrets::store::{MemoryBackend, NamesIndex, SecretBackend, SecretStore};
use trusty_secrets::{SecretKey, SecretValue, VaultName};

const VALUE: &str = "test-sentinel-9072-resolve-c0ffee";
/// A well-formed token no grant was minted for.
const FORGED: &str = "9072aaaabbbbccccddddeeeeffff00001111222233334444555566667777beef";
/// The fd number the token lands on in `tm`.
const TOKEN_FD: i32 = 7;

/// A server holding `API_KEY` and `OTHER_KEY`, both [`VALUE`], both flagged.
async fn live(repo: &str) -> Live {
    let backend = Arc::new(MemoryBackend::new());
    let live = Live::new(Arc::clone(&backend) as Arc<dyn SecretBackend>, repo).await;
    let store = SecretStore::new(backend, NamesIndex::at(&live.index));
    let vault = VaultName::new(&live.vault).expect("vault");
    for name in ["API_KEY", "OTHER_KEY"] {
        let key = SecretKey::new(name).expect("key");
        store
            .set(&vault, &key, &SecretValue::new(VALUE))
            .expect("seed");
        store.set_agents_may_use(&vault, &key, true).expect("flag");
    }
    live
}

/// `secrets.grant` `keys` to this test process; the token.
async fn grant(live: &Live, keys: &[&str]) -> String {
    let keys = keys.iter().map(|k| SecretKey::new(k).expect("key"));
    let request = ExecGrantRequest::new(std::process::id(), keys, 300);
    let params = serde_json::to_value(request).expect("encode");
    let answer = live.call("secrets.grant", params).await.expect("grant");
    let granted: ExecGrantResponse = serde_json::from_value(answer).expect("decode");
    granted.token
}

/// A read-only fd on an unlinked file holding `token`.
fn token_file(dir: &Path, token: &str) -> File {
    let path = dir.join("token");
    std::fs::write(&path, token).expect("token file");
    let file = File::open(&path).expect("open token");
    std::fs::remove_file(&path).expect("unlink token");
    file
}

/// `tm secrets resolve <key>` under `home`, with `token` on [`TOKEN_FD`].
async fn resolve(home: &Path, key: &str, token: Option<&str>) -> Output {
    let mut cmd = common::tm_command_in(home);
    cmd.args(["secrets", "resolve", key])
        .env("RUST_LOG", "trace")
        .env_remove(TOKEN_FD_ENV)
        .stdin(Stdio::null());
    let file = token.map(|t| token_file(home, t));
    if let Some(file) = &file {
        let raw = file.as_raw_fd();
        cmd.env(TOKEN_FD_ENV, TOKEN_FD.to_string());
        // SAFETY: dup2 and fcntl are async-signal-safe; the fd tm reads is
        // left without close-on-exec.
        unsafe {
            cmd.pre_exec(move || {
                let ok = if raw == TOKEN_FD {
                    libc::fcntl(raw, libc::F_SETFD, 0) >= 0
                } else {
                    libc::dup2(raw, TOKEN_FD) >= 0
                };
                if ok {
                    Ok(())
                } else {
                    Err(std::io::Error::last_os_error())
                }
            });
        }
    }
    let out = tokio::process::Command::from(cmd)
        .output()
        .await
        .expect("tm runs");
    drop(file);
    out
}

/// Fail if `out` carries any of `secrets` on stdout or stderr.
fn assert_carries_none(label: &str, out: &Output, secrets: &[&str]) {
    for stream in [&out.stdout, &out.stderr] {
        let text = String::from_utf8_lossy(stream);
        for secret in secrets {
            assert!(!text.contains(secret), "{label}: output carries a secret");
        }
    }
}

/// Why: #9072 — inside a live grant the verb prints the value and nothing
/// else: no newline, no label, empty stderr.
#[tokio::test(flavor = "multi_thread")]
async fn resolve_prints_only_the_granted_value() {
    let live = live("resolve-ok-9072").await;
    let token = grant(&live, &["API_KEY"]).await;
    let out = resolve(live.home.path(), "API_KEY", Some(&token)).await;
    assert!(
        out.status.success(),
        "status {}; stderr {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, VALUE.as_bytes());
    assert!(out.stderr.is_empty(), "stderr is not empty");
}

/// Why: #9072 — no grant, a malformed token, a token the server never
/// minted, a key outside the grant and a server that is down each exit
/// non-zero with their own fixed text and nothing on stdout.
#[tokio::test(flavor = "multi_thread")]
async fn resolve_failures_exit_nonzero_with_a_fixed_error_and_no_value() {
    let live = live("resolve-fail-9072").await;
    let token = grant(&live, &["API_KEY"]).await;
    let down = tempfile::Builder::new()
        .prefix("tsd")
        .tempdir_in("/tmp")
        .expect("down home");
    let home = live.home.path();
    let cases: [(&str, &Path, &str, Option<&str>, &str); 5] = [
        ("no grant", home, "API_KEY", None, "no exec grant"),
        (
            "malformed token",
            home,
            "API_KEY",
            Some("not-a-token"),
            "the grant token is unreadable or malformed",
        ),
        (
            "forged token",
            home,
            "API_KEY",
            Some(FORGED),
            "trusty-secrets refused the key (grant_refused)",
        ),
        (
            "key outside the grant",
            home,
            "OTHER_KEY",
            Some(&token),
            "trusty-secrets refused the key (grant_refused)",
        ),
        (
            "service down",
            down.path(),
            "API_KEY",
            Some(&token),
            "trusty-secrets is not running",
        ),
    ];
    for (label, home, key, token, expected) in cases {
        let out = resolve(home, key, token).await;
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{label}: {stderr}");
        assert!(out.stdout.is_empty(), "{label}: stdout is not empty");
        assert!(
            stderr.contains(&format!("tm secrets resolve: {expected}")),
            "{label}: {stderr}"
        );
    }
}

/// Why: #9072 — no error, log or trace line carries the value or the token,
/// even when the value itself is typed as the key, with tracing at TRACE.
#[tokio::test(flavor = "multi_thread")]
async fn resolve_error_output_never_carries_the_value_or_the_token() {
    let live = live("resolve-leak-9072").await;
    let token = grant(&live, &["API_KEY"]).await;
    let home = live.home.path();
    for (label, key, fd) in [
        ("value as key", VALUE, Some(token.as_str())),
        ("key outside the grant", "OTHER_KEY", Some(token.as_str())),
        ("forged token", "API_KEY", Some(FORGED)),
        ("invalid key", "!bad key", Some(token.as_str())),
    ] {
        let out = resolve(home, key, fd).await;
        assert!(!out.status.success(), "{label}: resolved");
        assert_carries_none(label, &out, &[VALUE, &token, FORGED]);
    }
}
