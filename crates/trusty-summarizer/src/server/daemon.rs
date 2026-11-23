//! The server side of the warm process: `trusty-summarizer server run`.
//!
//! What: binds the socket (mode 0600), then accepts connections until a
//! stop request or the idle timeout. One thread per connection; each reads
//! one request line and answers it. Summarizers are kept per distinct
//! configuration (at most [`MAX_CONFIGS`], least recently used dropped), so
//! a configuration's backends, converter workers and HTTP pools stay warm.
//! Shutdown removes the socket first, so a new client starts a new server
//! instead of reaching this one, then waits for in-flight batches, then
//! returns; dropping the summarizers stops the converter workers.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::{Reply, Request, RuntimeDir, ServerStatus, identity};
use crate::backend::{BackendRegistry, Credentials};
use crate::config::Config;
use crate::convert::Converters;
use crate::pipeline::{BatchOptions, Summarizer};

/// Configurations held warm at once.
pub const MAX_CONFIGS: usize = 4;
/// Largest request line accepted, in bytes.
const MAX_REQUEST_BYTES: u64 = 16 * 1024 * 1024;
/// How often the idle check runs.
const IDLE_TICK: Duration = Duration::from_millis(100);

#[derive(Debug)]
struct Activity {
    active: usize,
    last: Instant,
    shutting: bool,
}

struct State {
    identity: String,
    socket: PathBuf,
    started: Instant,
    idle_timeout: Duration,
    activity: Mutex<Activity>,
    drained: Condvar,
    summarizers: Mutex<Vec<(String, Arc<Summarizer>)>>,
    waker: UnixStream,
}

impl State {
    fn activity(&self) -> std::sync::MutexGuard<'_, Activity> {
        self.activity.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Marks the server as stopping and removes the socket; idempotent.
    /// Returns whether this call did it.
    fn shut_down(&self) -> bool {
        {
            let mut a = self.activity();
            if a.shutting {
                return false;
            }
            a.shutting = true;
        }
        // Still bound here, so no other server can own this path yet.
        if let Err(e) = std::fs::remove_file(&self.socket)
            && e.kind() != io::ErrorKind::NotFound
        {
            eprintln!("trusty-summarizer server: cannot remove socket: {e}");
        }
        let _ = (&self.waker).write_all(b"x");
        true
    }

    fn summarizer(&self, config: Config) -> crate::error::Result<Arc<Summarizer>> {
        let key = serde_json::to_string(&config)
            .map_err(|e| crate::error::Error::Config(format!("config encoding: {e}")))?;
        let mut all = self.summarizers.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(i) = all.iter().position(|(k, _)| *k == key) {
            let entry = all.remove(i);
            let s = Arc::clone(&entry.1);
            all.push(entry);
            return Ok(s);
        }
        let converters = Converters::standard(&config);
        let s = Arc::new(Summarizer::new(
            config,
            &BackendRegistry::with_builtins(),
            converters,
        )?);
        if all.len() >= MAX_CONFIGS {
            all.remove(0);
        }
        all.push((key, Arc::clone(&s)));
        Ok(s)
    }
}

/// Counts a connection as active until dropped.
struct ActiveGuard<'a> {
    state: &'a State,
    touch: bool,
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        let mut a = self.state.activity();
        a.active -= 1;
        if self.touch {
            a.last = Instant::now();
        }
        if a.active == 0 {
            self.state.drained.notify_all();
        }
    }
}

/// Runs the server in `dir` until stopped or idle for `idle_timeout`.
///
/// Why: the warm process (plan section 4).
/// What: returns `Ok(())` at once when another live server already owns
/// the socket. A socket file with no listener is replaced. Errors are I/O
/// failures preparing the directory or binding.
/// Test: `warm_output_matches_in_process_and_one_server_serves_both_calls`,
/// `server_exits_after_idle_timeout`
pub fn serve(dir: &RuntimeDir, idle_timeout: Duration) -> io::Result<()> {
    dir.prepare()?;
    let socket = dir.socket();
    if UnixStream::connect(&socket).is_ok() {
        return Ok(());
    }
    super::remove_stale_socket(&socket)?;
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let (waker, wake_rx) = UnixStream::pair()?;
    let state = Arc::new(State {
        identity: identity(),
        socket,
        started: Instant::now(),
        idle_timeout,
        activity: Mutex::new(Activity {
            active: 0,
            last: Instant::now(),
            shutting: false,
        }),
        drained: Condvar::new(),
        summarizers: Mutex::new(Vec::new()),
        waker,
    });
    eprintln!(
        "trusty-summarizer server: pid {} listening on {} (idle timeout {} s)",
        std::process::id(),
        state.socket.display(),
        idle_timeout.as_secs()
    );

    let watchdog = {
        let state = Arc::clone(&state);
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(IDLE_TICK);
                let idle = {
                    let a = state.activity();
                    if a.shutting {
                        return;
                    }
                    a.active == 0 && a.last.elapsed() >= state.idle_timeout
                };
                if idle && state.shut_down() {
                    eprintln!("trusty-summarizer server: idle, exiting");
                    return;
                }
            }
        })
    };

    accept_loop(&listener, &wake_rx, &state);
    drop(listener);

    let mut a = state.activity();
    while a.active > 0 {
        a = state.drained.wait(a).unwrap_or_else(|p| p.into_inner());
    }
    drop(a);
    let _ = watchdog.join();
    state
        .summarizers
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
    eprintln!("trusty-summarizer server: stopped");
    Ok(())
}

fn accept_loop(listener: &UnixListener, wake_rx: &UnixStream, state: &Arc<State>) {
    loop {
        let mut fds = [
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_rx.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: `fds` is a valid array of two pollfd for the duration of
        // the call; both descriptors are open.
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            eprintln!("trusty-summarizer server: poll failed: {e}");
            state.shut_down();
            return;
        }
        if fds[1].revents != 0 || state.activity().shutting {
            return;
        }
        if fds[0].revents == 0 {
            continue;
        }
        let stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) => {
                eprintln!("trusty-summarizer server: accept failed: {e}");
                continue;
            }
        };
        {
            let mut a = state.activity();
            if a.shutting {
                // Dropped unanswered: the client sees no `accepted` and
                // starts a new server.
                return;
            }
            a.active += 1;
        }
        let state = Arc::clone(state);
        std::thread::spawn(move || handle(stream, &state));
    }
}

fn handle(stream: UnixStream, state: &State) {
    let mut guard = ActiveGuard {
        state,
        touch: false,
    };
    let mut line = String::new();
    let read = BufReader::new((&stream).take(MAX_REQUEST_BYTES)).read_line(&mut line);
    let mut out = io::BufWriter::new(&stream);
    let request: Request = match read
        .map_err(|e| e.to_string())
        .and_then(|_| serde_json::from_str(&line).map_err(|e| format!("bad request: {e}")))
    {
        Ok(r) => r,
        Err(message) => {
            let _ = send(&mut out, &Reply::Fatal { message });
            return;
        }
    };
    // The line may hold keys; it is not needed past parsing.
    line.clear();
    line.shrink_to_fit();
    match request {
        Request::Status => {
            let status = status(state);
            let _ = send(&mut out, &Reply::Status(status));
        }
        Request::Stop => {
            state.shut_down();
            let _ = send(
                &mut out,
                &Reply::Stopping {
                    pid: std::process::id(),
                },
            );
        }
        Request::Summarize {
            identity,
            config,
            base,
            paths,
            credentials,
        } => {
            guard.touch = true;
            let credentials = Credentials::scoped(credentials);
            if identity != state.identity {
                let _ = send(
                    &mut out,
                    &Reply::Mismatch {
                        server_identity: state.identity.clone(),
                    },
                );
                return;
            }
            if send(&mut out, &Reply::Accepted).is_err() {
                return;
            }
            let summarizer = match state.summarizer(*config) {
                Ok(s) => s,
                Err(e) => {
                    let _ = send(
                        &mut out,
                        &Reply::Fatal {
                            message: e.to_string(),
                        },
                    );
                    return;
                }
            };
            let options = BatchOptions {
                base: Some(base),
                credentials,
            };
            let result = summarizer.summarize_batch_with(&paths, &options, |record| {
                send(
                    &mut out,
                    &Reply::Record {
                        record: Box::new(record),
                    },
                )
            });
            // `options` (and with it the keys) is dropped at the end of
            // this arm, when the batch is over.
            if let Ok(stats) = result {
                let _ = send(&mut out, &Reply::done(stats));
            }
        }
    }
    drop(guard);
}

fn status(state: &State) -> ServerStatus {
    let a = state.activity();
    ServerStatus {
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        identity: state.identity.clone(),
        socket: state.socket.clone(),
        uptime_secs: state.started.elapsed().as_secs_f64(),
        idle_secs: a.last.elapsed().as_secs_f64(),
        idle_timeout_secs: state.idle_timeout.as_secs(),
        active_batches: a.active.saturating_sub(1),
        configs_loaded: state
            .summarizers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .len(),
    }
}

fn send<W: Write>(out: &mut W, reply: &Reply) -> io::Result<()> {
    serde_json::to_writer(&mut *out, reply)?;
    out.write_all(b"\n")?;
    out.flush()
}
