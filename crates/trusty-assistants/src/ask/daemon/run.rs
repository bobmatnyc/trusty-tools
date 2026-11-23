//! `ta ask-daemon <id>`: one instance's daemon process (#11650).
//!
//! Why/What: see the parent module. This file is the process glue — stdout
//! readiness, the stdin lease, signals, logging — around [`super::bind`] and
//! [`super::BoundDaemon::serve`].
//! Test: `tests/ask_daemon.rs` drives it as a real process.

use std::io::Write as _;
use std::os::fd::AsRawFd as _;
use std::sync::Arc;

use tokio::signal::unix::{SignalKind, signal};

use super::{
    AskDaemonError, BUSY_LINE, Bound, EngineFactory, READY_LINE, ServeOptions, data_dir,
    instance_socket_in, resolve_home,
};
use crate::ask::AskEngine;

/// Run the ask daemon for `raw_id` until it is idle or signalled.
///
/// Why: the process the `ta ask` client starts on demand.
/// What, in order: refuse an unknown instance; install SIGTERM/SIGINT
/// handlers; bind the instance socket ([`super::bind`]). `busy` on stdout
/// means another daemon serves it, and this one exits 0. Otherwise print
/// `ready`, point stdout at `/dev/null`, and serve. The spawner holds the
/// daemon's stdin; its EOF is the release the idle exit waits for, so a
/// daemon run by hand from a terminal stays until that terminal closes stdin
/// or sends a signal. Lifecycle lines go to stderr, the instance log when
/// spawned. A bind failure is an error before `ready`, which the client
/// reports with the log path.
/// Test: `ask_starts_the_daemon_on_demand_and_it_exits_idle`,
/// `a_second_ask_reuses_the_running_daemon` (`tests/ask_daemon.rs`).
pub async fn run_daemon(raw_id: &str) -> Result<(), AskDaemonError> {
    init_tracing();
    let home = resolve_home(None, raw_id)?;
    let id = home.id().to_string();
    let socket = instance_socket_in(&data_dir()?, home.id());
    let health_socket = match trusty_agents_common::health::agents_socket() {
        Ok(path) => Some(path),
        Err(e) => {
            tracing::warn!(error = %e, "agents.health socket unresolved; not served");
            None
        }
    };
    let mut term =
        signal(SignalKind::terminate()).map_err(AskDaemonError::io("install SIGTERM"))?;
    let mut int = signal(SignalKind::interrupt()).map_err(AskDaemonError::io("install SIGINT"))?;

    let daemon = match super::bind(&socket).await? {
        Bound::Busy => {
            announce(BUSY_LINE)?;
            log_line(&format!(
                "ask daemon `{id}`: busy, another daemon serves {}",
                socket.display()
            ));
            return Ok(());
        }
        Bound::Serving(daemon) => daemon,
    };
    announce(READY_LINE)?;
    silence_stdout();
    let pid = std::process::id();
    log_line(&format!(
        "ask daemon `{id}`: serving {} as pid {pid}",
        socket.display()
    ));

    let factory: EngineFactory = {
        let home = home.clone();
        Arc::new(move || AskEngine::production(home.clone()))
    };
    let options = ServeOptions {
        health_socket,
        own_uid: trusty_common::uds::self_uid(),
    };
    let shutdown = async move {
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
    };
    let summary = daemon
        .serve(options, factory, stdin_closed(), shutdown)
        .await;
    log_line(&format!(
        "ask daemon `{id}`: pid {pid} exited ({summary:?})"
    ));
    Ok(())
}

/// Append one lifecycle line to stderr in a single write, so lines from
/// daemons sharing the instance log never interleave.
fn log_line(line: &str) {
    let _ = std::io::stderr().write_all(format!("{line}\n").as_bytes());
}

/// Print `line` on stdout for the spawner and flush it.
fn announce(line: &str) -> Result<(), AskDaemonError> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{line}")
        .and_then(|()| out.flush())
        .map_err(AskDaemonError::io("announce readiness"))
}

/// Point fd 1 at `/dev/null`.
///
/// Why: the spawner stops reading stdout after the readiness line; a later
/// stray `println!` would then fail on a closed pipe and panic.
fn silence_stdout() {
    match std::fs::OpenOptions::new().write(true).open("/dev/null") {
        Ok(null) => {
            // SAFETY: both descriptors are open for the duration of the call;
            // dup2 only replaces fd 1, which this process no longer reads.
            if unsafe { libc::dup2(null.as_raw_fd(), 1) } < 0 {
                tracing::warn!(error = %std::io::Error::last_os_error(), "stdout not redirected");
            }
        }
        Err(e) => tracing::warn!(error = %e, "stdout not redirected"),
    }
}

/// Resolve when stdin reaches EOF or fails: the spawner's release.
///
/// What: a plain thread does the blocking read, so the runtime's shutdown
/// never waits on it; the future wakes on the thread's message.
fn stdin_closed() -> impl std::future::Future<Output = ()> {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
        let _ = tx.send(());
    });
    async move {
        let _ = rx.await;
    }
}

/// Log to stderr at `info`, or at `RUST_LOG`, without colour codes: stderr
/// is the instance log file when the daemon was spawned.
fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init();
}
