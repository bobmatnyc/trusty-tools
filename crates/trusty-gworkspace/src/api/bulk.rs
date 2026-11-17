//! Bulk acquisition over Gmail and Drive: paginated listing, message body
//! decoding and Drive export, through the shared [`BaseClient`].
//!
//! Why: two callers acquire Google content in bulk. trusty-agents' OKG
//! ingest tools list to a ceiling in one call, and the trusty-okf import
//! adapters (`trusty-connectors`, #10746 S13) serve one page per protocol
//! request and must report a page token they did not follow. This code
//! lived in `trusty-agents/src/tools/okg/gapi.rs`; it moved here so both
//! reuse one copy without the adapters linking the agent core. Every
//! credential concern stays inside [`BaseClient`]. The convenience wrappers
//! in `services` hardcode their `fields` lists and drop `nextPageToken`, so
//! the list calls go through [`BaseClient::get`] with their own field sets.
//! What: the pure half ([`pct_encode`], [`gmail_query`], [`api_error`],
//! [`decode_body`], [`message_bodies`], [`message_header`],
//! [`message_attachments`]) converts JSON with no I/O. The async half lists
//! one page ([`gmail_list_page`], [`DriveWalk::next_page`]) or to a ceiling
//! ([`gmail_list_ids`], [`drive_list_files`]), and fetches one message
//! ([`gmail_message`]), one file's metadata ([`drive_file`]) or its content
//! ([`drive_export`], [`drive_content`]).
//! Test: `gmail_list_ids_follows_pages_through_a_loopback_root`,
//! `drive_walk_resumes_from_its_serialised_state` and the converter tests in
//! `bulk_tests.rs` beside this file.

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::client::BaseClient;
use crate::api::constants::{DRIVE_API_BASE, GMAIL_API_BASE};

/// Page size for list calls. Both APIs cap well above this.
pub const PAGE_SIZE: usize = 100;

/// Fields Drive must return for a ledger to detect a new revision.
/// `version` bumps on every content change; `modifiedTime` is the fallback.
/// #10746 S13: `createdTime` too, a source date the import keeps.
pub const DRIVE_FIELDS: &str = "nextPageToken,files(id,name,mimeType,modifiedTime,createdTime,\
                                version,size,parents,trashed)";

/// The MIME type of a Drive folder.
pub const DRIVE_FOLDER_MIME: &str = "application/vnd.google-apps.folder";

/// Percent-encode a query string value (RFC 3986 unreserved set).
///
/// Why: the crate's own encoder is `pub(crate)`, and a raw Gmail query
/// holds spaces and colons.
/// Test: `pct_encode_escapes_reserved`.
pub fn pct_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Compose the effective Gmail search string from a query plus a date window.
///
/// Why: a window kept apart from the query text can be widened later
/// without hand-editing a query string. This is where the two recombine.
/// What: appends `after:`/`before:` only when set, in a fixed order so the
/// same selection always yields the same query.
/// Test: `gmail_query_appends_window`, `gmail_query_without_window`.
pub fn gmail_query(query: &str, after: Option<&str>, before: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    let base = query.trim();
    if !base.is_empty() {
        parts.push(base.to_string());
    }
    if let Some(a) = after.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(format!("after:{a}"));
    }
    if let Some(b) = before.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(format!("before:{b}"));
    }
    parts.join(" ")
}

/// Surface a Google "soft error" payload as an error string.
///
/// Why: [`BaseClient`] returns 403/404 as `Ok(json!({"error": …, "status": …}))`
/// rather than `Err`. Treating that as an empty page would report "0 new
/// items" for a permission failure.
/// Test: `api_error_detects_soft_failure`.
pub fn api_error(value: &Value) -> Option<String> {
    let err = value.get("error")?;
    let detail = err
        .get("message")
        .and_then(Value::as_str)
        .map(String::from)
        .unwrap_or_else(|| err.to_string());
    let status = value
        .get("status")
        .and_then(Value::as_u64)
        .map(|s| format!(" (status {s})"))
        .unwrap_or_default();
    Some(format!("{detail}{status}"))
}

/// GET `url` and turn a soft error into `Err` (see [`api_error`]).
async fn get_checked(
    client: &BaseClient,
    url: &str,
    account: Option<&str>,
    what: &str,
) -> Result<Value> {
    let page = client
        .get(url, account)
        .await
        .with_context(|| what.to_string())?;
    if let Some(e) = api_error(&page) {
        anyhow::bail!("{what} failed: {e}");
    }
    Ok(page)
}

/// One page of a Gmail message listing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GmailPage {
    /// `(message ID, thread ID)` in listing order.
    pub messages: Vec<(String, Option<String>)>,
    /// The token of the next page, when Gmail returned one.
    pub next: Option<String>,
}

/// List one page of Gmail message IDs matching `query`.
///
/// Why: the import adapter answers one protocol request with one page and
/// must hand back the page token it did not follow (#10746 S13).
/// What: one `messages.list` call with `maxResults = max_results` (at
/// least 1) from `page_token`. Errors: transport, a soft error, or a page
/// with no `messages` array and no token is never an error — it is empty.
/// Test: `gmail_list_ids_follows_pages_through_a_loopback_root`.
pub async fn gmail_list_page(
    client: &BaseClient,
    account: Option<&str>,
    query: &str,
    page_token: Option<&str>,
    max_results: usize,
) -> Result<GmailPage> {
    let mut url = format!(
        "{GMAIL_API_BASE}/users/me/messages?q={}&maxResults={}",
        pct_encode(query),
        max_results.max(1)
    );
    if let Some(token) = page_token {
        url.push_str(&format!("&pageToken={}", pct_encode(token)));
    }
    let page = get_checked(client, &url, account, "gmail messages.list").await?;
    let messages = page
        .get("messages")
        .and_then(Value::as_array)
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| {
            let id = m.get("id").and_then(Value::as_str)?;
            let thread = m.get("threadId").and_then(Value::as_str).map(String::from);
            Some((id.to_string(), thread))
        })
        .collect();
    let next = page
        .get("nextPageToken")
        .and_then(Value::as_str)
        .map(String::from);
    Ok(GmailPage { messages, next })
}

/// List every Gmail message ID matching `query`, following `nextPageToken`.
///
/// `max_messages` bounds a backfill so one call cannot run unbounded; the
/// list stops there with no truncation flag (SPEC-OKF-01 §2.13 row 3 notes
/// it; the import adapter uses [`gmail_list_page`] instead).
/// Test: `gmail_list_ids_follows_pages_through_a_loopback_root`.
pub async fn gmail_list_ids(
    client: &BaseClient,
    account: Option<&str>,
    query: &str,
    max_messages: usize,
) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let page =
            gmail_list_page(client, account, query, page_token.as_deref(), PAGE_SIZE).await?;
        for (id, _) in page.messages {
            ids.push(id);
            if ids.len() >= max_messages {
                return Ok(ids);
            }
        }
        page_token = page.next;
        if page_token.is_none() {
            return Ok(ids);
        }
    }
}

/// Fetch one full (`format=full`) Gmail message through the shared service.
pub async fn gmail_message(
    client: &BaseClient,
    account: Option<&str>,
    message_id: &str,
) -> Result<Value> {
    let msg = crate::api::services::gmail::messages::get_gmail_message_content(
        client,
        serde_json::json!({ "message_id": message_id, "account": account }),
    )
    .await
    .with_context(|| format!("get_gmail_message_content({message_id})"))?;
    if let Some(e) = api_error(&msg) {
        anyhow::bail!("gmail message {message_id} failed: {e}");
    }
    Ok(msg)
}

/// The value of header `name` (case-insensitive) of a `format=full` message.
///
/// Test: `message_parts_decode_headers_bodies_and_attachments`.
pub fn message_header(msg: &Value, name: &str) -> Option<String> {
    msg.get("payload")?
        .get("headers")?
        .as_array()?
        .iter()
        .find(|h| {
            h.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| n.eq_ignore_ascii_case(name))
        })
        .and_then(|h| h.get("value").and_then(Value::as_str))
        .map(String::from)
}

/// The first decodable `text/plain` and `text/html` bodies of a MIME
/// payload, depth first. A part with a file name is an attachment, never a
/// body.
///
/// Test: `decode_body_walks_nested_parts`,
/// `message_parts_decode_headers_bodies_and_attachments`.
pub fn message_bodies(payload: &Value) -> (Option<String>, Option<String>) {
    fn collect(node: &Value, plain: &mut Option<String>, html: &mut Option<String>) {
        let mime = node.get("mimeType").and_then(Value::as_str).unwrap_or("");
        let attachment = node
            .get("filename")
            .and_then(Value::as_str)
            .is_some_and(|f| !f.is_empty());
        if !attachment
            && let Some(data) = node
                .get("body")
                .and_then(|b| b.get("data"))
                .and_then(Value::as_str)
            && let Ok(bytes) =
                base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(data.trim_end_matches('='))
            && let Ok(text) = String::from_utf8(bytes)
        {
            let slot: Option<&mut Option<String>> = match mime {
                "text/plain" => Some(&mut *plain),
                "text/html" => Some(&mut *html),
                _ => None,
            };
            if let Some(slot) = slot
                && slot.is_none()
            {
                *slot = Some(text);
            }
        }
        for part in node
            .get("parts")
            .and_then(Value::as_array)
            .map(|a| a.as_slice())
            .unwrap_or_default()
        {
            collect(part, plain, html);
        }
    }
    let (mut plain, mut html) = (None, None);
    collect(payload, &mut plain, &mut html);
    (plain, html)
}

/// The best text of a MIME payload: plain text, else HTML, else `None`.
///
/// Prefers `text/plain`; falls back to `text/html` so an HTML-only
/// newsletter still yields content rather than an empty entity.
/// Test: `decode_body_prefers_plain_text`, `decode_body_walks_nested_parts`.
pub fn decode_body(payload: &Value) -> Option<String> {
    let (plain, html) = message_bodies(payload);
    plain.or(html).filter(|t| !t.trim().is_empty())
}

/// One attachment of a message: listed, never downloaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentInfo {
    /// The file name.
    pub name: String,
    /// The MIME type.
    pub mime: String,
    /// The size in bytes Gmail reports.
    pub size: u64,
}

/// Every part of a MIME payload that carries a file name, depth first.
///
/// Test: `message_parts_decode_headers_bodies_and_attachments`.
pub fn message_attachments(payload: &Value) -> Vec<AttachmentInfo> {
    fn collect(node: &Value, out: &mut Vec<AttachmentInfo>) {
        if let Some(name) = node
            .get("filename")
            .and_then(Value::as_str)
            .filter(|f| !f.is_empty())
        {
            out.push(AttachmentInfo {
                name: name.to_string(),
                mime: node
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("application/octet-stream")
                    .to_string(),
                size: node
                    .get("body")
                    .and_then(|b| b.get("size"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            });
        }
        for part in node
            .get("parts")
            .and_then(Value::as_array)
            .map(|a| a.as_slice())
            .unwrap_or_default()
        {
            collect(part, out);
        }
    }
    let mut out = Vec::new();
    collect(payload, &mut out);
    out
}

/// A resumable walk over the files under one Drive folder.
///
/// Why: a folder listing spans several folders and pages; the import
/// adapter answers one request with one page and hands the walk back as an
/// opaque cursor, so the walk's whole state must serialise (#10746 S13).
/// What: `queue` holds folders still to list (last in, first listed),
/// `visited` the folders already started, `current` and `token` the folder
/// and page token in progress. [`DriveWalk::next_page`] lists one page and
/// returns its non-folder files; with `recursive` a subfolder joins the
/// queue. Trashed files are never listed. Before the first page the root
/// must be a readable, untrashed folder, else the walk fails.
/// Test: `drive_walk_resumes_from_its_serialised_state`,
/// `drive_walk_refuses_a_root_that_is_not_a_live_folder`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveWalk {
    /// Folders still to list.
    pub queue: Vec<String>,
    /// Folders already started; a folder reached twice is listed once.
    pub visited: BTreeSet<String>,
    /// The folder being listed.
    pub current: Option<String>,
    /// The next page token of `current`.
    pub token: Option<String>,
    /// Whether subfolders are walked.
    pub recursive: bool,
}

impl DriveWalk {
    /// A walk from `folder_id`.
    pub fn new(folder_id: &str, recursive: bool) -> DriveWalk {
        DriveWalk {
            queue: vec![folder_id.to_string()],
            recursive,
            ..DriveWalk::default()
        }
    }

    /// Whether no page is left.
    pub fn is_done(&self) -> bool {
        self.current.is_none() && self.queue.iter().all(|f| self.visited.contains(f))
    }

    /// List the next page, at most `page_size` entries (folders count).
    /// `Ok(None)` once the walk is done.
    pub async fn next_page(
        &mut self,
        client: &BaseClient,
        account: Option<&str>,
        page_size: usize,
    ) -> Result<Option<Vec<Value>>> {
        // #10746: Drive answers `files.list` for an unknown or unshared
        // folder with 200 and no files, which reads as a whole, empty
        // listing. Before the first page the root must be a live folder.
        if self.visited.is_empty()
            && self.current.is_none()
            && let Some(root) = self.queue.last()
        {
            check_root(client, account, root).await?;
        }
        let folder = loop {
            if let Some(f) = &self.current {
                break f.clone();
            }
            let Some(f) = self.queue.pop() else {
                return Ok(None);
            };
            if self.visited.insert(f.clone()) {
                self.current = Some(f);
                self.token = None;
            }
        };
        let q = parents_query(&folder);
        let mut url = format!(
            "{DRIVE_API_BASE}/files?q={}&pageSize={}&fields={}\
             &supportsAllDrives=true&includeItemsFromAllDrives=true",
            pct_encode(&q),
            page_size.max(1),
            pct_encode(DRIVE_FIELDS)
        );
        if let Some(token) = &self.token {
            url.push_str(&format!("&pageToken={}", pct_encode(token)));
        }
        let page = get_checked(client, &url, account, "drive files.list").await?;
        let mut files = Vec::new();
        for file in page
            .get("files")
            .and_then(Value::as_array)
            .map(|a| a.as_slice())
            .unwrap_or_default()
        {
            if file.get("mimeType").and_then(Value::as_str) == Some(DRIVE_FOLDER_MIME) {
                if self.recursive
                    && let Some(id) = file.get("id").and_then(Value::as_str)
                {
                    self.queue.push(id.to_string());
                }
                continue;
            }
            files.push(file.clone());
        }
        self.token = page
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(String::from);
        if self.token.is_none() {
            self.current = None;
        }
        Ok(Some(files))
    }
}

/// The `files.list` query for the untrashed children of `folder`, with
/// the ID escaped as a Drive query string literal.
///
/// Test: `parents_query_cannot_be_widened`.
pub fn parents_query(folder: &str) -> String {
    // #10746: Drive query syntax escapes `\` and `'` in a literal with a
    // backslash; unescaped, a crafted ID could close the literal and add a
    // clause that lists another folder's files.
    let id = folder.replace('\\', "\\\\").replace('\'', "\\'");
    format!("'{id}' in parents and trashed = false")
}

/// #10746: fail unless `folder` is a readable, untrashed Drive folder.
/// Errors: the `files.get` failure (403, 404, transport), a file that is
/// not a folder, or a trashed folder. #10879: it runs for every walk,
/// trusty-agents' okg drive ingest included; the `root` alias passes.
/// Test: `drive_walk_refuses_a_root_that_is_not_a_live_folder`,
/// `drive_walk_accepts_the_root_alias`.
async fn check_root(client: &BaseClient, account: Option<&str>, folder: &str) -> Result<()> {
    let meta = drive_file(client, account, folder).await?;
    if meta.get("mimeType").and_then(Value::as_str) != Some(DRIVE_FOLDER_MIME) {
        anyhow::bail!("drive root {folder} is not a folder");
    }
    if meta.get("trashed").and_then(Value::as_bool) == Some(true) {
        anyhow::bail!("drive root folder {folder} is trashed");
    }
    Ok(())
}

/// List Drive files under a folder, following `nextPageToken`, up to
/// `max_files`. Folders themselves are never returned.
///
/// Test: `drive_walk_resumes_from_its_serialised_state`.
pub async fn drive_list_files(
    client: &BaseClient,
    account: Option<&str>,
    folder_id: &str,
    recursive: bool,
    max_files: usize,
) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut walk = DriveWalk::new(folder_id, recursive);
    while let Some(files) = walk.next_page(client, account, PAGE_SIZE).await? {
        for file in files {
            out.push(file);
            if out.len() >= max_files {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

/// One Drive file's metadata: ID, name, MIME type, the two dates, version
/// and the trashed flag.
pub async fn drive_file(
    client: &BaseClient,
    account: Option<&str>,
    file_id: &str,
) -> Result<Value> {
    let url = format!(
        "{DRIVE_API_BASE}/files/{}?fields={}&supportsAllDrives=true",
        pct_encode(file_id),
        pct_encode("id,name,mimeType,modifiedTime,createdTime,version,trashed")
    );
    get_checked(
        client,
        &url,
        account,
        &format!("drive files.get({file_id})"),
    )
    .await
}

/// Download one Drive file, or export a native Google file as
/// `export_mime` (`text/plain` when `None`), through the shared service.
///
/// The reply is the service's: `content` for UTF-8 text, or `content` with
/// `encoding: base64` for anything else. Errors: transport or a soft error.
pub async fn drive_export(
    client: &BaseClient,
    account: Option<&str>,
    file_id: &str,
    export_mime: Option<&str>,
) -> Result<Value> {
    let mut args = serde_json::json!({ "file_id": file_id, "account": account });
    if let Some(mime) = export_mime {
        args["export_mime_type"] = Value::from(mime);
    }
    let result = crate::api::services::drive::files::get_drive_file_content(client, args)
        .await
        .with_context(|| format!("get_drive_file_content({file_id})"))?;
    if let Some(e) = api_error(&result) {
        anyhow::bail!("drive file {file_id} failed: {e}");
    }
    Ok(result)
}

/// Download or export one Drive file as text, or `None` when it is not
/// textual: a base64 payload is never written as text.
pub async fn drive_content(
    client: &BaseClient,
    account: Option<&str>,
    file_id: &str,
) -> Result<Option<String>> {
    let result = drive_export(client, account, file_id, None).await?;
    if result.get("encoding").and_then(Value::as_str) == Some("base64") {
        return Ok(None);
    }
    Ok(result
        .get("content")
        .and_then(Value::as_str)
        .map(String::from))
}

#[cfg(test)]
#[path = "bulk_tests.rs"]
mod tests;
