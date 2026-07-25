//! Google Workspace adapters — Gmail/Drive responses into OKG [`SourceItem`]s.
//!
//! Why: Gmail and Drive ingestion MUST reuse `trusty-gworkspace`'s authenticated
//! `BaseClient` (token storage, silent refresh, no browser) rather than adding a
//! second OAuth path. But that crate's convenience wrappers hardcode their
//! `fields` lists and drop `nextPageToken`, so a bulk backfill cannot paginate
//! through them — Drive's wrapper does not even request the token. We therefore
//! issue the list calls through `BaseClient::get` with our own field sets, which
//! keeps every credential concern inside `trusty-gworkspace` while giving us the
//! pagination and revision metadata an idempotent ledger needs.
//!
//! What: the pure half — [`gmail_query`], [`message_to_item`], [`drive_item`],
//! [`api_error`], [`decode_body`] — converts JSON to items with no I/O, so the
//! ingestion contract is unit-testable. The async half ([`gmail_list_ids`],
//! [`drive_list_files`], [`gmail_message`], [`drive_content`]) is thin plumbing
//! over the shared client.
//!
//! Test: see `super::tests` — `gmail_query_*`, `message_to_item_*`,
//! `drive_item_*`, `api_error_*`, `decode_body_*`.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use base64::Engine;
use serde_json::Value;
use trusty_gworkspace::api::client::BaseClient;
use trusty_gworkspace::api::constants::{DRIVE_API_BASE, GMAIL_API_BASE};
use trusty_kb::okg::ingest::SourceItem;

/// Page size for list calls. Both APIs cap well above this.
const PAGE_SIZE: usize = 100;

/// Fields Drive must return for the ledger to detect a new revision.
/// `version` bumps on every content change; `modifiedTime` is the fallback.
const DRIVE_FIELDS: &str =
    "nextPageToken,files(id,name,mimeType,modifiedTime,version,size,parents,trashed)";

/// Percent-encode a query string value (RFC 3986 unreserved set).
///
/// Why: `trusty-gworkspace`'s encoder is `pub(crate)`, and we build our own URLs
/// to get pagination. A raw Gmail query contains spaces and colons.
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
/// Why: the window is stored structurally in the source registry (not baked
/// into the query text) precisely so it can be WIDENED later without the
/// operator hand-editing a query string. This is where the two recombine.
/// What: appends `after:`/`before:` only when set, in a fixed order so the same
/// locator always yields the same query.
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
/// Why: `BaseClient` returns 403/404 as `Ok(json!({"error": …, "status": …}))`
/// rather than `Err`. Treating that as a successful empty page would silently
/// report "0 new items" for a permission failure — the exact silent-fallback
/// this codebase forbids.
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

/// List every Gmail message id matching `query`, following `nextPageToken`.
///
/// `max_messages` bounds a backfill so one call cannot run unbounded.
pub async fn gmail_list_ids(
    client: &BaseClient,
    account: Option<&str>,
    query: &str,
    max_messages: usize,
) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    let mut page_token: Option<String> = None;
    loop {
        let mut url = format!(
            "{GMAIL_API_BASE}/users/me/messages?q={}&maxResults={PAGE_SIZE}",
            pct_encode(query)
        );
        if let Some(token) = &page_token {
            url.push_str(&format!("&pageToken={}", pct_encode(token)));
        }
        let page = client
            .get(&url, account)
            .await
            .context("gmail messages.list")?;
        if let Some(e) = api_error(&page) {
            anyhow::bail!("gmail messages.list failed: {e}");
        }
        for msg in page
            .get("messages")
            .and_then(Value::as_array)
            .unwrap_or(&vec![])
        {
            if let Some(id) = msg.get("id").and_then(Value::as_str) {
                ids.push(id.to_string());
            }
            if ids.len() >= max_messages {
                return Ok(ids);
            }
        }
        page_token = page
            .get("nextPageToken")
            .and_then(Value::as_str)
            .map(String::from);
        if page_token.is_none() {
            return Ok(ids);
        }
    }
}

/// Fetch one full Gmail message through the shared service function.
pub async fn gmail_message(
    client: &BaseClient,
    account: Option<&str>,
    message_id: &str,
) -> Result<Value> {
    let msg = trusty_gworkspace::api::services::gmail::messages::get_gmail_message_content(
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

/// Convert a `format=full` Gmail message into a [`SourceItem`].
///
/// Why: a Gmail message id is IMMUTABLE and its content never changes, so the
/// id doubles as the fingerprint — which is exactly what makes "pull exactly
/// once, ever" true even when a widened window re-lists the same message.
/// What: pulls From/To/Subject/Date headers, decodes the body, and stamps
/// provenance. Returns `None` when the payload has no usable id.
/// Test: `message_to_item_extracts_headers_and_body`.
pub fn message_to_item(msg: &Value) -> Option<SourceItem> {
    let id = msg.get("id").and_then(Value::as_str)?;
    let header = |name: &str| -> Option<String> {
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
    };

    let subject = header("Subject").unwrap_or_else(|| "(no subject)".to_string());
    let timestamp = msg
        .get("internalDate")
        .and_then(|v| {
            v.as_str()
                .and_then(|s| s.parse::<i64>().ok())
                .or_else(|| v.as_i64())
        })
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string());

    let mut fields = BTreeMap::new();
    for (key, header_name) in [("from", "From"), ("to", "To"), ("cc", "Cc")] {
        if let Some(v) = header(header_name) {
            fields.insert(key.to_string(), v);
        }
    }
    if let Some(thread) = msg.get("threadId").and_then(Value::as_str) {
        fields.insert("thread_id".to_string(), thread.to_string());
    }

    let body = decode_body(msg.get("payload").unwrap_or(&Value::Null)).unwrap_or_else(|| {
        msg.get("snippet")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    });

    // Date-prefixing the entity name keeps a mailbox chronologically browsable
    // and makes two same-subject threads distinct files.
    let date_prefix = timestamp
        .as_deref()
        .and_then(|t| t.split('T').next())
        .unwrap_or("undated");

    Some(SourceItem {
        item_id: id.to_string(),
        fingerprint: id.to_string(),
        name: format!("{date_prefix} {subject}"),
        title: subject,
        timestamp,
        body,
        fields,
        // A Gmail message is immutable, so its id is a perfect change signal —
        // never volatile, which is what makes "pull exactly once, ever" hold.
        volatile: false,
    })
}

/// Depth-first walk of a MIME payload for the best text representation.
///
/// Prefers `text/plain`; falls back to `text/html` so an HTML-only newsletter
/// still yields content rather than an empty entity.
/// Test: `decode_body_prefers_plain_text`, `decode_body_walks_nested_parts`.
pub fn decode_body(payload: &Value) -> Option<String> {
    fn collect(node: &Value, plain: &mut Option<String>, html: &mut Option<String>) {
        let mime = node.get("mimeType").and_then(Value::as_str).unwrap_or("");
        if let Some(data) = node
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
            .unwrap_or(&vec![])
        {
            collect(part, plain, html);
        }
    }
    let (mut plain, mut html) = (None, None);
    collect(payload, &mut plain, &mut html);
    plain.or(html).filter(|t| !t.trim().is_empty())
}

/// List Drive files under a folder, following `nextPageToken`.
///
/// `recursive` descends into subfolders breadth-first; folders themselves are
/// never emitted as items.
pub async fn drive_list_files(
    client: &BaseClient,
    account: Option<&str>,
    folder_id: &str,
    recursive: bool,
    max_files: usize,
) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut queue = vec![folder_id.to_string()];
    let mut visited = std::collections::BTreeSet::new();

    while let Some(folder) = queue.pop() {
        if !visited.insert(folder.clone()) {
            continue;
        }
        let mut page_token: Option<String> = None;
        loop {
            let q = format!("'{folder}' in parents and trashed = false");
            let mut url = format!(
                "{DRIVE_API_BASE}/files?q={}&pageSize={PAGE_SIZE}&fields={}\
                 &supportsAllDrives=true&includeItemsFromAllDrives=true",
                pct_encode(&q),
                pct_encode(DRIVE_FIELDS)
            );
            if let Some(token) = &page_token {
                url.push_str(&format!("&pageToken={}", pct_encode(token)));
            }
            let page = client
                .get(&url, account)
                .await
                .context("drive files.list")?;
            if let Some(e) = api_error(&page) {
                anyhow::bail!("drive files.list failed: {e}");
            }
            for file in page
                .get("files")
                .and_then(Value::as_array)
                .unwrap_or(&vec![])
            {
                let mime = file.get("mimeType").and_then(Value::as_str).unwrap_or("");
                if mime == "application/vnd.google-apps.folder" {
                    if recursive && let Some(id) = file.get("id").and_then(Value::as_str) {
                        queue.push(id.to_string());
                    }
                    continue;
                }
                out.push(file.clone());
                if out.len() >= max_files {
                    return Ok(out);
                }
            }
            page_token = page
                .get("nextPageToken")
                .and_then(Value::as_str)
                .map(String::from);
            if page_token.is_none() {
                break;
            }
        }
    }
    Ok(out)
}

/// Download or export one Drive file as text, or `None` when it is not textual.
pub async fn drive_content(
    client: &BaseClient,
    account: Option<&str>,
    file_id: &str,
) -> Result<Option<String>> {
    let result = trusty_gworkspace::api::services::drive::files::get_drive_file_content(
        client,
        serde_json::json!({ "file_id": file_id, "account": account }),
    )
    .await
    .with_context(|| format!("get_drive_file_content({file_id})"))?;
    if let Some(e) = api_error(&result) {
        anyhow::bail!("drive file {file_id} failed: {e}");
    }
    // A base64 `encoding` marker means the payload is binary — degrade to
    // "no text", never write a base64 blob into a knowledge entity.
    if result.get("encoding").and_then(Value::as_str) == Some("base64") {
        return Ok(None);
    }
    Ok(result
        .get("content")
        .and_then(Value::as_str)
        .map(String::from))
}

/// Convert Drive file metadata + text into a [`SourceItem`].
///
/// Why: Drive content DOES change in place, so unlike Gmail the fingerprint
/// must track revisions — `version` when Drive supplies it (it bumps on every
/// content edit), `modifiedTime` otherwise. An unchanged file therefore skips
/// and an edited one re-ingests.
/// Test: `drive_item_fingerprints_on_version`, `drive_item_falls_back_to_mtime`.
pub fn drive_item(file: &Value, content: String) -> Option<SourceItem> {
    let id = file.get("id").and_then(Value::as_str)?;
    let name = file
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(id)
        .to_string();
    let modified = file
        .get("modifiedTime")
        .and_then(Value::as_str)
        .map(String::from);
    // `volatile` is the load-bearing half of this. A constant fingerprint like
    // `unversioned:<id>` is INDISTINGUISHABLE from "unchanged" to the ledger, so
    // marking the item volatile is what actually delivers the intended
    // re-ingest — the string alone silently froze the entity at its first-seen
    // content forever (code-critic HIGH 3).
    let (fingerprint, volatile) = match file.get("version").and_then(|v| {
        v.as_str()
            .map(String::from)
            .or_else(|| v.as_i64().map(|n| n.to_string()))
    }) {
        Some(version) => (format!("version:{version}"), false),
        None => match &modified {
            Some(m) => (format!("modified:{m}"), false),
            // No revision signal at all: re-ingest every run rather than
            // pretending the file is unchanged.
            None => (format!("unversioned:{id}"), true),
        },
    };

    let mut fields = BTreeMap::new();
    fields.insert("drive_file_id".to_string(), id.to_string());
    fields.insert(
        "url".to_string(),
        format!("https://drive.google.com/file/d/{id}/view"),
    );
    if let Some(mime) = file.get("mimeType").and_then(Value::as_str) {
        fields.insert("mime_type".to_string(), mime.to_string());
    }

    Some(SourceItem {
        item_id: id.to_string(),
        fingerprint,
        name: name.clone(),
        title: name,
        timestamp: modified,
        body: content,
        fields,
        volatile,
    })
}
