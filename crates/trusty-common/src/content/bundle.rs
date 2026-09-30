//! Loading an installed bundle: verify the bytes, then unpack them in memory.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Component, Path};

use serde::Deserialize;

use super::{ContentError, ContentLock};
use crate::integrity::Sha256Digest;

/// The manifest entry `scripts/package_content.sh` writes first in every bundle.
pub(super) const MANIFEST_ENTRY: &str = "bundle-manifest.toml";

/// Largest bundle file read. content-v0.1.0 is 0.65 MB; this is ~100x that.
pub const MAX_BUNDLE_BYTES: u64 = 64 * 1024 * 1024;

/// Largest total of entry sizes unpacked. content-v0.1.0 unpacks to 2.0 MB;
/// this is ~128x that, and bounds a gzip bomb.
pub const MAX_UNPACKED_BYTES: u64 = 256 * 1024 * 1024;

/// Most tar entries read. content-v0.1.0 has 270; this is ~74x that.
pub const MAX_BUNDLE_ENTRIES: usize = 20_000;

/// Tar framing allowed per permitted entry on top of the unpacked cap: its
/// header and padding plus pax and GNU long-name/long-link records.
const STREAM_SLACK_PER_ENTRY: u64 = 4 * 1024;

/// The caps [`load_verified_with`] enforces; tests lower them.
#[derive(Debug, Clone, Copy)]
pub(super) struct Limits {
    /// Cap on the bundle file's size.
    pub bundle_bytes: u64,
    /// Cap on the sum of every entry's size, as tar reads it (a pax `size=`
    /// record included). The decompressed stream is capped at this plus
    /// [`STREAM_SLACK_PER_ENTRY`] per permitted entry.
    pub unpacked_bytes: u64,
    /// Cap on the number of entries, directories included.
    pub entries: usize,
}

impl Limits {
    /// The production caps.
    pub(super) const DEFAULT: Self = Self {
        bundle_bytes: MAX_BUNDLE_BYTES,
        unpacked_bytes: MAX_UNPACKED_BYTES,
        entries: MAX_BUNDLE_ENTRIES,
    };

    /// Cap on the decompressed tar stream: every byte tar reads, extension
    /// records included, which tar reads whole before [`unpack`] sees an entry.
    fn stream_bytes(self) -> u64 {
        let entries = u64::try_from(self.entries).unwrap_or(u64::MAX);
        self.unpacked_bytes.saturating_add(
            entries
                .saturating_add(1)
                .saturating_mul(STREAM_SLACK_PER_ENTRY),
        )
    }
}

/// A reader that fails once more than `cap` bytes have passed through it,
/// counting into `read` so the caller can tell that failure from corruption.
struct CappedReader<'c, R> {
    inner: R,
    read: &'c Cell<u64>,
    cap: u64,
}

impl<R: Read> Read for CappedReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let over = || std::io::Error::other("decompressed stream over its cap");
        let used = self.read.get();
        if used > self.cap {
            return Err(over());
        }
        // One byte past the cap is enough to know the cap was exceeded.
        let room = usize::try_from((self.cap - used).saturating_add(1)).unwrap_or(usize::MAX);
        let len = buf.len().min(room);
        let n = self.inner.read(&mut buf[..len])?;
        let used = used + n as u64;
        self.read.set(used);
        if used > self.cap { Err(over()) } else { Ok(n) }
    }
}

#[derive(Deserialize)]
struct BundleManifest {
    tag: String,
}

/// Why [`unpack`] refused an archive.
enum UnpackError {
    /// Not an archive the packager could have written.
    Corrupt(String),
    /// Over one of the [`Limits`].
    TooLarge(String),
}

/// Reads the bundle the lock pins, checks its sha256 and its manifest tag, and
/// returns every regular file keyed by its bundle-relative path.
///
/// Why: the served bytes must be the verified bytes. The bundle is read once
/// into memory, hashed, and unpacked from that same buffer, so nothing can
/// change the content between the check and the read, and no unpacked tree
/// on disk exists to be edited behind the lock's back.
/// What: missing -> `BundleMissing`; unreadable -> `BundleUnreadable`; over a
/// size, unpacked-size or entry-count cap -> `BundleTooLarge`; digest differs
/// -> `ChecksumMismatch`; not a gzip tar, an unsafe entry path, a link or
/// device entry, a duplicate entry or no manifest -> `BundleCorrupt`; manifest
/// tag differs from the lock -> `TagMismatch`.
/// Test: `resolve_refuses_a_bundle_whose_sha256_does_not_match`,
/// `resolve_refuses_a_bundle_whose_manifest_names_another_tag`,
/// `resolve_refuses_a_bundle_with_a_climbing_entry`,
/// `resolve_refuses_a_bundle_with_a_duplicate_entry`,
/// `bundle_over_a_cap_is_too_large`,
/// `a_pax_size_override_counts_against_the_unpacked_cap`,
/// `resolve_with_a_missing_bundle_fails_closed`.
pub(super) fn load_verified(
    cache_dir: &Path,
    lock: &ContentLock,
) -> Result<BTreeMap<String, Vec<u8>>, ContentError> {
    load_verified_with(cache_dir, lock, Limits::DEFAULT)
}

/// [`load_verified`] under explicit [`Limits`].
pub(super) fn load_verified_with(
    cache_dir: &Path,
    lock: &ContentLock,
    limits: Limits,
) -> Result<BTreeMap<String, Vec<u8>>, ContentError> {
    let path = cache_dir.join(lock.bundle_file_name());
    let bytes = read_capped(&path, limits.bundle_bytes)?;
    let actual = Sha256Digest::of_bytes(&bytes);
    actual
        .verify(lock.sha256())
        .map_err(|_| ContentError::ChecksumMismatch {
            path: path.clone(),
            expected: lock.sha256().clone(),
            actual: actual.clone(),
        })?;
    let corrupt = |reason: String| ContentError::BundleCorrupt {
        path: path.clone(),
        reason,
    };
    let entries = unpack(&bytes, limits).map_err(|e| match e {
        UnpackError::Corrupt(reason) => corrupt(reason),
        UnpackError::TooLarge(reason) => ContentError::BundleTooLarge {
            path: path.clone(),
            reason,
        },
    })?;
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

/// Reads `path` whole, refusing it before the read when its size exceeds
/// `cap`, and after it when it grew past `cap` meanwhile.
fn read_capped(path: &Path, cap: u64) -> Result<Vec<u8>, ContentError> {
    let unreadable = |source| ContentError::BundleUnreadable {
        path: path.to_path_buf(),
        source,
    };
    let too_large = |len: u64| ContentError::BundleTooLarge {
        path: path.to_path_buf(),
        reason: format!("the file is {len} bytes, over the {cap}-byte cap"),
    };
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ContentError::BundleMissing {
                path: path.to_path_buf(),
            });
        }
        Err(source) => return Err(unreadable(source)),
    };
    let len = file.metadata().map_err(unreadable)?.len();
    if len > cap {
        return Err(too_large(len));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.take(cap.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    let read = bytes.len() as u64;
    if read > cap {
        return Err(too_large(read));
    }
    Ok(bytes)
}

/// Unpacks a gzip tar into `path -> bytes`, refusing any entry the packager
/// never writes (links, devices, absolute or `..` paths, a path twice) and any
/// archive over `limits`.
///
/// Why: a gzip bomb must fail before it is decompressed, and tar reads some
/// bytes on its own — a pax `size=` override, pax and GNU long-name records —
/// that a check on header fields never sees (#8378 review).
/// What: two binding bounds. The decompressed stream is read through a
/// [`CappedReader`] at [`Limits::stream_bytes`]; each entry's size as tar
/// reads it ([`tar::Entry::size`], not the raw header field) is summed against
/// `unpacked_bytes` before its data is read. The data is also read through a
/// `take` of the room left under `unpacked_bytes`; for a regular entry that
/// guard cannot trip, since tar yields at most `size` bytes and the sum check
/// already bounded it. It stays as defence in depth.
/// Test: `bundle_over_a_cap_is_too_large`,
/// `a_pax_size_override_counts_against_the_unpacked_cap`,
/// `an_oversized_extension_record_is_too_large`.
fn unpack(bytes: &[u8], limits: Limits) -> Result<BTreeMap<String, Vec<u8>>, UnpackError> {
    let streamed = Cell::new(0u64);
    let stream_cap = limits.stream_bytes();
    let io_err = |e: std::io::Error| {
        if streamed.get() > stream_cap {
            UnpackError::TooLarge(format!(
                "the decompressed stream is over {stream_cap} bytes"
            ))
        } else {
            UnpackError::Corrupt(e.to_string())
        }
    };
    let mut archive = tar::Archive::new(CappedReader {
        inner: flate2::read::GzDecoder::new(bytes),
        read: &streamed,
        cap: stream_cap,
    });
    let mut files = BTreeMap::new();
    let (mut count, mut total) = (0usize, 0u64);
    for entry in archive.entries().map_err(io_err)? {
        let mut entry = entry.map_err(io_err)?;
        count += 1;
        if count > limits.entries {
            return Err(UnpackError::TooLarge(format!(
                "more than {} entries",
                limits.entries
            )));
        }
        // Sizes count before any data is read — directories too, since
        // skipping an entry still decompresses its data. #8378 review:
        // `entry.size()` carries a pax `size=` override; the header's does not.
        let before = total;
        total = total.saturating_add(entry.size());
        if total > limits.unpacked_bytes {
            return Err(UnpackError::TooLarge(format!(
                "entries declare more than {} bytes",
                limits.unpacked_bytes
            )));
        }
        let kind = entry.header().entry_type();
        let raw = entry.path().map_err(io_err)?.into_owned();
        let name = relative_key(&raw).ok_or_else(|| {
            UnpackError::Corrupt(format!("entry {} is not a relative path", raw.display()))
        })?;
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() {
            return Err(UnpackError::Corrupt(format!(
                "entry {name} is not a regular file"
            )));
        }
        let room = limits.unpacked_bytes - before;
        let mut data = Vec::new();
        (&mut entry)
            .take(room.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(io_err)?;
        if data.len() as u64 > room {
            return Err(UnpackError::TooLarge(format!(
                "entry {name} holds more than {room} bytes"
            )));
        }
        // #8378 review: `./a` and `a` normalise to one key; a second entry
        // must not silently replace the first.
        if files.insert(name.clone(), data).is_some() {
            return Err(UnpackError::Corrupt(format!("entry {name} appears twice")));
        }
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
