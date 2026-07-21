//! `slack_read_file` — file **content** download, not just `files.info`
//! metadata (issue #3615).
//!
//! Why: the original nine tools never exposed file content at all — an agent
//! could see a permalink in a message but not read what the file actually
//! contains. This closes that gap for text-ish files (code, markdown, CSV,
//! logs); binary files (images, PDFs, archives, …) are reported as such
//! rather than embedded, since there is no useful way to hand a model raw
//! binary bytes inside a JSON/text MCP response — the caller gets the
//! permalink to fetch it directly instead.
//! Required OAuth scope: `files:read` (bot token).
//! What: [`read_file`] calls `files.info` for metadata (including
//! `url_private_download`), then downloads via
//! [`BaseClient::download_private_file`] (shared with `canvas::read_canvas`,
//! issue #3612 — both go through the same private-file-download path).
//! Test: `tests/tools_http.rs::read_file_returns_text_content`,
//! `::read_file_reports_binary_without_embedding_bytes`,
//! `::read_file_missing_arg_errors_before_network`.

use serde_json::{json, Value};

use super::args::require_str;
use super::clean::field_str;
use super::FILES_INFO;
use crate::slack::api::client::BaseClient;
use crate::slack::server::ToolCallError;
use trusty_common::slack_format::mrkdwn_escape;

/// Read a Slack file's metadata and (for text content) its body via
/// `files.info` + a private-file download.
///
/// Why: the primary file-content tool. Metadata alone (`files.info`) tells a
/// caller a file exists but not what's in it.
/// What: requires `file` (a Slack file id, e.g. `F0123ABCD`); returns
/// `{file: {id, name, mimetype, size, permalink}, content, binary}`.
/// `content` is the UTF-8-decoded, markup-escaped body when decoding
/// succeeds; when the bytes are not valid UTF-8 (a binary file), `binary` is
/// `true`, `content` is `null`, and the caller is left with `permalink` to
/// fetch the file directly — this is a normal, expected outcome for
/// non-text files, not an error.
/// Test: `tests/tools_http.rs::read_file_returns_text_content`,
/// `::read_file_reports_binary_without_embedding_bytes`.
pub(super) async fn read_file(client: &BaseClient, args: Value) -> Result<Value, ToolCallError> {
    let file_id = require_str(&args, "file")?;
    let body = json!({ "file": file_id.as_str() });
    let resp = client.call_method(FILES_INFO, &body).await?;
    let file = resp.get("file").cloned().unwrap_or(Value::Null);

    let file_summary = json!({
        "id": field_str(&file, "id"),
        "name": mrkdwn_escape(&field_str(&file, "name")),
        "mimetype": field_str(&file, "mimetype"),
        "size": file.get("size").and_then(Value::as_i64).unwrap_or(0),
        "permalink": field_str(&file, "permalink"),
    });

    let download_url = file
        .get("url_private_download")
        .and_then(Value::as_str)
        .map(str::to_string);

    let Some(url) = download_url else {
        return Ok(json!({ "file": file_summary, "content": Value::Null, "binary": false }));
    };

    let bytes = client.download_private_file(&url).await?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok(json!({
            "file": file_summary,
            "content": mrkdwn_escape(&text),
            "binary": false,
        })),
        Err(_) => Ok(json!({
            "file": file_summary,
            "content": Value::Null,
            "binary": true,
        })),
    }
}
