//! In-process tests for the LSP framing codec, the JSON-RPC client, document
//! versioning and configuration (#9078, L2).
//!
//! Why: framing and correlation bugs are cheapest to pin over an in-memory
//! duplex pipe, where the test plays the server byte by byte. The subprocess
//! half (spawn, crash, idle reap, leaks) lives in `tests/lsp_child.rs`.
//! What: a scripted peer on the far end of `tokio::io::duplex` reads the
//! client's frames and writes back whatever the test dictates.
//! Test: this module is the test.

use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};

use super::config::{LspConfig, ENV_LSP, ENV_LSP_IDLE_SECS, ENV_LSP_RUST_ANALYZER};
use super::documents::check_version;
use super::framing::{encode, FrameDecoder, MAX_FRAME_BYTES};
use super::manager::resolve_workspace_root;
use super::{LspClient, LspError, LspLanguage, LspManager};

fn frame(v: &Value) -> Vec<u8> {
    encode(v).expect("encode")
}

/// The server end of a duplex pair: reads the client's frames, writes replies.
struct Peer {
    io: DuplexStream,
    decoder: FrameDecoder,
}

impl Peer {
    async fn next(&mut self) -> Value {
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(v) = self
                .decoder
                .next_message()
                .expect("client sent a valid frame")
            {
                return v;
            }
            let n = self.io.read(&mut chunk).await.expect("peer read");
            assert!(n > 0, "client closed its end before sending a frame");
            self.decoder.extend(&chunk[..n]);
        }
    }

    async fn send(&mut self, v: &Value) {
        self.io.write_all(&frame(v)).await.expect("peer write");
    }
}

fn pair() -> (LspClient, Peer) {
    let (client_io, server_io) = tokio::io::duplex(1 << 16);
    let (rd, wr) = tokio::io::split(client_io);
    let client = LspClient::new(rd, wr);
    let peer = Peer {
        io: server_io,
        decoder: FrameDecoder::default(),
    };
    (client, peer)
}

// ---- framing ---------------------------------------------------------------

#[test]
fn framing_round_trips_a_message() {
    let msg = json!({"jsonrpc": "2.0", "id": 7, "method": "x", "params": {"a": "é"}});
    let mut d = FrameDecoder::default();
    d.extend(&frame(&msg));
    assert_eq!(d.next_message().unwrap(), Some(msg));
    assert_eq!(d.next_message().unwrap(), None);
    assert_eq!(d.buffered(), 0);
}

#[test]
fn framing_reassembles_a_frame_split_across_reads() {
    let msg = json!({"jsonrpc": "2.0", "id": 1, "result": [1, 2, 3]});
    let bytes = frame(&msg);
    let mut d = FrameDecoder::default();
    // Split inside the header, then inside the body.
    for piece in [&bytes[..5], &bytes[5..20], &bytes[20..bytes.len() - 1]] {
        d.extend(piece);
        assert_eq!(d.next_message().unwrap(), None, "no frame until complete");
    }
    d.extend(&bytes[bytes.len() - 1..]);
    assert_eq!(d.next_message().unwrap(), Some(msg));
}

#[test]
fn framing_yields_two_messages_from_one_read() {
    let a = json!({"jsonrpc": "2.0", "id": 1, "result": "a"});
    let b = json!({"jsonrpc": "2.0", "id": 2, "result": "b"});
    let mut bytes = frame(&a);
    bytes.extend(frame(&b));
    let mut d = FrameDecoder::default();
    d.extend(&bytes);
    assert_eq!(d.next_message().unwrap(), Some(a));
    assert_eq!(d.next_message().unwrap(), Some(b));
    assert_eq!(d.next_message().unwrap(), None);
}

#[test]
fn framing_accepts_extra_headers_in_any_case() {
    let body = br#"{"jsonrpc":"2.0","id":3,"result":null}"#;
    let raw = format!(
        "content-type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-LENGTH: {}\r\n\r\n",
        body.len()
    );
    let mut d = FrameDecoder::default();
    d.extend(raw.as_bytes());
    d.extend(body);
    assert_eq!(
        d.next_message().unwrap(),
        Some(json!({"jsonrpc": "2.0", "id": 3, "result": null}))
    );
}

/// #9078 Fail-Open Check: every malformed frame is an error, never a skip.
#[test]
fn framing_rejects_malformed_frames() {
    let too_big = format!("Content-Length: {}\r\n\r\n", MAX_FRAME_BYTES + 1);
    let cases: [(&str, &[u8]); 5] = [
        ("missing length", b"Content-Type: x\r\n\r\n{}"),
        ("non-numeric length", b"Content-Length: ten\r\n\r\n{}"),
        ("header line without colon", b"garbage\r\n\r\n{}"),
        ("body not JSON", b"Content-Length: 3\r\n\r\nabc"),
        ("oversized frame", too_big.as_bytes()),
    ];
    for (name, raw) in cases {
        let mut d = FrameDecoder::default();
        d.extend(raw);
        assert!(
            matches!(d.next_message(), Err(LspError::Protocol(_))),
            "{name}: must be a protocol error"
        );
    }
}

#[test]
fn framing_rejects_an_unterminated_header_block() {
    let mut d = FrameDecoder::default();
    d.extend(&vec![b'a'; 16 * 1024]);
    assert!(matches!(d.next_message(), Err(LspError::Protocol(_))));
}

// ---- client correlation ----------------------------------------------------

#[tokio::test]
async fn client_correlates_responses_that_arrive_out_of_order() {
    let (client, mut peer) = pair();
    let t = Duration::from_secs(5);
    let first = client.request("a", json!({}), t);
    let second = client.request("b", json!({}), t);
    let serve = async {
        let ra = peer.next().await;
        let rb = peer.next().await;
        // Answer the second request first.
        peer.send(&json!({"jsonrpc": "2.0", "id": rb["id"], "result": "for-b"}))
            .await;
        peer.send(&json!({"jsonrpc": "2.0", "id": ra["id"], "result": "for-a"}))
            .await;
        (ra["method"].clone(), rb["method"].clone())
    };
    let (a, b, methods) = tokio::join!(first, second, serve);
    assert_eq!(methods, (json!("a"), json!("b")));
    assert_eq!(a.unwrap(), json!("for-a"));
    assert_eq!(b.unwrap(), json!("for-b"));
}

#[tokio::test]
async fn late_response_after_timeout_does_not_desync() {
    let (client, mut peer) = pair();
    let slow = client
        .request("slow", json!({}), Duration::from_millis(50))
        .await;
    assert!(
        matches!(slow, Err(LspError::Timeout { ref method, .. }) if method == "slow"),
        "{slow:?}"
    );
    assert_eq!(client.consecutive_timeouts(), 1);
    let stale = peer.next().await;
    // The client cancels what it gave up on.
    let cancel = peer.next().await;
    assert_eq!(cancel["method"], "$/cancelRequest");
    assert_eq!(cancel["params"]["id"], stale["id"]);

    let next = client.request("fast", json!({}), Duration::from_secs(5));
    let serve = async {
        let fresh = peer.next().await;
        // The late answer to the timed-out request arrives first.
        peer.send(&json!({"jsonrpc": "2.0", "id": stale["id"], "result": "late"}))
            .await;
        peer.send(&json!({"jsonrpc": "2.0", "id": fresh["id"], "result": "fresh"}))
            .await;
    };
    let (answer, ()) = tokio::join!(next, serve);
    assert_eq!(answer.unwrap(), json!("fresh"));
    assert_eq!(client.consecutive_timeouts(), 0);
}

#[tokio::test]
async fn server_error_response_is_a_typed_error() {
    let (client, mut peer) = pair();
    let call = client.request("m", json!({}), Duration::from_secs(5));
    let serve = async {
        let r = peer.next().await;
        peer.send(&json!({"jsonrpc": "2.0", "id": r["id"],
            "error": {"code": -32601, "message": "nope"}}))
            .await;
    };
    let (res, ()) = tokio::join!(call, serve);
    assert_eq!(
        res,
        Err(LspError::ServerError {
            code: -32601,
            message: "nope".into()
        })
    );
}

#[tokio::test]
async fn peer_closing_mid_request_is_an_error_not_a_hang() {
    let (client, mut peer) = pair();
    let call = client.request("m", json!({}), Duration::from_secs(30));
    let serve = async {
        let _ = peer.next().await;
        drop(peer);
    };
    let (res, ()) =
        tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(call, serve) })
            .await
            .expect("a closed peer must fail the request, not hang it");
    assert!(matches!(res, Err(LspError::ServerExited { .. })), "{res:?}");
    // And every later request fails at once with the same reason.
    let again = client
        .request("m2", json!({}), Duration::from_secs(30))
        .await;
    assert!(
        matches!(again, Err(LspError::ServerExited { .. })),
        "{again:?}"
    );
}

#[tokio::test]
async fn partial_frame_then_eof_is_an_error() {
    let (client, mut peer) = pair();
    let call = client.request("m", json!({}), Duration::from_secs(30));
    let serve = async {
        let _ = peer.next().await;
        peer.io
            .write_all(b"Content-Length: 100\r\n\r\n{\"jsonrpc\"")
            .await
            .unwrap();
        drop(peer);
    };
    let (res, ()) = tokio::join!(call, serve);
    match res {
        Err(LspError::ServerExited { detail }) => assert!(detail.contains("mid-frame"), "{detail}"),
        other => panic!("expected ServerExited mid-frame, got {other:?}"),
    }
}

#[tokio::test]
async fn malformed_frame_closes_the_client_with_a_protocol_error() {
    let (client, mut peer) = pair();
    let call = client.request("m", json!({}), Duration::from_secs(30));
    let serve = async {
        let _ = peer.next().await;
        peer.io
            .write_all(b"Content-Length: 3\r\n\r\nabc")
            .await
            .unwrap();
        peer
    };
    let (res, _peer) = tokio::join!(call, serve);
    assert!(matches!(res, Err(LspError::Protocol(_))), "{res:?}");
    assert!(client.is_closed());
}

/// An `AsyncWrite` whose every write fails, standing in for a dead child's stdin.
struct DeadPipe;

impl AsyncWrite for DeadPipe {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Err(std::io::ErrorKind::BrokenPipe.into()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

/// An `AsyncRead` that never yields, so only the write path can fail.
struct Silent;

impl AsyncRead for Silent {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Pending
    }
}

/// #9078 Fail-Open Check: a write to a dead child is an error.
#[tokio::test]
async fn write_to_a_dead_peer_is_an_error() {
    let client = LspClient::new(Silent, DeadPipe);
    let req = client
        .request("m", json!({}), Duration::from_secs(30))
        .await;
    assert!(matches!(req, Err(LspError::Write(_))), "{req:?}");
    let note = client.notify("n", json!({})).await;
    assert!(matches!(note, Err(LspError::Write(_))), "{note:?}");
}

/// #9078 round 2: a peer that never reads its stdin must not wedge the writer.
/// The request's own deadline covers the write, and a write that cannot finish
/// closes the client.
#[tokio::test]
async fn a_peer_that_never_reads_cannot_wedge_a_request() {
    let (client_io, _unread) = tokio::io::duplex(64);
    let (rd, wr) = tokio::io::split(client_io);
    let client = LspClient::new(rd, wr);
    let pad = "x".repeat(64 * 1024);
    let res = tokio::time::timeout(
        Duration::from_secs(5),
        client.request("m", json!({"pad": pad}), Duration::from_millis(200)),
    )
    .await
    .expect("the write must be bounded by the request timeout");
    assert!(matches!(res, Err(LspError::Timeout { .. })), "{res:?}");
    assert_eq!(client.consecutive_timeouts(), 1);
    assert!(
        client.is_closed(),
        "a write that timed out closes the client"
    );
    let note = tokio::time::timeout(Duration::from_secs(5), client.notify("n", json!({})))
        .await
        .expect("a notification after a wedged write is bounded too");
    assert!(note.is_err(), "{note:?}");
}

/// #9078 round 2: cancelling a request mid-write must never leave half a frame
/// on the wire for the next message to follow.
#[tokio::test]
async fn a_cancelled_request_never_leaves_a_partial_frame() {
    let (client_io, server_io) = tokio::io::duplex(64);
    let (rd, wr) = tokio::io::split(client_io);
    let client = LspClient::new(rd, wr);
    let pad = "y".repeat(16 * 1024);
    let cancelled = tokio::time::timeout(
        Duration::from_millis(20),
        client.request("big", json!({"pad": pad}), Duration::from_secs(30)),
    )
    .await;
    assert!(cancelled.is_err(), "the caller gave up mid-write");
    let mut peer = Peer {
        io: server_io,
        decoder: FrameDecoder::default(),
    };
    let next = client.request("small", json!({}), Duration::from_secs(5));
    let serve = async {
        let first = tokio::time::timeout(Duration::from_secs(5), peer.next())
            .await
            .expect("frame");
        let second = tokio::time::timeout(Duration::from_secs(5), peer.next())
            .await
            .expect("frame");
        peer.send(&json!({"jsonrpc": "2.0", "id": second["id"], "result": "ok"}))
            .await;
        (first, second)
    };
    let (answer, (first, second)) = tokio::join!(next, serve);
    assert_eq!(first["method"], "big", "the cancelled frame arrives whole");
    assert_eq!(
        first["params"]["pad"].as_str().map(str::len),
        Some(16 * 1024)
    );
    assert_eq!(second["method"], "small");
    assert_eq!(answer, Ok(json!("ok")));
}

/// #9078 round 2 Fail-Open Check: a response with neither `result` nor `error`
/// is a protocol error, never a null success.
#[tokio::test]
async fn a_response_without_result_or_error_is_a_protocol_error() {
    let (client, mut peer) = pair();
    let call = client.request("m", json!({}), Duration::from_secs(5));
    let serve = async {
        let r = peer.next().await;
        peer.send(&json!({"jsonrpc": "2.0", "id": r["id"]})).await;
    };
    let (res, ()) = tokio::join!(call, serve);
    assert!(matches!(res, Err(LspError::Protocol(_))), "{res:?}");
}

#[tokio::test]
async fn server_initiated_requests_are_answered() {
    let (client, mut peer) = pair();
    peer.send(&json!({"jsonrpc": "2.0", "id": "s1",
        "method": "window/workDoneProgress/create", "params": {"token": "t"}}))
        .await;
    peer.send(&json!({"jsonrpc": "2.0", "id": 9,
        "method": "workspace/configuration", "params": {"items": [{}, {}]}}))
        .await;
    peer.send(&json!({"jsonrpc": "2.0", "id": 10, "method": "x/unknown"}))
        .await;
    let mut replies = Vec::new();
    for _ in 0..3 {
        replies.push(peer.next().await);
    }
    let by_id = |id: Value| {
        replies
            .iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no reply to {id}"))
            .clone()
    };
    assert_eq!(by_id(json!(10))["error"]["code"], -32601);
    assert_eq!(by_id(json!(9))["result"], json!([null, null]));
    let progress = by_id(json!("s1"));
    assert_eq!(progress["result"], Value::Null);
    assert!(progress.get("error").is_none());
    drop(client);
}

#[tokio::test]
async fn published_diagnostics_are_collected_by_version() {
    let (client, mut peer) = pair();
    let uri = "file:///w/src/lib.rs";
    let wait = client.wait_for_diagnostics(uri, 2, Duration::from_secs(5));
    let serve = async {
        for version in [1, 2] {
            peer.send(
                &json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                "params": {"uri": uri, "version": version, "diagnostics": [{"severity": 1}]}}),
            )
            .await;
        }
    };
    let (got, ()) = tokio::join!(wait, serve);
    let got = got.unwrap();
    assert_eq!(got.version, Some(2));
    assert_eq!(got.diagnostics.len(), 1);
    let timed_out = client
        .wait_for_diagnostics(uri, 3, Duration::from_millis(50))
        .await;
    assert!(
        matches!(timed_out, Err(LspError::Timeout { .. })),
        "{timed_out:?}"
    );
}

// ---- documents -------------------------------------------------------------

#[tokio::test]
async fn documents_open_at_version_one_and_bump_only_on_change() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("lib.rs");
    std::fs::write(&file, "fn a() {}\n").unwrap();
    let (client, mut peer) = pair();

    let opened = client.sync_document(&file, "rust").await.unwrap();
    assert_eq!((opened.version, opened.changed), (1, true));
    let open = peer.next().await;
    assert_eq!(open["method"], "textDocument/didOpen");
    assert_eq!(open["params"]["textDocument"]["version"], 1);
    assert_eq!(open["params"]["textDocument"]["text"], "fn a() {}\n");
    assert_eq!(open["params"]["textDocument"]["uri"], json!(opened.uri));

    let same = client.sync_document(&file, "rust").await.unwrap();
    assert_eq!((same.version, same.changed), (1, false));

    std::fs::write(&file, "fn b() {}\n").unwrap();
    let changed = client.sync_document(&file, "rust").await.unwrap();
    assert_eq!((changed.version, changed.changed), (2, true));
    // The unchanged call sent nothing: the next frame is the didChange.
    let change = peer.next().await;
    assert_eq!(change["method"], "textDocument/didChange");
    assert_eq!(change["params"]["textDocument"]["version"], 2);
    assert_eq!(change["params"]["contentChanges"][0]["text"], "fn b() {}\n");
    assert_eq!(client.document_version(&file).await, Some(2));
}

#[tokio::test]
async fn syncing_a_missing_file_is_a_document_error() {
    let dir = tempfile::tempdir().unwrap();
    let (client, _peer) = pair();
    let res = client
        .sync_document(&dir.path().join("nope.rs"), "rust")
        .await;
    assert!(matches!(res, Err(LspError::Document { .. })), "{res:?}");
}

#[test]
fn a_version_other_than_the_current_one_is_stale() {
    assert_eq!(check_version(3, 3), Ok(()));
    assert_eq!(
        check_version(2, 3),
        Err(LspError::Stale {
            requested: 2,
            current: 3
        })
    );
    // A version from a server that has since restarted (versions reset to 1).
    assert_eq!(
        check_version(5, 1),
        Err(LspError::Stale {
            requested: 5,
            current: 1
        })
    );
}

// ---- configuration, errors, roots -----------------------------------------

fn lookup<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
    move |k| {
        pairs
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| v.to_string())
    }
}

#[test]
fn config_is_off_by_default_and_parses_its_env() {
    let off = LspConfig::from_lookup(lookup(&[]));
    assert!(!off.enabled);
    assert_eq!(off.idle_timeout, Duration::from_secs(120));
    assert_eq!(off.command_for(LspLanguage::Rust).program, "rust-analyzer");
    for (v, want) in [
        ("1", true),
        ("true", true),
        ("ON", true),
        ("0", false),
        ("no", false),
        ("maybe", false),
    ] {
        assert_eq!(
            LspConfig::from_lookup(lookup(&[(ENV_LSP, v)])).enabled,
            want,
            "{ENV_LSP}={v}"
        );
    }
    let tuned = LspConfig::from_lookup(lookup(&[
        (ENV_LSP, "1"),
        (ENV_LSP_IDLE_SECS, "30"),
        (ENV_LSP_RUST_ANALYZER, "/opt/ra"),
    ]));
    assert_eq!(tuned.idle_timeout, Duration::from_secs(30));
    assert_eq!(tuned.command_for(LspLanguage::Rust).program, "/opt/ra");
    let bad_idle = LspConfig::from_lookup(lookup(&[(ENV_LSP_IDLE_SECS, "0")]));
    assert_eq!(bad_idle.idle_timeout, Duration::from_secs(120));
}

#[test]
fn every_error_has_a_stable_code() {
    assert_eq!(LspError::Disabled.code(), "lsp_disabled");
    assert_eq!(
        LspError::NotInstalled {
            program: "x".into()
        }
        .code(),
        "lsp_not_installed"
    );
    assert_eq!(
        LspError::Stale {
            requested: 1,
            current: 2
        }
        .code(),
        "lsp_stale"
    );
}

#[tokio::test]
async fn disabled_manager_refuses_with_lsp_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let m = LspManager::new(LspConfig::default());
    assert!(!m.is_enabled());
    let res = m.session(dir.path(), LspLanguage::Rust).await;
    assert_eq!(res.err(), Some(LspError::Disabled));
    assert!(m.live_pids().await.is_empty());
}

#[test]
fn workspace_root_prefers_the_index_root_then_the_git_toplevel() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let nested = repo.join("crates/a/src");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let file = nested.join("lib.rs");
    std::fs::write(&file, "").unwrap();
    let canon = |p: &Path| p.canonicalize().unwrap();

    let index_root = repo.join("crates/a");
    assert_eq!(
        resolve_workspace_root(&file, Some(&index_root)).unwrap(),
        canon(&index_root)
    );
    assert_eq!(resolve_workspace_root(&file, None).unwrap(), canon(&repo));
    // An index root that does not contain the path is ignored.
    let other = dir.path().join("other");
    std::fs::create_dir_all(&other).unwrap();
    assert_eq!(
        resolve_workspace_root(&file, Some(&other)).unwrap(),
        canon(&repo)
    );
    // Outside any work tree there is no root, and that is an error.
    let loose = dir.path().join("loose.rs");
    std::fs::write(&loose, "").unwrap();
    assert!(matches!(
        resolve_workspace_root(&loose, None),
        Err(LspError::Document { .. })
    ));
}

// ---- process group ---------------------------------------------------------

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// #9078 round 2: the monitor sweeps the group once, at reap time, and no
/// `killpg` on that pgid runs after the leader is reaped.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_group_is_swept_once_at_reap_and_never_signalled_again() {
    use super::group::{monitor, ProcessGroup};
    use std::sync::Arc;

    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("helper.pid");
    // The leader leaves a helper in its group and exits on its own.
    let script = format!("sleep 300 & echo $! > '{}'; exit 0", pidfile.display());
    let mut cmd = tokio::process::Command::new("/bin/sh");
    cmd.args(["-c", &script])
        .stdin(std::process::Stdio::null())
        .process_group(0);
    let child = cmd.spawn().unwrap();
    let pid = child.id().unwrap();
    let group = Arc::new(ProcessGroup::new(pid));
    let (exit_tx, mut exit) = tokio::sync::watch::channel(None);
    let kill = Arc::new(tokio::sync::Notify::new());
    tokio::spawn(monitor(
        child,
        Arc::clone(&group),
        Arc::clone(&kill),
        exit_tx,
    ));

    let status = tokio::time::timeout(Duration::from_secs(5), exit.wait_for(Option::is_some))
        .await
        .expect("the monitor observes the exit")
        .expect("watch")
        .clone();
    assert!(
        status.is_some_and(|s| s.contains('0')),
        "exit status recorded"
    );
    assert!(group.is_gone(), "the reaped group is marked gone");
    assert_eq!(group.signals_sent(), 1, "exactly one sweep, at reap time");

    // The sweep killed the helper the leader left behind.
    let helper: i32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while pid_alive(helper) && std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!pid_alive(helper), "the reap-time sweep killed the helper");

    // Every later kill request is a no-op on the reaped pgid.
    group.kill();
    kill.notify_one();
    group.kill();
    assert_eq!(group.signals_sent(), 1, "no killpg after the reap");
}
