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
use crate::api::services::{
    account_of, guess_mime_from_path, opt_str, require_str, sanitize_header_value,
};

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

/// Google-native MIME types whose content `update` can replace in place.
///
/// Why: Drive converts an uploaded body into a native Doc/Sheet/Slides file
/// only for these three; PATCHing bytes at anything else (a PDF, an image, a
/// folder) either corrupts it or fails opaquely, so the action refuses up
/// front rather than guessing (#6685).
/// What: The three `application/vnd.google-apps.*` editor types.
/// Test: `update_refuses_non_google_native_target`.
const IN_PLACE_UPDATABLE_MIMES: [&str; 3] = [
    "application/vnd.google-apps.document",
    "application/vnd.google-apps.spreadsheet",
    "application/vnd.google-apps.presentation",
];

/// Why: File-level mutations (create folder, rename, trash, upload, in-place
/// content replace) share one Drive API surface.
/// What: Dispatches `create_folder|rename|trash|delete|copy|move|upload|update`
/// to the Drive `/files` (and `/upload/.../files`) endpoints.
/// Test: `upload` multipart-body shape is unit-tested via
/// `build_multipart_related`; `update` is covered end-to-end against a
/// `wiremock` server by `update_refuses_non_google_native_target` and
/// `update_patches_media_to_the_existing_file_id`.
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
            let (bytes, resolved_mime) = read_source_bytes(&args, "upload")?;

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
        // #6685: in-place replace keeps file id and revision history.
        "update" => {
            update_drive_file(client, &args, account, DRIVE_API_BASE, DRIVE_UPLOAD_BASE).await
        }
        other => Err(anyhow!("unknown action for manage_drive_file: {other}")),
    }
}

/// Resolve an action's payload bytes and their MIME type from the arguments.
///
/// Why: `upload` and `update` accept the same two-of-one source — a local file
/// or inline text — and the same optional `mime_type` override. One reader
/// keeps the extension guess and the `text/plain` inline default identical
/// across both.
/// What: Reads `local_path` (MIME guessed from its extension) or `content`
/// (MIME defaults to `text/plain`), with `mime_type` overriding either. Errors
/// naming `action` when neither source is present.
/// Test: `source_bytes_prefers_local_path_and_guesses_mime`.
fn read_source_bytes(args: &Value, action: &str) -> Result<(Vec<u8>, String)> {
    let mime_arg = opt_str(args, "mime_type");
    if let Some(path) = opt_str(args, "local_path") {
        let data = std::fs::read(path).with_context(|| format!("read {action} source {path}"))?;
        let mime = mime_arg
            .map(str::to_string)
            .unwrap_or_else(|| guess_mime_from_path(path));
        return Ok((data, mime));
    }
    if let Some(content) = opt_str(args, "content") {
        return Ok((
            content.as_bytes().to_vec(),
            mime_arg.unwrap_or("text/plain").to_string(),
        ));
    }
    Err(anyhow!(
        "{action} requires either 'local_path' or inline 'content'"
    ))
}

/// Replace an existing Google editor file's content in place.
///
/// Why: Publishing into an EXISTING Google Doc must keep the file id, its
/// share link, its permissions, and its revision history. `upload` POSTs a
/// NEW file, so every workflow that re-publishes into a known doc id had to
/// drop out to a raw `curl` (#6685).
/// What: Reads the target's `mimeType` first and refuses anything that is not
/// a Google Doc/Sheet/Slides file — no fallback to creating a new file — then
/// PATCHes the source bytes to `/upload/drive/v3/files/{id}` with
/// `uploadType=media`, or `uploadType=multipart` when a `name` is also given
/// so the rename rides along in the same request. Drive converts the body
/// into the file's existing native type, which is why the request never
/// re-states `mimeType` in its metadata part.
/// `api_base`/`upload_base` are parameters, not constants, so the tests can
/// point both at a mock server.
/// Test: `update_refuses_non_google_native_target`,
/// `update_patches_media_to_the_existing_file_id`.
async fn update_drive_file(
    client: &BaseClient,
    args: &Value,
    account: Option<&str>,
    api_base: &str,
    upload_base: &str,
) -> Result<Value> {
    let id = require_str(args, "file_id")?;
    let meta_url = format!("{api_base}/files/{id}?fields=id,name,mimeType&supportsAllDrives=true");
    let meta = client.get(&meta_url, account).await?;
    if let Some(err) = meta.get("error") {
        return Ok(json!({ "error": err, "fileId": id }));
    }
    let mime = meta.get("mimeType").and_then(|v| v.as_str()).unwrap_or("");
    if !IN_PLACE_UPDATABLE_MIMES.contains(&mime) {
        let name = meta.get("name").and_then(|v| v.as_str()).unwrap_or("");
        return Ok(refuse_non_native_target(id, name, mime));
    }

    let (bytes, source_mime) = read_source_bytes(args, "update")?;
    let fields = "fields=id,name,mimeType,modifiedTime,version";
    // A rename rides along as a multipart metadata part; content-only updates
    // take the cheaper media path.
    if let Some(new_name) = opt_str(args, "name") {
        let boundary = format!("gworkspace-{}", uuid::Uuid::new_v4().simple());
        let body = build_multipart_related(
            &json!({ "name": new_name }),
            &source_mime,
            &bytes,
            &boundary,
        )?;
        let content_type = format!("multipart/related; boundary={boundary}");
        let url =
            format!("{upload_base}/{id}?uploadType=multipart&supportsAllDrives=true&{fields}");
        return client.patch_raw(&url, &content_type, body, account).await;
    }
    let url = format!("{upload_base}/{id}?uploadType=media&supportsAllDrives=true&{fields}");
    client
        .patch_raw(&url, &sanitize_header_value(&source_mime), bytes, account)
        .await
}

/// The structured refusal returned for a target `update` must not touch.
///
/// Why: The caller needs the ACTUAL mimeType to diagnose a wrong file id, and
/// a machine-readable payload beats a prose error an agent has to parse. A
/// silent fallback to `upload` would strand the share link the caller was
/// trying to preserve (#6685).
/// What: An `error` envelope (this crate's operational-failure shape) carrying
/// the file id, its name, its real `mimeType`, and the supported set.
/// Test: `update_refuses_non_google_native_target`.
fn refuse_non_native_target(file_id: &str, name: &str, mime: &str) -> Value {
    json!({
        "error": format!(
            "manage_drive_file action=update replaces the content of an existing Google Doc, \
             Sheet, or Slides file; file {file_id} has mimeType '{mime}'"
        ),
        "fileId": file_id,
        "name": name,
        "mimeType": mime,
        "supportedMimeTypes": IN_PLACE_UPDATABLE_MIMES,
    })
}

/// Why: Drive's multipart upload endpoint expects a `multipart/related` body
/// pairing a JSON metadata part with the raw media part; getting the CRLF
/// framing wrong yields a 400. `mime` comes from a caller-supplied
/// `mime_type` argument (or our own extension guess), so it must be
/// CRLF-sanitized before landing in the raw `Content-Type:` header line —
/// the same header-injection class as the Gmail compose headers.
/// What: Serialises `metadata` as the first part and `content` (under the
/// sanitized `mime`) as the second, delimited by `boundary` per RFC 2387.
/// Test: `multipart_body_has_metadata_and_media_parts`,
/// `multipart_related_sanitizes_crlf_in_mime` below.
fn build_multipart_related(
    metadata: &Value,
    mime: &str,
    content: &[u8],
    boundary: &str,
) -> Result<Vec<u8>> {
    let mime = sanitize_header_value(mime);
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
    use crate::api::auth::TokenStorage;
    use crate::api::auth::models::{OAuthToken, StoredToken, TokenMetadata};
    use chrono::{Duration as ChronoDuration, Utc};
    use std::collections::HashMap;
    use wiremock::matchers::{header, method, path as wm_path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const GOOGLE_DOC: &str = "application/vnd.google-apps.document";

    /// A client whose only profile ("a") holds a token an hour from expiry, so
    /// `get_access_token` never reaches the OAuth refresh path.
    fn client_with_token() -> BaseClient {
        let dir = std::env::temp_dir().join(format!("gw-drive-update-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp token dir");
        let client = BaseClient::for_test(TokenStorage::with_path(dir.join("tokens.json")));
        let mut map = HashMap::new();
        map.insert(
            "a".to_string(),
            StoredToken {
                version: 1,
                metadata: TokenMetadata {
                    service_name: "a".into(),
                    provider: "google".into(),
                    created_at: Utc::now(),
                    last_refreshed: None,
                    email: Some("a@example.com".into()),
                    is_default: true,
                },
                token: OAuthToken {
                    access_token: "test-access-token".into(),
                    refresh_token: Some("r".into()),
                    expires_at: Utc::now() + ChronoDuration::seconds(3600),
                    scopes: vec![],
                    token_type: "Bearer".into(),
                },
            },
        );
        client.storage().save(&map).expect("seed token storage");
        client
    }

    /// Mount the metadata GET the update action reads before doing anything.
    async fn mount_metadata(server: &MockServer, id: &str, name: &str, mime: &str) {
        Mock::given(method("GET"))
            .and(wm_path(format!("/files/{id}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({ "id": id, "name": name, "mimeType": mime })),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn update_refuses_non_google_native_target() {
        let server = MockServer::start().await;
        mount_metadata(&server, "pdf1", "report.pdf", "application/pdf").await;
        // Any PATCH at all would mean the refusal leaked into a write.
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
            .expect(0)
            .mount(&server)
            .await;

        let client = client_with_token();
        let upload_base = format!("{}/upload/files", server.uri());
        let out = update_drive_file(
            &client,
            &json!({ "file_id": "pdf1", "content": "hello" }),
            Some("a"),
            &server.uri(),
            &upload_base,
        )
        .await
        .expect("refusal is a structured payload, not a transport failure");

        assert_eq!(out["fileId"], "pdf1");
        assert_eq!(out["name"], "report.pdf");
        // The ACTUAL mimeType must be named, both as a field and in the prose.
        assert_eq!(out["mimeType"], "application/pdf");
        let msg = out["error"].as_str().expect("error string");
        assert!(
            msg.contains("application/pdf"),
            "error names the mime: {msg}"
        );
        assert!(msg.contains("pdf1"), "error names the file id: {msg}");
        assert_eq!(out["supportedMimeTypes"][0], GOOGLE_DOC);
    }

    #[tokio::test]
    async fn update_patches_media_to_the_existing_file_id() {
        let server = MockServer::start().await;
        mount_metadata(&server, "doc1", "Weekly Report", GOOGLE_DOC).await;
        Mock::given(method("PATCH"))
            // Same file id as the target — never a new file.
            .and(wm_path("/upload/files/doc1"))
            .and(query_param("uploadType", "media"))
            .and(header("content-type", "text/html"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id": "doc1",
                "name": "Weekly Report",
                "mimeType": GOOGLE_DOC,
            })))
            .expect(1)
            .mount(&server)
            .await;

        let client = client_with_token();
        let upload_base = format!("{}/upload/files", server.uri());
        let out = update_drive_file(
            &client,
            &json!({
                "file_id": "doc1",
                "content": "<h1>heading</h1>",
                "mime_type": "text/html",
            }),
            Some("a"),
            &server.uri(),
            &upload_base,
        )
        .await
        .expect("media PATCH succeeds");

        assert_eq!(out["id"], "doc1");
        assert_eq!(out["mimeType"], GOOGLE_DOC);
    }

    #[test]
    fn source_bytes_prefers_local_path_and_guesses_mime() {
        let path = std::env::temp_dir().join(format!("gw-src-{}.html", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"<p>hi</p>").expect("write source file");
        let args = json!({
            "local_path": path.to_string_lossy(),
            "content": "ignored when local_path is present",
        });
        let (bytes, mime) = read_source_bytes(&args, "update").expect("read source");
        assert_eq!(bytes, b"<p>hi</p>");
        assert_eq!(mime, "text/html");
        let _ = std::fs::remove_file(&path);

        // Inline content defaults to text/plain, and an explicit override wins.
        let (bytes, mime) = read_source_bytes(&json!({ "content": "raw" }), "update").unwrap();
        assert_eq!(bytes, b"raw");
        assert_eq!(mime, "text/plain");

        // Neither source is an error naming the action.
        let err = read_source_bytes(&json!({}), "update").unwrap_err();
        assert!(err.to_string().contains("update"));
    }

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

    #[test]
    fn multipart_related_sanitizes_crlf_in_mime() {
        // A malicious mime_type must not be able to smuggle a NEW header line
        // into the multipart/related part it is formatted into — the value
        // survives (merged harmlessly onto the Content-Type line) but the
        // CRLF that would have started a fresh "X-Injected:" header is gone.
        let metadata = json!({ "name": "hi.txt" });
        let malicious_mime = "text/plain\r\nX-Injected: evil";
        let body = build_multipart_related(&metadata, malicious_mime, b"data", "BOUND").unwrap();
        let text = String::from_utf8(body).expect("ascii-framed multipart");
        assert!(!text.contains("\r\nX-Injected:"));
        assert!(text.contains("Content-Type: text/plainX-Injected: evil\r\n\r\ndata"));
    }
}
