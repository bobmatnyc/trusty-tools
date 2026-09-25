//! Per-domain Google Workspace service modules.
//!
//! Why: Each module mirrors a Google product surface (Gmail, Drive, …) so
//! tool definitions and implementations stay co-located.
//! What: Every service function has signature
//! `async fn(&BaseClient, serde_json::Value) -> anyhow::Result<Value>` so
//! the MCP dispatcher in `crate::server` can route uniformly.
//! Test: Module-level smoke tests verify argument extraction; live API
//! tests are out-of-scope.

pub mod accounts;
pub mod calendar;
pub mod docs;
pub mod drive;
pub mod gmail;
pub mod sheets;
pub mod slides;
pub mod tasks;

use serde_json::Value;

/// Extract the optional `account` profile name from MCP arguments.
///
/// Why: Every tool accepts an optional `account` field; centralising the
/// extraction avoids subtle off-by-one bugs.
/// What: Returns `Some(name)` only when the field is a non-empty string.
/// Test: Implicitly covered by every service function.
pub(crate) fn account_of(args: &Value) -> Option<&str> {
    args.get("account")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// Extract a required string field, returning an error if missing/empty.
pub(crate) fn require_str<'a>(args: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("missing required field: {key}"))
}

/// Extract an optional string field.
pub(crate) fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

/// Build a request body from a nested object (`task`, `updates`) plus flat
/// top-level fields.
///
/// Why: Several tools advertise flat fields (`title`, `summary`, `name`, ...)
/// but read only the nested object, so a flat-shape caller was refused or
/// PATCHed `{}` as a silent no-op — Tasks (#8629) and Calendar/Gmail labels
/// (#8632). One helper keeps the precedence rule identical across tools.
/// What: `fields` pairs each flat argument name with the API body key it
/// maps to (`("time_zone", "timeZone")`). Starts from `args[object_key]` (an
/// error when present but not an object), then adds each flat field that is
/// present and non-null. The body is the UNION of both shapes: a field given
/// in both places with equal values is sent once; with different values it is
/// an error naming the field and the object, so no value is dropped
/// silently. An empty union is an error naming both shapes.
/// Test: `update_merges_flat_and_object`,
/// `update_conflict_is_refused_without_a_request`,
/// `update_with_no_fields_names_both_shapes` (in `calendar` and
/// `gmail::labels`); `create_with_both_shapes_merges_disjoint_fields`,
/// `create_with_conflicting_shapes_is_refused_without_a_request`,
/// `create_with_no_task_fields_names_both_shapes` (in `tasks`).
pub(crate) fn merge_flat_fields(
    args: &Value,
    object_key: &str,
    fields: &[(&str, &str)],
) -> anyhow::Result<Value> {
    let mut body = match args.get(object_key) {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => anyhow::bail!("'{object_key}' must be an object"),
    };
    for &(flat, key) in fields {
        let Some(value) = args.get(flat).filter(|v| !v.is_null()) else {
            continue;
        };
        if body.get(key).is_some_and(|nested| nested != value) {
            let alias = if flat == key {
                String::new()
            } else {
                format!(" (as '{key}')")
            };
            anyhow::bail!(
                "'{flat}' is set both at the top level and in '{object_key}'{alias} with \
                 different values; pass it once"
            );
        }
        body.insert(key.to_string(), value.clone());
    }
    if body.is_empty() {
        let names: Vec<&str> = fields.iter().map(|&(flat, _)| flat).collect();
        anyhow::bail!(
            "no fields provided: pass {} at the top level, or a non-empty '{object_key}' object",
            names.join("/")
        );
    }
    Ok(Value::Object(body))
}

/// Shared fixtures for the `wiremock` request-shape tests (#8632). The token
/// client is `BaseClient::for_test_with_token`.
#[cfg(test)]
pub(crate) mod test_support {
    use serde_json::{Value, json};
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// `base` with every key of `extra` added, plus `"account": "a"` so a
    /// `GWORKSPACE_ACCOUNT` in the test environment cannot redirect the token
    /// lookup.
    pub(crate) fn with_args(base: Value, extra: Value) -> Value {
        let mut args = base;
        let map = args.as_object_mut().expect("base args object");
        map.insert("account".into(), json!("a"));
        for (k, v) in extra.as_object().expect("extra args object") {
            map.insert(k.clone(), v.clone());
        }
        args
    }

    /// Mount a PATCH mock at `at` that must receive exactly `expect`
    /// requests, answering `{"id": "x1"}`; `Some(body)` also pins the JSON
    /// body.
    pub(crate) async fn mount_patch(
        server: &MockServer,
        at: &str,
        body: Option<Value>,
        expect: u64,
    ) {
        let mock = Mock::given(method("PATCH")).and(path(at));
        let mock = match body {
            Some(b) => mock.and(body_json(b)),
            None => mock,
        };
        mock.respond_with(ResponseTemplate::new(200).set_body_json(json!({ "id": "x1" })))
            .expect(expect)
            .mount(server)
            .await;
    }
}

/// Strip CR/LF from a value about to be interpolated into a raw RFC 2822
/// header line.
///
/// Why: Header values (Gmail `To`/`Cc`/`Bcc`/`Subject`/`In-Reply-To`, Drive
/// upload `Content-Type`) are formatted directly into hand-built header
/// lines. Several sources are attacker-influenceable: MCP tool arguments
/// directly, and — via the `reply` action — a fetched message's `Subject`/
/// `From` headers. An embedded `\r` or `\n` lets the value terminate the
/// current header and inject an arbitrary new one (e.g. a covert `Bcc:`),
/// a classic CRLF/header-injection vector. Every value placed into a header
/// line MUST be passed through this first.
/// What: Removes every `\r` and `\n` character; the result can never
/// terminate the header line it is placed into.
/// Test: `sanitize_header_value_strips_crlf` below; injection regression
/// tests live alongside each header-building call site.
pub(crate) fn sanitize_header_value(value: &str) -> String {
    value.chars().filter(|c| *c != '\r' && *c != '\n').collect()
}

/// Guess a MIME type from a filename extension (best-effort).
///
/// Why: Drive uploads and Gmail attachments both need a `Content-Type` when
/// the caller doesn't supply one; a small extension map covers the common
/// cases without pulling in a heavyweight mime-sniffing dependency.
/// What: Lowercases the file extension and maps it; unknown extensions fall
/// back to `application/octet-stream`.
/// Test: `guess_mime_covers_common_extensions` below.
pub(crate) fn guess_mime_from_path(path: &str) -> String {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "txt" | "text" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "zip" => "application/zip",
        "gz" | "gzip" => "application/gzip",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        _ => "application/octet-stream",
    };
    mime.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_header_value_strips_crlf() {
        assert_eq!(sanitize_header_value("plain value"), "plain value");
        assert_eq!(
            sanitize_header_value("a\r\nBcc: evil@x.com"),
            "aBcc: evil@x.com"
        );
        assert_eq!(sanitize_header_value("a\nb\rc"), "abc");
    }

    #[test]
    fn guess_mime_covers_common_extensions() {
        assert_eq!(guess_mime_from_path("/a/b/report.pdf"), "application/pdf");
        assert_eq!(guess_mime_from_path("logo.PNG"), "image/png");
        assert_eq!(guess_mime_from_path("notes.txt"), "text/plain");
        assert_eq!(guess_mime_from_path("data.json"), "application/json");
        assert_eq!(
            guess_mime_from_path("archive"),
            "application/octet-stream",
            "no extension -> octet-stream"
        );
        assert_eq!(
            guess_mime_from_path("weird.qwerty"),
            "application/octet-stream"
        );
    }
}
