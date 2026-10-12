//! Bounded subprocess runs: every child the crate spawns ends within a deadline.
//!
//! Why: the prototype runs tmux with `timeout=10` and the host probes with 5 s
//! and 10 s; A1's `Command::output()` had no bound, so one hung tmux server
//! froze the poller (#9171 A3a).
//! What: [`run_bounded`] spawns with stdin closed, drains stdout and stderr on
//! two threads (a full pipe never blocks the child), polls `try_wait` every
//! 10 ms until the deadline, then kills and reaps the child.
//! Test: `system_tmux_times_out_and_reaps_the_child`,
//! `system_tmux_reads_a_megabyte_of_stdout`.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// How often a running child is polled for exit.
const POLL: Duration = Duration::from_millis(10);

/// A finished child's exit status and captured output.
#[derive(Debug)]
pub(crate) struct Finished {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

/// Why a bounded run produced no [`Finished`].
#[derive(Debug)]
pub(crate) enum RunFailure {
    /// The child could not be started.
    Spawn(io::Error),
    /// The deadline passed; the child was killed and reaped.
    Timeout,
    /// Waiting on the child failed.
    Wait(io::Error),
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            // A read error ends the capture; the exit status still decides.
            let _ = p.read_to_end(&mut buf);
        }
        buf
    })
}

fn kill_and_reap(child: &mut Child) {
    // The child may have exited between the last poll and the kill.
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs `cmd` to completion or until `timeout` passes.
///
/// Postcondition: when this returns, the child has been waited on (no zombie),
/// whatever the result.
pub(crate) fn run_bounded(mut cmd: Command, timeout: Duration) -> Result<Finished, RunFailure> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(RunFailure::Spawn)?;
    let out = drain(child.stdout.take());
    let err = drain(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                kill_and_reap(&mut child);
                // The pipes close with the child; a grandchild holding them
                // open must not block the caller, so the readers are detached.
                return Err(RunFailure::Timeout);
            }
            Ok(None) => thread::sleep(POLL),
            Err(e) => {
                kill_and_reap(&mut child);
                return Err(RunFailure::Wait(e));
            }
        }
    };
    Ok(Finished {
        status,
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}
