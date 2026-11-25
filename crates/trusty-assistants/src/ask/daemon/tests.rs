//! The ask daemon in process, offline (#11650).
//!
//! Why: the socket, singleton, peer, cancel and idle-exit rules are proven
//! here against a real Unix socket and the scripted adapter. The built
//! binary's on-demand start, reuse and races are in `tests/ask_daemon.rs`.
//! Every test bounds its waits and leaves no socket behind.
//! Test: this module IS the test surface.

use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::client::{ask_home_in, parse_reply};
use super::*;
use crate::ask::tests::{Scripted, empty_servers, instance, text, tool_call};
use crate::ask::{AskEngine, OKG_SEARCH_TOOL_NAME, TurnLog};

/// A short directory under `/tmp`, so socket paths fit `sun_path` on macOS.
fn short_dir() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("ta-")
        .tempdir_in("/tmp")
        .expect("tempdir")
}

fn factory(home: &AssistantHome, llm: &Arc<Scripted>, servers: PathBuf) -> EngineFactory {
    let (home, llm) = (home.clone(), Arc::clone(llm));
    Arc::new(move || {
        let llm: Arc<dyn trusty_code::llm::InferenceAdapter> = llm.clone();
        Ok(AskEngine::new(home.clone(), llm).with_mcp_config(servers.clone()))
    })
}

/// A serving daemon. Dropping `lease` releases it; `stop` shuts it down.
struct Running {
    task: JoinHandle<ServeSummary>,
    lease: Option<oneshot::Sender<()>>,
    stop: Option<oneshot::Sender<()>>,
}

impl Running {
    fn release(&mut self) {
        self.lease.take();
    }

    async fn finish(mut self) -> ServeSummary {
        self.release();
        tokio::time::timeout(Duration::from_secs(20), self.task)
            .await
            .expect("daemon exits")
            .expect("join")
    }
}

async fn serve(
    socket: &Path,
    factory: EngineFactory,
    own_uid: u32,
    health: Option<PathBuf>,
) -> Running {
    let Bound::Serving(daemon) = bind(socket).await.expect("bind") else {
        panic!("socket already served");
    };
    let (lease_tx, lease_rx) = oneshot::channel::<()>();
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let options = ServeOptions {
        health_socket: health,
        own_uid,
    };
    let lease = async move {
        let _ = lease_rx.await;
    };
    let shutdown = async move {
        if stop_rx.await.is_err() {
            std::future::pending::<()>().await;
        }
    };
    let task = tokio::spawn(daemon.serve(options, factory, lease, shutdown));
    Running {
        task,
        lease: Some(lease_tx),
        stop: Some(stop_tx),
    }
}

fn own_uid() -> u32 {
    trusty_common::uds::self_uid()
}

#[test]
fn paths_follow_the_owners_spelling() {
    let id = AssistantInstanceId::new("izzie").unwrap();
    let dir = Path::new("/data/trusty-agents");
    assert_eq!(
        instance_socket_in(dir, &id),
        Path::new("/data/trusty-agents/trusty-agents-izzie.sock")
    );
    assert_eq!(
        instance_log_in(dir, &id),
        Path::new("/data/trusty-agents/trusty-agents-izzie.log")
    );
}

#[test]
fn an_unknown_instance_is_named_with_its_path() {
    let tmp = tempfile::tempdir().unwrap();
    let err = resolve_home(Some(tmp.path()), "nosuch").unwrap_err();
    let text = err.to_string();
    assert!(
        matches!(err, AskDaemonError::UnknownInstance { .. }),
        "{text}"
    );
    assert!(
        text.contains("unknown assistant instance `nosuch`"),
        "{text}"
    );
    assert!(
        text.contains(&tmp.path().join("nosuch").display().to_string()),
        "{text}"
    );
    // #11742: an empty root says no instance exists.
    assert!(text.contains("no instances exist yet"), "{text}");
    let err = resolve_home(Some(tmp.path()), "../x").unwrap_err();
    assert!(matches!(err, AskDaemonError::InvalidInstance(_)), "{err}");
}

#[tokio::test]
async fn an_ask_round_trips_and_the_daemon_exits_idle() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let dir = short_dir();
    let socket = instance_socket_in(&dir.path().join("d"), home.id());
    let llm = Arc::new(Scripted::new(vec![text("Hello, Robert.")]));
    let running = serve(
        &socket,
        factory(&home, &llm, empty_servers(&tmp)),
        own_uid(),
        None,
    )
    .await;

    let answer = ask_at(&socket, "Say hello").await.expect("answer");
    assert_eq!(answer, "Hello, Robert.");

    let summary = running.finish().await;
    assert_eq!(summary.exit, ServeExit::Idle);
    assert_eq!((summary.accepted, summary.answered), (1, 1), "{summary:?}");
    assert!(!socket.exists(), "the daemon removes its socket on exit");
    let turns = TurnLog::for_home(&home).read().unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].answer, "Hello, Robert.");
}

#[tokio::test]
async fn the_daemon_stays_while_a_connection_is_open() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let dir = short_dir();
    let socket = instance_socket_in(&dir.path().join("d"), home.id());
    let llm = Arc::new(Scripted::new(vec![text("one"), text("two")]));
    let mut running = serve(
        &socket,
        factory(&home, &llm, empty_servers(&tmp)),
        own_uid(),
        None,
    )
    .await;

    // A connection that sends nothing holds the daemon, as an in-flight ask
    // would. It queues ahead of the first ask, so it is accepted first.
    let hold = trusty_common::uds::connect_hardened(&socket)
        .await
        .expect("hold");
    assert_eq!(ask_at(&socket, "first").await.expect("first"), "one");
    running.release();
    assert_eq!(ask_at(&socket, "second").await.expect("second"), "two");
    assert!(
        !running.task.is_finished(),
        "released but a connection is open"
    );

    drop(hold);
    let summary = running.finish().await;
    assert_eq!(summary.exit, ServeExit::Idle);
    assert_eq!((summary.accepted, summary.answered), (3, 2), "{summary:?}");
    assert!(!socket.exists());
}

/// #11650: a connection queued before the release keeps the daemon serving.
///
/// The hold connects and the lease is released before the serve loop first
/// runs, with no await in between, so the loop sees the release before its
/// listener reports the queued connection. The ask after it must find the
/// same daemon still serving its socket.
#[tokio::test]
async fn a_connection_queued_before_the_release_keeps_the_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let dir = short_dir();
    let socket = instance_socket_in(&dir.path().join("d"), home.id());
    let llm = Arc::new(Scripted::new(vec![text("still here")]));
    let mut running = serve(
        &socket,
        factory(&home, &llm, empty_servers(&tmp)),
        own_uid(),
        None,
    )
    .await;

    let hold = std::os::unix::net::UnixStream::connect(&socket).expect("hold");
    running.release();
    assert_eq!(
        ask_at(&socket, "are you there?").await.expect("answer"),
        "still here"
    );
    assert!(
        socket.exists(),
        "the daemon kept its socket while a queued connection was open"
    );
    assert!(!running.task.is_finished(), "the hold keeps the daemon");

    drop(hold);
    let summary = running.finish().await;
    assert_eq!(summary.exit, ServeExit::Idle);
    assert_eq!((summary.accepted, summary.answered), (2, 1), "{summary:?}");
    assert!(!socket.exists());
}

#[tokio::test]
async fn a_client_disconnect_cancels_its_ask() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let dir = short_dir();
    let socket = instance_socket_in(&dir.path().join("d"), home.id());
    let in_chat = Arc::new(tokio::sync::Notify::new());
    let signal = Arc::clone(&in_chat);
    let llm = Arc::new(
        Scripted::new(vec![
            tool_call(OKG_SEARCH_TOOL_NAME, json!({ "query": "x" })),
            text("never reached"),
        ])
        .on_call(move || signal.notify_one())
        .delayed(Duration::from_millis(300)),
    );
    let running = serve(
        &socket,
        factory(&home, &llm, empty_servers(&tmp)),
        own_uid(),
        None,
    )
    .await;

    let mut stream = trusty_common::uds::connect_hardened(&socket)
        .await
        .expect("connect");
    let request = json!({ "protocol": PROTOCOL_VERSION, "question": "Find x" });
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .expect("send");
    tokio::time::timeout(Duration::from_secs(10), in_chat.notified())
        .await
        .expect("the model call started");
    drop(stream);

    let summary = running.finish().await;
    assert_eq!((summary.abandoned, summary.answered), (1, 0), "{summary:?}");
    assert_eq!(llm.requests().len(), 1, "no model call after the hang-up");
    assert!(
        !TurnLog::for_home(&home).path().exists(),
        "the log is unchanged"
    );
}

#[tokio::test]
async fn a_connection_from_another_uid_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let dir = short_dir();
    let socket = instance_socket_in(&dir.path().join("d"), home.id());
    let llm = Arc::new(Scripted::new(vec![text("unused")]));
    // The daemon expects a uid this test does not run as.
    let foreign = own_uid().wrapping_add(1);
    let running = serve(
        &socket,
        factory(&home, &llm, empty_servers(&tmp)),
        foreign,
        None,
    )
    .await;

    // #11650: the daemon closes the refused connection while the client may
    // still be writing, so the client sees a clean EOF or a reset/broken pipe.
    let result = ask_at(&socket, "hi").await;
    let refused = match &result {
        Err(AskDaemonError::Disconnected) => true,
        Err(AskDaemonError::Io { source, .. }) => matches!(
            source.kind(),
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
        ),
        _ => false,
    };
    assert!(refused, "{result:?}");
    let summary = running.finish().await;
    assert_eq!((summary.refused, summary.accepted), (1, 0), "{summary:?}");
    assert!(
        llm.requests().is_empty(),
        "a refused peer never reaches the model"
    );
}

#[tokio::test]
async fn a_second_daemon_cannot_bind_a_served_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let dir = short_dir();
    let socket = instance_socket_in(&dir.path().join("d"), home.id());
    let llm = Arc::new(Scripted::new(vec![text("still me")]));
    let running = serve(
        &socket,
        factory(&home, &llm, empty_servers(&tmp)),
        own_uid(),
        None,
    )
    .await;
    let inode = std::fs::symlink_metadata(&socket).unwrap().ino();

    assert!(matches!(bind(&socket).await.expect("bind"), Bound::Busy));
    assert_eq!(
        std::fs::symlink_metadata(&socket).unwrap().ino(),
        inode,
        "never unlinked"
    );
    assert_eq!(ask_at(&socket, "who?").await.expect("answer"), "still me");
    running.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_binds_end_with_exactly_one_daemon() {
    let dir = short_dir();
    for round in 0..10 {
        let socket = dir
            .path()
            .join(format!("d{round}"))
            .join("trusty-agents-race.sock");
        let binds: Vec<_> = (0..4)
            .map(|_| {
                let socket = socket.clone();
                tokio::spawn(async move { bind(&socket).await })
            })
            .collect();
        let mut serving = Vec::new();
        for b in binds {
            if let Bound::Serving(d) = b.await.expect("join").expect("bind") {
                serving.push(d);
            }
        }
        assert_eq!(serving.len(), 1, "round {round}: exactly one daemon binds");
        std::fs::remove_file(&socket).unwrap();
    }
}

#[tokio::test]
async fn a_stale_socket_file_is_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let dir = short_dir();
    let parent = dir.path().join("d");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = parent.join("trusty-agents-stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    assert!(
        std::fs::symlink_metadata(&socket)
            .unwrap()
            .file_type()
            .is_socket(),
        "the stale socket file is present"
    );
    let dead = std::os::unix::net::UnixStream::connect(&socket);
    assert!(dead.is_err(), "nothing serves the stale socket: {dead:?}");

    // #11650: Linux can reuse the inode after unlink + rebind, so the proof of
    // replacement is the new daemon answering on the old path.
    let llm = Arc::new(Scripted::new(vec![text("new daemon")]));
    let running = serve(
        &socket,
        factory(&home, &llm, empty_servers(&tmp)),
        own_uid(),
        None,
    )
    .await;
    assert_eq!(ask_at(&socket, "who?").await.expect("answer"), "new daemon");
    let summary = running.finish().await;
    assert_eq!((summary.accepted, summary.answered), (1, 1), "{summary:?}");
    assert!(!socket.exists(), "the daemon removes its socket on exit");
}

#[tokio::test]
async fn the_socket_is_0600_in_a_0700_directory() {
    let dir = short_dir();
    let parent = dir.path().join("d");
    std::fs::create_dir(&parent).unwrap();
    std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let socket = parent.join("trusty-agents-mode.sock");

    let Bound::Serving(daemon) = bind(&socket).await.expect("bind") else {
        panic!("busy");
    };
    let mode = |p: &Path| std::fs::symlink_metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&socket), 0o600);
    assert_eq!(mode(&parent), 0o700, "a wide directory is narrowed");
    drop(daemon);
    std::fs::remove_file(&socket).unwrap();
}

#[tokio::test]
async fn a_protocol_mismatch_is_refused_both_ways() {
    let err = parse_reply(r#"{"protocol":999,"daemon_pid":1,"outcome":"answer","answer":"x"}"#)
        .unwrap_err();
    assert!(
        matches!(
            err,
            AskDaemonError::ProtocolMismatch {
                ours: PROTOCOL_VERSION,
                theirs: 999
            }
        ),
        "{err}"
    );
    assert!(
        err.to_string().contains("protocol version mismatch"),
        "{err}"
    );

    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let dir = short_dir();
    let socket = instance_socket_in(&dir.path().join("d"), home.id());
    let llm = Arc::new(Scripted::new(vec![]));
    let running = serve(
        &socket,
        factory(&home, &llm, empty_servers(&tmp)),
        own_uid(),
        None,
    )
    .await;
    let stream = trusty_common::uds::connect_hardened(&socket)
        .await
        .expect("connect");
    let (read, mut write) = stream.into_split();
    write
        .write_all(b"{\"protocol\":999,\"question\":\"hi\"}\n")
        .await
        .unwrap();
    let mut line = String::new();
    BufReader::new(read).read_line(&mut line).await.unwrap();
    let reply: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(reply["kind"], "protocol_mismatch", "{reply}");
    assert!(
        reply["message"].as_str().unwrap().contains("999"),
        "{reply}"
    );
    drop(write);
    running.finish().await;
    assert!(llm.requests().is_empty());
}

#[tokio::test]
async fn the_health_socket_is_served_and_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let dir = short_dir();
    let socket = instance_socket_in(&dir.path().join("d"), home.id());
    let health = dir.path().join("d").join("trusty-agents.sock");
    let llm = Arc::new(Scripted::new(vec![]));
    let running = serve(
        &socket,
        factory(&home, &llm, empty_servers(&tmp)),
        own_uid(),
        Some(health.clone()),
    )
    .await;

    let request = json!({"jsonrpc": "2.0", "id": 1, "method": "agents.health", "params": {}});
    let mut answer: Option<Value> = None;
    for _ in 0..100 {
        match trusty_common::uds::send_framed_request::<_, Value>(
            &health,
            &request,
            Duration::from_secs(2),
        )
        .await
        {
            Ok(v) => {
                answer = Some(v);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    let answer = answer.expect("agents.health answered");
    assert_eq!(answer["result"]["status"], "ok", "{answer}");

    running.finish().await;
    assert!(!socket.exists());
    assert!(
        !health.exists(),
        "the daemon removes the health socket it bound"
    );
}

#[tokio::test]
async fn shutdown_removes_the_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let dir = short_dir();
    let socket = instance_socket_in(&dir.path().join("d"), home.id());
    let llm = Arc::new(Scripted::new(vec![]));
    let mut running = serve(
        &socket,
        factory(&home, &llm, empty_servers(&tmp)),
        own_uid(),
        None,
    )
    .await;
    let _ = running.stop.take().expect("stop").send(());
    let summary = tokio::time::timeout(Duration::from_secs(20), running.task)
        .await
        .expect("exits")
        .expect("join");
    assert_eq!(summary.exit, ServeExit::Shutdown);
    assert!(!socket.exists());
}

#[tokio::test]
async fn a_daemon_that_dies_at_start_names_its_log() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let dir = short_dir();
    let launcher =
        DaemonLauncher::program("/bin/sh").with_args(["-c", "echo daemon-boom >&2; exit 3"]);
    let err = ask_home_in(&home, dir.path(), "hi", &launcher)
        .await
        .unwrap_err();
    let log = instance_log_in(dir.path(), home.id());
    let text = err.to_string();
    assert!(matches!(err, AskDaemonError::StartFailed { .. }), "{text}");
    assert!(text.contains(&log.display().to_string()), "{text}");
    assert!(text.contains("daemon-boom"), "{text}");
    assert!(text.contains('3'), "{text}");
}

#[tokio::test]
async fn a_daemon_that_never_gets_ready_is_stopped() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let dir = short_dir();
    let launcher = DaemonLauncher::program("/bin/sh")
        .with_args(["-c", "exec sleep 30"])
        .with_budget(Duration::from_millis(300));
    let err = ask_home_in(&home, dir.path(), "hi", &launcher)
        .await
        .unwrap_err();
    assert!(matches!(err, AskDaemonError::StartTimeout { .. }), "{err}");
    assert!(err.to_string().contains("its log is"), "{err}");
}
