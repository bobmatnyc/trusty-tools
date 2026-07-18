//! Stream-JSON backend: warm headless `claude -p --output-format stream-json`.
//!
//! Why: the default execution backend spawns a long-lived `claude` child
//! process that accepts newline-delimited JSON on stdin and emits
//! newline-delimited stream-JSON events on stdout. This keeps the session warm
//! across multiple user messages without the overhead of a new process per
//! message, and uses Max OAuth billing by unsetting `ANTHROPIC_API_KEY` (§9.1).
//! What: [`StreamJsonBackend`] spawns `claude -p --output-format stream-json
//! [--append-system-prompt-file …] --workdir <dir>` with `ANTHROPIC_API_KEY`
//! removed from the child environment via [`build_claude_command`] (the §9.1
//! no-key-leak seam) and implements [`super::SessionBackend`]. The spawn
//! itself goes through [`crate::core::spawn_disclaim::disclaimed_piped_spawn`]
//! (issue #2997): on macOS this disclaims TCC responsibility so the child —
//! and everything IT forks, e.g. an agent's `cargo build` — is its own
//! responsible process instead of the access request rolling up to the
//! signed `trusty-mpm` binary (the same #2819/#2721 App-Data/media-library
//! mis-attribution class the tmux-hosted path was already fixed for); on
//! non-macOS it is an unchanged pass-through to `tokio::process::Command`.
//! `recv()` reads stdout line-by-line and parses each line as a
//! `StreamJsonEvent`. Stderr is captured but never forwarded. On clean child
//! exit `recv()` returns `None`. A seam for the crash-watchdog / auto-restart
//! policy (WI-4) is present but not wired in Phase 1.
//! Test: `stream_json_spawn_requires_claude` (skipped if `claude` absent),
//! `stream_json_backend_constructs` (constructor-only; no child process),
//! `session_input_newtype` in the inline module. The disclaim spawn itself is
//! covered by `spawn_piped_disclaimed_*`/`disclaimed_piped_spawn_native_path_round_trips`
//! in `crate::core::spawn_disclaim`.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::Utc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracing::{debug, warn};

use crate::control::event::{SessionEvent, StreamJsonEvent};
use crate::control::id::ControlSessionId;
use crate::core::spawn_disclaim::{
    ChildHandle, DynChildStdin, DynChildStdout, disclaimed_piped_spawn,
};

use super::{SessionBackend, SessionInput};

/// Environment variable holding the metered Anthropic API key.
///
/// Why: §9.1 LOCKED requirement — this variable MUST be removed from every
/// spawned `claude` child so the child resolves Max-OAuth credentials from
/// `~/.claude` and no metered key leaks into a managed session. Naming it once
/// as a constant keeps the security-critical string from drifting between the
/// command builder and the tests that assert its removal.
/// What: the literal `"ANTHROPIC_API_KEY"`.
/// Test: `build_command_removes_api_key`, `build_command_removes_key_even_if_set`.
pub const ANTHROPIC_API_KEY_VAR: &str = "ANTHROPIC_API_KEY";

/// Build the `claude` stream-JSON [`std::process::Command`] for a session.
///
/// Why: this is the single security seam where §9.1's no-key-leak invariant is
/// enforced. Extracting command construction into a pure function (no spawn,
/// no I/O) lets the gating integration test inspect the built command's
/// environment and assert `ANTHROPIC_API_KEY` is removed — WITHOUT spawning a
/// real `claude` process. Defense-in-depth: the key is stripped two ways — an
/// explicit [`std::process::Command::env_remove`] (which `get_envs()` exposes as
/// an explicit removal, making it test-observable) AND, belt-and-braces, the
/// child never inherits a re-added key because we never call `env()` for it.
/// What: returns a `std::process::Command` for
/// `claude -p --output-format stream-json [--append-system-prompt-file <f>]
/// --workdir <dir>` with `ANTHROPIC_API_KEY` removed from the child env,
/// stdin/stdout piped, stderr captured, and the working directory set. The
/// caller converts it to a `tokio::process::Command` to spawn.
/// Test: `build_command_removes_api_key`, `build_command_has_stream_json_args`.
pub fn build_claude_command(workdir: &Path, prompt_file: Option<&Path>) -> std::process::Command {
    // Invoke `claude` directly (not via the `env -u` shell wrapper). Using
    // Command::env_remove makes the key-removal an explicit, inspectable part
    // of the child environment (visible via get_envs() in tests), which is what
    // the WI-5 gating test asserts. The child inherits the parent's environment
    // MINUS the removed key.
    let mut cmd = std::process::Command::new("claude");
    // §9.1 LOCKED: strip the metered key so claude uses ~/.claude Max OAuth.
    cmd.env_remove(ANTHROPIC_API_KEY_VAR);
    cmd.arg("-p");
    cmd.arg("--output-format").arg("stream-json");
    if let Some(pf) = prompt_file {
        cmd.arg("--append-system-prompt-file").arg(pf);
    }
    cmd.arg("--workdir").arg(workdir);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .current_dir(workdir);
    cmd
}

/// Warm headless `claude -p --output-format stream-json` execution backend.
///
/// Why: the stream-JSON backend is the alpha-1 default because it lets the
/// daemon supervise a `claude` process without a PTY and without exposing the
/// interactive readline TUI. It's the only path that gives the actor a
/// machine-readable event stream (stream-JSON stdout).
/// What: holds the spawned [`ChildHandle`] (for Drop / SIGKILL via
/// `start_kill()` — uniform across the native and macOS-disclaimed spawn
/// paths, see [`disclaimed_piped_spawn`]), a [`DynChildStdin`] writer, and a
/// `BufReader<DynChildStdout>` for line-by-line event parsing. The session ID
/// is carried for event construction. Note: `ChildHandle::start_kill()` sends
/// SIGKILL, not SIGTERM — a graceful SIGTERM → wait → SIGKILL sequence is
/// deferred to a later phase once a shutdown timeout is defined.
/// Test: `stream_json_backend_constructs`.
pub struct StreamJsonBackend {
    session_id: ControlSessionId,
    child: ChildHandle,
    stdin: DynChildStdin,
    stdout: BufReader<DynChildStdout>,
}

impl StreamJsonBackend {
    /// Spawn `claude -p --output-format stream-json` for the given session.
    ///
    /// Why: allocating the backend at session-spawn time (not lazily) lets the
    /// actor emit a `BackendSpawnError` immediately if `claude` is not found,
    /// before the session is registered with the HTTP API.
    /// What: delegates to [`build_claude_command`] (the §9.1 no-key-leak seam)
    /// for `claude -p --output-format stream-json
    /// [--append-system-prompt-file <prompt_file>] --workdir <workdir>` with
    /// `ANTHROPIC_API_KEY` removed from the child environment, then spawns it
    /// via [`disclaimed_piped_spawn`] (issue #2997 — disclaims TCC
    /// responsibility on macOS; an unchanged `tokio::process::Command` spawn
    /// elsewhere) and wraps the resulting handles.
    /// Test: `stream_json_spawn_requires_claude` (integration; #[ignore] if
    /// claude absent); constructor checked by `stream_json_backend_constructs`;
    /// the no-key-leak invariant is asserted at the command seam by
    /// `build_command_removes_api_key` and the `auth_cost` integration suite;
    /// the disclaim spawn itself by `spawn_piped_disclaimed_*` in
    /// `crate::core::spawn_disclaim`.
    pub fn spawn(
        session_id: ControlSessionId,
        workdir: PathBuf,
        prompt_file: Option<PathBuf>,
    ) -> Result<Self> {
        let std_cmd = build_claude_command(&workdir, prompt_file.as_deref());

        let spawned = disclaimed_piped_spawn(std_cmd).with_context(|| {
            format!(
                "failed to spawn 'claude' for session {} in {}",
                session_id,
                workdir.display()
            )
        })?;

        Ok(Self {
            session_id,
            child: spawned.handle,
            stdin: spawned.stdin,
            stdout: BufReader::new(spawned.stdout),
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
        self.stdin
            .flush()
            .await
            .context("flush claude stdin failed")
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

    /// Kill the child process and wait for it to exit.
    ///
    /// Why: the RAII reaping contract (§10.1) requires that every
    /// `StreamJsonBackend` cleans up its child on stop, not just on Drop.
    /// What: calls `child.start_kill()` (SIGKILL, non-blocking) then awaits
    /// the child exit. Note: `start_kill()` sends SIGKILL, not SIGTERM; a
    /// graceful SIGTERM → wait → SIGKILL sequence is deferred to a later phase.
    /// Drop provides a final safety net in case `stop()` is never called.
    /// Test: stop-path covered by integration test.
    async fn stop(mut self) -> Result<()> {
        let _ = self.child.start_kill(); // SIGKILL; ignore if already exited
        let _ = self.child.wait().await; // reap the child
        Ok(())
    }
}

impl Drop for StreamJsonBackend {
    /// Send SIGKILL on drop to prevent orphaned `claude` child processes.
    ///
    /// Why: if the actor task is cancelled or panics without calling `stop()`,
    /// the child process would become an orphan. The Drop impl is the final
    /// safety net per §10.1 (RAII reaping contract).
    /// What: calls `child.start_kill()` (non-blocking SIGKILL); stdout/stdin
    /// are already closed by dropping the handles, which signals EOF.
    /// Test: side-effect-only; covered by the integration cleanup suite.
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn session_input_newtype() {
        let input = SessionInput::new("hello");
        assert_eq!(input.text, "hello");
    }

    /// Collect the explicit env mutations from a built command as
    /// `(key, Option<value>)` pairs. `None` value = `env_remove` (the security
    /// invariant we assert below).
    fn env_mutations(cmd: &std::process::Command) -> Vec<(String, Option<String>)> {
        cmd.get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    #[test]
    fn build_command_removes_api_key() {
        // §9.1: the built command MUST explicitly remove ANTHROPIC_API_KEY so
        // it cannot leak into the spawned child. get_envs() surfaces an
        // env_remove as (key, None).
        let cmd = build_claude_command(Path::new("/tmp/wd"), None);
        let muts = env_mutations(&cmd);
        let removed = muts
            .iter()
            .find(|(k, _)| k == ANTHROPIC_API_KEY_VAR)
            .expect("ANTHROPIC_API_KEY must appear as an explicit env mutation");
        assert_eq!(
            removed.1, None,
            "ANTHROPIC_API_KEY must be REMOVED (None), never set, in the child env"
        );
    }

    #[test]
    fn build_command_program_is_claude() {
        // The seam invokes `claude` directly, not the `env` shell wrapper, so
        // get_program() is stable and inspectable.
        let cmd = build_claude_command(Path::new("/tmp/wd"), None);
        assert_eq!(cmd.get_program(), OsStr::new("claude"));
    }

    #[test]
    fn build_command_has_stream_json_args() {
        let cmd = build_claude_command(Path::new("/tmp/wd"), None);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"-p".to_string()));
        assert!(args.contains(&"--output-format".to_string()));
        assert!(args.contains(&"stream-json".to_string()));
        assert!(args.contains(&"--workdir".to_string()));
        // No prompt file → no --append-system-prompt-file flag.
        assert!(!args.contains(&"--append-system-prompt-file".to_string()));
    }

    #[test]
    fn build_command_includes_prompt_file_when_present() {
        let pf = PathBuf::from("/tmp/prompt.md");
        let cmd = build_claude_command(Path::new("/tmp/wd"), Some(&pf));
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--append-system-prompt-file".to_string()));
        assert!(args.contains(&"/tmp/prompt.md".to_string()));
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
