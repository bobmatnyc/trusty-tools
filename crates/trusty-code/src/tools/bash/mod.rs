//! `bash` tool — execute a shell command with timeout enforcement and output capture.
//!
//! Why: The coder agent loop needs to run commands (pytest, cargo test, etc.)
//! and read their output to iterate. Surfacing a non-zero exit code as a
//! recoverable `ToolResult` (not a hard error) lets the LLM read failing test
//! output and self-correct without aborting the loop.
//! What: `BashTool` implements `ToolExecutor`; it runs a command via `sh -c`
//! inside `tokio::process::Command`, enforces a configurable timeout, kills the
//! child (and its process group on Unix) on expiry, captures stdout + stderr
//! (each truncated to 100 KB), and returns a structured result that includes
//! the exit code, captured output, and a timeout flag.
//! Test: Unit tests live in `bash::tests`; they cover fast success, non-zero
//! exit, timeout-kill, cwd honoring, stdout/stderr truncation, registry
//! registration, and dispatch.

#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;

use crate::tools::traits::{ServiceTier, ToolExecutor, ToolResult};

/// Maximum bytes captured from stdout or stderr before truncation.
pub(crate) const MAX_OUTPUT_BYTES: usize = 100 * 1024; // 100 KB

/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Executes arbitrary shell commands on behalf of the coder agent loop.
///
/// Why: Lets the LLM run test suites (pytest, cargo test, etc.), build steps,
/// and diagnostic commands without a separate MCP bridge. Capturing output
/// as a `ToolResult` (not a fatal error) on non-zero exit means the model
/// can read failing test output and iterate.
/// What: Wraps `tokio::process::Command` launched via `sh -c`; enforces a
/// configurable per-call timeout; kills the child process group (POSIX) on
/// expiry; truncates large outputs.
/// Test: `bash::tests::fast_command_exit_zero`, `nonzero_exit_is_recoverable`,
/// `timeout_kills_child`, `cwd_is_honored`, `stdout_truncation`,
/// `registry_registration_and_dispatch`.
pub struct BashTool {
    /// Working directory for the spawned process. `None` means inherit the
    /// tcode process's current directory.
    pub working_dir: Option<PathBuf>,
    /// Default timeout applied when the caller does not supply `timeout_secs`.
    pub default_timeout: Duration,
}

impl BashTool {
    /// Construct with an optional working directory and default timeout.
    ///
    /// Why: Callers providing a `RunContext` can inject `working_dir` so the
    /// shell respects the project root without mutating global process state.
    /// What: Stores `working_dir` and `default_timeout`.
    /// Test: Constructed in all `BashTool` tests.
    pub fn new(working_dir: Option<PathBuf>, default_timeout: Duration) -> Self {
        Self {
            working_dir,
            default_timeout,
        }
    }

    /// Construct with default settings (no cwd override, 120 s timeout).
    ///
    /// Why: Convenience constructor for callers that accept process defaults.
    /// What: Delegates to `new(None, DEFAULT_TIMEOUT_SECS seconds)`.
    /// Test: `bash::tests::fast_command_exit_zero`.
    pub fn default_config() -> Self {
        Self::new(None, Duration::from_secs(DEFAULT_TIMEOUT_SECS))
    }
}

#[async_trait]
impl ToolExecutor for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    /// OpenAI-function schema for the bash tool.
    ///
    /// Why: The LLM function-calling loop requires a JSON schema to generate
    /// valid calls. Declaring `command` as required and `timeout_secs` as
    /// optional keeps the schema minimal while supporting per-call overrides.
    /// What: Returns a `{ type: "function", function: { … } }` Value.
    /// Test: Checked via `bash::tests::registry_registration_and_dispatch`.
    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Execute a shell command and return its stdout, stderr, and exit code. Non-zero exit codes are surfaced as recoverable results, not errors — the model should read the output and iterate.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "Shell command to run (passed to `sh -c`)."
                        },
                        "timeout_secs": {
                            "type": "integer",
                            "description": "Optional timeout in seconds. Defaults to 120. The child process (and its group) is killed on expiry.",
                            "minimum": 1
                        }
                    },
                    "required": ["command"],
                    "additionalProperties": false
                }
            }
        })
    }

    /// Mark as RESTRICTED so ReadOnly/Analytics callers cannot invoke it.
    ///
    /// Why: Shell execution is a privileged capability. Untrusted or
    /// analytics-only callers must not be able to run arbitrary commands.
    /// What: Returns `[ReadOnly, Analytics]` — callers in either tier are
    /// blocked at the registry's `dispatch_for_user` boundary.
    /// Test: Covered by `crate::tools::registry` tier-gating tests; also
    /// checked via `bash::tests::restricted_tiers_includes_readonly_and_analytics`.
    fn restricted_tiers(&self) -> &[ServiceTier] {
        &[ServiceTier::ReadOnly, ServiceTier::Analytics]
    }

    async fn execute(&self, args: Value) -> ToolResult {
        // ── Parse arguments ────────────────────────────────────────────────
        let Some(command) = args.get("command").and_then(Value::as_str) else {
            return ToolResult::err("bash: missing required 'command' parameter");
        };
        if command.trim().is_empty() {
            return ToolResult::err("bash: 'command' must not be empty");
        }

        let timeout_secs: u64 = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(self.default_timeout.as_secs());
        let call_timeout = Duration::from_secs(timeout_secs);

        // ── Build the subprocess ───────────────────────────────────────────
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(command);
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.stdin(std::process::Stdio::null());

        if let Some(ref dir) = self.working_dir {
            cmd.current_dir(dir);
        }

        // On Unix, spawn the child into its own process group so that a
        // timeout kill reaches the entire process tree, not just the shell.
        // SAFETY: `setsid()` is documented async-signal-safe. The closure
        // runs in the forked child after `fork()` but before `exec()`.
        #[cfg(unix)]
        unsafe {
            cmd.pre_exec(|| {
                // Create a new session; this process becomes the leader of
                // a new process group with pgid == pid.
                if libc::setsid() == -1 {
                    // Non-fatal: fall back to the parent's pgid.
                }
                Ok(())
            });
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::err(format!("bash: failed to spawn command: {e}"));
            }
        };

        // Grab the piped handles before consuming `child` in the wait call.
        let mut stdout_handle = child.stdout.take().expect("stdout is piped");
        let mut stderr_handle = child.stderr.take().expect("stderr is piped");

        // ── Read stdout + stderr + wait with timeout ───────────────────────
        let run_result = timeout(call_timeout, async {
            let mut stdout_bytes = Vec::new();
            let mut stderr_bytes = Vec::new();

            // Read both streams concurrently to avoid pipe deadlocks.
            let (stdout_res, stderr_res, wait_res) = tokio::join!(
                read_capped(&mut stdout_handle, MAX_OUTPUT_BYTES),
                read_capped(&mut stderr_handle, MAX_OUTPUT_BYTES),
                child.wait()
            );

            stdout_bytes.extend_from_slice(&stdout_res);
            stderr_bytes.extend_from_slice(&stderr_res);

            let status = wait_res?;
            Ok::<_, std::io::Error>((stdout_bytes, stderr_bytes, status))
        })
        .await;

        match run_result {
            // ── Timed out ─────────────────────────────────────────────────
            Err(_elapsed) => {
                // Kill the child's entire process group on Unix so background
                // processes spawned by the shell are also reaped.
                #[cfg(unix)]
                {
                    if let Some(pid) = child.id() {
                        // SAFETY: `kill` is a safe POSIX syscall. The pid value
                        // comes from the kernel; `i32::try_from` is fallible but
                        // realistically cannot overflow for valid process IDs.
                        if let Ok(signed_pid) = i32::try_from(pid) {
                            // Negative pgid means "kill the whole process group".
                            unsafe {
                                libc::kill(-signed_pid, libc::SIGKILL);
                            }
                        }
                    }
                }
                // Always attempt child.kill() as a fallback (handles non-Unix
                // or the case where setsid failed and we fall back to the child
                // itself).
                let _ = child.kill().await;
                let _ = child.wait().await;

                ToolResult::err(format!(
                    "bash: command timed out after {timeout_secs}s and was killed\ncommand: {command}"
                ))
            }

            // ── Completed (exit 0 or non-zero) ────────────────────────────
            Ok(Ok((stdout_bytes, stderr_bytes, status))) => {
                let stdout = lossy_truncated(&stdout_bytes, MAX_OUTPUT_BYTES);
                let stderr = lossy_truncated(&stderr_bytes, MAX_OUTPUT_BYTES);
                let exit_code = status.code().unwrap_or(-1);

                let mut out = String::new();
                if !stdout.is_empty() {
                    out.push_str("stdout:\n");
                    out.push_str(&stdout);
                    if !stdout.ends_with('\n') {
                        out.push('\n');
                    }
                }
                if !stderr.is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str("stderr:\n");
                    out.push_str(&stderr);
                    if !stderr.ends_with('\n') {
                        out.push('\n');
                    }
                }
                out.push_str(&format!("exit_code: {exit_code}"));

                // Non-zero exit is a *recoverable* result (failing pytest output
                // should be fed back to the model, not abort the loop).
                ToolResult::ok(out)
            }

            // ── IO error from wait() ───────────────────────────────────────
            Ok(Err(io_err)) => {
                ToolResult::err(format!("bash: IO error while waiting for child: {io_err}"))
            }
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Read up to `cap` bytes from an async reader without exceeding memory.
///
/// Why: Commands like `find /` can produce GB of output; we must bound
/// memory without blocking the executor.
/// What: Reads the full stream into a `Vec<u8>` capped at `cap` bytes;
/// silently discards the remainder (the caller appends a truncation notice).
/// Test: `bash::tests::stdout_truncation`.
pub(crate) async fn read_capped<R: AsyncReadExt + Unpin>(reader: &mut R, cap: usize) -> Vec<u8> {
    let mut buf = Vec::with_capacity(cap.min(4096));
    let mut tmp = [0u8; 4096];
    loop {
        match reader.read(&mut tmp).await {
            Ok(0) => break,
            Ok(n) => {
                let remaining = cap.saturating_sub(buf.len());
                if remaining == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n.min(remaining)]);
            }
            Err(_) => break,
        }
    }
    buf
}

/// Convert bytes to a lossy UTF-8 string, appending a truncation notice when
/// the captured byte count exactly equals `cap`.
///
/// Why: The LLM needs text; raw bytes are not useful. The truncation notice
/// tells the model that output was cut so it can adjust.
/// What: Calls `String::from_utf8_lossy`, then checks for truncation.
/// Test: `bash::tests::stdout_truncation`.
pub(crate) fn lossy_truncated(bytes: &[u8], cap: usize) -> String {
    let s = String::from_utf8_lossy(bytes).into_owned();
    if bytes.len() >= cap {
        format!("{s}\n[... output truncated at {cap} bytes ...]")
    } else {
        s
    }
}
