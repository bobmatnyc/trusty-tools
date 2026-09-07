//! The headless Claude Code child that answers a diverted bulk read (#6887).
//!
//! Why: the worker has to be a model the developer is already paying for under
//! the login they already have. Owner ruling 2026-09-07 settles which one —
//! Claude Code's own Haiku, run headless as `claude -p`, under the parent
//! session's `CLAUDE_CONFIG_DIR`. That removes every provider, credential, and
//! registry the earlier draft carried: there is no API key here, no provider
//! selector, and nothing to configure beyond a model name.
//!
//! What: [`worker_argv`] and [`wrap_files`] build the child's argv and its
//! stdin payload — the file bytes travel on stdin, never in argv, so they stay
//! out of every process listing. [`spawn_worker`] runs it with the parent's
//! session bindings removed ([`NESTED_SESSION_ENV`]) and a timeout;
//! [`parse_worker_json`] turns `--output-format json` into a [`WorkerReply`].
//! [`worker_available`] is the cheap predicate the `PreToolUse` hook uses to
//! avoid blocking into a dead end.
//!
//! Nesting works: `claude -p` starts normally inside a running Claude Code
//! session even with `CLAUDECODE=1` and the messaging socket present. The scrub
//! is not a workaround for a refusal — it exists because those variables bind
//! the child to the PARENT's session (its id, its IPC socket and token, its
//! output cap and effort level), all of which are wrong for a detached worker.
//! Test: `divert_worker_tests.rs`.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

/// The external binary the worker runs.
///
/// Why: resolved through `trusty_common::bin_resolve::resolve_binary`, the
/// workspace's single answer to "find this executable", so a hook firing under
/// Claude Code's minimal `PATH` still locates it (#1298).
/// What: `"claude"`.
/// Test: `worker_available_reports_the_resolver`.
pub(crate) const WORKER_BINARY: &str = "claude";

/// Model the worker runs on when the manifest names none.
///
/// Why: Haiku 4.5 is the arm the #6882 POC measured (-45% cost, -54% output
/// tokens against the undiverted baseline).
/// What: `"claude-haiku-4-5"` — an alias `claude --model` accepts.
/// Test: `worker_argv_carries_the_model_and_no_file_content`.
pub(crate) const DEFAULT_WORKER_MODEL: &str = "claude-haiku-4-5";

/// Seconds the worker may run before it is killed.
///
/// Why: the agent is blocked on this command. A worker that hangs must become a
/// fall-through the agent can recover from, not an indefinite stall.
/// What: `60`. Overridable per invocation with `--timeout-secs`.
/// Test: `spawn_worker_at_times_out`.
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Environment variables that bind a process to the PARENT Claude Code session.
///
/// Why: the child is a detached worker, not a continuation of this session.
/// Inheriting `CLAUDE_CODE_MESSAGING_SOCKET`/`_TOKEN` would point it at the
/// parent's IPC channel; inheriting `CLAUDE_CODE_SESSION_ID` would make it
/// claim an id that is not its own; inheriting `CLAUDE_CODE_MAX_OUTPUT_TOKENS`
/// and `CLAUDE_EFFORT` would apply the expensive session's budget to the cheap
/// worker, which is the exact cost this feature exists to avoid.
///
/// `CLAUDE_CONFIG_DIR` is deliberately NOT in this list: it is the login, and
/// the whole point is that the child authenticates as the same developer.
/// What: the ten variables scrubbed from the child's environment.
/// Test: `scrubbed_env_drops_every_nested_session_binding`,
/// `scrubbed_env_keeps_the_config_dir`.
pub(crate) const NESTED_SESSION_ENV: [&str; 10] = [
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_MAX_OUTPUT_TOKENS",
    "CLAUDE_EFFORT",
    "CLAUDE_PID",
];

/// The fixed summarization instruction the child runs under.
///
/// Why: passed as `--system-prompt`, which REPLACES Claude Code's own harness
/// prompt. That is most of the saving — the default prompt plus tool schemas
/// measured ~48k cache-write tokens on this workstation; replacing it and
/// dropping tools and MCP brings the same call to ~9k.
/// What: a summarizer with no tools to reach for and no latitude to invent.
/// Test: `worker_argv_carries_the_model_and_no_file_content`.
const SYSTEM_PROMPT: &str = "You are a file-reading worker for a coding agent. \
     The user message contains whole files wrapped in <file path=\"...\">...</file> \
     tags, followed by a question. Answer the question about those files \
     precisely and briefly. Quote exact identifiers. Never invent content that \
     is not present in the supplied files. You have no tools; answer from the \
     supplied text alone.";

/// What the child reported about one worker call.
///
/// Why: the usage numbers are acceptance criterion 6 — the ledger line records
/// what the diversion actually cost, so the saving can be checked rather than
/// asserted.
/// What: the answer text plus the child's own `usage` and `total_cost_usd`.
/// Test: `parse_worker_json_extracts_text_and_usage`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WorkerReply {
    /// The answer, printed verbatim to `tm divert`'s stdout.
    pub(crate) text: String,
    /// Model the child actually billed against.
    pub(crate) model: String,
    /// Fresh (uncached) prompt tokens.
    pub(crate) input_tokens: u64,
    /// Completion tokens — what the session absorbs by reading this answer.
    pub(crate) output_tokens: u64,
    /// Prompt tokens served from cache.
    pub(crate) cache_read_tokens: u64,
    /// Prompt tokens written to cache.
    pub(crate) cache_creation_tokens: u64,
    /// The child's own cost estimate for the call.
    pub(crate) cost_usd: f64,
}

/// Whether a worker binary is resolvable right now.
///
/// Why (acceptance criterion 4): the `PreToolUse` hook must never block into a
/// dead end. With no `claude` on `PATH` the replacement command cannot run, so
/// the hook has to let the original read through instead.
/// What: `true` when `resolve_binary("claude")` finds an executable. No process
/// is spawned — a hook has a 10-second budget and this runs on every `Read`.
/// Test: `worker_available_reports_the_resolver`.
pub(crate) fn worker_available() -> bool {
    resolve_worker_binary().is_some()
}

/// Locate the worker binary.
///
/// Why: one resolution path, shared with the rest of the workspace.
/// What: `trusty_common::bin_resolve::resolve_binary(WORKER_BINARY)`.
/// Test: covered via `worker_available_reports_the_resolver`.
fn resolve_worker_binary() -> Option<PathBuf> {
    trusty_common::bin_resolve::resolve_binary(WORKER_BINARY)
}

/// Build the child's argument vector.
///
/// Why: pure, so a test can assert the exact flags AND assert that no file
/// content ever reaches argv — content travels on stdin, which keeps it out of
/// `ps` output and off any argv length limit.
/// What: headless print mode on `model`, JSON output so usage comes back
/// structured, a replaced system prompt, and every capability the worker does
/// not need switched off: `--restricted` drops the command-running tools and
/// WebFetch, `--strict-mcp-config` with no `--mcp-config` loads no MCP server,
/// `--disable-slash-commands` skips skills, and `--permission-prompts none`
/// denies anything that would prompt, since nobody is there to answer.
/// Test: `worker_argv_carries_the_model_and_no_file_content`.
pub(crate) fn worker_argv(model: &str) -> Vec<String> {
    [
        "-p",
        "--model",
        model,
        "--output-format",
        "json",
        "--system-prompt",
        SYSTEM_PROMPT,
        "--restricted",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--permission-prompts",
        "none",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

/// Wrap file contents and the question into the child's stdin payload.
///
/// Why: the child needs to know which bytes came from which file. This is
/// shunt's own wire shape (`<file path="…">…</file>`), kept identical so the
/// #6882 measurements carry over.
/// What: one `<file>` element per `(path, content)` pair, then a blank line and
/// `Question: <question>`.
/// Test: `wrap_files_uses_the_shunt_envelope`.
pub(crate) fn wrap_files(files: &[(String, String)], question: &str) -> String {
    let mut out = String::new();
    for (path, content) in files {
        out.push_str(&format!("<file path=\"{path}\">\n{content}\n</file>\n"));
    }
    out.push_str(&format!("\nQuestion: {question}\n"));
    out
}

/// Extract the answer and usage from the child's `--output-format json`.
///
/// Why: the child reports failure IN the JSON with `is_error: true` and a
/// human-readable `result` (a logged-out worker prints `Not logged in · Please
/// run /login` and still exits 0), so a zero exit code alone does not mean it
/// answered.
/// What: `Ok` only when `is_error` is absent or false and `result` is non-empty.
/// Usage fields default to zero individually so a schema that gains or loses an
/// optional counter does not turn a good answer into a failure.
/// Test: `parse_worker_json_extracts_text_and_usage`,
/// `parse_worker_json_rejects_an_error_result`.
pub(crate) fn parse_worker_json(stdout: &str) -> Result<WorkerReply, String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim())
        .map_err(|e| format!("worker output is not JSON: {e}"))?;

    let text = value
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();

    if value.get("is_error").and_then(|v| v.as_bool()) == Some(true) {
        return Err(format!("worker reported an error: {text}"));
    }
    if text.is_empty() {
        return Err("worker returned an empty answer".to_string());
    }

    let usage = value.get("usage");
    let n = |key: &str| {
        usage
            .and_then(|u| u.get(key))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0)
    };
    Ok(WorkerReply {
        text,
        model: value
            .get("modelUsage")
            .and_then(|m| m.as_object())
            .and_then(|m| m.keys().next().cloned())
            .unwrap_or_else(|| DEFAULT_WORKER_MODEL.to_string()),
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_read_tokens: n("cache_read_input_tokens"),
        cache_creation_tokens: n("cache_creation_input_tokens"),
        cost_usd: value
            .get("total_cost_usd")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0),
    })
}

/// Run the worker and return its reply.
///
/// Why: every failure mode the agent could hit — binary missing, non-zero exit,
/// timeout, unparseable output, a logged-out child — has to come back as one
/// `Err` string, because the caller's only move on any of them is the same:
/// fall through and let the agent read the file itself.
/// What: spawns [`WORKER_BINARY`] with [`worker_argv`], writes `payload` to its
/// stdin, and waits at most `timeout`. Kills the child on timeout so a hung
/// worker leaves nothing behind. The parent's session bindings are removed from
/// the child's environment ([`NESTED_SESSION_ENV`]) and `TRUSTY_MPM_DISABLE_HOOKS`
/// is set so this crate's own hooks — the diversion hook included — never fire
/// inside the worker.
/// Test: `spawn_worker_at_reports_an_unstartable_binary`, `spawn_worker_at_times_out`.
pub(crate) async fn spawn_worker(
    model: &str,
    payload: &str,
    timeout: Duration,
) -> Result<WorkerReply, String> {
    let program =
        resolve_worker_binary().ok_or_else(|| format!("`{WORKER_BINARY}` is not on PATH"))?;
    spawn_worker_at(&program, model, payload, timeout).await
}

/// Run a specific worker binary and return its reply.
///
/// Why: the program path is a PARAMETER rather than something this function
/// resolves, so a test drives the real spawn, timeout, and kill against a stand-in
/// script without touching `PATH` — this bin target's env-mutation ratchet
/// (#5544) forbids a process-global `set_var`, and a `PATH` rewrite in one test
/// corrupts every other test in the binary regardless.
/// What: see [`spawn_worker`], which resolves the path and delegates here.
/// Test: `spawn_worker_at_reports_an_unstartable_binary`, `spawn_worker_at_times_out`.
pub(crate) async fn spawn_worker_at(
    program: &Path,
    model: &str,
    payload: &str,
    timeout: Duration,
) -> Result<WorkerReply, String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(worker_argv(model))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // #6887: the child is a detached worker, not this session continued.
    for name in NESTED_SESSION_ENV {
        command.env_remove(name);
    }
    command.env(crate::commands::misc::DISABLE_HOOKS_ENV, "1");

    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start `{WORKER_BINARY}`: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(payload.as_bytes())
            .await
            .map_err(|e| format!("cannot write the payload to `{WORKER_BINARY}`: {e}"))?;
        stdin
            .shutdown()
            .await
            .map_err(|e| format!("cannot close the payload stream: {e}"))?;
    }

    let output = match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("`{WORKER_BINARY}` failed: {e}")),
        Err(_) => return Err(format!("`{WORKER_BINARY}` timed out after {timeout:?}")),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`{WORKER_BINARY}` exited {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    parse_worker_json(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(test)]
#[path = "divert_worker_tests.rs"]
mod tests;
