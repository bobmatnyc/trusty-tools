//! Shared harness for the `trusty-events` binary tests (#9075).
//!
//! Every test runs the real binary on a socket and log directory inside its
//! own tempdir, with `TRUSTY_DATA_DIR_OVERRIDE` pointed there too, so no test
//! touches the live data directory or the live socket.

#![allow(dead_code)] // each test target uses a different subset

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use trusty_common::control_bus::{
    EventId, HarnessEvent, HarnessPayload, HarnessSource, PushClient, SubscribeFilter,
    SubscribeItem, SubscribeParams, SubscribeRequest,
};
use trusty_common::uds::{
    FramedStream, SocketVerdict, probe_socket_verdict, send_framed_stream_request,
};

/// How long a test waits for the daemon to start, exit or answer.
pub const BOUND: Duration = Duration::from_secs(10);

/// The socket a daemon rooted at `root` binds.
pub fn socket_in(root: &Path) -> PathBuf {
    root.join("s").join("trusty-events.sock")
}

/// The log directory a daemon rooted at `root` writes.
pub fn log_dir_in(root: &Path) -> PathBuf {
    root.join("event_log")
}

/// `trusty-events serve` rooted at `root`, with `extra` arguments appended.
pub fn command(root: &Path, extra: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_trusty-events"));
    cmd.arg("serve")
        .arg("--socket")
        .arg(socket_in(root))
        .arg("--log-dir")
        .arg(log_dir_in(root))
        .args(extra)
        .env(trusty_common::DATA_DIR_OVERRIDE_ENV, root.join("data"))
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    cmd
}

/// A running daemon. Killed on drop if a test did not stop it.
pub struct Daemon {
    child: Child,
    stderr: PathBuf,
    pub socket: PathBuf,
}

/// Spawn the daemon with stderr captured to `root/<name>.stderr`.
pub fn spawn(root: &Path, name: &str, extra: &[&str]) -> Daemon {
    std::fs::create_dir_all(root).expect("create test root");
    let stderr = root.join(format!("{name}.stderr"));
    let file = File::create(&stderr).expect("create stderr capture");
    let child = command(root, extra)
        .stderr(Stdio::from(file))
        .spawn()
        .expect("spawn trusty-events");
    Daemon {
        child,
        stderr,
        socket: socket_in(root),
    }
}

/// Spawn the daemon and wait until its socket answers.
pub async fn start(root: &Path, extra: &[&str]) -> Daemon {
    let mut daemon = spawn(root, "daemon", extra);
    let deadline = tokio::time::Instant::now() + BOUND;
    loop {
        // The socket answers from the bind, before the log is open; the
        // "serving" line is logged once the log is open too.
        if daemon.stderr_text().contains("trusty-events serving")
            && probe_socket_verdict(&daemon.socket, Duration::from_millis(200)).await
                == SocketVerdict::Serving
        {
            return daemon;
        }
        if let Ok(Some(status)) = daemon.child.try_wait() {
            panic!(
                "trusty-events exited before serving ({status}); stderr:\n{}",
                daemon.stderr_text()
            );
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "trusty-events did not serve within {BOUND:?}; stderr:\n{}",
            daemon.stderr_text()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

impl Daemon {
    /// What the daemon wrote to stderr so far.
    pub fn stderr_text(&self) -> String {
        std::fs::read_to_string(&self.stderr).unwrap_or_default()
    }

    /// Wait for the process to exit on its own, up to `bound`.
    pub async fn wait_exit(&mut self, bound: Duration) -> Option<ExitStatus> {
        let deadline = tokio::time::Instant::now() + bound;
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Some(status);
            }
            if tokio::time::Instant::now() >= deadline {
                return None;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// SIGKILL the daemon and reap it: no shutdown path runs.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Send SIGTERM (what launchd sends) without waiting.
    pub fn sigterm(&self) {
        let pid = self.child.id().to_string();
        let sent = Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .expect("run kill");
        assert!(sent.success(), "kill -TERM {pid} failed");
    }

    /// SIGTERM the daemon (what launchd sends) and wait for it to exit.
    pub async fn terminate(&mut self) -> ExitStatus {
        let pid = self.child.id().to_string();
        let sent = Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .expect("run kill");
        assert!(sent.success(), "kill -TERM {pid} failed");
        self.wait_exit(BOUND).await.unwrap_or_else(|| {
            panic!(
                "trusty-events ignored SIGTERM; stderr:\n{}",
                self.stderr_text()
            )
        })
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A `Ping` event tagged with `session`.
pub fn event(session: &str) -> HarnessEvent {
    payload_event(session, HarnessPayload::Ping)
}

/// A `Hook` event carrying `bytes` bytes of data, to fill socket buffers fast.
pub fn large_event(session: &str, bytes: usize) -> HarnessEvent {
    payload_event(
        session,
        HarnessPayload::Hook {
            kind: "fill".to_string(),
            data: serde_json::Value::String("x".repeat(bytes)),
        },
    )
}

fn payload_event(session: &str, payload: HarnessPayload) -> HarnessEvent {
    HarnessEvent {
        source: HarnessSource::Mpm,
        session: Some(session.to_string()),
        seq: 0,
        at: chrono::Utc::now(),
        payload,
        id: Default::default(),
        parent_id: None,
    }
}

/// Push `events` through a `PushClient`, as a producer does, and require
/// every frame to be written.
pub async fn push(socket: &Path, events: Vec<HarnessEvent>) {
    let count = events.len();
    let client = PushClient::new(socket);
    for event in events {
        client.send(event);
    }
    assert_eq!(
        client.dropped(),
        0,
        "the test overflowed its own PushClient"
    );
    let outcome = client.flush().await;
    assert!(outcome.error.is_none(), "flush failed: {:?}", outcome.error);
    assert_eq!(outcome.sent, count, "flush wrote every buffered frame");
}

/// Open an `events.subscribe` stream.
///
/// The client's per-frame timeout is set far above [`BOUND`]: a live
/// subscription is quiet between events, and each test bounds its own reads.
pub async fn subscribe(
    socket: &Path,
    since_seq: Option<u64>,
    filters: SubscribeFilter,
) -> FramedStream<SubscribeItem> {
    resume(socket, since_seq, None, filters).await
}

/// Open an `events.subscribe` stream that resumes at `since_seq`, naming the
/// event the reader holds there (#9075).
pub async fn resume(
    socket: &Path,
    since_seq: Option<u64>,
    since_id: Option<EventId>,
    filters: SubscribeFilter,
) -> FramedStream<SubscribeItem> {
    let request = SubscribeRequest::new(
        1,
        SubscribeParams {
            since_seq,
            since_id,
            filters,
        },
    );
    send_framed_stream_request(socket, &request, Duration::from_secs(600))
        .await
        .expect("open events.subscribe")
}

/// The next frame of `stream`, whatever it is, bounded by [`BOUND`].
pub async fn next_frame(
    stream: &mut FramedStream<SubscribeItem>,
) -> Option<Result<SubscribeItem, trusty_common::uds::UdsRpcError>> {
    tokio::time::timeout(BOUND, stream.next_frame())
        .await
        .expect("a frame or the end of the stream within the bound")
}

/// Today's day file in the log directory under `root`.
pub fn todays_day_file(root: &Path) -> PathBuf {
    let name = format!(
        "{}.ndjson",
        chrono::Utc::now().date_naive().format("%Y-%m-%d")
    );
    log_dir_in(root).join(name)
}

/// Put a FIFO where today's day file goes. The writer's next open or read of
/// it blocks with no peer, so every event after this is broadcast but never
/// written: the writer has not drained when the daemon dies.
pub fn stall_the_writer(root: &Path) -> PathBuf {
    let fifo = todays_day_file(root);
    let made = Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo {}", fifo.display());
    fifo
}

/// Set `path`'s permission bits.
pub fn chmod(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
}

/// The next item, failing the test after [`BOUND`] or on a stream error.
pub async fn next_item(stream: &mut FramedStream<SubscribeItem>) -> SubscribeItem {
    match tokio::time::timeout(BOUND, stream.next_frame()).await {
        Ok(Some(Ok(item))) => item,
        Ok(Some(Err(e))) => panic!("subscription failed: {e}"),
        Ok(None) => panic!("subscription ended"),
        Err(_) => panic!("no subscription item within {BOUND:?}"),
    }
}

/// The next item, which must be an event.
pub async fn next_full_event(stream: &mut FramedStream<SubscribeItem>) -> HarnessEvent {
    match next_item(stream).await {
        SubscribeItem::Event { event, .. } => *event,
        other => panic!("expected an event, got {other:?}"),
    }
}

/// The next item, which must be an event; returns `(seq, session, persisted)`.
pub async fn next_event(stream: &mut FramedStream<SubscribeItem>) -> (u64, String, bool) {
    match next_item(stream).await {
        SubscribeItem::Event { event, persisted } => {
            (event.seq, event.session.unwrap_or_default(), persisted)
        }
        other => panic!("expected an event, got {other:?}"),
    }
}
