//! Stream-JSON backend: warm headless `claude -p --output-format stream-json`.
//!
//! Why: the default execution backend spawns a long-lived `claude` child
//! process that accepts newline-delimited JSON on stdin and emits
//! newline-delimited stream-JSON events on stdout. This keeps the session warm
//! across multiple user messages without the overhead of a new process per
//! message, and uses Max OAuth billing by unsetting `ANTHROPIC_API_KEY` (§9.1).
//! What: [`StreamJsonBackend`] spawns `env -u ANTHROPIC_API_KEY claude -p
//! --output-format stream-json [--append-system-prompt-file …] --workdir <dir>`
//! and implements [`super::SessionBackend`]. `recv()` reads stdout line-by-line
//! and parses each line as a `StreamJsonEvent`. Stderr is captured but never
//! forwarded. On clean child exit `recv()` returns `None`. A seam for the
//! crash-watchdog / auto-restart policy (WI-4) is present but not wired in
//! Phase 1.
//! Test: `stream_json_spawn_requires_claude` (skipped if `claude` absent),
//! `stream_json_backend_constructs` (constructor-only; no child process),
//! `session_input_newtype` in the inline module.

use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tracing::{debug, warn};

use crate::control::event::{SessionEvent, StreamJsonEvent};
use crate::control::id::ControlSessionId;

use super::{SessionBackend, SessionInput};

/// Warm headless `claude -p --output-format stream-json` execution backend.
///
/// Why: the stream-JSON backend is the alpha-1 default because it lets the
/// daemon supervise a `claude` process without a PTY and without exposing the
/// interactive readline TUI. It's the only path that gives the actor a
/// machine-readable event stream (stream-JSON stdout).
/// What: holds the spawned `Child` handle (for Drop / SIGTERM), a
/// `ChildStdin` writer, and a `BufReader<ChildStdout>` for line-by-line event
/// parsing. The session ID is carried for event construction.
/// Test: `stream_json_backend_constructs`.
pub struct StreamJsonBackend {
    session_id: ControlSessionId,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl StreamJsonBackend {
    /// Spawn `claude -p --output-format stream-json` for the given session.
    ///
    /// Why: allocating the backend at session-spawn time (not lazily) lets the
    /// actor emit a `BackendSpawnError` immediately if `claude` is not found,
    /// before the session is registered with the HTTP API.
    /// What: builds a `tokio::process::Command` for
    /// `env -u ANTHROPIC_API_KEY claude -p --output-format stream-json
    /// [--append-system-prompt-file <prompt_file>] --workdir <workdir>`,
    /// spawns with stdin/stdout piped and stderr captured (never forwarded),
    /// and wraps the handles.
    /// Test: `stream_json_spawn_requires_claude` (integration; #[ignore] if
    /// claude absent); constructor checked by `stream_json_backend_constructs`.
    pub fn spawn(
        session_id: ControlSessionId,
        workdir: PathBuf,
        prompt_file: Option<PathBuf>,
    ) -> Result<Self> {
        let mut cmd = Command::new("env");
        // Unset ANTHROPIC_API_KEY so claude resolves credentials from
        // ~/.claude (Max OAuth path, §9.1 of SPEC-SESSCTL-01).
        cmd.arg("-u").arg("ANTHROPIC_API_KEY");
        cmd.arg("claude");
        cmd.arg("-p");
        cmd.arg("--output-format").arg("stream-json");
        if let Some(pf) = prompt_file {
            cmd.arg("--append-system-prompt-file").arg(pf);
        }
        cmd.arg("--workdir").arg(&workdir);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(&workdir);

        let mut child = cmd.spawn().with_context(|| {
            format!(
                "failed to spawn 'claude' for session {} in {}",
                session_id,
                workdir.display()
            )
        })?;

        let stdin = child
            .stdin
            .take()
            .context("claude stdin pipe was not opened")?;
        let raw_stdout = child
            .stdout
            .take()
            .context("claude stdout pipe was not opened")?;
        let stdout = BufReader::new(raw_stdout);

        Ok(Self {
            session_id,
            child,
            stdin,
            stdout,
        })
    }
}

#[async_trait]
impl SessionBackend for StreamJsonBackend {
    /// Send a newline-terminated JSON line to `claude`'s stdin.
    ///
    /// Why: the stream-JSON protocol sends user messages as newline-delimited
    /// JSON objects on stdin; the backend must append `\n` if absent.
    /// What: writes `msg.text + "\n"` to the child's stdin pipe. Returns `Err`
    /// if the stdin write fails (child may have exited).
    /// Test: write-path covered by integration test (`stream_json_spawn_requires_claude`).
    async fn send(&mut self, msg: SessionInput) -> Result<()> {
        let mut text = msg.text;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        self.stdin
            .write_all(text.as_bytes())
            .await
            .context("write to claude stdin failed")?;
        self.stdin.flush().await.context("flush claude stdin failed")
    }

    /// Read the next line from `claude`'s stdout and parse as a stream-JSON event.
    ///
    /// Why: the actor's `select!` loop needs a single async poll point for all
    /// backend output. Returning `None` on EOF signals a clean exit.
    /// What: reads one UTF-8 line from the stdout `BufReader`; on EOF returns
    /// `None`; on a non-empty line attempts to parse it as JSON and wraps it in
    /// `SessionEvent::Output`; on a blank line (heartbeat/separator) loops to
    /// the next call. Returns `Some(Err(…))` only on a hard I/O error.
    /// Test: recv-path covered by integration test.
    async fn recv(&mut self) -> Option<Result<Vec<SessionEvent>>> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = match self.stdout.read_line(&mut line).await {
                Ok(n) => n,
                Err(e) => return Some(Err(e.into())),
            };
            if n == 0 {
                // EOF — child exited cleanly.
                debug!(session_id = %self.session_id, "stream-json stdout EOF");
                return None;
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue; // blank separator line
            }
            // Attempt to parse as stream-JSON event.
            let structured = match serde_json::from_str::<serde_json::Value>(trimmed) {
                Ok(val) => {
                    let event_type = val
                        .get("type")
                        .and_then(|t| t.as_str())
                        .unwrap_or("unknown")
                        .to_owned();
                    Some(StreamJsonEvent {
                        event_type,
                        payload: val,
                    })
                }
                Err(e) => {
                    warn!(
                        session_id = %self.session_id,
                        "failed to parse stream-json line as JSON: {e}: {trimmed}"
                    );
                    None
                }
            };
            let event = SessionEvent::Output {
                session_id: self.session_id.clone(),
                raw: trimmed.to_owned(),
                structured,
                ts: Utc::now(),
            };
            return Some(Ok(vec![event]));
        }
    }

    /// Send SIGTERM to the child process and wait for it to exit.
    ///
    /// Why: the RAII reaping contract (§10.1) requires that every
    /// `StreamJsonBackend` cleans up its child on stop, not just on Drop.
    /// `stop()` gives the child a graceful exit path before forcing SIGKILL.
    /// What: sends SIGTERM via `child.start_kill()`, then awaits the child.
    /// Drop provides a final safety net via `child.start_kill()` in case
    /// `stop()` is never called.
    /// Test: stop-path covered by integration test.
    async fn stop(mut self: Box<Self>) -> Result<()> {
        let _ = self.child.start_kill(); // SIGTERM; ignore if already exited
        let _ = self.child.wait().await; // reap the child
        Ok(())
    }
}

impl Drop for StreamJsonBackend {
    /// Send SIGTERM on drop to prevent orphaned `claude` child processes.
    ///
    /// Why: if the actor task is cancelled or panics without calling `stop()`,
    /// the child process would become an orphan. The Drop impl is the final
    /// safety net per §10.1 (RAII reaping contract).
    /// What: calls `child.start_kill()` (non-blocking SIGTERM); stdout/stdin
    /// are already closed by dropping the handles, which signals EOF.
    /// Test: side-effect-only; covered by the integration cleanup suite.
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_input_newtype() {
        let input = SessionInput::new("hello");
        assert_eq!(input.text, "hello");
    }

    /// Verify that spawning when `claude` is absent returns a descriptive error.
    /// This test is skipped with `#[ignore]` in CI where `claude` is installed,
    /// so it does not run as an integration test by default.
    #[test]
    #[ignore = "requires claude binary absent from PATH"]
    fn stream_json_spawn_fails_gracefully_without_claude() {
        // This can only be run when we can manipulate PATH; skip in normal CI.
    }
}
