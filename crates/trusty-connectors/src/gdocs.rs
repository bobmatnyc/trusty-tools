//! The `gdocs` adapter: native Google Docs through Drive (SPEC-OKF-01
//! §2.15 items 3, 7, 8; Appendix C "Google Docs").
//!
//! Why: a Docs folder can be imported with deletion detection only when the
//! listing is recursive and whole, so the adapter must report every page
//! token it leaves, and a native Doc must be exported, never downloaded.
//! What: every request is refused `policy_refused` unless the grant file
//! records a passed capability-grant review for its store and account
//! ([`authorise`], #10746). The selection is `account`, then either `folder_id` (with
//! `recursive`) or `document_ids`, never both, and `max_items` (the
//! adapter's ceiling, default 5,000). A folder `list` walks one Drive page
//! (`trashed = false`, so a trashed Doc is absent; #10746: the first page
//! fails unless the root is a readable, untrashed folder) and hands the
//! `DriveWalk` back as the cursor `w1:<json>`; a native Doc is an entry
//! with its `version`, else `modifiedTime`, as the revision; any other file
//! is an entry skipped `unsupported_format`. A `document_ids` list reads
//! each file's metadata, 100 per page, cursor `i<offset>`; a missing file is
//! an entry the fetch then fails. At the ceiling the cursor is returned
//! with `complete: false`, as in [`crate::gmail`]. A `fetch` exports
//! `text/markdown`, else `text/html`; a binary export arrives as a base64
//! part (the host reports it `unsupported_format`); embedded images are
//! dropped with a note ([`drop_embedded_images`]).
//! Test: `gdocs::tests`, `c91_*`, `c94_*`, `c132_*`,
//! `gdocs_needs_a_capability_grant_before_any_drive_request`
//! (`tests/conformance.rs`).
//!
//! # Spec References
//! - SPEC-OKF-01 §2.15 items 3, 7, 8, "Native-Doc export"; Appendix B
//!   "Acquire role"; Appendix C.

use std::collections::BTreeMap;
use std::ffi::OsString;

use serde_json::{Map, Value};
use trusty_gworkspace::api::bulk::{DriveWalk, PAGE_SIZE, drive_export, drive_file};
use trusty_okf::import::record::{Part, SourceRecord};

use crate::config::Config;
use crate::google::{Google, account, ceiling, fault, run_main, text};
use crate::server::{Adapter, Entry, Fault, Page, Request};

/// The MIME type of a native Google Doc.
pub const DOC_MIME: &str = "application/vnd.google-apps.document";

/// The export order (Appendix C "Structure").
pub const EXPORTS: [&str; 2] = ["text/markdown", "text/html"];

/// The prefix of a folder-walk cursor.
const WALK: &str = "w1:";

/// The `okf-adapter-gdocs` entry point; returns the exit code.
pub fn main(args: &[OsString]) -> i32 {
    // #10746: the grant file reaches the adapter, which enforces it.
    run_main(args, Docs::new)
}

/// Refuse a request no passed capability-grant review covers (§2.15
/// "Native-Doc export", item 10 B; Appendix B "Acquire role": the adapter
/// enforces the check). The semantics of `gmail::authorise` for inbound
/// mail: an unusable grant file, or no row for exactly this store and
/// account, is `policy_refused`.
pub fn authorise(
    req: &Request,
    account: &str,
    config: &Result<Config, String>,
) -> Result<(), Fault> {
    let refuse = |why: String| Err(Fault::new("policy_refused", why));
    match config {
        Err(e) => refuse(format!(
            "native Doc export refused: the grant file is unusable: {e}"
        )),
        Ok(c) if !c.gdocs_granted(&req.store, account) => refuse(format!(
            "native Doc export refused: no passed capability-grant review for store {} account {account}",
            req.store
        )),
        Ok(_) => Ok(()),
    }
}

/// What one request lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A folder, walked recursively or not.
    Folder {
        /// The folder ID.
        id: String,
        /// Whether subfolders are walked.
        recursive: bool,
    },
    /// Named documents.
    Documents(Vec<String>),
}

/// Read a request's selection: `(account, source, ceiling)`. Errors:
/// `selection_required`, `selection_invalid`.
pub fn selection(sel: &Map<String, Value>) -> Result<(String, Source, usize), Fault> {
    let invalid = |m: &str| Fault::new("selection_invalid", m.to_string());
    let folder = text(sel, "folder_id")?;
    let docs = match sel.get("document_ids") {
        None | Some(Value::Null) => None,
        Some(Value::Array(a)) => Some(
            a.iter()
                .map(|v| v.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .filter(|v| !v.is_empty() && v.iter().all(|s| !s.is_empty()))
                .ok_or_else(|| invalid("document_ids must be non-empty strings"))?,
        ),
        Some(_) => return Err(invalid("document_ids is not a list")),
    };
    let recursive = match sel.get("recursive") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(invalid("recursive is not a boolean")),
    };
    let source = match (folder, docs) {
        (Some(_), Some(_)) => return Err(invalid("name folder_id or document_ids, not both")),
        (Some(id), None) => Source::Folder { id, recursive },
        (None, Some(ids)) => Source::Documents(ids),
        (None, None) => {
            return Err(Fault::new(
                "selection_required",
                "the selection names no folder_id or document_ids",
            ));
        }
    };
    Ok((account(sel)?, source, ceiling(sel)?))
}

fn str_of(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| {
        x.as_str()
            .map(str::to_string)
            .or_else(|| x.as_i64().map(|n| n.to_string()))
    })
}

/// The listing entry of one Drive file's metadata.
pub fn file_entry(file: &Value) -> Option<Entry> {
    let id = str_of(file, "id")?;
    let doc = file.get("mimeType").and_then(Value::as_str) == Some(DOC_MIME);
    Some(Entry {
        item_id: id,
        revision: str_of(file, "version").or_else(|| str_of(file, "modifiedTime")),
        timestamp: str_of(file, "modifiedTime"),
        skip: (!doc).then(|| "unsupported_format".to_string()),
        deleted: file.get("trashed").and_then(Value::as_bool) == Some(true),
        group: None,
    })
}

/// Drop embedded (data-URI) images from a Markdown or HTML export; returns
/// the text and how many were dropped (Appendix C "Attachments").
pub fn drop_embedded_images(text: &str, mime: &str) -> (String, usize) {
    let mut dropped = 0;
    if mime == "text/html" {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(at) = rest.find("<img") {
            out.push_str(&rest[..at]);
            match rest[at..].find('>') {
                Some(end) => {
                    dropped += 1;
                    rest = &rest[at + end + 1..];
                }
                None => {
                    rest = &rest[at..];
                    break;
                }
            }
        }
        out.push_str(rest);
        return (out, dropped);
    }
    let mut out = Vec::new();
    for line in text.split_inclusive('\n') {
        // A reference definition: `[image1]: <data:image/png;base64,...>`.
        let t = line.trim_start();
        if t.starts_with('[') && (t.contains("]: <data:") || t.contains("]: data:")) {
            dropped += 1;
            continue;
        }
        let mut l = line.to_string();
        // An inline image: `![alt](data:...)`.
        while let Some(at) = l.find("](data:") {
            let end = l[at..].find(')').map_or(l.len(), |e| at + e + 1);
            l.replace_range(at..end, "]()");
            dropped += 1;
        }
        out.push(l);
    }
    (out.concat(), dropped)
}

/// The record of a Doc from its metadata and one export reply.
pub fn doc_record(meta: &Value, mime: &str, export: &Value) -> SourceRecord {
    let id = str_of(meta, "id").unwrap_or_default();
    let mut notes = Vec::new();
    let base64 = export.get("encoding").and_then(Value::as_str) == Some("base64");
    let content = str_of(export, "content").unwrap_or_default();
    let data = if base64 {
        content
    } else {
        let (text, n) = drop_embedded_images(&content, mime);
        if n > 0 {
            notes.push(format!("{n} embedded image(s) dropped"));
        }
        text
    };
    let mut dates = BTreeMap::new();
    for (k, src) in [
        ("modified_time", "modifiedTime"),
        ("created_time", "createdTime"),
    ] {
        if let Some(v) = str_of(meta, src) {
            dates.insert(k.to_string(), v);
        }
    }
    let mut fields = Map::new();
    fields.insert(
        "url".into(),
        Value::String(format!("https://docs.google.com/document/d/{id}/edit")),
    );
    fields.insert("mime_type".into(), Value::String(DOC_MIME.into()));
    SourceRecord {
        revision: str_of(meta, "version").or_else(|| str_of(meta, "modifiedTime")),
        title: str_of(meta, "name"),
        item_id: id,
        dates,
        parts: vec![Part {
            mime: mime.to_string(),
            encoding: base64.then(|| "base64".to_string()),
            data,
        }],
        notes,
        fields,
        ..SourceRecord::default()
    }
}

/// The `gdocs` adapter of one process.
pub struct Docs {
    google: Google,
    /// #10746: the grant file ([`authorise`]).
    config: Result<Config, String>,
    /// Entries listed so far in this process (the ceiling's count).
    listed: usize,
}

impl Docs {
    /// An adapter over `google` with the grant file `config`.
    pub fn new(google: Google, config: Result<Config, String>) -> Docs {
        Docs {
            google,
            config,
            listed: 0,
        }
    }

    /// The page that stops at the ceiling with `cursor` still left.
    fn at_ceiling(cursor: Option<String>) -> Page {
        Page {
            entries: Vec::new(),
            cursor,
            complete: false,
        }
    }

    async fn list_folder(
        &mut self,
        account: &str,
        start: (String, bool),
        cursor: Option<&str>,
        room: usize,
        ceiling: usize,
    ) -> Result<Page, Fault> {
        let mut walk = match cursor {
            None => DriveWalk::new(&start.0, start.1),
            Some(c) => c
                .strip_prefix(WALK)
                .and_then(|j| serde_json::from_str(j).ok())
                .ok_or_else(|| Fault::new("protocol_error", "not a gdocs cursor"))?,
        };
        let files = walk
            .next_page(&self.google.client, Some(account), room)
            .await
            .map_err(|e| fault(&e))?
            .unwrap_or_default();
        let entries: Vec<Entry> = files.iter().filter_map(file_entry).collect();
        self.listed += entries.len();
        let next = if walk.is_done() {
            None
        } else {
            let state = serde_json::to_string(&walk)
                .map_err(|e| Fault::new("fetch_error", e.to_string()))?;
            Some(format!("{WALK}{state}"))
        };
        let cut = self.listed >= ceiling && next.is_some();
        Ok(Page {
            entries,
            cursor: next,
            complete: !cut,
        })
    }

    async fn list_documents(
        &mut self,
        account: &str,
        ids: &[String],
        cursor: Option<&str>,
        room: usize,
        ceiling: usize,
    ) -> Result<Page, Fault> {
        let from = match cursor {
            None => 0,
            Some(c) => c
                .strip_prefix('i')
                .and_then(|n| n.parse::<usize>().ok())
                .filter(|n| *n <= ids.len())
                .ok_or_else(|| Fault::new("protocol_error", "not a gdocs cursor"))?,
        };
        let to = ids.len().min(from + room);
        let mut entries = Vec::new();
        for id in &ids[from..to] {
            match drive_file(&self.google.client, Some(account), id).await {
                Ok(meta) => entries.push(file_entry(&meta).ok_or_else(|| {
                    Fault::new("protocol_error", format!("metadata for {id} has no id"))
                })?),
                Err(e) => {
                    let f = fault(&e);
                    // #10746 S13: a credential failure fails the run; a
                    // missing or unreadable document fails at its fetch.
                    if f.code == "auth_required" {
                        return Err(f);
                    }
                    entries.push(Entry {
                        item_id: id.clone(),
                        ..Entry::default()
                    });
                }
            }
        }
        self.listed += entries.len();
        let next = (to < ids.len()).then(|| format!("i{to}"));
        let cut = self.listed >= ceiling && next.is_some();
        Ok(Page {
            entries,
            cursor: next,
            complete: !cut,
        })
    }
}

impl Adapter for Docs {
    const KIND: &'static str = "gdocs";

    async fn list(&mut self, req: &Request) -> Result<Page, Fault> {
        let (account, source, ceiling) = selection(&req.selection)?;
        // #10746: no Drive call before the capability-grant check.
        authorise(req, &account, &self.config)?;
        if self.listed >= ceiling {
            return Ok(Docs::at_ceiling(req.cursor.clone()));
        }
        let room = PAGE_SIZE.min(ceiling - self.listed);
        let cursor = req.cursor.as_deref();
        match source {
            Source::Folder { id, recursive } => {
                self.list_folder(&account, (id, recursive), cursor, room, ceiling)
                    .await
            }
            Source::Documents(ids) => {
                self.list_documents(&account, &ids, cursor, room, ceiling)
                    .await
            }
        }
    }

    async fn fetch(&mut self, req: &Request) -> Result<SourceRecord, Fault> {
        let (account, _, _) = selection(&req.selection)?;
        // #10746: no Drive call before the capability-grant check.
        authorise(req, &account, &self.config)?;
        let id = req
            .item_id
            .as_deref()
            .ok_or_else(|| Fault::new("protocol_error", "fetch names no item_id"))?;
        let client = &self.google.client;
        let meta = drive_file(client, Some(&account), id)
            .await
            .map_err(|e| fault(&e))?;
        if str_of(&meta, "id").as_deref() != Some(id) {
            return Err(Fault::new(
                "protocol_error",
                format!("asked for {id}, got another file"),
            ));
        }
        let mime = meta.get("mimeType").and_then(Value::as_str).unwrap_or("");
        if mime != DOC_MIME {
            return Err(Fault::new(
                "unsupported_format",
                format!("{id} is {mime}, not a native Google Doc"),
            ));
        }
        let mut last = None;
        for export in EXPORTS {
            match drive_export(client, Some(&account), id, Some(export)).await {
                Ok(v) => return Ok(doc_record(&meta, export, &v)),
                Err(e) => last = Some(fault(&e)),
            }
        }
        let f = last.unwrap_or_else(|| Fault::new("fetch_error", "no export"));
        Err(Fault::new("export_failed", f.message))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sel(v: Value) -> Result<(String, Source, usize), Fault> {
        selection(v.as_object().unwrap())
    }

    #[test]
    fn selection_names_one_source() {
        let (a, s, c) = sel(json!({"account": "w", "folder_id": "F", "recursive": true})).unwrap();
        assert_eq!((a.as_str(), c), ("w", 5000));
        assert_eq!(
            s,
            Source::Folder {
                id: "F".into(),
                recursive: true
            }
        );
        let (_, s, _) = sel(json!({"account": "w", "document_ids": ["a", "b"]})).unwrap();
        assert_eq!(s, Source::Documents(vec!["a".into(), "b".into()]));
        let code = |v| sel(v).unwrap_err().code;
        assert_eq!(code(json!({"account": "w"})), "selection_required");
        assert_eq!(code(json!({"folder_id": "F"})), "selection_required");
        assert_eq!(
            code(json!({"account": "w", "folder_id": "F", "document_ids": ["a"]})),
            "selection_invalid"
        );
        assert_eq!(
            code(json!({"account": "w", "document_ids": []})),
            "selection_invalid"
        );
        assert_eq!(
            code(json!({"account": "w", "folder_id": "F", "recursive": "yes"})),
            "selection_invalid"
        );
    }

    #[test]
    fn entries_revise_on_version_and_skip_non_docs() {
        let e = file_entry(&json!({"id": "d", "mimeType": DOC_MIME, "version": "7",
                                    "modifiedTime": "2026-01-01T00:00:00Z"}))
        .unwrap();
        assert_eq!(
            (e.revision.as_deref(), e.skip.as_deref()),
            (Some("7"), None)
        );
        let e = file_entry(&json!({"id": "p", "mimeType": "application/pdf",
                                    "modifiedTime": "m"}))
        .unwrap();
        assert_eq!(
            (e.revision.as_deref(), e.skip.as_deref()),
            (Some("m"), Some("unsupported_format"))
        );
        assert!(file_entry(&json!({"mimeType": DOC_MIME})).is_none());
    }

    #[test]
    fn embedded_images_are_dropped_with_a_count() {
        let md = "# T\n\n![a][image1] and ![b](data:image/png;base64,QQ==) [l](https://x)\n\n\
                  [image1]: <data:image/png;base64,AAAA>\n";
        let (out, n) = drop_embedded_images(md, "text/markdown");
        assert_eq!(n, 2);
        assert!(!out.contains("data:"), "{out}");
        assert!(out.contains("[l](https://x)"));
        let (out, n) = drop_embedded_images("<p>a<img src=\"data:x\">b</p>", "text/html");
        assert_eq!((out.as_str(), n), ("<p>ab</p>", 1));
    }

    #[test]
    fn doc_record_keeps_dates_and_marks_base64() {
        let meta = json!({"id": "d", "name": "Plan", "mimeType": DOC_MIME, "version": 3,
                          "modifiedTime": "2026-02-01T00:00:00Z",
                          "createdTime": "2026-01-01T00:00:00Z"});
        let r = doc_record(&meta, "text/markdown", &json!({"content": "# Plan\n"}));
        assert_eq!(
            (r.revision.as_deref(), r.title.as_deref()),
            (Some("3"), Some("Plan"))
        );
        assert_eq!(r.dates["created_time"], "2026-01-01T00:00:00Z");
        assert_eq!(r.parts[0].encoding, None);
        let r = doc_record(
            &meta,
            "text/markdown",
            &json!({"content": "AAAA", "encoding": "base64"}),
        );
        assert_eq!(r.parts[0].encoding.as_deref(), Some("base64"));
    }
}
