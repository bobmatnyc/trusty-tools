//! Opt-in raw capture of Bedrock reviewer replies to local files (#9310).
//!
//! Why: 37 of 80 Sonnet 5.5 reviews in the Q86 model eval did not parse, and
//! their raw replies were not kept, so the failing reply shape is unknown.
//! Part 2 of #9310 needs those replies to design the structured-output fix.
//! What: off by default. With [`CAPTURE_DIR_ENV`] set to a directory, every
//! reviewer call (schema `review_output`: the unified call and every
//! map-reduce chunk call, parsed or not) writes one JSON file there. The file
//! holds the raw reply, so it is diagnostic only. It never reaches a PR
//! comment, an MCP response or `ReviewResult::error`; only its file name is
//! logged. Capture is fail-open: a write failure is logged with `warn!` and
//! never fails or changes the review.
//! Test: `capture_dir_is_off_unless_the_env_var_names_a_dir`,
//! `capture_off_by_default_writes_nothing`,
//! `capture_on_writes_one_private_file_per_reviewer_call`,
//! `capture_skips_non_reviewer_calls`,
//! `capture_write_failure_leaves_the_response_identical`.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use aws_sdk_bedrockruntime::operation::converse::ConverseOutput;
use serde_json::{Value, json};
use tracing::{debug, warn};

use super::{arn, extract_converse_text, extract_token_usage, tool_use};
use crate::llm::LlmRequest;

/// The environment variable naming the capture directory.
pub(crate) const CAPTURE_DIR_ENV: &str = "TRUSTY_REVIEW_CAPTURE_DIR";

/// The reviewer's response-schema name; only its calls are captured.
const REVIEWER_SCHEMA: &str = crate::pipeline::prompt::REVIEW_SCHEMA_NAME;

/// Attempts at a fresh file name before a capture gives up.
const NAME_ATTEMPTS: u32 = 8;

/// Process-wide sequence that keeps file names unique within one process.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The capture directory, or `None` when capture is off.
///
/// What: the trimmed value of [`CAPTURE_DIR_ENV`] read through `env`; unset
/// or blank is off.
/// Test: `capture_dir_is_off_unless_the_env_var_names_a_dir`.
pub(crate) fn capture_dir(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    env(CAPTURE_DIR_ENV)
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Capture one reply to `dir` when `req` is a reviewer call; never fails.
///
/// Why: the capture is diagnostic, so it must not be able to fail or alter a
/// review (#9310).
/// What: builds the record ([`capture_record`]) and writes it; logs
/// `raw reply captured: <file name>` at `debug!`, or the error at `warn!`.
/// `reply` is the text handed to the review parser.
/// Test: `capture_on_writes_one_private_file_per_reviewer_call`,
/// `capture_write_failure_leaves_the_response_identical`.
pub(crate) fn capture_reply(
    dir: &Path,
    req: &LlmRequest,
    model: &str,
    resp: &ConverseOutput,
    reply: &str,
) {
    let reviewer = req
        .response_schema
        .as_ref()
        .is_some_and(|s| s.name == REVIEWER_SCHEMA);
    if !reviewer {
        return;
    }
    match write_capture(dir, |id| capture_record(id, model, resp, reply)) {
        Ok(name) => debug!("raw reply captured: {name}"),
        Err(e) => warn!("raw reply capture failed; the review is unaffected (#9310): {e}"),
    }
}

/// The JSON record for one reply, with file stem `id`.
///
/// What: `id`, `captured_utc`, `schema`, `model` (ARN account id masked),
/// `stop_reason`, `input_tokens`, `output_tokens`, `block_kinds`, `tool_use`,
/// `tool_use_input` (the first `toolUse` block's raw input, or null), `text`
/// (the joined text blocks, or null) and `reply` (what the parser read). No
/// credential is read from config or env.
fn capture_record(id: &str, model: &str, resp: &ConverseOutput, reply: &str) -> Value {
    let (input_tokens, output_tokens) = extract_token_usage(resp);
    let block_kinds = tool_use::reply_block_kinds(resp);
    let tool_use_input = tool_use::extract_tool_use_json(resp)
        .map(|raw| serde_json::from_str(&raw).unwrap_or(Value::String(raw)));
    json!({
        "id": id,
        "captured_utc": chrono::Utc::now().to_rfc3339(),
        "schema": REVIEWER_SCHEMA,
        "model": arn::mask_account_ids(model),
        "stop_reason": resp.stop_reason().as_str(),
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "tool_use": block_kinds.contains(&"tool_use"),
        "block_kinds": block_kinds,
        "tool_use_input": tool_use_input,
        "text": extract_converse_text(resp),
        "reply": reply,
    })
}

/// Write `record(id)` to a new file in `dir`; returns the file name.
///
/// What: creates `dir` (mode 0700 on Unix) when missing, then a file
/// `reply-<UTC time>-<pid>-<seq>.json` opened with `create_new` (mode 0600),
/// so an existing file is never overwritten; a taken name moves to the next
/// sequence number.
fn write_capture(dir: &Path, record: impl Fn(&str) -> Value) -> std::io::Result<String> {
    create_private_dir(dir)?;
    let mut last = std::io::Error::other("no capture name tried");
    for _ in 0..NAME_ATTEMPTS {
        let id = format!(
            "reply-{}-{}-{:06}",
            chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let name = format!("{id}.json");
        match create_private_file(&dir.join(&name)) {
            Ok(mut file) => {
                let body = serde_json::to_vec_pretty(&record(&id))?;
                file.write_all(&body)?;
                return Ok(name);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// Create `dir` and its parents, mode 0700 on Unix; an existing dir is kept.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Create `path`, failing if it exists, mode 0600 on Unix.
fn create_private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}
