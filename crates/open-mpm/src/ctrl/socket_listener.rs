//! Per-project Unix-socket listener that lets the second `open-mpm` invocation
//! forward its argv into the running controller.
//!
//! Why: Keeps a single REPL process owning the long-running services (memory
//! seed, docs index, message bus) while subsequent CLI invocations just stream
//! their request through this socket and exit.
//! What: `spawn_socket_listener`, `handle_socket_connection`, the small
//! `write_socket_line` helper, and the client-side `forward_to_controller`.
//! Test: Manual — `open-mpm` in one terminal, `open-mpm "task"` in another.

use std::path::{Path, PathBuf};

use anyhow::Result;
use tokio::io::AsyncWriteExt;

use crate::events::{self, Event};

use super::config::SessionOverrides;
use super::pm_task::run_pm_task_with_history;
use super::state::ConversationTurn;

/// Spawn the per-project Unix-socket accept loop alongside the stdin REPL.
///
/// Why: After binding the controller socket, every subsequent `open-mpm`
/// invocation in the same project routes its argv into the running
/// controller via this listener. The listener stays "thin": it accepts
/// connections, parses one NDJSON command, dispatches a single PM round-trip
/// scoped to the request's `cwd`, and streams replies back. It does NOT
/// share state with the stdin REPL — it just reuses `run_pm_task` so both
/// paths exercise the same PM logic.
/// What: Reads exactly one JSON line per connection. Recognized command
/// types: `task` (run a PM task and stream output), `status` (return a
/// minimal liveness payload), `shutdown` (acknowledged but not yet wired
/// to actually stop the controller — Phase A leaves graceful shutdown for
/// later). Each connection gets its own tokio task so a slow PM call does
/// not block the listener.
/// Test: Manual — `open-mpm` from terminal A, then `open-mpm "hello"` from
/// terminal B prints the PM's output in B and exits while A keeps running.
pub async fn spawn_socket_listener(listener: tokio::net::UnixListener) {
    tracing::info!("ctrl: socket listener accepting connections");
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                tokio::spawn(handle_socket_connection(stream));
            }
            Err(e) => {
                tracing::warn!(error = %e, "ctrl: socket accept failed (continuing)");
                // Brief backoff to avoid a hot error loop on EMFILE / ENFILE.
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// Handle one inbound CLI connection: parse one command, stream replies.
///
/// Why: Pulled into its own function so the accept loop stays tiny and so
/// connection-scoped errors surface as warnings instead of poisoning the
/// listener.
/// What: Reads one line, dispatches by `type`, writes NDJSON replies, and
/// always finishes with a `done` or `error` envelope so the client knows
/// when to disconnect.
async fn handle_socket_connection(stream: tokio::net::UnixStream) {
    use tokio::io::AsyncBufReadExt;

    let (read_half, write_half) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(read_half);
    let writer = std::sync::Arc::new(tokio::sync::Mutex::new(write_half));

    let mut line = String::new();
    if let Err(e) = reader.read_line(&mut line).await {
        tracing::warn!(error = %e, "ctrl socket: failed to read command line");
        return;
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return;
    }

    let parsed: serde_json::Value = match serde_json::from_str(trimmed) {
        Ok(v) => v,
        Err(e) => {
            let _ = write_socket_line(
                &writer,
                &serde_json::json!({"type": "error", "error": format!("invalid JSON: {e}")}),
            )
            .await;
            return;
        }
    };

    let kind = parsed
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let id = parsed
        .get("id")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    match kind.as_str() {
        "status" => {
            let payload = serde_json::json!({
                "type": "output",
                "id": id,
                "text": format!("controller alive (pid={})", std::process::id()),
            });
            let _ = write_socket_line(&writer, &payload).await;
            let _ = write_socket_line(
                &writer,
                &serde_json::json!({"type": "done", "id": id, "status": "success"}),
            )
            .await;
        }
        "shutdown" => {
            let _ = write_socket_line(
                &writer,
                &serde_json::json!({
                    "type": "output",
                    "id": id,
                    "text": "shutdown requested (not yet implemented)",
                }),
            )
            .await;
            let _ = write_socket_line(
                &writer,
                &serde_json::json!({"type": "done", "id": id, "status": "success"}),
            )
            .await;
        }
        "task" => {
            let text = parsed
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let cwd = parsed
                .get("cwd")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

            let _ = write_socket_line(
                &writer,
                &serde_json::json!({
                    "type": "output",
                    "id": id,
                    "text": format!("Dispatching task in {}...", cwd.display()),
                }),
            )
            .await;

            // #192 Phase B: emit `SessionStarted` + `SessionDone` so SSE
            // subscribers see the controller-routed task in real time. The
            // socket request `id` doubles as the session_id so the UI can
            // correlate filtered streams (`?session_id=<id>`) to this task.
            let project_label = cwd
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("(cwd)")
                .to_string();
            events::publish(Event::SessionStarted {
                session_id: id.clone(),
                project: project_label,
            });

            // Parse optional history array from the task message.
            // Each element: {"user": "...", "assistant": "..."}
            // Missing or malformed history → treat as empty (backward-compatible
            // with older clients that don't send the history field).
            let history: Vec<ConversationTurn> = parsed
                .get("history")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|item| {
                            let user = item.get("user").and_then(|v| v.as_str())?.to_string();
                            let assistant =
                                item.get("assistant").and_then(|v| v.as_str())?.to_string();
                            Some(ConversationTurn { user, assistant })
                        })
                        .collect()
                })
                .unwrap_or_default();

            let result = run_pm_task_with_history(
                &cwd,
                &text,
                &history,
                Some(id.clone()),
                SessionOverrides::default(),
            )
            .await;
            match result {
                Ok(out) => {
                    let _ = write_socket_line(
                        &writer,
                        &serde_json::json!({
                            "type": "output",
                            "id": id,
                            "text": out,
                        }),
                    )
                    .await;
                    events::publish(Event::SessionDone {
                        session_id: id.clone(),
                        status: "success".to_string(),
                    });
                    let _ = write_socket_line(
                        &writer,
                        &serde_json::json!({
                            "type": "done",
                            "id": id,
                            "status": "success",
                        }),
                    )
                    .await;
                }
                Err(e) => {
                    events::publish(Event::SessionDone {
                        session_id: id.clone(),
                        status: "error".to_string(),
                    });
                    let _ = write_socket_line(
                        &writer,
                        &serde_json::json!({
                            "type": "error",
                            "id": id,
                            "error": format!("{e:#}"),
                        }),
                    )
                    .await;
                }
            }
        }
        other => {
            let _ = write_socket_line(
                &writer,
                &serde_json::json!({
                    "type": "error",
                    "id": id,
                    "error": format!("unknown command type: {other}"),
                }),
            )
            .await;
        }
    }
}

/// Write one JSON value as an NDJSON line to a shared writer.
async fn write_socket_line(
    writer: &std::sync::Arc<tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>>,
    value: &serde_json::Value,
) -> std::io::Result<()> {
    let mut line = serde_json::to_string(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    line.push('\n');
    let mut g = writer.lock().await;
    g.write_all(line.as_bytes()).await?;
    g.flush().await
}

/// CLI-side: forward this invocation to a running controller and stream
/// its replies to stdout/stderr until `done` or `error` arrives.
///
/// Why: Lets `main()` short-circuit when a controller is already running —
/// the second `open-mpm` invocation just becomes a thin client. Streaming
/// (rather than buffering) means the user sees PM progress immediately
/// once the controller emits it.
/// What: Writes one `task` command line, then reads NDJSON replies in a
/// loop. `output` envelopes print to stdout; `done` returns Ok; `error`
/// returns Err with the controller's message.
/// Test: Manual — start a controller in terminal A, run
/// `open-mpm "say hi"` in terminal B, observe streamed output.
///
/// The `project_dir` argument is the project root that the controller should
/// resolve agent configs against (it lands in the `cwd` field of the task
/// envelope). Callers MUST pass their resolved project root rather than
/// relying on `std::env::current_dir()` here — the REPL maintains its own
/// `project_dir` that may differ from the OS cwd (e.g., after `/connect` or
/// `/cd`), and using process cwd would route agent-config lookups to the
/// wrong directory (issue #238).
pub async fn forward_to_controller(
    stream: tokio::net::UnixStream,
    task_text: String,
    history: &[ConversationTurn],
    project_dir: &Path,
) -> Result<String> {
    use tokio::io::AsyncBufReadExt;

    let (read_half, mut write_half) = stream.into_split();
    let id = uuid::Uuid::new_v4().to_string();
    let cwd = project_dir.display().to_string();
    // Why: Serialize the caller's conversation history into the task envelope so
    // the server-side `handle_socket_connection` can reconstruct turns and call
    // `run_pm_task_with_history`. Empty slice → empty array (backward-compatible).
    let history_json: Vec<serde_json::Value> = history
        .iter()
        .map(|t| serde_json::json!({"user": t.user, "assistant": t.assistant}))
        .collect();
    let cmd = serde_json::json!({
        "type": "task",
        "id": id,
        "text": task_text,
        "cwd": cwd,
        "history": history_json,
    });
    let mut line = serde_json::to_string(&cmd)?;
    line.push('\n');
    write_half.write_all(line.as_bytes()).await?;
    write_half.flush().await?;

    let mut reader = tokio::io::BufReader::new(read_half);
    let mut buf = String::new();
    let mut accumulated = String::new();
    // Why: The first "output" envelope from the controller is always a
    // progress preamble (e.g. "Dispatching task in /path..."); it is not
    // part of the actual PM response. Skipping it keeps the accumulated
    // string clean so the REPL can render only the real response. We also
    // do NOT write to stdout inline anymore — inline writes conflict with
    // the REPL's crossterm-driven status bar and cause the response to be
    // clobbered. The caller prints the final accumulated string once.
    let mut is_first_output = true;
    loop {
        buf.clear();
        let n = reader.read_line(&mut buf).await?;
        if n == 0 {
            // Controller closed the connection without `done`.
            anyhow::bail!("controller closed connection unexpectedly");
        }
        let value: serde_json::Value = match serde_json::from_str(buf.trim()) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, line = %buf.trim(), "controller emitted invalid JSON");
                continue;
            }
        };
        let kind = value.get("type").and_then(|v| v.as_str()).unwrap_or("");
        match kind {
            "output" => {
                if let Some(text) = value.get("text").and_then(|v| v.as_str()) {
                    if is_first_output {
                        // Skip the server-side "Dispatching task in X..."
                        // preamble; only accumulate real PM output.
                        is_first_output = false;
                    } else {
                        accumulated.push_str(text);
                        if !text.ends_with('\n') {
                            accumulated.push('\n');
                        }
                    }
                }
            }
            "done" => return Ok(accumulated),
            "error" => {
                let msg = value
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no error message)")
                    .to_string();
                anyhow::bail!("controller error: {msg}");
            }
            _ => {
                // Unknown envelope type — log and keep streaming.
                tracing::debug!(kind = %kind, "unknown reply type from controller");
            }
        }
    }
}
