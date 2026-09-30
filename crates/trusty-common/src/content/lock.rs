//! `content-lock.toml` — the pin on the installed content bundle.

use std::path::Path;

use serde::Deserialize;

use super::ContentError;
use crate::integrity::Sha256Digest;

/// The tag prefix every content release carries (`content-vX.Y.Z`).
pub const TAG_PREFIX: &str = "content-v";

/// The pinned content release: its tag and the sha256 of its bundle.
///
/// Why: ADR-0064 decision 5 (i) — the lock pins the installed release's tag
/// and a sha256 of the bundle, and the resolver refuses a bundle whose bytes
/// do not match. The tag also names the cached bundle file, so it is
/// validated as `content-vX.Y.Z[-pre]` before it ever reaches a path.
/// What: two fields, both validated on construction and on load.
/// [`ContentLock::store`] writes it atomically through
/// [`crate::atomic_file::write_atomic`].
/// Test: `lock_round_trips_through_store_and_load`,
/// `lock_rejects_a_tag_that_would_escape_the_cache`,
/// `lock_load_rejects_a_malformed_sha256`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentLock {
    tag: String,
    sha256: Sha256Digest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLock {
    tag: String,
    sha256: String,
}

impl ContentLock {
    /// Builds a lock after validating `tag` as a content release tag.
    ///
    /// Test: `lock_rejects_a_tag_that_would_escape_the_cache`.
    pub fn new(tag: impl Into<String>, sha256: Sha256Digest) -> Result<Self, ContentError> {
        let tag = tag.into();
        validate_tag(&tag)?;
        Ok(Self { tag, sha256 })
    }

    /// Reads and validates the lock at `path`.
    ///
    /// A missing file is [`ContentError::NotInstalled`]; any other read
    /// failure is [`ContentError::LockUnreadable`]; bad TOML, an unknown key,
    /// a bad tag or a bad digest is [`ContentError::LockInvalid`].
    ///
    /// Test: `resolve_without_a_lock_is_not_installed`,
    /// `resolve_with_an_unreadable_lock_fails_closed`,
    /// `lock_load_rejects_a_malformed_sha256`.
    pub fn load(path: &Path) -> Result<Self, ContentError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ContentError::NotInstalled {
                    lock_path: path.to_path_buf(),
                });
            }
            Err(source) => {
                return Err(ContentError::LockUnreadable {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        let invalid = |reason: String| ContentError::LockInvalid {
            path: path.to_path_buf(),
            reason,
        };
        let raw: RawLock = toml::from_str(&text).map_err(|e| invalid(e.to_string()))?;
        let sha256 = Sha256Digest::parse_hex(&raw.sha256).map_err(|e| invalid(e.to_string()))?;
        Self::new(raw.tag, sha256).map_err(|e| invalid(e.to_string()))
    }

    /// Writes the lock to `path` atomically, creating its parent directory.
    ///
    /// Test: `lock_round_trips_through_store_and_load`.
    pub fn store(&self, path: &Path) -> Result<(), ContentError> {
        let write_err = |source| ContentError::LockWrite {
            path: path.to_path_buf(),
            source,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(write_err)?;
        }
        let body = format!(
            "# Written by the trusty content resolver (ADR-0064). Do not edit.\n\
             tag = \"{}\"\nsha256 = \"{}\"\n",
            self.tag, self.sha256
        );
        crate::atomic_file::write_atomic(path, body.as_bytes()).map_err(write_err)
    }

    /// The pinned release tag, e.g. `content-v0.1.0`.
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// The pinned sha256 of the bundle.
    pub fn sha256(&self) -> &Sha256Digest {
        &self.sha256
    }

    /// The cached bundle's file name: `<tag>.tar.gz`, the release asset name.
    pub fn bundle_file_name(&self) -> String {
        format!("{}.tar.gz", self.tag)
    }
}

/// Accepts exactly `content-v` + SemVer `X.Y.Z[-pre]`, the grammar
/// `scripts/package_content.sh` enforces when it cuts a release.
///
/// Why: the tag becomes a file name inside the cache directory, so anything
/// outside this grammar (a `/`, a `..`) is refused before a path is built.
/// Test: `lock_rejects_a_tag_that_would_escape_the_cache`.
fn validate_tag(tag: &str) -> Result<(), ContentError> {
    let invalid = || ContentError::InvalidTag {
        tag: tag.to_owned(),
    };
    let version = tag.strip_prefix(TAG_PREFIX).ok_or_else(invalid)?;
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let numeric = |part: &str| {
        !part.is_empty()
            && part.bytes().all(|b| b.is_ascii_digit())
            && (part == "0" || !part.starts_with('0'))
    };
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() != 3 || !parts.iter().all(|p| numeric(p)) {
        return Err(invalid());
    }
    if let Some(pre) = pre {
        let ok = !pre.is_empty()
            && pre
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-');
        if !ok {
            return Err(invalid());
        }
    }
    Ok(())
}
