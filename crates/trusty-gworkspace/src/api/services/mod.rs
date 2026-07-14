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
