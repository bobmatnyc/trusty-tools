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
//! [`read_stdin_hook_payload_strict`] applies it to the process stdin under a
//! caller-chosen budget — [`HOOK_STDIN_TIMEOUT`] for an advisory hook,
//! [`PM_GUARD_STDIN_TIMEOUT`] for the guard, whose read now gates the call.
//! [`unreadable_payload_deny_reason`] and [`unclassifiable_payload_deny_reason`]
//! are the deny texts the guard prints. Callers whose result gates nothing keep
//! the `Option` view in `misc::read_stdin_hook_payload`.
//!
//! Test: `hook_stdin_tests.rs`; end to end through the binary in
//! `tests/tm_hook_pm_guard_stdin_7975.rs`.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};

/// How long an ADVISORY hook waits for Claude Code to close stdin.
///
/// Why: a hook must never block the user's prompt if a caller leaves stdin
/// open. 500 ms was widened from 200 ms after a PR #1968 review found 200 ms
/// tight on a loaded host. A read that misses this budget costs an advisory
/// caller only a skipped rewrite or a missing observability field, so the tight
/// bound is the right trade there. #7975: the guard, whose read DENIES on
/// failure, uses [`PM_GUARD_STDIN_TIMEOUT`] instead.
/// Test: `hook_stdin_only_the_advisory_read_uses_the_short_budget`.
pub(crate) const HOOK_STDIN_TIMEOUT: Duration = Duration::from_millis(500);

/// How long `tm hook --pm-guard` waits for Claude Code to close stdin.
///
/// Why (#7975 round 2): the guard's read fails CLOSED, so a read that misses
/// its budget no longer costs a no-op — it DENIES a legitimate tool call, and
/// the guard is registered with `matcher: ""`, which fires for every tool
/// (`core::session_launch::settings::pm_guard_hook_value`). Reusing the 500 ms
/// advisory budget would put every tool call behind a bound a PR #1968 review
/// already called tight at 200 ms, with a megabyte-scale `tool_input` (a large
/// `Write` `content`) to cross the pipe inside it.
/// What: 5 s. A deny's worst case is three bounds, not two. `main.rs` resolves
/// the daemon URL through
/// [`trusty_mpm::core::discovery::GATEWAY_PROBE_TIMEOUT`] (500 ms) BEFORE
/// dispatching, and the registered guard command carries no `--url`
/// (`build_tree::PM_GUARD_SUFFIX`), so that probe is a fixed prefix on every
/// invocation. 500 ms + 5 s + [`crate::commands::pm_guard::AUDIT_POST_TIMEOUT`]
/// (2 s) = 7.5 s of the 10 s `REGISTERED_HOOK_TIMEOUT` Claude Code is told to
/// allow, leaving 2.5 s for exec and classification.
/// Test: `guard_read_budget_leaves_the_audit_post_inside_the_hook_timeout`,
/// which sums all three so lowering any one of them trips the gate;
/// `read_hook_stdin_reads_a_slow_megabyte_inside_the_guard_budget`.
pub(crate) const PM_GUARD_STDIN_TIMEOUT: Duration = Duration::from_secs(5);

/// The `timeout` Claude Code is told to allow the guard hook, in seconds.
///
/// Why (#7975 round 2): the read and audit budgets above are only safe relative
/// to this number, so it is named rather than left implicit. Mirrored, not
/// imported: `core::session_launch::settings` is a private module of the
/// library and unreachable from this binary.
/// Test-only: the production side is the literal in `pm_guard_hook_value`, and
/// duplicating it here would be the way the two drift.
/// Test: `guard_read_budget_leaves_the_audit_post_inside_the_hook_timeout`;
/// the registered value itself by `write_project_hooks_registers_pm_guard`.
#[cfg(test)]
pub(crate) const REGISTERED_HOOK_TIMEOUT: Duration = Duration::from_secs(10);

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

/// [`read_hook_stdin`] over the process stdin, under `timeout`.
///
/// Why (#7975): the entry point every hook reads its payload through. The
/// budget is the caller's because it follows what a missed read costs: the
/// guard denies the call ([`PM_GUARD_STDIN_TIMEOUT`]), an advisory hook only
/// skips work ([`HOOK_STDIN_TIMEOUT`]).
/// What: see [`read_hook_stdin`].
/// Test: `tests/tm_hook_pm_guard_stdin_7975.rs`.
pub(crate) async fn read_stdin_hook_payload_strict(
    timeout: Duration,
) -> Result<serde_json::Value, HookStdinError> {
    read_hook_stdin(tokio::io::stdin(), timeout).await
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

/// Why a parsed payload does not describe a tool call the guard can classify.
///
/// Why (#7975 round 2): the guard's rules all read `tool_name` and
/// `tool_input`; a payload carrying neither in the documented shape is a
/// delivery fault, and the deny reason says which field is wrong.
/// What: `None` when `payload` carries a string `tool_name` and an object
/// `tool_input`; otherwise the field-naming detail.
/// Test: `unclassifiable_payload_detail_names_the_bad_field`.
fn unclassifiable_payload_detail(payload: &serde_json::Value) -> Option<&'static str> {
    if !payload
        .get("tool_name")
        .is_some_and(serde_json::Value::is_string)
    {
        return Some("`tool_name` is missing or is not a string");
    }
    if !payload
        .get("tool_input")
        .is_some_and(serde_json::Value::is_object)
    {
        return Some("`tool_input` is missing or is not an object");
    }
    None
}

/// The `permissionDecisionReason` for a payload the guard cannot classify.
///
/// Why (#7975 round 2): the agent sees only this text, so it names the bad
/// field and why an unclassifiable payload cannot be allowed through.
/// What: one sentence naming `detail`, the deny, and the issue.
/// Test: `unclassifiable_payload_deny_reason_names_the_failure`.
pub(crate) fn unclassifiable_payload_deny_reason(detail: &str) -> String {
    format!(
        "tm hook --pm-guard cannot classify this tool call: {detail}. Claude Code sends every \
         PreToolUse hook a string `tool_name` and an object `tool_input`, so a payload missing \
         either is a delivery fault, not a call with nothing to guard — allowing it would skip \
         every rule the guard enforces, so it denies (#7975). Retry the call; a repeat means the \
         hook is not receiving Claude Code's PreToolUse JSON."
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
/// `pm_guard_reads_a_megabyte_tool_input_and_decides_on_its_content`,
/// `pm_guard_allows_a_parsed_payload_naming_no_guarded_operation`.
pub(crate) async fn read_stdin_payload_or_deny(url: &str) -> serde_json::Value {
    match read_stdin_hook_payload_strict(PM_GUARD_STDIN_TIMEOUT).await {
        Ok(payload) => payload,
        // Neither session nor tool name could be read, so the audit carries
        // neither.
        Err(err) => {
            audit_then_deny_and_exit(url, "", "", unreadable_payload_deny_reason(&err)).await
        }
    }
}

/// The guarded `tool_name`, or a printed deny and process exit.
///
/// Why (#7975 round 2, code-critic MEDIUM): the guard used to ALLOW, silently
/// and with no audit record, any payload whose `tool_name` was not a string —
/// and to classify a `Bash` payload with no `tool_input` against an empty
/// command, which every rule allows. Both are global bypasses of the ABSOLUTE
/// rules this module exists to enforce, reached through the exact delivery
/// fault the rest of #7975 fails closed on. Claude Code documents `tool_name`
/// and `tool_input` as always present on `PreToolUse`
/// (<https://code.claude.com/docs/en/hooks>, "PreToolUse input"), the same
/// premise the unreadable-payload deny rests on, so the same answer follows:
/// deny. `hook_event_name` is deliberately not consulted — the guard is
/// registered on `PreToolUse` only, and reading it could only add an
/// allow-path back.
/// What: returns the payload's `tool_name` when
/// [`unclassifiable_payload_detail`] passes it. Otherwise audits the deny with
/// every field still readable — the `session_id`, and the `tool_name` itself on
/// the `tool_input` arm, where only the input was malformed — prints the deny
/// carrying [`unclassifiable_payload_deny_reason`], and exits 0. The audit
/// record is the only alarm this path raises, so it names what it can (round 2,
/// code-critic MEDIUM); a non-string `tool_name` leaves that field empty
/// because there is nothing to name.
/// Test: `pm_guard_denies_a_payload_whose_tool_name_is_not_a_string`,
/// `pm_guard_denies_a_bash_payload_with_no_tool_input`,
/// `pm_guard_allows_a_parsed_payload_naming_no_guarded_operation`.
pub(crate) async fn guarded_tool_name_or_deny<'a>(
    url: &str,
    payload: &'a serde_json::Value,
) -> &'a str {
    let tool_name = payload
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let Some(detail) = unclassifiable_payload_detail(payload) else {
        return tool_name;
    };
    let session_id = payload
        .get("session_id")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    audit_then_deny_and_exit(
        url,
        session_id,
        tool_name,
        unclassifiable_payload_deny_reason(detail),
    )
    .await
}

/// Audit `reason`, print its deny, and end the process without returning.
///
/// Why (#7975): the two fail-closed paths above differ only in what they can
/// name, and both must outlive a stdin that never closes — see
/// [`exit_after_decision`].
/// What: best-effort audit POST, then the `permissionDecision: "deny"` object
/// on stdout, then exit 0.
/// Test: covered through its two callers' tests.
async fn audit_then_deny_and_exit(
    url: &str,
    session_id: &str,
    tool_name: &str,
    reason: String,
) -> ! {
    crate::commands::pm_guard::audit_denied_tool(url, session_id, tool_name, &reason).await;
    println!(
        "{}",
        crate::commands::pm_guard_response::build_pretooluse_deny_response(&reason)
    );
    exit_after_decision()
}

#[cfg(test)]
#[path = "hook_stdin_tests.rs"]
mod tests;
