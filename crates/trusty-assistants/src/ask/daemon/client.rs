//! The client side: find or start the instance's daemon, then ask (#11650).
//!
//! Why/What: see the parent module. The client waits on the daemon's
//! readiness line, an event; its one timer is [`START_BUDGET`], which bounds
//! how long a CLI call waits for a daemon that never gets ready.
//! Test: `super::tests` and `tests/ask_daemon.rs`.

use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, ChildStdin, Command};
use trusty_common::uds::{UdsSecurityError, connect_hardened};

use crate::assistants::AssistantHome;

use super::{
    AskDaemonError, AskReply, AskRequest, BUSY_LINE, DAEMON_VERB, MAX_LINE_BYTES, PROTOCOL_VERSION,
    READY_LINE, ReplyErrorKind, ReplyOutcome, data_dir, instance_log_in, instance_socket_in,
    resolve_home,
};

/// How long a client waits for a spawned daemon's readiness line.
pub const START_BUDGET: Duration = Duration::from_secs(15);

/// How many daemons one ask may start before giving up. Each start that
/// loses a bind race, or whose daemon exits before the connect, costs one.
const MAX_STARTS: u32 = 5;

/// A daemon log larger than this is truncated when a new daemon starts.
const LOG_CAP_BYTES: u64 = 1024 * 1024;

/// How to start an instance's daemon.
///
/// Why: the CLI runs its own executable (`ta ask-daemon <id>`); a test runs
/// a stand-in to reach the start-failure paths.
/// What: `program`, then `args`, then `ask-daemon <id>`. stdin and stdout
/// are pipes, stderr appends to the instance log, and the child gets its own
/// process group so a Ctrl-C at the client's terminal does not reach it.
/// Test: `super::tests::a_daemon_that_dies_at_start_names_its_log`,
/// `super::tests::a_daemon_that_never_gets_ready_is_stopped`.
#[derive(Debug, Clone)]
pub struct DaemonLauncher {
    program: PathBuf,
    args: Vec<String>,
    budget: Duration,
}

impl DaemonLauncher {
    /// This executable as the daemon.
    pub fn current_exe() -> Result<Self, AskDaemonError> {
        let program =
            std::env::current_exe().map_err(AskDaemonError::io("locate this executable"))?;
        Ok(Self::program(program))
    }

    /// `program` as the daemon.
    pub fn program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            budget: START_BUDGET,
        }
    }

    /// Arguments placed before `ask-daemon <id>`.
    #[must_use]
    pub fn with_args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Replace [`START_BUDGET`].
    #[must_use]
    pub fn with_budget(mut self, budget: Duration) -> Self {
        self.budget = budget;
        self
    }
}

/// What one start produced.
enum Started {
    /// A daemon is ready; dropping the lease releases it.
    Ready(ChildStdin),
    /// Another daemon already serves the socket.
    Busy,
}

/// Ask instance `raw_id` a question, starting its daemon if none is running.
///
/// Why: requirement 1, the `ta ask` path.
/// What: refuses an unknown id before anything starts. Then, up to
/// [`MAX_STARTS`] times: connect (socket checked for owner, 0600 and a 0700
/// directory); on "nobody serving" start a daemon and wait for its readiness
/// line; on `ready` connect and release the daemon; on `busy` connect to the
/// winner. Any other connect failure is returned at once, never "fixed" by
/// starting a daemon over it. `root` `None` means the default assistants root.
/// Test: `ask_starts_the_daemon_on_demand_and_it_exits_idle`,
/// `a_second_ask_reuses_the_running_daemon`,
/// `racing_asks_all_answer_through_one_daemon_at_a_time`
/// (`tests/ask_daemon.rs`); `super::tests::an_unknown_instance_is_named_with_its_path`.
pub async fn ask_instance(
    root: Option<&Path>,
    raw_id: &str,
    question: &str,
    launcher: &DaemonLauncher,
) -> Result<String, AskDaemonError> {
    let home = resolve_home(root, raw_id)?;
    ask_home_in(&home, &data_dir()?, question, launcher).await
}

/// [`ask_instance`] for a resolved home and an explicit data directory.
/// Test: `super::tests::a_daemon_that_dies_at_start_names_its_log`.
pub(super) async fn ask_home_in(
    home: &AssistantHome,
    dir: &Path,
    question: &str,
    launcher: &DaemonLauncher,
) -> Result<String, AskDaemonError> {
    let socket = instance_socket_in(dir, home.id());
    let log = instance_log_in(dir, home.id());
    let id = home.id().to_string();
    for _ in 0..MAX_STARTS {
        if let Some(stream) = connect(&socket).await? {
            return exchange(stream, question).await;
        }
        match start(launcher, &id, &log).await? {
            Started::Ready(lease) => {
                let stream = connect(&socket).await?;
                // #11650: the connect is queued or accepted; the daemon may now
                // exit once this connection closes.
                drop(lease);
                if let Some(stream) = stream {
                    return exchange(stream, question).await;
                }
            }
            Started::Busy => {}
        }
    }
    Err(AskDaemonError::Unreachable {
        id,
        log,
        attempts: MAX_STARTS,
    })
}

/// Ask the daemon already serving `socket`; never starts one.
/// Test: `super::tests::an_ask_round_trips_and_the_daemon_exits_idle`,
/// `super::tests::a_protocol_mismatch_is_refused_both_ways`.
pub async fn ask_at(socket: &Path, question: &str) -> Result<String, AskDaemonError> {
    match connect(socket).await? {
        Some(stream) => exchange(stream, question).await,
        None => Err(AskDaemonError::Disconnected),
    }
}

/// Connect to `socket`; `None` when nobody serves it (absent or refused).
async fn connect(socket: &Path) -> Result<Option<UnixStream>, AskDaemonError> {
    // No socket yet: the directory may still be at its created mode, which
    // `connect_hardened` would refuse before it looked for the socket. The
    // daemon's bind hardens the directory.
    if matches!(std::fs::symlink_metadata(socket), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
    {
        return Ok(None);
    }
    match connect_hardened(socket).await {
        Ok(stream) => Ok(Some(stream)),
        Err(UdsSecurityError::StatForConnect { source, .. })
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            Ok(None)
        }
        Err(UdsSecurityError::Connect { source, .. })
            if matches!(
                source.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            Ok(None)
        }
        Err(e) => Err(AskDaemonError::Connect(e)),
    }
}

/// Send one request on `stream` and read its reply.
///
/// What: the write half stays open until the reply is read; closing it
/// earlier would read as a hang-up and cancel the ask.
async fn exchange(stream: UnixStream, question: &str) -> Result<String, AskDaemonError> {
    let (read, mut write) = stream.into_split();
    let request = AskRequest {
        protocol: PROTOCOL_VERSION,
        question: question.to_string(),
    };
    let mut line =
        serde_json::to_string(&request).map_err(|e| AskDaemonError::BadReply(e.to_string()))?;
    line.push('\n');
    write
        .write_all(line.as_bytes())
        .await
        .map_err(AskDaemonError::io("send the question"))?;
    let mut reply = String::new();
    let read = BufReader::new(read)
        .take(MAX_LINE_BYTES)
        .read_line(&mut reply)
        .await
        .map_err(AskDaemonError::io("read the answer"))?;
    drop(write);
    if read == 0 {
        return Err(AskDaemonError::Disconnected);
    }
    parse_reply(&reply)
}

/// The answer in `line`, or the failure it reports.
/// Test: `super::tests::a_protocol_mismatch_is_refused_both_ways`.
pub(super) fn parse_reply(line: &str) -> Result<String, AskDaemonError> {
    let value: serde_json::Value =
        serde_json::from_str(line).map_err(|e| AskDaemonError::BadReply(e.to_string()))?;
    let theirs = value.get("protocol").and_then(serde_json::Value::as_u64);
    if theirs != Some(u64::from(PROTOCOL_VERSION)) {
        return Err(AskDaemonError::ProtocolMismatch {
            ours: PROTOCOL_VERSION,
            theirs: theirs.and_then(|v| u32::try_from(v).ok()).unwrap_or(0),
        });
    }
    let reply: AskReply =
        serde_json::from_value(value).map_err(|e| AskDaemonError::BadReply(e.to_string()))?;
    match reply.outcome {
        ReplyOutcome::Answer { answer } => Ok(answer),
        ReplyOutcome::Error { kind, message } => Err(match kind {
            ReplyErrorKind::Ask => AskDaemonError::Ask(message),
            // A daemon refusing our version replies in its own, caught above;
            // with equal versions this arm is a malformed refusal.
            ReplyErrorKind::BadRequest | ReplyErrorKind::ProtocolMismatch => {
                AskDaemonError::BadRequest(message)
            }
        }),
    }
}

/// Spawn one daemon and wait for its readiness line.
///
/// Why: requirement 6 — a daemon that dies or hangs at start is reported
/// with the path of its log, never retried silently.
async fn start(launcher: &DaemonLauncher, id: &str, log: &Path) -> Result<Started, AskDaemonError> {
    let stderr = open_log(log)?;
    let mut child = Command::new(&launcher.program)
        .args(&launcher.args)
        .arg(DAEMON_VERB)
        .arg(id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .process_group(0)
        .spawn()
        .map_err(|source| AskDaemonError::Spawn {
            program: launcher.program.clone(),
            source,
        })?;
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.start_kill();
        return Err(start_failed(
            id,
            log,
            "its stdio pipes were not created".into(),
        ));
    };
    let mut line = String::new();
    let ready = tokio::time::timeout(
        launcher.budget,
        BufReader::new(stdout).take(256).read_line(&mut line),
    )
    .await;
    match ready {
        Err(_) => {
            let _ = child.start_kill();
            reap(child);
            Err(AskDaemonError::StartTimeout {
                id: id.to_string(),
                log: log.to_path_buf(),
                secs: launcher.budget.as_secs(),
            })
        }
        Ok(Ok(_)) if line.trim() == READY_LINE => {
            reap(child);
            Ok(Started::Ready(stdin))
        }
        Ok(Ok(_)) if line.trim() == BUSY_LINE => {
            reap(child);
            Ok(Started::Busy)
        }
        Ok(read) => {
            let status = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            let mut detail = match status {
                Ok(Ok(status)) => format!("exited with {status}"),
                _ => {
                    let _ = child.start_kill();
                    reap(child);
                    "closed its stdout without a readiness line".to_string()
                }
            };
            if let Err(e) = read {
                detail.push_str(&format!("; reading it failed: {e}"));
            }
            if let Some(last) = last_log_line(log) {
                detail.push_str(&format!("; last log line: {last}"));
            }
            Err(start_failed(id, log, detail))
        }
    }
}

fn start_failed(id: &str, log: &Path, detail: String) -> AskDaemonError {
    AskDaemonError::StartFailed {
        id: id.to_string(),
        log: log.to_path_buf(),
        detail,
    }
}

/// Wait for `child` in the background so it is never left a zombie.
fn reap(mut child: Child) {
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
}

/// Open the instance log (0600 when created) for appending, truncating it
/// past [`LOG_CAP_BYTES`].
fn open_log(log: &Path) -> Result<std::fs::File, AskDaemonError> {
    let too_big = std::fs::metadata(log).is_ok_and(|m| m.len() > LOG_CAP_BYTES);
    let mut options = std::fs::OpenOptions::new();
    options.create(true).mode(0o600);
    if too_big {
        options.write(true).truncate(true);
    } else {
        options.append(true);
    }
    options
        .open(log)
        .map_err(AskDaemonError::io("open the ask daemon log"))
}

/// The last non-empty line of `log`, for a start-failure message.
fn last_log_line(log: &Path) -> Option<String> {
    let body = std::fs::read_to_string(log).ok()?;
    body.lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
}
