//! Document versions: what the server has been told about each file (#9078).
//!
//! Why: DOC-72 §3 stamps every answer with the document version it was
//! computed against, and refuses a caller who names another one. That only
//! means something if analyze is the one party that sends file text, and
//! versions move exactly when the text does.
//! What: [`LspClient::sync_document`] reads the file from disk at call time. The
//! first sync sends `textDocument/didOpen` at version 1; a later sync sends
//! `textDocument/didChange` (full text) at version + 1 only when the content
//! hash differs, and sends nothing otherwise. [`check_version`] is the `Stale`
//! rule.
//! Test: `documents_open_at_version_one_and_bump_only_on_change`,
//! `syncing_a_missing_file_is_a_document_error`,
//! `a_version_other_than_the_current_one_is_stale`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::json;
use xxhash_rust::xxh3::xxh3_64;

use super::client::LspClient;
use super::LspError;

#[derive(Debug, Clone, Copy)]
struct DocState {
    version: i64,
    hash: u64,
}

/// Per-client table of open documents, keyed by canonical path.
#[derive(Debug, Default)]
pub(crate) struct DocumentSet {
    docs: tokio::sync::Mutex<HashMap<PathBuf, DocState>>,
}

/// The result of one [`LspClient::sync_document`] call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedDocument {
    /// The `file://` URI the server knows the document by.
    pub uri: String,
    /// The version the server now holds.
    pub version: i64,
    /// Whether this call sent `didOpen` or `didChange`.
    pub changed: bool,
}

impl LspClient {
    /// Bring the server's copy of `path` up to date with the file on disk.
    ///
    /// Why/What: see the module doc. The table lock is held across the send so
    /// two concurrent syncs of one file cannot reorder versions.
    ///
    /// # Errors
    /// [`LspError::Document`] when the file cannot be read, is not UTF-8, or has
    /// no `file://` form; the write or close error when the notification fails.
    ///
    /// Test: `documents_open_at_version_one_and_bump_only_on_change`.
    pub async fn sync_document(
        &self,
        path: &Path,
        language_id: &str,
    ) -> Result<SyncedDocument, LspError> {
        let path = tokio::fs::canonicalize(path)
            .await
            .map_err(|e| doc_error(path, e.to_string()))?;
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| doc_error(&path, e.to_string()))?;
        let hash = xxh3_64(&bytes);
        let text = String::from_utf8(bytes).map_err(|_| doc_error(&path, "not UTF-8".into()))?;
        let uri = file_uri(&path)?;

        let mut docs = self.documents.docs.lock().await;
        let (version, method, params) = match docs.get(&path) {
            Some(s) if s.hash == hash => {
                return Ok(SyncedDocument {
                    uri,
                    version: s.version,
                    changed: false,
                })
            }
            Some(s) => {
                let v = s.version + 1;
                let params = json!({"textDocument": {"uri": uri, "version": v},
                                    "contentChanges": [{"text": text}]});
                (v, "textDocument/didChange", params)
            }
            None => {
                let params = json!({"textDocument": {"uri": uri, "languageId": language_id,
                                                     "version": 1, "text": text}});
                (1, "textDocument/didOpen", params)
            }
        };
        self.notify(method, params).await?;
        docs.insert(path, DocState { version, hash });
        Ok(SyncedDocument {
            uri,
            version,
            changed: true,
        })
    }

    /// The version the server holds for `path`, if it was ever synced.
    pub async fn document_version(&self, path: &Path) -> Option<i64> {
        let path = tokio::fs::canonicalize(path).await.ok()?;
        self.documents
            .docs
            .lock()
            .await
            .get(&path)
            .map(|s| s.version)
    }
}

/// Refuse any `requested` version other than the server's `current` one.
///
/// A higher number is refused too: versions restart at 1 when a server is
/// respawned, so it names a dead server's state, not a future one.
///
/// # Errors
/// [`LspError::Stale`] when the two differ.
///
/// Test: `a_version_other_than_the_current_one_is_stale`.
pub fn check_version(requested: i64, current: i64) -> Result<(), LspError> {
    if requested == current {
        Ok(())
    } else {
        Err(LspError::Stale { requested, current })
    }
}

/// The `file://` URI for an absolute path.
///
/// # Errors
/// [`LspError::Document`] for a relative path.
pub fn file_uri(path: &Path) -> Result<String, LspError> {
    reqwest::Url::from_file_path(path)
        .map(String::from)
        .map_err(|()| doc_error(path, "not an absolute path".into()))
}

fn doc_error(path: &Path, reason: String) -> LspError {
    LspError::Document {
        path: path.display().to_string(),
        reason,
    }
}
