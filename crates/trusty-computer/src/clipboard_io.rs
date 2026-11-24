//! The pasteboard seam of the clipboard tools (#11696 Phase C).
//!
//! Why: the bounded cua-driver daemon cannot expose the clipboard, so the
//! broker reads and writes it itself; tests must not touch the user's real
//! clipboard, so the access sits behind [`Pasteboard`]. A hung `pbpaste` or
//! `pbcopy` must not hold the broker's lock, so each run has a deadline.
//! What: [`SystemPasteboard`] runs `/usr/bin/pbpaste` and `/usr/bin/pbcopy`
//! with a cleared environment (`LANG=en_US.UTF-8` only, so non-ASCII text
//! round-trips). A read stops after `limit` bytes and kills `pbpaste`, so an
//! oversized clipboard is never held in full. A command still running at
//! [`TIMEOUT`] is killed and the call answers an error.
//! Test: `pasteboard_command_is_killed_at_its_deadline`,
//! `live_clipboard_write_then_redacted_read` (`tests/live_clipboard.rs`).

use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long one `pbpaste` or `pbcopy` may run.
pub const TIMEOUT: Duration = Duration::from_secs(5);

/// Plain-text clipboard access.
pub trait Pasteboard: Send + Sync {
    /// The clipboard's plain text as bytes, at most `limit` of them.
    fn read(&self, limit: usize) -> Result<Vec<u8>, String>;
    /// Replace the clipboard with `text`.
    fn write(&self, text: &str) -> Result<(), String>;
}

/// The user's clipboard through `pbpaste` / `pbcopy`.
pub struct SystemPasteboard {
    /// The read command, program first.
    pub paste: Vec<String>,
    /// The write command, program first; it reads the text on stdin.
    pub copy: Vec<String>,
    /// The deadline of each command.
    pub timeout: Duration,
}

impl Default for SystemPasteboard {
    fn default() -> SystemPasteboard {
        let argv = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect();
        SystemPasteboard {
            paste: argv(&["/usr/bin/pbpaste", "-Prefer", "txt"]),
            copy: argv(&["/usr/bin/pbcopy"]),
            timeout: TIMEOUT,
        }
    }
}

fn spawn(argv: &[String], stdin: Stdio, stdout: Stdio) -> Result<Child, String> {
    let (program, args) = argv.split_first().ok_or("empty pasteboard command")?;
    Command::new(program)
        .args(args)
        .env_clear()
        .env("LANG", "en_US.UTF-8")
        .stdin(stdin)
        .stdout(stdout)
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))
}

/// Kill `child` and reap it; the error names the deadline.
fn timed_out(child: &mut Child, name: &str, timeout: Duration) -> String {
    let _ = child.kill();
    let _ = child.wait();
    format!("{name}: timed out after {} ms", timeout.as_millis())
}

/// Wait for `child` until `deadline`; killed and an error past it.
pub(crate) fn wait_until(
    child: &mut Child,
    name: &str,
    start: Instant,
    timeout: Duration,
) -> Result<(), String> {
    loop {
        match child.try_wait().map_err(|e| format!("{name}: {e}"))? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => return Err(format!("{name}: {status}")),
            None if start.elapsed() >= timeout => return Err(timed_out(child, name, timeout)),
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

impl Pasteboard for SystemPasteboard {
    fn read(&self, limit: usize) -> Result<Vec<u8>, String> {
        let start = Instant::now();
        let mut child = spawn(&self.paste, Stdio::null(), Stdio::piped())?;
        let stdout = child.stdout.take().ok_or("pbpaste: no stdout")?;
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let read = stdout.take(limit as u64).read_to_end(&mut out);
            let _ = tx.send(read.map(|_| out));
        });
        let out = match rx.recv_timeout(self.timeout) {
            Ok(read) => read.map_err(|e| format!("pbpaste: {e}"))?,
            Err(_) => return Err(timed_out(&mut child, "pbpaste", self.timeout)),
        };
        if out.len() >= limit {
            // #11696 Phase C: never hold more than the cap; the rest is refused anyway.
            let _ = child.kill();
            let _ = child.wait();
            return Ok(out);
        }
        wait_until(&mut child, "pbpaste", start, self.timeout)?;
        Ok(out)
    }

    fn write(&self, text: &str) -> Result<(), String> {
        let start = Instant::now();
        let mut child = spawn(&self.copy, Stdio::piped(), Stdio::null())?;
        let mut stdin = child.stdin.take().ok_or("pbcopy: no stdin")?;
        let bytes = text.as_bytes().to_vec();
        let (tx, rx) = mpsc::channel();
        // The pipe can fill; the write runs off this thread so the deadline holds.
        std::thread::spawn(move || {
            let _ = tx.send(stdin.write_all(&bytes));
        });
        match rx.recv_timeout(self.timeout) {
            Ok(wrote) => wrote.map_err(|e| format!("pbcopy: {e}"))?,
            Err(_) => return Err(timed_out(&mut child, "pbcopy", self.timeout)),
        }
        wait_until(&mut child, "pbcopy", start, self.timeout)
    }
}
