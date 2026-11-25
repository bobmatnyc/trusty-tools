//! The client side of the warm process: connect, or start a server, then
//! stream a batch.
//!
//! What: [`summarize`] connects to the socket, starting a server under the
//! start lock when nothing listens, sends one request and hands each record
//! to the sink as it arrives. Every way the warm path can fail is a
//! [`WarmError`]; none is turned into a silent partial success.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{Reply, Request, RuntimeDir, ServerStatus, identity, key_variables, looks_secret};
use crate::config::Config;
use crate::pipeline::BatchStats;
use crate::record::FileRecord;

/// Time allowed for a started server to accept connections.
const START_TIMEOUT: Duration = Duration::from_secs(20);
/// Time allowed to take the start lock (another client is starting one).
const LOCK_TIMEOUT: Duration = Duration::from_secs(30);
/// Time allowed for a stopped server to exit.
const STOP_TIMEOUT: Duration = Duration::from_secs(30);
/// Interval of the bounded checks above.
const TICK: Duration = Duration::from_millis(10);
/// Attempts when a server goes away before accepting a request.
const ATTEMPTS: usize = 3;
/// The server log is cut back to empty above this size when a server starts.
const MAX_LOG_BYTES: u64 = 1024 * 1024;

/// A warm-path failure. The CLI reports it and exits 1.
#[derive(Debug, thiserror::Error)]
pub enum WarmError {
    /// The runtime directory, lock or socket could not be used.
    #[error("warm server: {0}")]
    Io(#[from] io::Error),
    /// A server was started and did not accept connections in time.
    #[error("warm server did not start within {} s (see {})", .timeout.as_secs(), .log.display())]
    StartTimeout {
        /// The bound.
        timeout: Duration,
        /// The server log.
        log: PathBuf,
    },
    /// The server refused the request (bad configuration and the like).
    #[error("{0}")]
    Fatal(String),
    /// The connection ended before the batch's `done` message.
    #[error(
        "warm server connection lost after {received} records (server crashed, was killed \
         or stopped mid-batch); the batch is incomplete"
    )]
    Lost {
        /// Records received before the loss.
        received: usize,
    },
    /// The server's answer broke the protocol.
    #[error("warm server protocol error: {0}")]
    Protocol(String),
    /// The sink failed (for example, stdout closed).
    #[error("{0}")]
    Sink(io::Error),
}

/// Runs `paths` through the warm server, starting one if needed, and hands
/// each record to `sink` in input order as it arrives.
///
/// Why: the warm path of `summarize`; same records as the in-process
/// [`crate::Summarizer::summarize_batch`].
/// What: `config` must already be resolved ([`Config::resolve_paths`]).
/// The keys named by the config's `api_key_env` options are read from this
/// process's environment and sent with the request. A server with another
/// [`identity`] is stopped and replaced; a server that vanishes before
/// accepting is retried; one that vanishes after is [`WarmError::Lost`].
/// Test: `warm_output_matches_in_process_and_one_server_serves_both_calls`,
/// `server_killed_mid_batch_fails_the_call`, `racing_calls_start_one_server`
pub fn summarize<F>(
    dir: &RuntimeDir,
    config: &Config,
    base: &Path,
    paths: &[PathBuf],
    mut sink: F,
) -> Result<BatchStats, WarmError>
where
    F: FnMut(FileRecord) -> io::Result<()>,
{
    dir.prepare()?;
    let mine = identity();
    for attempt in 1..=ATTEMPTS {
        let stream = connect_or_start(dir, config)?;
        let credentials: BTreeMap<String, String> = key_variables(config)
            .into_iter()
            .filter_map(|k| std::env::var(&k).ok().map(|v| (k, v)))
            .collect();
        let request = Request::Summarize {
            identity: mine.clone(),
            config: Box::new(config.clone()),
            base: base.to_path_buf(),
            paths: paths.to_vec(),
            credentials,
        };
        let mut reader = BufReader::new(&stream);
        if write_line(&stream, &request).is_err() {
            continue;
        }
        drop(request);
        match read_reply(&mut reader)? {
            None if attempt < ATTEMPTS => continue,
            None => return Err(WarmError::Lost { received: 0 }),
            Some(Reply::Mismatch { .. }) => {
                stop_and_wait(dir)?;
                continue;
            }
            Some(Reply::Fatal { message }) => return Err(WarmError::Fatal(message)),
            Some(Reply::Accepted) => return stream_batch(&mut reader, &mut sink),
            Some(other) => {
                return Err(WarmError::Protocol(format!(
                    "expected accepted, got {other:?}"
                )));
            }
        }
    }
    Err(WarmError::Protocol(format!(
        "no server accepted the request after {ATTEMPTS} attempts"
    )))
}

fn stream_batch<R: BufRead, F>(reader: &mut R, sink: &mut F) -> Result<BatchStats, WarmError>
where
    F: FnMut(FileRecord) -> io::Result<()>,
{
    let mut seen = BatchStats::default();
    loop {
        match read_reply(reader) {
            Ok(Some(Reply::Record { record })) => {
                seen.files += 1;
                match &*record {
                    FileRecord::Error(_) => seen.errors += 1,
                    FileRecord::Ok(r) if r.cache == crate::record::CacheStatus::Hit => {
                        seen.cache_hits += 1;
                    }
                    FileRecord::Ok(_) => {}
                }
                sink(*record).map_err(WarmError::Sink)?;
            }
            Ok(Some(Reply::Done {
                files,
                errors,
                cache_hits,
            })) => {
                let told = BatchStats {
                    files,
                    errors,
                    cache_hits,
                };
                if told != seen {
                    return Err(WarmError::Protocol(format!(
                        "server reported {told:?}, client received {seen:?}"
                    )));
                }
                return Ok(seen);
            }
            Ok(Some(Reply::Fatal { message })) => return Err(WarmError::Fatal(message)),
            Ok(Some(other)) => {
                return Err(WarmError::Protocol(format!("unexpected {other:?}")));
            }
            Ok(None) | Err(WarmError::Io(_)) => {
                return Err(WarmError::Lost {
                    received: seen.files,
                });
            }
            Err(e) => return Err(e),
        }
    }
}

/// The running server's status; `None` when nothing listens.
pub fn status(dir: &RuntimeDir) -> Result<Option<ServerStatus>, WarmError> {
    let Ok(stream) = UnixStream::connect(dir.socket()) else {
        return Ok(None);
    };
    write_line(&stream, &Request::Status)?;
    match read_reply(&mut BufReader::new(&stream))? {
        Some(Reply::Status(s)) => Ok(Some(s)),
        None => Ok(None),
        Some(other) => Err(WarmError::Protocol(format!("unexpected {other:?}"))),
    }
}

/// Stops the running server and waits (bounded) for its process to exit.
/// Returns its pid; `None` when nothing listened.
pub fn stop(dir: &RuntimeDir) -> Result<Option<u32>, WarmError> {
    let Ok(stream) = UnixStream::connect(dir.socket()) else {
        return Ok(None);
    };
    write_line(&stream, &Request::Stop)?;
    let pid = match read_reply(&mut BufReader::new(&stream))? {
        Some(Reply::Stopping { pid }) => pid,
        None => return Ok(None),
        Some(other) => return Err(WarmError::Protocol(format!("unexpected {other:?}"))),
    };
    let started = Instant::now();
    while process_alive(pid) {
        if started.elapsed() > STOP_TIMEOUT {
            return Err(WarmError::Protocol(format!(
                "server pid {pid} did not exit within {} s of stop",
                STOP_TIMEOUT.as_secs()
            )));
        }
        std::thread::sleep(TICK);
    }
    Ok(Some(pid))
}

/// Whether process `pid` exists (a zombie counts as gone).
pub fn process_alive(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: kill with signal 0 only checks for existence.
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    // A server this process started is its child: reap it once it exits,
    // so it does not count as alive as a zombie.
    let mut status = 0;
    // SAFETY: waitpid on a pid with WNOHANG never blocks; a non-child pid
    // returns -1 and is left alone.
    let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
    reaped != pid
}

/// Asks a server that has the wrong identity to stop, so a fresh one can
/// start. In-flight batches on it finish under their own configuration.
fn stop_and_wait(dir: &RuntimeDir) -> Result<(), WarmError> {
    // The socket is removed before the reply; waiting for the old process
    // is not needed for correctness, only the path.
    let Ok(stream) = UnixStream::connect(dir.socket()) else {
        return Ok(());
    };
    write_line(&stream, &Request::Stop)?;
    let _ = read_reply(&mut BufReader::new(&stream))?;
    Ok(())
}

fn connect_or_start(dir: &RuntimeDir, config: &Config) -> Result<UnixStream, WarmError> {
    if let Ok(s) = UnixStream::connect(dir.socket()) {
        return Ok(s);
    }
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(dir.lock())?;
    lock_bounded(&lock)?;
    // Another client may have started one while this one waited.
    if let Ok(s) = UnixStream::connect(dir.socket()) {
        return Ok(s);
    }
    let socket = dir.socket();
    super::remove_stale_socket(&socket)?;
    let mut child = spawn_server(dir, config)?;
    let started = Instant::now();
    loop {
        if let Ok(s) = UnixStream::connect(&socket) {
            return Ok(s);
        }
        if let Some(status) = child.try_wait()? {
            return Err(WarmError::Fatal(format!(
                "warm server exited at start ({status}); see {}",
                dir.log().display()
            )));
        }
        if started.elapsed() > START_TIMEOUT {
            let _ = child.kill();
            return Err(WarmError::StartTimeout {
                timeout: START_TIMEOUT,
                log: dir.log(),
            });
        }
        std::thread::sleep(TICK);
    }
}

fn lock_bounded(lock: &File) -> Result<(), WarmError> {
    let started = Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) if started.elapsed() < LOCK_TIMEOUT => {
                std::thread::sleep(TICK);
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(WarmError::Protocol(format!(
                    "start lock held for over {} s",
                    LOCK_TIMEOUT.as_secs()
                )));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
    }
}

fn spawn_server(dir: &RuntimeDir, config: &Config) -> Result<std::process::Child, WarmError> {
    let exe = std::env::current_exe()?;
    let log_path = dir.log();
    if std::fs::metadata(&log_path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        std::fs::remove_file(&log_path)?;
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log_path)?;
    let mut cmd = Command::new(exe);
    cmd.arg("server")
        .arg("run")
        .arg("--idle-timeout-secs")
        .arg(config.warm.idle_timeout_secs.to_string())
        .env(super::RUNTIME_DIR_ENV, &dir.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        // Its own process group: a Ctrl-C to this call does not reach it.
        .process_group(0);
    // Keys reach the server per request only, never through its environment.
    for name in key_variables(config) {
        cmd.env_remove(name);
    }
    for (name, _) in std::env::vars_os() {
        if looks_secret(&name.to_string_lossy()) {
            cmd.env_remove(name);
        }
    }
    Ok(cmd.spawn()?)
}

fn write_line<T: serde::Serialize>(mut stream: &UnixStream, value: &T) -> io::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    let result = stream.write_all(&line).and_then(|()| stream.flush());
    // The line may hold keys.
    line.fill(0);
    result
}

/// The next reply; `None` at end of stream.
fn read_reply<R: BufRead>(reader: &mut R) -> Result<Option<Reply>, WarmError> {
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => Ok(None),
        Ok(_) if !line.ends_with('\n') => Ok(None),
        Ok(_) => serde_json::from_str(&line)
            .map(Some)
            .map_err(|e| WarmError::Protocol(format!("bad reply: {e}"))),
        Err(e) if e.kind() == io::ErrorKind::ConnectionReset => Ok(None),
        Err(e) => Err(e.into()),
    }
}
