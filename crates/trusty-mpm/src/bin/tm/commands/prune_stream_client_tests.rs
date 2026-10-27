//! #7619 PR 2: the CLI's stream reader prints each worktree, falls back to the
//! unary request on an older daemon, and never reads a cut stream as success.
//!
//! Every daemon here is a loopback `RpcRouter` on a temp socket.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::Parser;
use serde_json::{Value, json};
use trusty_common::uds::server::{RpcError, RpcRouter, RpcServeOptions, serve_until};
use trusty_mpm::client::DaemonClient;
use trusty_mpm::daemon::managed_routes::prune_stream::PRUNE_STREAM_METHOD;

use super::{StreamReply, send_prune, stream_prune};
use crate::cli::{Cli, Command, SessionAction};
use crate::commands::managed_merged_prs::force_banner;

/// Serve `router` on a fresh socket until the returned sender fires or drops.
fn serve(
    router: RpcRouter,
) -> (
    tempfile::TempDir,
    std::path::PathBuf,
    tokio::sync::oneshot::Sender<()>,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("s").join("trusty-mpm.sock");
    std::fs::create_dir_all(socket.parent().expect("parent")).expect("dir");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind");
    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        serve_until(
            &listener,
            Arc::new(router),
            RpcServeOptions::default(),
            async {
                let _ = shutdown.await;
            },
        )
        .await;
    });
    (dir, socket, stop)
}

/// A stream method sending `items` in order, then ending.
fn streaming(items: Vec<Result<Value, RpcError>>) -> RpcRouter {
    RpcRouter::new().typed_stream(PRUNE_STREAM_METHOD, move |_: Value| {
        let items = items.clone();
        async move {
            let (tx, rx) = tokio::sync::mpsc::channel(16);
            tokio::spawn(async move {
                for item in items {
                    let _ = tx.send(item).await;
                }
            });
            Ok(rx)
        }
    })
}

fn never() -> std::future::Pending<()> {
    std::future::pending()
}

async fn stream(socket: &Path, detach: bool) -> anyhow::Result<StreamReply> {
    let client = DaemonClient::over_socket(socket);
    stream_prune(&client, json!({"dry_run": true}), detach, never(), |_| {}).await
}

/// 🔴 #7619 (a), client side: a plain prune — no `--merged-prs` — prints one
/// line per worktree the daemon reports, then hands back the reply body.
/// Fails before #7619: the unary request printed nothing until the end.
#[tokio::test]
async fn worktree_7619_the_plain_prune_prints_a_line_per_worktree() {
    let trees = ["/r/.worktrees/a", "/r/.worktrees/b", "/r/.worktrees/c"];
    let mut items: Vec<Result<Value, RpcError>> = trees
        .iter()
        .enumerate()
        .map(|(n, t)| Ok(json!({"kind": "progress", "line": format!("orphan sweep: inspecting {}/3 {t}", n + 1)})))
        .collect();
    items.push(Ok(json!({"kind": "keepalive", "elapsed_s": 15})));
    items.push(Ok(json!({"kind": "result", "body": {"paths": trees}})));
    let (_dir, socket, _stop) = serve(streaming(items));

    let mut lines = Vec::new();
    let client = DaemonClient::over_socket(&socket);
    let reply = stream_prune(
        &client,
        json!({"dry_run": true, "merged_prs": false}),
        false,
        never(),
        |l| lines.push(l),
    )
    .await
    .expect("the stream completes");

    let StreamReply::Body(body) = reply else {
        panic!("expected the body, got {reply:?}")
    };
    assert_eq!(body["paths"], json!(trees));
    for t in trees {
        assert!(
            lines.iter().any(|l| l.contains(t)),
            "no line names {t}: {lines:?}"
        );
    }
    assert!(
        lines.iter().any(|l| l.contains("15s elapsed")),
        "the keepalive prints: {lines:?}"
    );
}

/// 🔴 #7619 (d2): a daemon without the stream method — one serving other
/// streams, and one serving none — reads as `Unsupported`, and `send_prune`
/// then sends the unary request once and says so.
#[tokio::test]
async fn worktree_7619_a_daemon_without_the_stream_falls_back_to_unary() {
    let other_streams =
        RpcRouter::new().typed_stream("mpm.control.connect", |_: Value| async move {
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        });
    let unary_only = RpcRouter::new().typed("mpm.managed.prune_worktrees", |_: Value| async move {
        Ok::<_, RpcError>(json!({}))
    });
    for router in [other_streams, unary_only] {
        let (_dir, socket, _stop) = serve(router);
        let reply = stream(&socket, false)
            .await
            .expect("a refusal is not an error");
        assert!(matches!(reply, StreamReply::Unsupported(_)), "{reply:?}");

        let mut streaming = true;
        let sent = std::cell::Cell::new(0);
        let client = DaemonClient::over_socket(&socket);
        let body = send_prune(
            &client,
            json!({"dry_run": true}),
            false,
            &mut streaming,
            |payload| {
                sent.set(sent.get() + 1);
                async move { Ok(json!({"unary": payload})) }
            },
        )
        .await
        .expect("the fallback answers");
        assert!(!streaming, "the fallback is remembered for --force");
        assert_eq!(sent.get(), 1);
        assert_eq!(body, Some(json!({"unary": {"dry_run": true}})));
    }
}

/// 🔴 #7619 (d2), Fail-Open Check: a stream that ends with no result is an
/// error, not an empty success; a daemon error after progress is that error,
/// never a fallback that would send the request a second time.
#[tokio::test]
async fn worktree_7619_a_stream_without_its_result_is_an_error() {
    let progress = || Ok(json!({"kind": "progress", "line": "orphan sweep: removing 1/2 /r/x"}));
    let (_dir, socket, _stop) = serve(streaming(vec![progress()]));
    let err = stream(&socket, false)
        .await
        .expect_err("a cut stream fails");
    assert!(err.to_string().contains("without its result"), "{err}");

    let refused = Err(RpcError::new(
        trusty_common::uds::server::CODE_STREAM_UNSUPPORTED,
        "late",
    ));
    let (_dir, socket, _stop) = serve(streaming(vec![progress(), refused]));
    let err = stream(&socket, false)
        .await
        .expect_err("a late refusal is an error");
    assert!(err.to_string().contains("late"), "{err}");
}

/// 🔴 #7619: an interrupt drops the socket — the daemon's channel closes,
/// which is its cancel — and the error says a removal in flight finishes.
#[tokio::test]
async fn worktree_7619_an_interrupt_drops_the_stream_and_says_so() {
    let closed = Arc::new(AtomicBool::new(false));
    let seen = Arc::clone(&closed);
    let router = RpcRouter::new().typed_stream(PRUNE_STREAM_METHOD, move |_: Value| {
        let seen = Arc::clone(&seen);
        async move {
            let (tx, rx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                let _ = tx
                    .send(Ok(json!({"kind": "progress", "line": "removing 1/5"})))
                    .await;
                tx.closed().await;
                seen.store(true, Ordering::SeqCst);
            });
            Ok(rx)
        }
    });
    let (_dir, socket, _stop) = serve(router);
    let (fire, fired) = tokio::sync::oneshot::channel::<()>();
    let mut fire = Some(fire);
    let client = DaemonClient::over_socket(&socket);
    let err = stream_prune(
        &client,
        json!({}),
        false,
        async {
            let _ = fired.await;
        },
        |_| {
            if let Some(fire) = fire.take() {
                let _ = fire.send(());
            }
        },
    )
    .await
    .expect_err("an interrupt is an error");
    assert!(
        err.to_string().contains("removal in flight finishes"),
        "{err}"
    );

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !closed.load(Ordering::SeqCst) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        closed.load(Ordering::SeqCst),
        "the daemon never saw the client leave"
    );
}

/// #7619 `--detach`: the request carries `detach`, and the reader returns on
/// the daemon's acknowledgement instead of waiting for the pass.
#[tokio::test]
async fn worktree_7619_detach_returns_on_the_acknowledgement() {
    let router = RpcRouter::new().typed_stream(PRUNE_STREAM_METHOD, |params: Value| async move {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        let kind = if params["detach"] == true {
            "detached"
        } else {
            "progress"
        };
        tokio::spawn(async move {
            let _ = tx.send(Ok(json!({"kind": kind}))).await;
            // The pass would run on; hold the stream open.
            tx.closed().await;
        });
        Ok(rx)
    });
    let (_dir, socket, _stop) = serve(router);
    let reply = tokio::time::timeout(Duration::from_secs(10), stream(&socket, true))
        .await
        .expect("returns on the acknowledgement")
        .expect("no error");
    assert!(matches!(reply, StreamReply::Detached), "{reply:?}");
}

/// #7619: `--detach` on a daemon that cannot stream sends no removal request.
#[tokio::test]
async fn worktree_7619_detach_without_the_stream_sends_nothing() {
    let client = DaemonClient::new("http://127.0.0.1:1");
    let mut streaming = true;
    let sent = std::cell::Cell::new(false);
    let err = send_prune(
        &client,
        json!({"dry_run": false}),
        true,
        &mut streaming,
        |_| {
            sent.set(true);
            async { Ok(json!({})) }
        },
    )
    .await
    .expect_err("detach needs the stream");
    assert!(err.to_string().contains("--detach"), "{err}");
    assert!(!sent.get(), "a removal request was sent");
}

/// #7619: the `--force` banner states what Ctrl-C does on each transport.
#[test]
fn worktree_7619_the_force_banner_states_what_ctrl_c_does() {
    assert!(force_banner(3, true, false).contains("Ctrl-C stops new removals"));
    assert!(force_banner(3, true, true).contains("keeps going after this command exits"));
    assert!(force_banner(3, false, false).contains("not the daemon's pass"));
}

/// #7619: `--detach` parses only beside `--force`.
#[test]
fn cli_prune_worktrees_detach_requires_force() {
    assert!(Cli::try_parse_from(["tm", "session", "prune-worktrees", "--detach"]).is_err());
    match Cli::try_parse_from(["tm", "session", "prune-worktrees", "--force", "--detach"])
        .expect("parses")
        .command
    {
        Some(Command::Session {
            action: SessionAction::PruneWorktrees { detach, force, .. },
        }) => assert!(detach && force),
        other => panic!("expected session prune-worktrees, got {other:?}"),
    }
}
