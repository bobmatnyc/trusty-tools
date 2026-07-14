//! Drive file listing, search, content fetch, shared drives, file mgmt.
//!
//! Why: Drive is the most-used MCP surface — listing folders, searching by
//! name, downloading file content (especially Docs as text/HTML).
//! What: Each tool is a thin wrapper over the v3 REST endpoints.
//! Test: Live only.

use anyhow::{Context, Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

use crate::api::client::BaseClient;
use crate::api::constants::{DRIVE_API_BASE, DRIVE_UPLOAD_BASE};
use crate::api::services::{account_of, guess_mime_from_path, opt_str, require_str};

/// Why: Folder listing is the entry point for any Drive navigation tool.
/// What: Queries `/files` filtered by parent id, returning name/mimeType/id for each child.
/// Test: Live API.
pub async fn list_drive_contents(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let folder_id = opt_str(&args, "folder_id").unwrap_or("root");
    let q = format!("'{folder_id}' in parents and trashed = false");
    let max = args
        .get("max_results")
        .and_then(|v| v.as_i64())
        .unwrap_or(100);
    let url = format!(
        "{DRIVE_API_BASE}/files?q={}&pageSize={max}&fields=files(id,name,mimeType,modifiedTime,size,parents)&supportsAllDrives=true&includeItemsFromAllDrives=true",
        encode(&q)
    );
    client.get(&url, account).await
}

/// Why: Full-text/metadata search across Drive needs the Drive `q` query DSL.
/// What: Forwards the user-supplied `query` to `/files?q=...` and returns matched files.
/// Test: Live API.
pub async fn search_drive_files(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let query = require_str(&args, "query")?;
    let max = args
        .get("max_results")
        .and_then(|v| v.as_i64())
        .unwrap_or(20);
    let url = format!(
        "{DRIVE_API_BASE}/files?q={}&pageSize={max}&fields=files(id,name,mimeType,modifiedTime,owners,parents)&supportsAllDrives=true&includeItemsFromAllDrives=true",
        encode(query)
    );
    client.get(&url, account).await
}

/// Why: Fetching a file body must be MIME-aware — Google-native docs need
/// `/export`, and binary files (images, PDFs, zips) must never be forced
/// through UTF-8 decoding, which silently corrupts them (parity fix #2627).
/// What: Resolves the file's MIME, downloads the body as raw bytes, then:
/// text-like content is returned inline as `content`; binary content is
/// base64-encoded (`encoding: "base64"`) unless `save_path` is set, in which
/// case the raw bytes are written to disk. `output_format: "raw"` forces the
/// binary path even for text MIME types (mirrors Python's `auto|raw`).
/// Test: `is_textual_mime` branching is unit-tested below; end-to-end 200 is
/// live-only.
pub async fn get_drive_file_content(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let id = require_str(&args, "file_id")?;
    let export_mime = opt_str(&args, "export_mime_type");
    let save_path = opt_str(&args, "save_path");
    let output_format = opt_str(&args, "output_format").unwrap_or("auto");

    // Get metadata first to know mime type.
    let meta_url =
        format!("{DRIVE_API_BASE}/files/{id}?fields=id,name,mimeType&supportsAllDrives=true");
    let meta = client.get(&meta_url, account).await?;
    let mime = meta.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");
    let name = meta.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let is_google_native = mime.starts_with("application/vnd.google-apps");

    let content_url = if is_google_native {
        // Google native doc — must use export.
        let target = export_mime.unwrap_or("text/plain");
        format!(
            "{DRIVE_API_BASE}/files/{id}/export?mimeType={}",
            encode(target)
        )
    } else {
        format!("{DRIVE_API_BASE}/files/{id}?alt=media&supportsAllDrives=true")
    };

    let raw = client.get_bytes(&content_url, account).await?;
    if !raw.status.is_success() {
        let text = String::from_utf8_lossy(&raw.bytes).into_owned();
        return Ok(json!({ "error": text, "status": raw.status.as_u16() }));
    }

    // Effective MIME: an export target, else the response header, else metadata.
    let effective_mime: String = if is_google_native {
        export_mime.unwrap_or("text/plain").to_string()
    } else {
        raw.content_type
            .as_deref()
            .map(|ct| ct.split(';').next().unwrap_or(ct).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| mime.to_string())
    };

    // Persisting to disk always writes the exact bytes, text or binary.
    if let Some(path) = save_path {
        std::fs::write(path, &raw.bytes).with_context(|| format!("write drive file to {path}"))?;
        return Ok(json!({
            "id": id,
            "name": name,
            "mimeType": effective_mime,
            "savedTo": path,
            "bytesWritten": raw.bytes.len(),
        }));
    }

    let treat_as_text = output_format != "raw" && is_textual_mime(&effective_mime);
    if treat_as_text {
        // Only return inline text when the bytes are valid UTF-8; otherwise
        // fall through to base64 so we never emit lossy content.
        match String::from_utf8(raw.bytes) {
            Ok(text) => {
                return Ok(json!({
                    "id": id,
                    "name": name,
                    "mimeType": effective_mime,
                    "content": text,
                }));
            }
            Err(e) => {
                return Ok(binary_content_json(id, name, &effective_mime, e.as_bytes()));
            }
        }
    }
    Ok(binary_content_json(id, name, &effective_mime, &raw.bytes))
}

/// Why: Binary payloads must round-trip losslessly through JSON.
/// What: Base64-encodes `bytes` and tags the payload `encoding: "base64"`.
/// Test: Covered indirectly by the `get_drive_file_content` binary branch.
fn binary_content_json(id: &str, name: &str, mime: &str, bytes: &[u8]) -> Value {
    json!({
        "id": id,
        "name": name,
        "mimeType": mime,
        "encoding": "base64",
        "size": bytes.len(),
        "content": STANDARD.encode(bytes),
    })
}

/// Why: Deciding text-vs-binary handling requires a MIME classifier that
/// covers the common textual families Drive serves.
/// What: Returns true for `text/*`, JSON/XML (incl. `+json`/`+xml` suffixes),
/// CSV, JavaScript, and SVG; false otherwise. Case- and parameter-insensitive.
/// Test: `textual_mime_classification` below.
pub(crate) fn is_textual_mime(mime: &str) -> bool {
    let m = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();
    m.starts_with("text/")
        || m == "application/json"
        || m == "application/xml"
        || m == "application/xhtml+xml"
        || m == "application/javascript"
        || m == "application/x-ndjson"
        || m == "application/csv"
        || m == "image/svg+xml"
        || m.ends_with("+json")
        || m.ends_with("+xml")
}

/// Why: Shared drives are a separate Drive endpoint from regular `/files`.
/// What: GETs `/drives` and returns name+id for each shared drive the user can access.
/// Test: Live API.
pub async fn list_shared_drives(client: &BaseClient, args: Value) -> Result<Value> {
    let account = account_of(&args);
    let url = format!("{DRIVE_API_BASE}/drives?pageSize=100");
    client.get(&url, account).await
}

/// Why: File-level mutations (create folder, rename, trash, upload) share one
/// Drive API surface.
/// What: Dispatches `create_folder|rename|trash|delete|copy|move|upload` to the
/// Drive `/files` (and `/upload/.../files`) endpoints.
/// Test: `upload` multipart-body shape is unit-tested via
/// `build_multipart_related`; live 200 validation is deferred.
pub async fn manage_drive_file(client: &BaseClient, args: Value) -> Result<Value> {
    let action = require_str(&args, "action")?;
    let account = account_of(&args);
    match action {
        "create_folder" => {
            let name = require_str(&args, "name")?;
            let parent = opt_str(&args, "parent_id");
            let mut body = json!({
                "name": name,
                "mimeType": "application/vnd.google-apps.folder",
            });
            if let Some(p) = parent {
                body["parents"] = json!([p]);
            }
            let url = format!("{DRIVE_API_BASE}/files?supportsAllDrives=true");
            client.post(&url, body, account).await
        }
        "rename" => {
            let id = require_str(&args, "file_id")?;
            let name = require_str(&args, "name")?;
            let body = json!({ "name": name });
            let url = format!("{DRIVE_API_BASE}/files/{id}?supportsAllDrives=true");
            client.patch(&url, body, account).await
        }
        "trash" => {
            let id = require_str(&args, "file_id")?;
            let body = json!({ "trashed": true });
            let url = format!("{DRIVE_API_BASE}/files/{id}?supportsAllDrives=true");
            client.patch(&url, body, account).await
        }
        "delete" => {
            let id = require_str(&args, "file_id")?;
            let url = format!("{DRIVE_API_BASE}/files/{id}?supportsAllDrives=true");
            client.delete(&url, account).await
        }
        "copy" => {
            let id = require_str(&args, "file_id")?;
            let body = json!({
                "name": opt_str(&args, "name").unwrap_or("Copy"),
            });
            let url = format!("{DRIVE_API_BASE}/files/{id}/copy?supportsAllDrives=true");
            client.post(&url, body, account).await
        }
        "move" => {
            let id = require_str(&args, "file_id")?;
            let parent = require_str(&args, "parent_id")?;
            let url =
                format!("{DRIVE_API_BASE}/files/{id}?addParents={parent}&supportsAllDrives=true");
            client.patch(&url, json!({}), account).await
        }
        "upload" => {
            let name = require_str(&args, "name")?;
            let parent = opt_str(&args, "parent_id");
            let mime_arg = opt_str(&args, "mime_type");

            // Source bytes come from either a local file or inline content.
            let (bytes, resolved_mime) = if let Some(path) = opt_str(&args, "local_path") {
                let data =
                    std::fs::read(path).with_context(|| format!("read upload source {path}"))?;
                let mime = mime_arg
                    .map(str::to_string)
                    .unwrap_or_else(|| guess_mime_from_path(path));
                (data, mime)
            } else if let Some(content) = opt_str(&args, "content") {
                let mime = mime_arg.unwrap_or("text/plain").to_string();
                (content.as_bytes().to_vec(), mime)
            } else {
                return Err(anyhow!(
                    "upload requires either 'local_path' or inline 'content'"
                ));
            };

            let mut metadata = json!({ "name": name, "mimeType": resolved_mime });
            if let Some(p) = parent {
                metadata["parents"] = json!([p]);
            }

            let boundary = format!("gworkspace-{}", uuid::Uuid::new_v4().simple());
            let body = build_multipart_related(&metadata, &resolved_mime, &bytes, &boundary)?;
            let content_type = format!("multipart/related; boundary={boundary}");
            let url = format!(
                "{DRIVE_UPLOAD_BASE}?uploadType=multipart&supportsAllDrives=true&fields=id,name,mimeType,size,parents"
            );
            client.post_raw(&url, &content_type, body, account).await
        }
        other => Err(anyhow!("unknown action for manage_drive_file: {other}")),
    }
}

/// Why: Drive's multipart upload endpoint expects a `multipart/related` body
/// pairing a JSON metadata part with the raw media part; getting the CRLF
/// framing wrong yields a 400.
/// What: Serialises `metadata` as the first part and `content` (under `mime`)
/// as the second, delimited by `boundary` per RFC 2387.
/// Test: `multipart_body_has_metadata_and_media_parts` below.
fn build_multipart_related(
    metadata: &Value,
    mime: &str,
    content: &[u8],
    boundary: &str,
) -> Result<Vec<u8>> {
    let meta_json = serde_json::to_vec(metadata).context("serialize upload metadata")?;
    let mut body = Vec::with_capacity(meta_json.len() + content.len() + 256);
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(b"Content-Type: application/json; charset=UTF-8\r\n\r\n");
    body.extend_from_slice(&meta_json);
    body.extend_from_slice(format!("\r\n--{boundary}\r\n").as_bytes());
    body.extend_from_slice(format!("Content-Type: {mime}\r\n\r\n").as_bytes());
    body.extend_from_slice(content);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Ok(body)
}

pub(crate) fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~') {
            out.push(c);
        } else {
            for b in c.to_string().bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textual_mime_classification() {
        // Text families are inline-able.
        for m in [
            "text/plain",
            "text/html; charset=UTF-8",
            "application/json",
            "application/vnd.api+json",
            "application/xml",
            "image/svg+xml",
            "application/csv",
            "TEXT/CSV",
        ] {
            assert!(is_textual_mime(m), "{m} should be textual");
        }
        // Binary families must not be decoded as UTF-8.
        for m in [
            "image/png",
            "application/pdf",
            "application/zip",
            "application/octet-stream",
            "video/mp4",
        ] {
            assert!(!is_textual_mime(m), "{m} should be binary");
        }
    }

    #[test]
    fn binary_content_json_is_base64_encoded() {
        // Non-UTF-8 bytes must survive JSON round-tripping via base64.
        let bytes = [0x89u8, 0x50, 0x4E, 0x47, 0x00, 0xFF];
        let v = binary_content_json("id1", "logo.png", "image/png", &bytes);
        assert_eq!(v["encoding"], "base64");
        assert_eq!(v["size"], 6);
        let decoded = STANDARD
            .decode(v["content"].as_str().unwrap())
            .expect("valid base64");
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn multipart_body_has_metadata_and_media_parts() {
        let metadata = json!({ "name": "hi.txt", "mimeType": "text/plain" });
        let body =
            build_multipart_related(&metadata, "text/plain", b"hello world", "BOUND").unwrap();
        let text = String::from_utf8(body).expect("ascii-framed multipart");
        // Two boundaries + closing delimiter.
        assert_eq!(text.matches("--BOUND\r\n").count(), 2);
        assert!(text.ends_with("--BOUND--\r\n"));
        // JSON metadata part precedes the media part.
        assert!(text.contains("Content-Type: application/json; charset=UTF-8\r\n\r\n"));
        assert!(text.contains("\"name\":\"hi.txt\""));
        assert!(text.contains("Content-Type: text/plain\r\n\r\nhello world\r\n"));
        // Metadata part must come before the media body.
        let meta_at = text.find("\"name\":\"hi.txt\"").unwrap();
        let media_at = text.find("hello world").unwrap();
        assert!(meta_at < media_at);
    }
}
