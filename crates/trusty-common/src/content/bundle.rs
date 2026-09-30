//! Loading an installed bundle: verify the bytes, then unpack them in memory.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path};

use serde::Deserialize;

use super::{ContentError, ContentLock};
use crate::integrity::Sha256Digest;

/// The manifest entry `scripts/package_content.sh` writes first in every bundle.
pub(super) const MANIFEST_ENTRY: &str = "bundle-manifest.toml";

#[derive(Deserialize)]
struct BundleManifest {
    tag: String,
}

/// Reads the bundle the lock pins, checks its sha256 and its manifest tag, and
/// returns every regular file keyed by its bundle-relative path.
///
/// Why: the served bytes must be the verified bytes. The bundle is read once
/// into memory, hashed, and unpacked from that same buffer, so nothing can
/// change the content between the check and the read, and no unpacked tree
/// on disk exists to be edited behind the lock's back.
/// What: missing -> `BundleMissing`; unreadable -> `BundleUnreadable`; digest
/// differs -> `ChecksumMismatch`; not a gzip tar, an unsafe entry path, a
/// link entry or no manifest -> `BundleCorrupt`; manifest tag differs from the
/// lock -> `TagMismatch`.
/// Test: `resolve_refuses_a_bundle_whose_sha256_does_not_match`,
/// `resolve_refuses_a_bundle_whose_manifest_names_another_tag`,
/// `resolve_refuses_a_bundle_with_a_climbing_entry`,
/// `resolve_with_a_missing_bundle_fails_closed`.
pub(super) fn load_verified(
    cache_dir: &Path,
    lock: &ContentLock,
) -> Result<BTreeMap<String, Vec<u8>>, ContentError> {
    let path = cache_dir.join(lock.bundle_file_name());
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ContentError::BundleMissing { path });
        }
        Err(source) => return Err(ContentError::BundleUnreadable { path, source }),
    };
    let actual = Sha256Digest::of_bytes(&bytes);
    if &actual != lock.sha256() {
        return Err(ContentError::ChecksumMismatch {
            path,
            expected: lock.sha256().clone(),
            actual,
        });
    }
    let corrupt = |reason: String| ContentError::BundleCorrupt {
        path: path.clone(),
        reason,
    };
    let entries = unpack(&bytes).map_err(corrupt)?;
    let manifest = entries
        .get(MANIFEST_ENTRY)
        .ok_or_else(|| corrupt(format!("no {MANIFEST_ENTRY} entry")))?;
    let manifest = std::str::from_utf8(manifest)
        .map_err(|e| corrupt(format!("{MANIFEST_ENTRY} is not UTF-8: {e}")))?;
    let manifest: BundleManifest =
        toml::from_str(manifest).map_err(|e| corrupt(format!("{MANIFEST_ENTRY}: {e}")))?;
    if manifest.tag != lock.tag() {
        return Err(ContentError::TagMismatch {
            lock_tag: lock.tag().to_owned(),
            bundle_tag: manifest.tag,
        });
    }
    Ok(entries)
}

/// Unpacks a gzip tar into `path -> bytes`, refusing any entry the packager
/// never writes (links, devices, absolute or `..` paths).
fn unpack(bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut files = BTreeMap::new();
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let kind = entry.header().entry_type();
        let raw = entry.path().map_err(|e| e.to_string())?.into_owned();
        let name = relative_key(&raw)
            .ok_or_else(|| format!("entry {} is not a relative path", raw.display()))?;
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return Err(format!("entry {name} is not a regular file"));
        }
        let mut data = Vec::new();
        entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
        files.insert(name, data);
    }
    Ok(files)
}

/// Joins a path's components with `/` when every one is a plain name; `None`
/// for an absolute path, a `..`, or an empty path. `.` components are dropped.
pub(super) fn relative_key(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}
