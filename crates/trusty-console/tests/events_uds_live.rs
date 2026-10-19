//! Console's bus client against the real `trusty-events` daemon (#9076).
//!
//! Why: the stub tests in `src/events_uds/tests.rs` prove what the client
//! does with each frame; only the real daemon proves the client and the
//! daemon agree on the contract — that a resume the daemon accepts continues
//! without a gap, that the refusals it really sends lead to a resync, and that
//! the gaps it really reports reach the relay. ADR-0065 condition 1 forbids
//! linking the daemon's library, so these run its binary.
//! What: each test runs `trusty-events serve` on a socket and log directory
//! inside its own tempdir, with `TRUSTY_DATA_DIR_OVERRIDE` pointed there too,
//! pushes events with `PushClient` as a producer does, and reads them through
//! `trusty_console::events_uds`. The binary is the one built beside this
//! test: `cargo build -p trusty-events` first.
//! Test: itself (`#[ignore]`; `--include-ignored`).

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::broadcast;
use trusty_common::control_bus::{
    EventId, HarnessEvent, HarnessPayload, HarnessSource, PushClient,
};
use trusty_common::uds::{SocketVerdict, probe_socket_verdict};
use trusty_console::events_uds::{
    BusState, BusStatus, ClientConfig, EventsRelay, FailureKind, RelayItem, ResumeToken, Start,
    hung_bound, spawn,
};

/// How long any one step may take.
const BOUND: Duration = Duration::from_secs(10);

/// The `trusty-events` binary built beside this test, refused when it is
/// older than the crate's sources: a stale build would test another contract.
fn daemon_binary() -> PathBuf {
    let sibling = Path::new(env!("CARGO_BIN_EXE_trusty-console")).with_file_name("trusty-events");
    let built = std::fs::metadata(&sibling)
        .and_then(|m| m.modified())
        .unwrap_or_else(|e| {
            panic!(
                "no trusty-events binary at {} ({e}): build it in the same cargo run, e.g. \
                 `cargo test -p trusty-events -p trusty-console --test events_uds_live \
                 --test daemon_wire -- --include-ignored`",
                sibling.display()
            )
        });
    let sources = Path::new(env!("CARGO_MANIFEST_DIR")).join("../trusty-events/src");
    let newest = newest_mtime(&sources);
    assert!(
        newest.is_none_or(|newest| built >= newest),
        "{} is older than {}: rebuild it in the same cargo run",
        sibling.display(),
        sources.display()
    );
    sibling
}

/// The newest modification time of any file under `dir`.
fn newest_mtime(dir: &Path) -> Option<std::time::SystemTime> {
    let mut newest = None;
    for entry in std::fs::read_dir(dir).ok()?.filter_map(Result::ok) {
        let path = entry.path();
        let time = if path.is_dir() {
            newest_mtime(&path)
        } else {
            entry.metadata().and_then(|m| m.modified()).ok()
        };
        newest = newest.max(time);
    }
    newest
}

/// A running daemon rooted at a test's tempdir. Killed on drop.
struct Daemon {
    child: Child,
    stderr: PathBuf,
    socket: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        // A test that stopped the daemon may have failed before resuming it.
        let _ = Command::new("kill")
            .args(["-CONT", &self.child.id().to_string()])
            .status();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn socket_in(root: &Path) -> PathBuf {
    root.join("s").join("trusty-events.sock")
}

fn log_dir_in(root: &Path) -> PathBuf {
    root.join("event_log")
}

/// Start the daemon on `root` and wait until it serves.
async fn start(root: &Path, name: &str, extra: &[&str]) -> Daemon {
    let stderr = root.join(format!("{name}.stderr"));
    let file = std::fs::File::create(&stderr).expect("create stderr capture");
    let child = Command::new(daemon_binary())
        .arg("serve")
        .arg("--socket")
        .arg(socket_in(root))
        .arg("--log-dir")
        .arg(log_dir_in(root))
        .args(extra)
        .env(trusty_common::DATA_DIR_OVERRIDE_ENV, root.join("data"))
        .env("RUST_LOG", "info")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(file))
        .spawn()
        .expect("spawn trusty-events");
    let mut daemon = Daemon {
        child,
        stderr,
        socket: socket_in(root),
    };
    let deadline = tokio::time::Instant::now() + BOUND;
    loop {
        let text = std::fs::read_to_string(&daemon.stderr).unwrap_or_default();
        if text.contains("trusty-events serving")
            && probe_socket_verdict(&daemon.socket, Duration::from_millis(200)).await
                == SocketVerdict::Serving
        {
            return daemon;
        }
        if let Ok(Some(status)) = daemon.child.try_wait() {
            panic!("trusty-events exited before serving ({status}):\n{text}");
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "trusty-events did not serve within {BOUND:?}:\n{text}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

impl Daemon {
    /// Send `signal` (`STOP`, `CONT`, ...) to the daemon.
    fn signal(&self, signal: &str) {
        let pid = self.child.id().to_string();
        let sent = Command::new("kill")
            .args([&format!("-{signal}"), &pid])
            .status()
            .expect("run kill");
        assert!(sent.success(), "kill -{signal} {pid}");
    }

    /// SIGTERM, as launchd stops it, and wait for the exit.
    async fn stop(&mut self) {
        let pid = self.child.id().to_string();
        let sent = Command::new("kill")
            .args(["-TERM", &pid])
            .status()
            .expect("run kill");
        assert!(sent.success(), "kill -TERM {pid}");
        let deadline = tokio::time::Instant::now() + BOUND;
        while self.child.try_wait().ok().flatten().is_none() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "trusty-events ignored SIGTERM"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

fn event(session: &str) -> HarnessEvent {
    HarnessEvent {
        source: HarnessSource::Mpm,
        session: Some(session.to_string()),
        seq: 0,
        at: chrono::Utc::now(),
        payload: HarnessPayload::Ping,
        id: EventId::new(),
        parent_id: None,
    }
}

/// Push `sessions` as one producer flush and require every frame written.
async fn push(socket: &Path, sessions: &[&str]) {
    let client = PushClient::new(socket);
    for session in sessions {
        client.send(event(session));
    }
    let outcome = client.flush().await;
    assert!(outcome.error.is_none(), "flush failed: {:?}", outcome.error);
    assert_eq!(outcome.sent, sessions.len());
}

/// The client task, aborted on drop: dropping it is a console restart.
struct Console {
    relay: Arc<EventsRelay>,
    items: broadcast::Receiver<RelayItem>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Console {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn console(socket: &Path, start: Start) -> Console {
    console_with(
        socket,
        start,
        ClientConfig::new(socket.to_path_buf()).heartbeat,
    )
}

/// [`console`] asking the bus for a heartbeat every `heartbeat` (#10071).
fn console_with(socket: &Path, start: Start, heartbeat: Duration) -> Console {
    let mut config = ClientConfig::new(socket.to_path_buf());
    config.start = start;
    config.heartbeat = heartbeat;
    config.liveness_interval = Duration::from_millis(200);
    config.backoff_initial = Duration::from_millis(20);
    config.backoff_max = Duration::from_millis(200);
    let relay = Arc::new(EventsRelay::new());
    let items = relay.subscribe();
    let task = spawn(config, Arc::clone(&relay));
    Console { relay, items, task }
}

impl Console {
    async fn next(&mut self) -> RelayItem {
        match tokio::time::timeout(BOUND, self.items.recv()).await {
            Ok(Ok(item)) => item,
            other => panic!(
                "no relayed item: {other:?}; status {:?}",
                self.relay.status()
            ),
        }
    }

    /// The next item, which must be an event: `(token, session)`.
    async fn next_event(&mut self) -> (ResumeToken, String) {
        match self.next().await {
            RelayItem::Event { token, event, .. } => {
                (token, event.session.clone().unwrap_or_default())
            }
            other => panic!("expected an event, got {other:?}"),
        }
    }

    async fn sessions(&mut self, n: usize) -> Vec<String> {
        let mut out = Vec::new();
        for _ in 0..n {
            out.push(self.next_event().await.1);
        }
        out
    }

    /// Nothing more is relayed within `quiet`.
    async fn nothing_within(&mut self, quiet: Duration) {
        if let Ok(item) = tokio::time::timeout(quiet, self.items.recv()).await {
            panic!("unexpected item: {item:?}");
        }
    }

    async fn status_until(&self, what: &str, f: impl Fn(&BusStatus) -> bool) -> BusStatus {
        let mut watch = self.relay.watch();
        match tokio::time::timeout(BOUND, watch.wait_for(|s| f(s))).await {
            Ok(Ok(status)) => status.clone(),
            _ => panic!("status never reached {what}: {:?}", self.relay.status()),
        }
    }
}

/// The one day file the daemon has written under `root`.
fn day_file(root: &Path) -> PathBuf {
    let files: Vec<_> = std::fs::read_dir(log_dir_in(root))
        .expect("read the log dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ndjson"))
        .collect();
    assert_eq!(files.len(), 1, "one day file: {files:?}");
    files[0].clone()
}

/// Wait until the day file under `root` holds `n` lines.
async fn logged(root: &Path, n: usize) {
    let deadline = tokio::time::Instant::now() + BOUND;
    loop {
        let lines: usize = std::fs::read_dir(log_dir_in(root))
            .map(|dir| {
                dir.filter_map(Result::ok)
                    .filter(|e| e.path().extension().is_some_and(|x| x == "ndjson"))
                    .filter_map(|e| std::fs::read_to_string(e.path()).ok())
                    .map(|text| text.lines().count())
                    .sum()
            })
            .unwrap_or(0);
        if lines >= n {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "{lines} of {n} events logged"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A console restarted with the SSE `Last-Event-ID` it last sent resumes
/// after exactly that event: no repeat, no gap.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "spawns the built trusty-events binary; not linkable per ADR-0065"]
async fn console_resumes_from_its_last_event_id_after_a_restart() {
    let tmp = TempDir::new().expect("tempdir");
    let daemon = start(tmp.path(), "daemon", &[]).await;

    let mut first = console(&daemon.socket, Start::Beginning);
    push(&daemon.socket, &["e1", "e2", "e3"]).await;
    assert_eq!(first.sessions(2).await, ["e1", "e2"]);
    let (token, session) = first.next_event().await;
    assert_eq!(session, "e3");
    let last_event_id = token.to_string();
    drop(first);

    push(&daemon.socket, &["e4", "e5"]).await;
    let resumed: ResumeToken = last_event_id.parse().expect("the SSE id parses back");
    let mut second = console(&daemon.socket, Start::After(resumed));
    assert_eq!(second.sessions(2).await, ["e4", "e5"]);
    second.nothing_within(Duration::from_millis(300)).await;
    let status = second.relay.status();
    assert_eq!((status.resyncs, status.duplicates), (0, 0), "{status:?}");
}

/// The daemon restarts having lost the tail of its log, so the seq the
/// console holds is gone: the resume is refused, the console replays from 0,
/// and what it already relayed is not relayed again.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "spawns the built trusty-events binary; not linkable per ADR-0065"]
async fn a_refused_resume_resyncs_from_zero_without_repeating_events() {
    let tmp = TempDir::new().expect("tempdir");
    let mut first = start(tmp.path(), "first", &[]).await;
    let mut console = console(&first.socket, Start::Beginning);
    push(&first.socket, &["e1", "e2", "e3"]).await;
    assert_eq!(console.sessions(3).await, ["e1", "e2", "e3"]);
    logged(tmp.path(), 3).await;
    first.stop().await;

    // The crash this models: e3 was relayed but its log line did not survive.
    let file = day_file(tmp.path());
    let text = std::fs::read_to_string(&file).expect("read the day file");
    let kept: Vec<&str> = text.lines().take(2).collect();
    std::fs::write(&file, format!("{}\n", kept.join("\n"))).expect("truncate the day file");

    let second = start(tmp.path(), "second", &[]).await;
    let status = console
        .status_until("a resync with two duplicates", |s| {
            s.resyncs >= 1 && s.duplicates >= 2
        })
        .await;
    assert!(
        status.last_error.is_some(),
        "the refusal is visible: {status:?}"
    );
    push(&second.socket, &["e4"]).await;
    assert_eq!(
        console.sessions(1).await,
        ["e4"],
        "e1 and e2 were relayed again"
    );
    console.nothing_within(Duration::from_millis(300)).await;
    let status = console.relay.status();
    assert_eq!((status.resyncs, status.duplicates), (1, 2), "{status:?}");
}

/// Events the ring evicted and the log no longer holds reach the relay as a
/// gap of the right size, before the events that follow it.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "spawns the built trusty-events binary; not linkable per ADR-0065"]
async fn a_gap_the_bus_reports_reaches_the_relay() {
    let tmp = TempDir::new().expect("tempdir");
    let daemon = start(tmp.path(), "daemon", &["--ring-capacity", "2"]).await;
    push(&daemon.socket, &["e1", "e2", "e3", "e4", "e5"]).await;
    logged(tmp.path(), 5).await;
    // Retention, simulated: the log no longer holds e1..e3; the ring holds e4, e5.
    std::fs::remove_file(day_file(tmp.path())).expect("remove the day file");

    let mut console = console(&daemon.socket, Start::Beginning);
    match console.next().await {
        RelayItem::Gap { skipped, after } => {
            assert_eq!((skipped, after), (3, None));
        }
        other => panic!("the gap was not relayed: {other:?}"),
    }
    assert_eq!(console.sessions(2).await, ["e4", "e5"]);
    let status = console.relay.status();
    assert_eq!((status.gaps, status.skipped), (1, 3), "{status:?}");
}

/// The daemon stops mid-stream and comes back: the console shows it is
/// reconnecting while it is down, then resumes after the last event it held.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "spawns the built trusty-events binary; not linkable per ADR-0065"]
async fn the_client_rides_out_a_daemon_restart_mid_stream() {
    let tmp = TempDir::new().expect("tempdir");
    let mut first = start(tmp.path(), "first", &[]).await;
    let mut console = console(&first.socket, Start::Beginning);
    push(&first.socket, &["e1", "e2"]).await;
    assert_eq!(console.sessions(2).await, ["e1", "e2"]);
    logged(tmp.path(), 2).await;

    first.stop().await;
    let down = console
        .status_until("reconnecting", |s| s.state == BusState::Reconnecting)
        .await;
    assert!(down.last_error.is_some(), "the outage is named: {down:?}");
    assert!(down.retry_in_ms.is_some(), "a retry is scheduled: {down:?}");

    let second = start(tmp.path(), "second", &[]).await;
    console
        .status_until("connected", |s| s.state == BusState::Connected)
        .await;
    push(&second.socket, &["e3"]).await;
    assert_eq!(console.sessions(1).await, ["e3"]);
    console.nothing_within(Duration::from_millis(300)).await;
    let status = console.relay.status();
    assert_eq!((status.resyncs, status.duplicates), (0, 0), "{status:?}");
}

// ─── #10071: hung-daemon detection against the real daemon ───────────────────

/// The heartbeat these tests ask for: the bus's minimum, so a bound is short.
const BEAT: Duration = Duration::from_millis(100);

/// A healthy daemon with nothing to say keeps the console connected for four
/// hung bounds: its heartbeats, not events, are what keep the client calm.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "spawns the built trusty-events binary; not linkable per ADR-0065"]
async fn a_quiet_healthy_daemon_stays_connected() {
    let tmp = TempDir::new().expect("tempdir");
    let daemon = start(tmp.path(), "daemon", &[]).await;
    let console = console_with(&daemon.socket, Start::Live, BEAT);
    console
        .status_until("connected", |s| s.state == BusState::Connected)
        .await;

    let mut watch = console.relay.watch();
    let quiet = tokio::time::timeout(
        hung_bound(BEAT) * 4,
        watch.wait_for(|s| s.state != BusState::Connected),
    )
    .await;
    let status = console.relay.status();
    assert!(quiet.is_err(), "a quiet daemon left connected: {status:?}");
    assert_eq!(
        (
            status.last_error,
            status.failures,
            status.delivered,
            status.undecodable
        ),
        (None, 0, 0, 0),
        "{status:?}"
    );
    drop(daemon);
}

/// A daemon that is stopped (SIGSTOP) keeps its socket and the kernel keeps
/// accepting, so only the missing heartbeat shows it. The console reports
/// `hung` within the bound and is connected again after SIGCONT, with no
/// event lost.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "spawns the built trusty-events binary; not linkable per ADR-0065"]
async fn a_stopped_daemon_is_reported_hung_and_recovers_after_sigcont() {
    let tmp = TempDir::new().expect("tempdir");
    let daemon = start(tmp.path(), "daemon", &[]).await;
    let mut console = console_with(&daemon.socket, Start::Beginning, BEAT);
    push(&daemon.socket, &["e1"]).await;
    assert_eq!(console.sessions(1).await, ["e1"]);

    daemon.signal("STOP");
    let stopped = tokio::time::Instant::now();
    let status = console
        .status_until("hung", |s| s.last_error == Some(FailureKind::Hung))
        .await;
    let elapsed = stopped.elapsed();
    assert_eq!(status.state, BusState::Reconnecting, "{status:?}");
    assert!(
        elapsed < hung_bound(BEAT) + Duration::from_secs(1),
        "hung reported after {elapsed:?}"
    );

    daemon.signal("CONT");
    console
        .status_until("connected", |s| s.state == BusState::Connected)
        .await;
    push(&daemon.socket, &["e2"]).await;
    assert_eq!(console.sessions(1).await, ["e2"]);
    let status = console.relay.status();
    assert_eq!(
        (status.duplicates, status.gaps, status.resyncs),
        (0, 0, 0),
        "{status:?}"
    );
}
