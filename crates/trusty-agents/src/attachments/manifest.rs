//! The per-session sidecar that records what was stored (#7370).
//!
//! Why: the bytes on disk carry a name and nothing else — not the media type
//! the upload declared, not the id a chat turn references, not the digest that
//! proves the file is the one that was accepted. A sidecar is what makes
//! `[[attachment:<id>]]` in a persisted turn resolvable back to a card after a
//! reload. It is JSON at `attachments/<session>/manifest.json` rather than a
//! redb table because the assistant home is deliberately human-browsable (see
//! [`crate::assistants::home`]): a user opening that directory can read the
//! manifest in the same editor they use for `instructions.md`.
//!
//! What: [`Attachment`] — the in-memory row, carrying the ABSOLUTE
//! [`Attachment::stored_path`] callers need — and [`Manifest`], the on-disk
//! document, which stores only the file's NAME inside the session directory.
//! The home is the user's and they may move it; a manifest full of absolute
//! paths would break the moment they did, while a name resolves against
//! wherever the directory now lives.
//!
//! Concurrency: [`append`] takes an exclusive `fs4` advisory lock on the
//! manifest for the whole read-modify-write, the same mechanism
//! `crate::state_writer` and `crate::memory::code_store` use. Two uploads
//! racing on one session therefore serialize instead of one silently
//! overwriting the other's row.
//!
//! Test: `super::tests::manifest_tests`.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::error::AttachmentError;

/// The manifest file's name inside a session directory.
pub const MANIFEST_FILE: &str = "manifest.json";

/// Schema version written into every manifest.
const SCHEMA_VERSION: u32 = 1;

/// One stored attachment, as callers see it.
///
/// Why: `stored_path` is absolute because every consumer — the download route,
/// the model-input renderer, a test asserting where the file landed — needs a
/// path it can open, and re-deriving one from a relative name at each call site
/// is how two call sites end up disagreeing.
/// What: a plain value; construction goes through [`Manifest::append`] or
/// [`Manifest::rows`], never by hand outside this module's tests.
/// Test: `super::tests::store_tests::store_writes_the_file_at_the_exact_path`.
/// Deliberately NOT `Serialize`: `stored_path` is an absolute server path, and
/// a derive here is all it would take for a route to hand it to a browser.
/// Wire bodies are built field by field in `api::server::attachments`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// 32 lowercase hex characters, minted per upload.
    pub id: String,
    /// The chat session this attachment belongs to.
    pub session_id: String,
    /// The name the upload declared, after the single-segment guard.
    pub file_name: String,
    /// IANA media type, `application/octet-stream` when unknown.
    pub media_type: String,
    /// Byte length of the stored file.
    pub size: u64,
    /// Lowercase hex SHA-256 of the stored bytes.
    pub sha256: String,
    /// Absolute path the bytes were written to.
    pub stored_path: PathBuf,
}

/// One row as it is written to disk.
///
/// Why separate from [`Attachment`]: see the module doc — the file records a
/// NAME, never an absolute path, so the home stays movable.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Row {
    id: String,
    file_name: String,
    media_type: String,
    size: u64,
    sha256: String,
    /// The file's name inside the session directory. Equal to `file_name`
    /// except when a same-named file with different content already existed
    /// (see `super::store`).
    stored_name: String,
    created_at: String,
}

/// The on-disk document.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Document {
    version: u32,
    session_id: String,
    attachments: Vec<Row>,
}

/// One session's manifest, bound to the directory that holds it.
pub struct Manifest {
    session_dir: PathBuf,
    session_id: String,
}

impl Manifest {
    /// Bind a manifest to an EXISTING, already-confined session directory.
    ///
    /// Why: confinement is [`super::AttachmentStore`]'s job and is done before
    /// this type is constructed, so nothing here re-derives a path from
    /// untrusted text.
    /// Test: `super::tests::manifest_tests::append_then_rows_round_trips`.
    pub fn at(session_dir: impl Into<PathBuf>, session_id: impl Into<String>) -> Self {
        Self {
            session_dir: session_dir.into(),
            session_id: session_id.into(),
        }
    }

    /// The manifest file's path.
    pub fn path(&self) -> PathBuf {
        self.session_dir.join(MANIFEST_FILE)
    }

    /// Every recorded attachment, oldest first.
    ///
    /// Why: `Ok(vec![])` for a session with no manifest yet — an empty session
    /// and a broken one must not read the same, so a manifest that EXISTS but
    /// does not decode is [`AttachmentError::Manifest`], never an empty list.
    /// Test: `super::tests::manifest_tests::missing_manifest_reads_empty`,
    /// `super::tests::manifest_tests::corrupt_manifest_is_an_error`.
    pub fn rows(&self) -> Result<Vec<Attachment>, AttachmentError> {
        let path = self.path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(AttachmentError::Io { path, source }),
        };
        let document = self.decode(&raw, &path)?;
        document
            .attachments
            .into_iter()
            .map(|row| self.hydrate(row))
            .collect()
    }

    /// The row with this id, or [`AttachmentError::NotFound`].
    ///
    /// Test: `super::tests::store_tests::get_rejects_an_unknown_id`.
    pub fn get(&self, id: &str) -> Result<Attachment, AttachmentError> {
        self.rows()?
            .into_iter()
            .find(|row| row.id == id)
            .ok_or_else(|| AttachmentError::NotFound {
                id: id.to_string(),
                session: self.session_id.clone(),
            })
    }

    /// Take the session's exclusive write lock.
    ///
    /// Why: the lock has to cover MORE than the manifest's own read-modify-
    /// write. `AttachmentStore::store` chooses a file name from what is already
    /// on disk, writes the bytes, and then records the row — and all three have
    /// to be one critical section, or two concurrent uploads of the same name
    /// both choose the unsuffixed name and one of them records a digest for
    /// bytes that are not there. Handing the caller a guard is what lets the
    /// section span those three steps instead of only the last.
    /// What: opens (creating) the manifest and takes an `fs4` exclusive
    /// advisory lock on it, the mechanism `crate::state_writer` uses. The lock
    /// is released when the guard drops, on every path including a panic.
    /// Test: `super::tests::store_tests::concurrent_same_name_uploads_keep_their_bytes`.
    pub fn lock(&self) -> Result<ManifestGuard, AttachmentError> {
        let path = self.path();
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| AttachmentError::Io {
                path: path.clone(),
                source,
            })?;
        // fs4 1.0 renamed `lock_exclusive` to `lock`.
        fs4::FileExt::lock(&file).map_err(|source| AttachmentError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(ManifestGuard { file, path })
    }

    /// Record one attachment, taking the lock for the duration of the call.
    ///
    /// Why: the shorthand for a caller that has nothing else to do under the
    /// lock. `AttachmentStore::store` uses [`Self::lock`] +
    /// [`Self::append_with`] instead, because it does.
    /// Test: `super::tests::manifest_tests::append_then_rows_round_trips`,
    /// `super::tests::manifest_tests::append_is_idempotent_on_one_id`.
    #[allow(clippy::too_many_arguments)]
    pub fn append(
        &self,
        id: &str,
        file_name: &str,
        media_type: &str,
        size: u64,
        sha256: &str,
        stored_name: &str,
        created_at: &str,
    ) -> Result<Attachment, AttachmentError> {
        let mut guard = self.lock()?;
        self.append_with(
            &mut guard,
            id,
            file_name,
            media_type,
            size,
            sha256,
            stored_name,
            created_at,
        )
    }

    /// Record one attachment under a lock the caller already holds.
    ///
    /// Why/What: see [`Self::lock`]. An id already present REPLACES its row
    /// rather than duplicating it, so a retried write of the same id is
    /// idempotent.
    /// Test: `super::tests::manifest_tests::append_is_idempotent_on_one_id`.
    #[allow(clippy::too_many_arguments)]
    pub fn append_with(
        &self,
        guard: &mut ManifestGuard,
        id: &str,
        file_name: &str,
        media_type: &str,
        size: u64,
        sha256: &str,
        stored_name: &str,
        created_at: &str,
    ) -> Result<Attachment, AttachmentError> {
        let path = guard.path.clone();
        self.append_locked(
            &mut guard.file,
            &path,
            Row {
                id: id.to_string(),
                file_name: file_name.to_string(),
                media_type: media_type.to_string(),
                size,
                sha256: sha256.to_string(),
                stored_name: stored_name.to_string(),
                created_at: created_at.to_string(),
            },
        )
    }

    /// The body of an append, with the lock already held.
    fn append_locked(
        &self,
        file: &mut File,
        path: &Path,
        row: Row,
    ) -> Result<Attachment, AttachmentError> {
        let mut raw = String::new();
        file.read_to_string(&mut raw)
            .map_err(|source| AttachmentError::Io {
                path: path.to_path_buf(),
                source,
            })?;
        let mut document = self.decode(&raw, path)?;
        document
            .attachments
            .retain(|existing| existing.id != row.id);
        document.attachments.push(row.clone());
        let encoded = serde_json::to_string_pretty(&document).map_err(|source| {
            AttachmentError::Manifest {
                path: path.to_path_buf(),
                source,
            }
        })?;
        let io = |source| AttachmentError::Io {
            path: path.to_path_buf(),
            source,
        };
        file.seek(SeekFrom::Start(0)).map_err(io)?;
        file.set_len(0).map_err(io)?;
        file.write_all(encoded.as_bytes()).map_err(io)?;
        file.write_all(b"\n").map_err(io)?;
        file.flush().map_err(io)?;
        self.hydrate(row)
    }

    /// Decode a manifest body, treating an EMPTY file as a fresh session.
    ///
    /// Why: [`Self::append`] creates the file before it knows whether one
    /// existed, so a first write always sees zero bytes. That is not corruption
    /// — but a non-empty body that fails to parse is, and is reported.
    fn decode(&self, raw: &str, path: &Path) -> Result<Document, AttachmentError> {
        if raw.trim().is_empty() {
            return Ok(Document {
                version: SCHEMA_VERSION,
                session_id: self.session_id.clone(),
                attachments: Vec::new(),
            });
        }
        serde_json::from_str(raw).map_err(|source| AttachmentError::Manifest {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Resolve a stored row's name against this session's directory.
    ///
    /// Why: this is the confinement for a manifest that has been EDITED. The
    /// file is plain JSON in the user's own home tree, so its contents are not
    /// this crate's word — and `Path::join` replaces the base when handed an
    /// absolute component, so an unchecked `stored_name` of `/etc/passwd`
    /// resolves to `/etc/passwd` rather than to something under the session.
    /// The row is REFUSED, never repaired: a manifest that has been tampered
    /// with must surface, not silently read a neighbouring file instead.
    /// What: `stored_name` must be one ordinary path segment
    /// ([`super::store::single_segment`], the same guard the upload applies to
    /// the name it accepts). `AttachmentStore::read` then re-checks the
    /// canonical result, which is what catches a symlink the name check cannot
    /// see.
    /// Test: `super::tests::manifest_tests::an_absolute_stored_name_is_refused`,
    /// `super::tests::manifest_tests::a_traversal_stored_name_is_refused`.
    fn hydrate(&self, row: Row) -> Result<Attachment, AttachmentError> {
        let stored_name = super::store::single_segment(&row.stored_name).map_err(|reason| {
            AttachmentError::TamperedManifest {
                path: self.path(),
                id: row.id.clone(),
                reason: format!(
                    "its stored name `{}` is rejected: {reason}",
                    row.stored_name
                ),
            }
        })?;
        Ok(Attachment {
            id: row.id,
            session_id: self.session_id.clone(),
            file_name: row.file_name,
            media_type: row.media_type,
            size: row.size,
            sha256: row.sha256,
            stored_path: self.session_dir.join(stored_name),
        })
    }
}

/// An exclusive advisory lock on one session's manifest.
///
/// Why: a guard rather than a scoped closure, so the critical section can span
/// the caller's own filesystem work (see [`Manifest::lock`]). Unlocking on
/// `Drop` is what makes every exit path — early `?`, panic, ordinary return —
/// release it.
pub struct ManifestGuard {
    file: File,
    path: PathBuf,
}

impl Drop for ManifestGuard {
    fn drop(&mut self) {
        let _ = fs4::FileExt::unlock(&self.file);
    }
}
