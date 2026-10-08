//! MCP JSON-RPC dispatch + stdio loop for `gchat-mcp`.
//!
//! Why: a session asks a person a question, sends a review notice, and reads
//! the answer through MCP tools that cannot send any other way than the
//! route gate (#9448 ruling 7). The server holds the project's single
//! [`GchatChannel`] and runs the only poller for its subscription.
//! What: [`AppState`] holds the shared channel and the poller's status.
//! [`handle_message`] answers `initialize`, `ping`, `tools/list` and
//! `tools/call`. A tool failure is an MCP tool error (`isError: true`) whose
//! text is a JSON object with a stable `error` kind: `refused` (with the
//! refusal `reason`), `sent_not_recorded` (with the question id; never
//! retried), `send_failed`, `not_found` or `invalid_arguments`. No error
//! carries the message text. [`serve`] spawns the poller and runs the stdio
//! loop; stdout carries JSON-RPC only and logs go to stderr.
//! Test: `src/gchat/tests/server.rs`, `tests/gchat_mcp_bin.rs`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::gchat::channel::GchatChannel;
use crate::gchat::doctor::report_for_channel;
use crate::gchat::error::SendError;
use crate::gchat::poller::{interval_ticks, read_status, Poller, SharedPollStatus, PULL_BATCH};
use crate::gchat::tools::{self, UNTRUSTED_ANSWER};
use trusty_mcp::{error_codes, initialize_response, run_stdio_loop, Request, Response};

/// State shared by every request.
#[derive(Debug, Clone)]
pub struct AppState {
    /// The project's one channel.
    pub channel: Arc<GchatChannel>,
    /// The poller's status, read by `gchat_doctor`.
    pub poll_status: SharedPollStatus,
}

/// A failed tool call, rendered as an MCP tool error.
#[derive(Debug)]
struct ToolError(Value);

impl ToolError {
    fn new(kind: &str, message: impl Into<String>) -> Self {
        Self(json!({ "error": kind, "message": message.into() }))
    }
}

impl From<SendError> for ToolError {
    // #9448: a refusal names its rule; a sent-but-unrecorded question is its
    // own kind so the caller never resends it.
    fn from(e: SendError) -> Self {
        let message = e.to_string();
        if let Some(reason) = e.refusal_code() {
            let mut v = Self::new("refused", message);
            v.0["reason"] = json!(reason);
            return v;
        }
        match e {
            SendError::SentNotRecorded {
                id, message_name, ..
            } => Self(json!({
                "error": "sent_not_recorded",
                "message": format!(
                    "the message was sent but not recorded: question id {id}. It will not be \
                     retried; do not resend it. {message}"
                ),
                "question_id": id,
                "message_name": message_name,
            })),
            _ => Self::new("send_failed", message),
        }
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key).and_then(Value::as_str).ok_or_else(|| {
        ToolError::new(
            "invalid_arguments",
            format!("`{key}` is required and must be a string"),
        )
    })
}

/// Dispatch one tool call.
///
/// Why: one routing table; every send goes through the channel's gate.
/// What: `Ok(Some(result))` for a known tool's success, `Err` for its tool
/// error, `Ok(None)` for an unknown tool name.
/// Test: `ask_to_an_unrouted_recipient_is_a_tool_error_with_no_request`,
/// `sent_not_recorded_is_its_own_tool_error`.
async fn call_tool(state: &AppState, name: &str, args: &Value) -> Result<Option<Value>, ToolError> {
    let channel = &state.channel;
    let result = match name {
        tools::ASK => {
            let sent = channel
                .send_question(str_arg(args, "to")?, str_arg(args, "text")?)
                .await?;
            json!({
                "question_id": sent.id,
                "token": sent.token,
                "route": sent.route,
                "message_name": sent.message_name,
            })
        }
        tools::NOTIFY_REVIEW => {
            let sent = channel
                .send_review_notice(
                    str_arg(args, "to")?,
                    str_arg(args, "text")?,
                    str_arg(args, "url")?,
                )
                .await?;
            json!({ "route": sent.route, "message_name": sent.message_name })
        }
        tools::ANSWER => answer(channel, args)?,
        tools::DOCTOR => {
            let offline = args
                .get("offline")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let poller = read_status(&state.poll_status).clone();
            let report = report_for_channel(channel, offline, Some(poller)).await;
            json!({ "ok": report.ok(), "text": report.render(), "report": report })
        }
        _ => return Ok(None),
    };
    Ok(Some(result))
}

/// `gchat_answer`: one question's status and, once answered, only its
/// answer text and time.
fn answer(channel: &GchatChannel, args: &Value) -> Result<Value, ToolError> {
    let id = args
        .get("question_id")
        .and_then(Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            ToolError::new(
                "invalid_arguments",
                "`question_id` must be a positive integer",
            )
        })?;
    let q = channel
        .question(id)
        .ok_or_else(|| ToolError::new("not_found", format!("no question with id {id}")))?;
    Ok(match q.answer {
        None => json!({ "question_id": id, "status": "open" }),
        Some(a) => json!({
            "question_id": id,
            "status": "answered",
            "answer": { "text": a.text, "answered_at": a.answered_at },
            "answer_trust": UNTRUSTED_ANSWER,
        }),
    })
}

fn content(value: &Value, is_error: bool) -> Value {
    let text = serde_json::to_string(value).unwrap_or_default();
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// Translate one JSON-RPC request into a result payload.
///
/// Why: the same shape as the sibling Slack and Telegram servers.
/// What: `Value::Null` for a notification; `{"error": {code, message}}` for
/// an unknown method or tool; otherwise the result, with tool failures as
/// `isError: true` content.
/// Test: `tools_list_is_exactly_the_four_tools_with_schemas`,
/// `notify_review_refuses_http_and_a_valid_one_opens_no_question`.
pub async fn handle_message(state: AppState, req: Value) -> Value {
    let method = req["method"].as_str().unwrap_or("");
    match method {
        "initialize" => initialize_response("gchat-mcp", env!("CARGO_PKG_VERSION"), None),
        "notifications/initialized" | "notifications/cancelled" => Value::Null,
        "ping" => json!({}),
        "tools/list" => tools::tool_list_response(),
        "tools/call" => {
            let name = req["params"]["name"].as_str().unwrap_or("");
            let args = match req["params"].get("arguments") {
                Some(v) if !v.is_null() => v.clone(),
                _ => json!({}),
            };
            match call_tool(&state, name, &args).await {
                Ok(Some(result)) => content(&result, false),
                Ok(None) => json!({ "error": {
                    "code": error_codes::METHOD_NOT_FOUND,
                    "message": format!("unknown tool: {name}"),
                }}),
                Err(ToolError(e)) => content(&e, true),
            }
        }
        _ => json!({ "error": {
            "code": error_codes::METHOD_NOT_FOUND,
            "message": format!("Method not found: {method}"),
        }}),
    }
}

/// Serve MCP on stdio with the poller running beside it.
///
/// Why: the `gchat-mcp` server entry point; one process holds the channel,
/// answers tools and polls (one consumer per subscription).
/// What: spawns [`Poller::run`] on [`interval_ticks`]`(poll_interval)` and
/// runs the stdio loop until stdin closes. Must run inside a Tokio runtime.
/// Test: `stdout_carries_only_json_rpc_and_logs_go_to_stderr`.
pub async fn serve(channel: GchatChannel, poll_interval: Duration) -> anyhow::Result<()> {
    let channel = Arc::new(channel);
    let poller = Poller::new(Arc::clone(&channel), PULL_BATCH);
    let state = AppState {
        channel,
        poll_status: poller.status(),
    };
    let ticks = interval_ticks(poll_interval);
    let poll_task = tokio::spawn(async move { poller.run(ticks).await });
    let result = run_stdio(state).await;
    poll_task.abort();
    result
}

/// Run the stdio JSON-RPC loop over [`handle_message`].
pub async fn run_stdio(state: AppState) -> anyhow::Result<()> {
    run_stdio_loop(move |req: Request| {
        let state = state.clone();
        async move {
            let id = req.id.clone();
            let raw = serde_json::to_value(&req).unwrap_or(Value::Null);
            let resp = handle_message(state, raw).await;
            if resp.is_null() {
                return Response::suppressed();
            }
            if let Some(err) = resp.get("error") {
                let code = err
                    .get("code")
                    .and_then(Value::as_i64)
                    .and_then(|c| i32::try_from(c).ok())
                    .unwrap_or(error_codes::INTERNAL_ERROR);
                let message = err["message"].as_str().unwrap_or("Internal error");
                return Response::err(id, code, message);
            }
            Response::ok(id, resp)
        }
    })
    .await
}
