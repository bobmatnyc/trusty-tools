//! The attachments directory, and every guard that stands in front of it (#7370).
//!
//! Why: an upload names its own file, and a chat client names its own session.
//! Both strings are attacker-reachable, and both are used to BUILD A PATH. The
//! contract here is the one
//! [`crate::assistants::AssistantHome::store_root`] already holds for store
//! roots: a name that is not a single ordinary path segment is REFUSED, never
//! repaired. Substituting a safe name would write the user's file somewhere
//! they will never look for it while reporting success — worse than the
//! refusal, because nothing surfaces.
//!
//! What: [`AttachmentStore`] resolves `<root>/<session>/<file>`, writes the
//! bytes, digests them, and records the row through
//! [`super::manifest::Manifest`]. Reads go back through the manifest, never
//! through a path the caller supplied — an id is looked up, and the path comes
//! from the row. That is what keeps the download route from being a static
//! file mount with a traversal hole in it.
//!
//! Size: [`MAX_ATTACHMENT_BYTES`] is checked BEFORE anything is created, so a
//! rejected upload leaves no partial file behind. The HTTP layer additionally
//! bounds the request body, so an oversize payload is refused twice — once by
//! the transport and once here, where the constant lives.
//!
//! Test: `super::tests::store_tests`.

use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

use super::error::AttachmentError;
use super::manifest::{Attachment, Manifest};
use crate::assistants::AssistantHome;

/// Largest attachment this store accepts, in bytes (10 MiB).
///
/// Why: an assistant home is the user's own directory, and a chat thread is
/// not a file server — the cap is sized for the documents, spreadsheets, logs
/// and screenshots a conversation actually carries, not for archives. It is
/// named here so the HTTP body limit can be derived from it rather than
/// drifting from it.
/// Test: `super::tests::store_tests::store_rejects_an_oversize_payload`.
pub const MAX_ATTACHMENT_BYTES: u64 = 10 * 1024 * 1024;

/// The media type used when nothing better can be determined.
pub const FALLBACK_MEDIA_TYPE: &str = "application/octet-stream";

/// One assistant's attachments tree.
pub struct AttachmentStore {
    root: PathBuf,
    max_bytes: u64,
}

impl AttachmentStore {
    /// A store rooted at an attachments directory.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_bytes: MAX_ATTACHMENT_BYTES,
        }
    }

    /// The store for an assistant's home — `<home>/attachments`.
    ///
    /// Why: the path is [`AssistantHome::attachments_dir`]'s to own, so this
    /// never rebuilds it from the constant.
    pub fn for_home(home: &AssistantHome) -> Self {
        Self::new(home.attachments_dir())
    }

    /// Override the size cap. Tests only — production uses
    /// [`MAX_ATTACHMENT_BYTES`].
    #[cfg(test)]
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// The attachments root this store writes under.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `<root>/<session>`, refusing anything that is not one path segment.
    ///
    /// Why: see the module doc — refuse, never repair.
    /// Test: `super::tests::store_tests::session_traversal_is_refused`.
    pub fn session_dir(&self, session_id: &str) -> Result<PathBuf, AttachmentError> {
        let segment =
            single_segment(session_id).map_err(|reason| AttachmentError::UnsafeSessionId {
                session: session_id.to_string(),
                reason,
            })?;
        Ok(self.root.join(segment))
    }

    /// Store `bytes` for `session_id` under `file_name`.
    ///
    /// Why: this is the one write path. Every guard runs before a directory or
    /// a file is created, so a refusal leaves the tree exactly as it was —
    /// requirement 2 of #7370 ("no partial file left").
    /// What: validates the session id and the file name, enforces
    /// [`MAX_ATTACHMENT_BYTES`], resolves the media type (see
    /// [`resolve_media_type`]), writes the bytes, and records the row. A file
    /// of that name already holding IDENTICAL bytes is reused rather than
    /// rewritten; one holding different bytes is left alone and the new file
    /// lands at `<stem>.<id prefix><ext>` — never clobbered, never silently
    /// merged.
    /// Test: `super::tests::store_tests::store_writes_the_file_at_the_exact_path`,
    /// `super::tests::store_tests::traversal_file_name_writes_nothing`,
    /// `super::tests::store_tests::store_rejects_an_oversize_payload`,
    /// `super::tests::store_tests::same_name_different_bytes_does_not_clobber`.
    pub fn store(
        &self,
        session_id: &str,
        file_name: &str,
        declared_media_type: Option<&str>,
        bytes: &[u8],
    ) -> Result<Attachment, AttachmentError> {
        let session_dir = self.session_dir(session_id)?;
        let name = single_segment(file_name).map_err(|reason| AttachmentError::UnsafeFileName {
            name: file_name.to_string(),
            reason,
        })?;
        let size = bytes.len() as u64;
        if size > self.max_bytes {
            return Err(AttachmentError::TooLarge {
                name: name.clone(),
                size,
                cap: self.max_bytes,
            });
        }

        let id = uuid::Uuid::new_v4().simple().to_string();
        let digest = hex_digest(bytes);
        let media_type = resolve_media_type(&name, declared_media_type);

        std::fs::create_dir_all(&session_dir).map_err(|source| AttachmentError::Io {
            path: session_dir.clone(),
            source,
        })?;
        let stored_name = self.reserve_name(&session_dir, &name, &id, bytes, &digest)?;
        let target = session_dir.join(&stored_name);
        if !target.exists() {
            std::fs::write(&target, bytes).map_err(|source| AttachmentError::Io {
                path: target.clone(),
                source,
            })?;
        }

        Manifest::at(&session_dir, session_id).append(
            &id,
            &name,
            &media_type,
            size,
            &digest,
            &stored_name,
            &chrono::Utc::now().to_rfc3339(),
        )
    }

    /// Every attachment recorded for a session, oldest first.
    ///
    /// Test: `super::tests::store_tests::list_returns_what_was_stored`.
    pub fn list(&self, session_id: &str) -> Result<Vec<Attachment>, AttachmentError> {
        let session_dir = self.session_dir(session_id)?;
        Manifest::at(&session_dir, session_id).rows()
    }

    /// One attachment's row, by id.
    ///
    /// Why: the id is validated for SHAPE before it is compared, so a
    /// path-shaped id is refused as an id rather than searched for.
    /// Test: `super::tests::store_tests::get_rejects_an_unknown_id`.
    pub fn get(&self, session_id: &str, id: &str) -> Result<Attachment, AttachmentError> {
        if !is_attachment_id(id) {
            return Err(AttachmentError::InvalidId(id.to_string()));
        }
        let session_dir = self.session_dir(session_id)?;
        Manifest::at(&session_dir, session_id).get(id)
    }

    /// One attachment's row AND its bytes.
    ///
    /// Why: the path opened is the manifest's, never the caller's — the
    /// property that keeps the download route from being a file mount.
    /// Test: `super::tests::store_tests::read_returns_the_stored_bytes`.
    pub fn read(
        &self,
        session_id: &str,
        id: &str,
    ) -> Result<(Attachment, Vec<u8>), AttachmentError> {
        let row = self.get(session_id, id)?;
        let bytes = match std::fs::read(&row.stored_path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(AttachmentError::MissingFile {
                    id: row.id,
                    path: row.stored_path,
                });
            }
            Err(source) => {
                return Err(AttachmentError::Io {
                    path: row.stored_path,
                    source,
                });
            }
        };
        Ok((row, bytes))
    }

    /// Pick the name the bytes will live under.
    ///
    /// Why: two uploads can name the same file. Overwriting the first would
    /// silently change what an earlier turn's marker resolves to, which is the
    /// same class of defect as a renamed target: a reference that now points at
    /// content nobody agreed to. Identical bytes are the one safe case — the
    /// file already IS the upload — so they are reused.
    /// What: `name` when free or already byte-identical; otherwise
    /// `<stem>.<first 8 of id><ext>`.
    fn reserve_name(
        &self,
        session_dir: &Path,
        name: &str,
        id: &str,
        bytes: &[u8],
        digest: &str,
    ) -> Result<String, AttachmentError> {
        let target = session_dir.join(name);
        match std::fs::read(&target) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(name.to_string()),
            Err(source) => {
                return Err(AttachmentError::Io {
                    path: target,
                    source,
                });
            }
            Ok(existing) => {
                if existing.len() == bytes.len() && hex_digest(&existing) == digest {
                    return Ok(name.to_string());
                }
            }
        }
        let short = &id[..8];
        let path = Path::new(name);
        Ok(match (path.file_stem(), path.extension()) {
            (Some(stem), Some(ext)) => format!(
                "{}.{short}.{}",
                stem.to_string_lossy(),
                ext.to_string_lossy()
            ),
            _ => format!("{name}.{short}"),
        })
    }
}

/// Lowercase hex SHA-256 of `bytes`.
fn hex_digest(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// Whether `id` has the shape this module mints — 32 lowercase hex characters.
///
/// Test: `super::tests::store_tests::get_rejects_an_unknown_id`.
pub fn is_attachment_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Accept `raw` only if it is exactly one ordinary path segment.
///
/// Why: this is the whole traversal guard. It rejects rather than sanitizes —
/// see the module doc — and it rejects on the RAW string as well as on the
/// parsed components, because a name can be a single component on one platform
/// and a separator on another (`a\b` is one segment on unix, two on Windows).
/// What: `Err(reason)` for an empty name, one containing a `/`, `\`, NUL or any
/// other control character, for `.` and `..`, and for anything whose
/// [`Path::components`] is not exactly one [`Component::Normal`] equal to the
/// input. `Ok(name)` otherwise.
/// Test: `super::tests::store_tests::traversal_file_name_writes_nothing`,
/// `super::tests::store_tests::session_traversal_is_refused`,
/// `super::tests::store_tests::absolute_file_name_is_refused`.
fn single_segment(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("it is empty".to_string());
    }
    if trimmed.len() > 255 {
        return Err("it is longer than 255 bytes".to_string());
    }
    if trimmed.contains(['/', '\\']) {
        return Err("it contains a path separator; a name must be one path segment".to_string());
    }
    if trimmed.chars().any(char::is_control) {
        return Err("it contains a control character".to_string());
    }
    let mut components = Path::new(trimmed).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(only)), None) if only == std::ffi::OsStr::new(trimmed) => {
            Ok(trimmed.to_string())
        }
        (Some(Component::ParentDir), _) => {
            Err("it climbs out of the attachments directory with `..`".to_string())
        }
        (Some(Component::RootDir | Component::Prefix(_)), _) => {
            Err("it is an absolute path; a name is relative to its session".to_string())
        }
        _ => Err("it is not a single ordinary path segment".to_string()),
    }
}

/// The media type to record for `file_name`.
///
/// Why: #7370 requires an unknown type to be stored as
/// [`FALLBACK_MEDIA_TYPE`], never guessed at from the client's word alone. The
/// extension is the more trustworthy of the two signals — a browser sends
/// whatever the OS told it, and a scripted upload sends whatever it likes — so
/// the guess wins, and the declared value is consulted only when there is no
/// guess to be had.
/// What: `mime_guess` from the extension; otherwise a well-formed declared
/// `type/subtype`; otherwise [`FALLBACK_MEDIA_TYPE`].
/// Test: `super::tests::store_tests::media_type_prefers_the_extension`,
/// `super::tests::store_tests::unknown_media_type_becomes_octet_stream`.
fn resolve_media_type(file_name: &str, declared: Option<&str>) -> String {
    if let Some(guess) = mime_guess::from_path(file_name).first() {
        return guess.essence_str().to_string();
    }
    let declared = declared.map(str::trim).unwrap_or_default();
    let essence = declared.split(';').next().unwrap_or_default().trim();
    let well_formed = essence.split('/').count() == 2
        && essence
            .split('/')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_graphic()))
        && !essence.contains(['\\', '"']);
    if well_formed && essence != FALLBACK_MEDIA_TYPE {
        return essence.to_ascii_lowercase();
    }
    FALLBACK_MEDIA_TYPE.to_string()
}
