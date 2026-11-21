//! The escalation hand-off to `pm-notify`.
//!
//! Why: `update --esc architect` queues a `decision` through the D10 intake by
//! running a command. That command is not a ledger file, so it stays outside the
//! ledger code; this module only starts it with `datg.py`'s arguments and bounds
//! it at 30 seconds.
//! What: [`run`] starts `<cmd> --kind decision --session <s> <text>`, waits up to
//! 30 s (killing it after), and returns `Err` with `exit N: <output>` or the
//! spawn error. The caller logs a failure as `notify_failure` and keeps going.
//! Test: `notify_failure_is_logged_and_not_fatal` in `tests/il_ops.rs`.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);

/// Runs the notify command.
///
/// # Errors
/// A one-line reason for a spawn failure, a timeout or a non-zero exit.
pub fn run(cmd: &str, session: &str, text: &str) -> Result<(), String> {
    let mut child = Command::new(cmd)
        .args(["--kind", "decision", "--session", session, text])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => {
                let out = child.wait_with_output().map_err(|e| e.to_string())?;
                let msg = String::from_utf8_lossy(if out.stderr.is_empty() {
                    &out.stdout
                } else {
                    &out.stderr
                })
                .trim()
                .to_owned();
                let short: String = msg.chars().take(200).collect();
                return Err(format!("exit {}: {short}", status.code().unwrap_or(-1)));
            }
            Ok(None) if start.elapsed() >= TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("timed out after 30s".to_owned());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(e) => return Err(e.to_string()),
        }
    }
}
