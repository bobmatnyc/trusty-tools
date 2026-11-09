//! Google Workspace adapters — Gmail/Drive responses into OKG [`SourceItem`]s.
//!
//! Why: Gmail and Drive ingestion MUST reuse `trusty-gworkspace`'s authenticated
//! `BaseClient` (token storage, silent refresh, no browser) rather than adding a
//! second OAuth path. #10746 S13: the listing, fetch and body-decoding plumbing
//! moved into `trusty_gworkspace::api::bulk`, so the trusty-okf import adapters
//! (`trusty-connectors`) reuse it without linking this crate; it is re-exported
//! here unchanged, so the tool call sites keep their `gapi::` paths.
//!
//! What: the OKG half that stays here — [`message_to_item`] and [`drive_item`]
//! convert API JSON into [`SourceItem`]s with no I/O.
//!
//! Test: see `super::tests` — `message_to_item_*`, `drive_item_*`; the moved
//! plumbing is tested in `trusty-gworkspace` (`api::bulk::tests`).

use std::collections::BTreeMap;

use serde_json::Value;
use trusty_gworkspace::api::bulk::{decode_body, message_header};
pub use trusty_gworkspace::api::bulk::{
    drive_content, drive_list_files, gmail_list_ids, gmail_message, gmail_query,
};
use trusty_okf::build::ingest::SourceItem;

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
    let header = |name: &str| message_header(msg, name);

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
        // #10746: the connector supplies no structured frontmatter.
        frontmatter: Default::default(),
        // A Gmail message is immutable, so its id is a perfect change signal —
        // never volatile, which is what makes "pull exactly once, ever" hold.
        volatile: false,
    })
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
        // #10746: the connector supplies no structured frontmatter.
        frontmatter: Default::default(),
        volatile,
    })
}
