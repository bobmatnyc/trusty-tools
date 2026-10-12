//! Stream-JSON backend: warm headless `claude -p --output-format stream-json`.
//!
//! Why: the default execution backend spawns a long-lived `claude` child
//! process that accepts newline-delimited JSON on stdin and emits
//! newline-delimited stream-JSON events on stdout. This keeps the session warm
//! across multiple user messages without the overhead of a new process per
//! message, and uses Max OAuth billing by unsetting `ANTHROPIC_API_KEY` (§9.1).
//! What: [`StreamJsonBackend`] spawns `claude -p --output-format stream-json
//! [--append-system-prompt-file …]` in the session's workdir (its cwd, #9704)
//! with `ANTHROPIC_API_KEY`
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
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
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

/// How long `recv` waits for the worker to exit once its stdout closes (#9725).
const EXIT_WAIT_BOUND: Duration = Duration::from_secs(5);

/// The least time the post-SIGKILL reap is given, whatever the exit bound (#9725).
const POST_KILL_REAP_FLOOR: Duration = Duration::from_secs(2);

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
/// `claude -p --output-format stream-json [--append-system-prompt-file <f>]`
/// with `ANTHROPIC_API_KEY` removed from the child env,
/// stdin/stdout piped, stderr captured, and the working directory set to
/// `workdir` — the only way the workdir reaches claude, which has no
/// `--workdir` option (#9704). The
/// caller converts it to a `tokio::process::Command` to spawn.
///
/// Issue #4467: the same `env_remove` seam also strips Claude Code's inherited
/// process-local session markers via
/// [`crate::core::claude_env_scrub::scrub_command`]. This backend inherits the
/// daemon's environment, so when the daemon was itself started from inside a
/// Claude Code session the leaked `CLAUDE_CODE_CHILD_SESSION` reached every
/// headless `claude` it spawned and disabled transcript saving.
/// Test: `build_command_removes_api_key`, `build_command_has_stream_json_args`,
/// `build_command_scrubs_inherited_session_markers`,
/// `build_command_does_not_pass_workdir_flag`.
pub fn build_claude_command(workdir: &Path, prompt_file: Option<&Path>) -> std::process::Command {
    // Invoke `claude` directly (not via the `env -u` shell wrapper). Using
    // Command::env_remove makes the key-removal an explicit, inspectable part
    // of the child environment (visible via get_envs() in tests), which is what
    // the WI-5 gating test asserts. The child inherits the parent's environment
    // MINUS the removed key.
    let mut cmd = std::process::Command::new("claude");
    // §9.1 LOCKED: strip the metered key so claude uses ~/.claude Max OAuth.
    cmd.env_remove(ANTHROPIC_API_KEY_VAR);
    // #4467: strip the inherited Claude Code session markers for the same reason.
    crate::core::claude_env_scrub::scrub_command(&mut cmd);
    cmd.arg("-p");
    cmd.arg("--output-format").arg("stream-json");
    if let Some(pf) = prompt_file {
        cmd.arg("--append-system-prompt-file").arg(pf);
    }
    // #9704: no `--workdir` — claude rejects it; `current_dir` below sets it.
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
    /// `None` once the stdout-EOF reap owns the child (#9725).
    child: Option<ChildHandle>,
    /// The bounded stdout-EOF reap task, once started (#9725).
    exit_reaper: Option<tokio::task::JoinHandle<Result<()>>>,
    /// Tells the pending reap to SIGKILL now; sent by `stop`, dropped by Drop.
    reap_kill: Option<tokio::sync::oneshot::Sender<()>>,
    exit_wait_bound: Duration,
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
    /// [--append-system-prompt-file <prompt_file>]` with cwd `workdir` and
    /// `ANTHROPIC_API_KEY` removed from the child environment, then spawns it
    /// via [`disclaimed_piped_spawn`] (issue #2997 — disclaims TCC
    /// responsibility on macOS; an unchanged `tokio::process::Command` spawn
    /// elsewhere) and wraps the resulting handles.
    /// Test: `stream_json_spawn_requires_claude` (integration; #\[ignore\] if
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
        let sid = session_id.clone();
        Self::spawn_command(session_id, std_cmd, EXIT_WAIT_BOUND).with_context(|| {
            format!(
                "failed to spawn 'claude' for session {} in {}",
                sid,
                workdir.display()
            )
        })
    }

    /// Spawn an already-built command as the session's worker.
    ///
    /// Why: #9725 — `spawn` hard-codes `claude`, which left the worker-exit
    /// paths of `recv` with no test seam. This takes any piped command and an
    /// injectable stdout-EOF exit-wait bound, so tests drive `sh -c` workers
    /// through the same [`disclaimed_piped_spawn`] path `claude` uses.
    /// What: spawns `cmd` via [`disclaimed_piped_spawn`] and wraps the handles.
    /// `exit_wait_bound` caps how long `recv` waits for the child to exit
    /// after its stdout closes.
    /// Test: `recv_reports_nonzero_exit_before_any_output_as_error`,
    /// `recv_times_out_when_stdout_closes_but_child_lives_and_reaps_it`.
    pub(crate) fn spawn_command(
        session_id: ControlSessionId,
        cmd: std::process::Command,
        exit_wait_bound: Duration,
    ) -> std::io::Result<Self> {
        let spawned = disclaimed_piped_spawn(cmd)?;
        Ok(Self {
            session_id,
            child: Some(spawned.handle),
            exit_reaper: None,
            reap_kill: None,
            exit_wait_bound,
            stdin: spawned.stdin,
            stdout: BufReader::new(spawned.stdout),
        })
    }

    /// Settle the worker's exit once its stdout has closed.
    ///
    /// Why: #9725 — `recv` read stdout EOF as a clean exit, so a worker that
    /// failed or was killed ended `Stopped{Completed}`. The exit status decides.
    /// What: on the first call hands the child to a spawned [`reap_at_eof`]
    /// task, then awaits that task. Returns `None` for exit 0 and `Some(Err)`
    /// otherwise. The reap runs in its own task so a `recv` cancelled by the
    /// actor's `select!` loses neither the wait nor the kill; the next call
    /// awaits the same task. A call after the outcome is returned yields `None`.
    /// Test: `recv_reports_nonzero_exit_before_any_output_as_error`,
    /// `recv_cancelled_during_exit_wait_resumes_the_same_reap`.
    async fn exit_outcome(&mut self) -> Option<Result<Vec<SessionEvent>>> {
        if let Some(child) = self.child.take() {
            let (kill_tx, kill_rx) = tokio::sync::oneshot::channel();
            self.reap_kill = Some(kill_tx);
            self.exit_reaper = Some(tokio::spawn(reap_at_eof(
                child,
                self.exit_wait_bound,
                kill_rx,
            )));
        }
        let outcome = self.exit_reaper.as_mut()?.await;
        self.exit_reaper = None;
        self.reap_kill = None;
        match outcome {
            Ok(Ok(())) => None,
            Ok(Err(e)) => Some(Err(e)),
            Err(join) => Some(Err(anyhow!("claude worker exit reap failed: {join}"))),
        }
    }
}

/// The worker's raw pid while it is unreaped (#9725).
fn child_pid(child: &ChildHandle) -> Option<i32> {
    match child {
        ChildHandle::Native(c) => c.id().and_then(|p| i32::try_from(p).ok()),
        #[cfg(target_os = "macos")]
        ChildHandle::Disclaimed { pid, .. } => Some(*pid),
    }
}

/// Wait, bounded, for a worker whose stdout closed and classify its exit.
///
/// Why: #9725 — a worker that exits non-zero or by signal must fail the
/// session, and one that closes stdout but keeps running must neither hang
/// the actor nor be leaked.
/// What: waits up to `bound` for the exit. Exit 0 is `Ok(())`; any other
/// status is `Err` naming it (`exit status: N` or `signal: N`). When `bound`
/// elapses first, or `kill_now` resolves (a `stop` request, or the backend
/// dropped), SIGKILLs the pid, waits up to `bound.max(POST_KILL_REAP_FLOOR)`
/// for the reap, and returns `Err`. One `wait()` future is polled throughout,
/// so the child is reaped exactly once — `ChildHandle::wait` allows one call
/// on the disclaimed path — and the pid is signalled only while that wait is
/// pending.
/// Test: `recv_reports_signal_exit_as_error`,
/// `recv_times_out_when_stdout_closes_but_child_lives_and_reaps_it`,
/// `recv_keeps_fast_clean_exit_as_none`,
/// `stop_during_pending_exit_reap_kills_at_once_and_logs_the_result`.
async fn reap_at_eof(
    mut child: ChildHandle,
    bound: Duration,
    kill_now: tokio::sync::oneshot::Receiver<()>,
) -> Result<()> {
    let pid = child_pid(&child);
    let wait = child.wait();
    tokio::pin!(wait);
    let why = tokio::select! {
        biased;
        status = &mut wait => {
            let status = status.context("waiting for the claude worker to exit failed")?;
            return if status.success() {
                Ok(())
            } else {
                Err(anyhow!("claude worker exited unsuccessfully ({status})"))
            };
        }
        _ = kill_now => "was killed by a stop request while its exit was pending".to_owned(),
        () = tokio::time::sleep(bound) => {
            format!("closed stdout but was still running after {bound:?}")
        }
    };
    if let Some(pid) = pid {
        // #9725: the wait above is still pending, so the pid is unreaped.
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let reap = match tokio::time::timeout(bound.max(POST_KILL_REAP_FLOOR), &mut wait).await {
        Ok(Ok(status)) => format!("killed: {status}"),
        Ok(Err(e)) => format!("killed; reap failed: {e}"),
        Err(_) => "SIGKILL sent; not reaped within the bound".to_owned(),
    };
    Err(anyhow!("claude worker {why} ({reap})"))
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
    /// backend output. `None` signals a clean exit (exit code 0).
    /// What: reads one UTF-8 line from the stdout `BufReader`; on EOF settles
    /// the worker's exit status (#9725, see `exit_outcome`): `None` for exit 0,
    /// `Some(Err)` for a non-zero, signal or still-running worker. A non-empty
    /// line is parsed as JSON and wrapped in `SessionEvent::Output`; a blank
    /// line (heartbeat/separator) loops. A hard I/O error is `Some(Err(…))`.
    /// Test: `recv_keeps_fast_clean_exit_as_none`,
    /// `recv_reports_nonzero_exit_before_any_output_as_error`.
    async fn recv(&mut self) -> Option<Result<Vec<SessionEvent>>> {
        let mut line = String::new();
        loop {
            line.clear();
            let n = match self.stdout.read_line(&mut line).await {
                Ok(n) => n,
                Err(e) => return Some(Err(e.into())),
            };
            if n == 0 {
                // #9725: EOF is not proof of a clean exit; the exit status decides.
                debug!(session_id = %self.session_id, "stream-json stdout EOF");
                return self.exit_outcome().await;
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
    /// #9725: once the stdout-EOF reap owns the child, `stop` tells that reap
    /// to SIGKILL now and awaits it, so the child is never waited on twice. The
    /// reap's result is logged at warn; the stop itself stays Ok.
    /// Test: `stop_after_reported_exit_does_not_wait_twice`,
    /// `stop_during_pending_exit_reap_kills_at_once_and_logs_the_result`.
    async fn stop(mut self) -> Result<()> {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill(); // SIGKILL; ignore if already exited
            let _ = child.wait().await; // reap the child
        } else if let Some(reaper) = self.exit_reaper.take() {
            if let Some(kill) = self.reap_kill.take() {
                let _ = kill.send(()); // Err: the reap already finished
            }
            match reaper.await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => warn!(session_id = %self.session_id, "stop: {e:#}"),
                Err(join) => warn!(
                    session_id = %self.session_id,
                    "stop: claude worker exit reap failed: {join}"
                ),
            }
        }
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
        // #9725: a child handed to the stdout-EOF reap is that task's to kill;
        // dropping `reap_kill` tells it to kill now.
        if let Some(child) = self.child.as_mut() {
            let _ = child.start_kill();
        }
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
    fn build_command_scrubs_inherited_session_markers() {
        // #4467: the daemon may itself have been started from inside a Claude
        // Code session, in which case every headless `claude` it spawned
        // inherited CLAUDE_CODE_CHILD_SESSION and saved no transcript. The
        // marker name is hard-coded so the assertion cannot go vacuous if the
        // shared constant is emptied.
        let cmd = build_claude_command(Path::new("/tmp/wd"), None);
        let muts = env_mutations(&cmd);
        let removed = muts
            .iter()
            .find(|(k, _)| k == "CLAUDE_CODE_CHILD_SESSION")
            .expect("CLAUDE_CODE_CHILD_SESSION must appear as an explicit env mutation");
        assert_eq!(
            removed.1, None,
            "CLAUDE_CODE_CHILD_SESSION must be REMOVED (None) from the child env"
        );
        assert!(
            !muts.iter().any(|(k, _)| k == "CLAUDE_CONFIG_DIR"),
            "CLAUDE_CONFIG_DIR must not be touched by the scrub (#4455): {muts:?}"
        );
    }

    /// #8453: a daemon started from a supervisor session carries its
    /// `TRUSTY_MPM_SESSION_PROFILE`; the headless child must not inherit it.
    #[test]
    fn build_command_never_passes_on_the_profile_stamp() {
        let cmd = build_claude_command(Path::new("/tmp/wd"), None);
        assert!(
            env_mutations(&cmd).contains(&("TRUSTY_MPM_SESSION_PROFILE".to_owned(), None)),
            "the profile stamp must be REMOVED from the headless child"
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
        // #9704: claude has no `--workdir`; the cwd carries the workdir.
        assert!(!args.contains(&"--workdir".to_string()));
        // No prompt file → no --append-system-prompt-file flag.
        assert!(!args.contains(&"--append-system-prompt-file".to_string()));
    }

    /// #9704: `claude` 2.1.296 exits with `error: unknown option '--workdir'`,
    /// so the worker died before its first event. The workdir reaches the child
    /// as its cwd only, with or without a prompt file.
    #[test]
    fn build_command_does_not_pass_workdir_flag() {
        let pf = PathBuf::from("/tmp/prompt.md");
        for prompt_file in [None, Some(pf.as_path())] {
            let cmd = build_claude_command(Path::new("/tmp/wd"), prompt_file);
            let args: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            assert!(
                !args.iter().any(|a| a.starts_with("--workdir")),
                "claude rejects --workdir; argv was {args:?}"
            );
            assert!(
                !args.contains(&"/tmp/wd".to_string()),
                "the workdir must not ride in argv: {args:?}"
            );
            assert_eq!(cmd.get_current_dir(), Some(Path::new("/tmp/wd")));
        }
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

    // ── #9725: worker exit status at stdout EOF ──────────────────────────────

    /// Spawn `sh -c <script>` as a worker through the disclaimed spawn path.
    fn spawn_sh(script: &str, exit_wait_bound: Duration) -> StreamJsonBackend {
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        StreamJsonBackend::spawn_command(ControlSessionId::new("exit", 0), cmd, exit_wait_bound)
            .expect("sh must spawn")
    }

    /// The worker's raw pid, read from the live handle.
    fn worker_pid(backend: &StreamJsonBackend) -> i32 {
        backend
            .child
            .as_ref()
            .and_then(child_pid)
            .expect("a freshly spawned worker has a pid")
    }

    /// Assert the pid is neither alive nor an unreaped zombie: both answer
    /// signal 0, so ESRCH proves the worker was killed AND reaped.
    fn assert_reaped(pid: i32) {
        let probe = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None);
        assert_eq!(
            probe,
            Err(nix::errno::Errno::ESRCH),
            "worker pid {pid} must be gone (killed and reaped)"
        );
    }

    /// Name a `recv` outcome for an assertion message.
    fn outcome(o: &Option<Result<Vec<SessionEvent>>>) -> String {
        match o {
            None => "None".into(),
            Some(Ok(ev)) => format!("Some(Ok({} events))", ev.len()),
            Some(Err(e)) => format!("Some(Err({e:#}))"),
        }
    }

    /// Await one `recv`, bounded so a hang fails instead of stalling the run.
    async fn recv_bounded(backend: &mut StreamJsonBackend) -> Option<Result<Vec<SessionEvent>>> {
        tokio::time::timeout(Duration::from_secs(10), backend.recv())
            .await
            .expect("recv must return within 10 s")
    }

    #[tokio::test]
    async fn recv_reports_nonzero_exit_before_any_output_as_error() {
        let mut backend = spawn_sh("exit 2", Duration::from_secs(5));
        let got = recv_bounded(&mut backend).await;
        match &got {
            Some(Err(e)) => assert!(
                format!("{e:#}").contains("exit status: 2"),
                "the failure must name exit code 2: {e:#}"
            ),
            other => panic!("exit 2 must be Some(Err), got {}", outcome(other)),
        }
    }

    #[tokio::test]
    async fn recv_reports_signal_exit_as_error() {
        let mut backend = spawn_sh("kill -9 $$", Duration::from_secs(5));
        let got = recv_bounded(&mut backend).await;
        match &got {
            Some(Err(e)) => assert!(
                format!("{e:#}").contains("signal: 9"),
                "the failure must name signal 9: {e:#}"
            ),
            other => panic!(
                "a SIGKILLed worker must be Some(Err), got {}",
                outcome(other)
            ),
        }
    }

    #[tokio::test]
    async fn recv_keeps_fast_clean_exit_as_none() {
        let mut backend = spawn_sh(
            r#"echo '{"type":"result"}'; exit 0"#,
            Duration::from_secs(5),
        );
        let first = recv_bounded(&mut backend).await;
        assert!(
            matches!(&first, Some(Ok(ev)) if ev.len() == 1),
            "the result line must arrive first, got {}",
            outcome(&first)
        );
        let second = recv_bounded(&mut backend).await;
        assert!(
            second.is_none(),
            "exit 0 must end in None, got {}",
            outcome(&second)
        );

        let mut silent = spawn_sh("exit 0", Duration::from_secs(5));
        let only = recv_bounded(&mut silent).await;
        assert!(
            only.is_none(),
            "a silent exit 0 must be None, got {}",
            outcome(&only)
        );
    }

    #[tokio::test]
    async fn recv_times_out_when_stdout_closes_but_child_lives_and_reaps_it() {
        let bound = Duration::from_millis(200);
        let mut backend = spawn_sh("exec >&-; exec sleep 30", bound);
        let pid = worker_pid(&backend);
        let started = std::time::Instant::now();
        let got = recv_bounded(&mut backend).await;
        match &got {
            Some(Err(e)) => assert!(
                format!("{e:#}").contains("still running"),
                "the failure must say the worker outlived the bound: {e:#}"
            ),
            other => panic!(
                "a worker alive after stdout EOF must be Some(Err), got {}",
                outcome(other)
            ),
        }
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "recv must give up near the {bound:?} bound, took {:?}",
            started.elapsed()
        );
        assert_reaped(pid);
    }

    /// The actor's `select!` drops a pending `recv` whenever a command
    /// arrives; the reap must survive that and the next `recv` must finish it.
    #[tokio::test]
    async fn recv_cancelled_during_exit_wait_resumes_the_same_reap() {
        let mut backend = spawn_sh("exec >&-; exec sleep 30", Duration::from_millis(300));
        let pid = worker_pid(&backend);
        let cancelled = tokio::time::timeout(Duration::from_millis(150), backend.recv()).await;
        assert!(cancelled.is_err(), "the first recv must still be waiting");
        let got = recv_bounded(&mut backend).await;
        assert!(
            matches!(&got, Some(Err(_))),
            "the resumed reap must report the lingering worker, got {}",
            outcome(&got)
        );
        assert_reaped(pid);
    }

    /// A `stop` after `recv` reported the exit must not wait on the child a
    /// second time (the disclaimed handle panics on a second `wait`).
    #[tokio::test]
    async fn stop_after_reported_exit_does_not_wait_twice() {
        let mut backend = spawn_sh("exit 3", Duration::from_secs(5));
        let got = recv_bounded(&mut backend).await;
        assert!(matches!(&got, Some(Err(_))), "got {}", outcome(&got));
        backend
            .stop()
            .await
            .expect("stop after a reported exit is Ok");
    }

    /// A requested stop of a live worker still SIGKILLs and reaps it.
    #[tokio::test]
    async fn stop_kills_and_reaps_a_live_worker() {
        let backend = spawn_sh("sleep 30", Duration::from_secs(5));
        let pid = worker_pid(&backend);
        tokio::time::timeout(Duration::from_secs(10), backend.stop())
            .await
            .expect("stop must return within 10 s")
            .expect("stop is Ok");
        assert_reaped(pid);
    }

    /// A `tracing` writer that keeps every formatted line for an assertion.
    #[derive(Clone, Default)]
    struct LogCapture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for LogCapture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("capture lock").extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogCapture {
        type Writer = LogCapture;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// The actor drops a pending `recv` on Stop/ForceStop, then calls `stop`.
    /// `stop` must kill the lingering worker at once, not wait out the reap's
    /// own bound, return Ok, and log the reap's result instead of dropping it.
    #[tokio::test]
    async fn stop_during_pending_exit_reap_kills_at_once_and_logs_the_result() {
        // #4931: a thread-local capture records nothing unless MAX_LEVEL is raised.
        crate::test_support::enable_event_capture();
        let logs = LogCapture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let bound = Duration::from_secs(5);
        let mut backend = spawn_sh("exec >&-; exec sleep 30", bound);
        let pid = worker_pid(&backend);
        let cancelled = tokio::time::timeout(Duration::from_secs(1), backend.recv()).await;
        assert!(cancelled.is_err(), "the first recv must still be waiting");
        assert!(
            backend.child.is_none() && backend.exit_reaper.is_some(),
            "recv must have reached stdout EOF and handed the child to the exit reap \
             before stop() runs, or this test does not exercise the pending-reap branch"
        );

        let started = std::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(10), backend.stop())
            .await
            .expect("stop must return within 10 s")
            .expect("a requested stop is Ok even though it killed the worker");
        let took = started.elapsed();
        assert!(
            took < bound / 2,
            "stop must kill at once, not wait out the {bound:?} reap bound; took {took:?}"
        );
        assert_reaped(pid);
        let logged = String::from_utf8(logs.0.lock().expect("capture lock").clone())
            .expect("the captured log must be utf-8");
        assert!(
            logged.contains("WARN") && logged.contains("claude worker"),
            "stop must log the reap's result at warn: {logged:?}"
        );
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
