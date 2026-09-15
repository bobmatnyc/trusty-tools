//! Hook stdin payload read that reports why it failed (#7975).
//!
//! Why: `tm hook --pm-guard` decides whether a tool call runs. It read its
//! payload through a helper that turned every failure into `None`, and it
//! treated `None` as "nothing to guard" and allowed the call. Claude Code
//! registers the guard on `PreToolUse` only, and its hooks reference states
//! that a command hook's input arrives on stdin as JSON carrying the common
//! fields plus `tool_name`, `tool_input` and `tool_use_id` for that event
//! (<https://code.claude.com/docs/en/hooks>, "Common input fields" and
//! "PreToolUse input", read 2026-09-14). An absent payload is therefore never a
//! legitimate case for the guard. A guard must tell a failed read apart from a
//! payload that names nothing it guards, and an `Option` cannot carry that.
//!
//! What: [`read_hook_stdin`] reads a bounded reader and returns the parsed JSON
//! object or a [`HookStdinError`] naming the failure.
//! [`read_stdin_hook_payload_strict`] applies it to the process stdin.
//! [`unreadable_payload_deny_reason`] is the deny text the guard prints.
//! Callers whose result gates nothing keep the `Option` view in
//! `misc::read_stdin_hook_payload`.
//!
//! Test: `hook_stdin_tests.rs`; end to end through the binary in
//! `tests/tm_hook_pm_guard_stdin_7975.rs`.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

/// How long a hook waits for Claude Code to finish writing and close stdin.
///
/// Why: a hook must never block the user's prompt if a caller leaves stdin
/// open. 500 ms was widened from 200 ms after a PR #1968 review found 200 ms
/// tight on a loaded host; it stays well inside the guard's 10-second hook
/// timeout.
pub(crate) const HOOK_STDIN_TIMEOUT: Duration = Duration::from_millis(500);

/// Why a hook's stdin payload could not be used.
///
/// Why (#7975): each variant is a different fault to debug, and the guard's
/// deny reason names which one fired.
/// What: `Empty` is no bytes or only whitespace; `Read` is an I/O error,
/// including non-UTF-8 input; `TimedOut` is stdin still open at the deadline;
/// `Parse` is text that is not JSON, including a truncated object; `NotAnObject`
/// is valid JSON whose top level is not an object.
#[derive(Debug)]
pub(crate) enum HookStdinError {
    /// Stdin closed with no bytes, or only whitespace.
    Empty,
    /// Reading stdin returned an I/O error.
    Read(std::io::Error),
    /// Stdin was still open when the read deadline passed.
    TimedOut(Duration),
    /// The bytes were not valid JSON.
    Parse(serde_json::Error),
    /// The JSON parsed, but its top level is not an object.
    NotAnObject,
}

impl std::fmt::Display for HookStdinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("stdin was empty"),
            Self::Read(e) => write!(f, "reading stdin failed: {e}"),
            Self::TimedOut(limit) => write!(f, "stdin was not closed within {limit:?}"),
            Self::Parse(e) => write!(f, "stdin was not valid JSON: {e}"),
            Self::NotAnObject => f.write_str("stdin JSON was not an object"),
        }
    }
}

/// Read and parse one hook payload from `reader`, reporting any failure.
///
/// Why (#7975): the reader is a parameter so every failure arm, including an
/// I/O error and a timeout, is testable without a child process.
/// What: reads `reader` to EOF under `timeout`, then parses the text. Returns
/// `Ok` only for a JSON object; surrounding whitespace is accepted. Every other
/// outcome returns the matching [`HookStdinError`].
/// Test: `read_hook_stdin_reports_empty_input`,
/// `read_hook_stdin_reports_a_read_error`, `read_hook_stdin_reports_a_timeout`,
/// `read_hook_stdin_reports_truncated_and_non_object_json`,
/// `read_hook_stdin_returns_a_parsed_object`.
pub(crate) async fn read_hook_stdin<R: AsyncRead + Unpin>(
    mut reader: R,
    timeout: Duration,
) -> Result<serde_json::Value, HookStdinError> {
    let mut buf = String::new();
    match tokio::time::timeout(timeout, reader.read_to_string(&mut buf)).await {
        Err(_) => return Err(HookStdinError::TimedOut(timeout)),
        Ok(Err(e)) => return Err(HookStdinError::Read(e)),
        Ok(Ok(_)) => {}
    }
    if buf.trim().is_empty() {
        return Err(HookStdinError::Empty);
    }
    let value: serde_json::Value = serde_json::from_str(&buf).map_err(HookStdinError::Parse)?;
    if !value.is_object() {
        return Err(HookStdinError::NotAnObject);
    }
    Ok(value)
}

/// [`read_hook_stdin`] over the process stdin with [`HOOK_STDIN_TIMEOUT`].
///
/// Why (#7975): the entry point for a hook whose decision gates a tool call.
/// What: see [`read_hook_stdin`].
/// Test: `tests/tm_hook_pm_guard_stdin_7975.rs`.
pub(crate) async fn read_stdin_hook_payload_strict() -> Result<serde_json::Value, HookStdinError> {
    read_hook_stdin(tokio::io::stdin(), HOOK_STDIN_TIMEOUT).await
}

/// The `permissionDecisionReason` for a tool call whose payload failed to read.
///
/// Why (#7975): the agent sees only this text, so it names the failure and what
/// to try next.
/// What: one sentence naming `err`, the deny, and the issue.
/// Test: `unreadable_payload_deny_reason_names_the_failure`.
pub(crate) fn unreadable_payload_deny_reason(err: &HookStdinError) -> String {
    format!(
        "tm hook --pm-guard could not use this tool call's stdin payload ({err}), so it \
         cannot tell whether the call is permitted and denies it (#7975). Retry the call; \
         a repeat means the hook is not receiving Claude Code's PreToolUse JSON."
    )
}

/// Flush stdout and end the process with exit 0, after a decision is printed.
///
/// Why (#7975): tokio reads stdin on a blocking thread it cannot cancel, so
/// after a timed-out read the runtime's shutdown waits for stdin to close.
/// Claude Code cancels a hook at its `timeout` and discards its output, and a
/// cancelled `PreToolUse` command hook does not block the call (hooks
/// reference, "Timeouts"). Returning normally would turn a printed deny into an
/// allow.
/// What: flushes stdout, ignoring a flush error because nothing is left to
/// report it to, then calls `std::process::exit(0)`.
/// Test: `pm_guard_denies_a_stdin_that_stays_open_past_the_read_timeout`.
fn exit_after_decision() -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    std::process::exit(0)
}

/// The guard's `PreToolUse` payload, or a printed deny and process exit.
///
/// Why (#7975): `tm hook --pm-guard` must fail closed on an unusable payload,
/// and the deny must outlive a stdin that never closes — see
/// [`exit_after_decision`]. Kept here rather than in `pm_guard.rs`, which sits
/// at the 500-SLOC cap.
/// What: returns the parsed object from [`read_stdin_hook_payload_strict`]. On
/// any [`HookStdinError`] it sends the guard's best-effort audit POST with no
/// session or tool name (neither could be read), prints the
/// `permissionDecision: "deny"` object carrying
/// [`unreadable_payload_deny_reason`], and exits 0 without returning.
/// Test: `pm_guard_denies_an_empty_stdin_payload`,
/// `pm_guard_denies_a_stdin_read_error`,
/// `pm_guard_denies_truncated_or_non_object_json`,
/// `pm_guard_denies_a_stdin_that_stays_open_past_the_read_timeout`,
/// `pm_guard_allows_a_parsed_payload_naming_no_guarded_operation`.
pub(crate) async fn read_stdin_payload_or_deny(url: &str) -> serde_json::Value {
    match read_stdin_hook_payload_strict().await {
        Ok(payload) => payload,
        Err(err) => {
            let reason = unreadable_payload_deny_reason(&err);
            crate::commands::pm_guard::audit_denied_tool(url, "", "", &reason).await;
            println!(
                "{}",
                crate::commands::pm_guard_response::build_pretooluse_deny_response(&reason)
            );
            exit_after_decision()
        }
    }
}

#[cfg(test)]
#[path = "hook_stdin_tests.rs"]
mod tests;
