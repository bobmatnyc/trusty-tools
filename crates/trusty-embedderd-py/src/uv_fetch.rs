//! The pinned, SHA-256-verified `uv` cache — the offline half of #9468 (D1a).
//!
//! Why: owner ruling D1a — use an installed uv, else a pinned, verified one.
//! The download feeding [`install_verified`] is PR B2; no network here.
//! What: `resolve_uv` orders `TRUSTY_UV_BIN` → installed uv → the verified
//! cache at `<data>/py-embedder/uv/<UV_VERSION>/uv`.
//! Test: `uv_fetch_tests.rs`; `locate_uv_*` in `bootstrap_tests.rs`.

use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

use fs4::FileExt;
use trusty_common::integrity::{IntegrityError, Sha256Digest};

/// The pinned uv release (#9468, delivery plan of 2026-10-08).
pub const UV_VERSION: &str = "0.12.23";

/// Name prefix of an install's temp file, `.uv.tmp.<pid>`.
const TEMP_PREFIX: &str = ".uv.tmp.";

/// SHA-256 of `uv-<triple>.tar.gz` for [`UV_VERSION`] — the only trust root.
/// Ported from PR #9473, checked there against Astral's `.sha256` sidecars.
#[rustfmt::skip]
const PINNED_DIGESTS: &[(&str, &str)] = &[
    ("aarch64-apple-darwin", "50487ae565ccd96e499056b4674d438f4c53170202617b4c759defe0c6a1b544"),
    ("x86_64-unknown-linux-gnu", "9167d72b3319674b6303c4cbe071854bba13ebdf3d76b1a7cbdc175471fb66d6"),
];

/// Why `uv` could not be resolved or cached (#9468); never a fall-through.
/// Test: `digest_mismatch_places_nothing_and_names_both_digests`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum UvError {
    #[error("TRUSTY_UV_BIN={value:?} does not point to an existing file")]
    BadOverride { value: String },
    #[error(
        "`uv` not found on PATH or in the well-known install dirs (/opt/homebrew/bin, \
         /usr/local/bin, ~/.local/bin, ~/.cargo/bin), TRUSTY_UV_BIN is unset, and no \
         verified uv is cached at {}; fetching the pinned uv {UV_VERSION} is not yet \
         available (#9468). Install uv (https://docs.astral.sh/uv/) or set \
         TRUSTY_UV_BIN=/path/to/uv. Until then trusty-search uses the Rust ort embedder.",
        cache.display()
    )]
    NotFound { cache: PathBuf },
    #[error("cached uv {} is not usable and was not run: {reason}", path.display())]
    CacheRejected { path: PathBuf, reason: String },
    #[error("uv archive {} failed its checksum; nothing extracted: {source}", archive.display())]
    Checksum {
        archive: PathBuf,
        source: IntegrityError,
    },
    #[error("uv archive {} is unusable: {reason}", archive.display())]
    Archive { archive: PathBuf, reason: String },
    #[error("{action} {}: {source}", path.display())]
    Io {
        action: &'static str,
        path: PathBuf,
        source: io::Error,
    },
}

/// The order behind `bootstrap::locate_uv` (#9468, D1a).
///
/// Why: an installed uv always wins; a bad override or a refused cache is an
/// error, never a fall-through to something less trusted.
/// What: `explicit` → `installed()` → [`cached_uv`] under `py_root()`, which
/// runs only when no uv is installed.
/// Test: `resolve_uv_prefers_explicit_override_and_never_fetches`,
/// `resolve_uv_prefers_path_then_the_verified_cache`,
/// `nothing_installed_or_cached_says_the_fetch_is_not_yet_available`.
pub(crate) fn resolve_uv(
    explicit: Option<&str>,
    installed: &dyn Fn() -> Option<PathBuf>,
    py_root: &dyn Fn() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<PathBuf> {
    if let Some(value) = explicit {
        let path = PathBuf::from(value);
        if !path.is_file() {
            let value = value.to_owned();
            return Err(UvError::BadOverride { value }.into());
        }
        return Ok(path);
    }
    if let Some(found) = installed() {
        return Ok(found);
    }
    let root = py_root()?;
    let cache = cache_path(&root);
    cached_uv(&root)?.ok_or_else(|| UvError::NotFound { cache }.into())
}

/// `<py_root>/uv/<UV_VERSION>/uv`, where `py_root` is `<data>/py-embedder`.
pub fn cache_path(py_root: &Path) -> PathBuf {
    py_root.join("uv").join(UV_VERSION).join("uv")
}

/// The embedded archive digest for `triple`; `None` for an unsupported host.
pub fn pinned_digest(triple: &str) -> Option<Sha256Digest> {
    let (_, hex) = PINNED_DIGESTS.iter().find(|(t, _)| *t == triple)?;
    Sha256Digest::parse_hex(hex).ok()
}

/// The verified cached uv under `py_root`, if an install completed (#9468).
///
/// Why: a hit must prove the file is the one an install verified (#9473
/// review finding "cache re-verify"), not merely that it exists.
/// What: `Ok(None)` when the binary or its `uv.sha256` is absent. A symlink,
/// a non-file, or bytes that no longer match the digest is `CacheRejected`.
/// Test: `a_tampered_cached_uv_is_refused_not_run`.
pub fn cached_uv(py_root: &Path) -> Result<Option<PathBuf>, UvError> {
    let path = cache_path(py_root);
    let reject = |reason: String| UvError::CacheRejected {
        path: path.clone(),
        reason,
    };
    let read = (
        fs::symlink_metadata(&path),
        fs::read_to_string(path.with_file_name("uv.sha256")),
    );
    let (meta, recorded) = match read {
        (Ok(meta), Ok(recorded)) => (meta, recorded),
        (Err(e), _) | (_, Err(e)) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        (Err(e), _) | (_, Err(e)) => return Err(reject(format!("cannot read it: {e}"))),
    };
    if !meta.file_type().is_file() {
        return Err(reject("not a regular file".to_owned()));
    }
    Sha256Digest::from_sidecar(&recorded)
        .and_then(|want| Sha256Digest::of_file(&path)?.verify(&want))
        .map_err(|e| reject(e.to_string()))?;
    Ok(Some(path))
}

/// Check `archive` against `expected` (from [`pinned_digest`]), then place its
/// `uv` at [`cache_path`]. PR B2 calls this after its download.
///
/// Why: the cache must only ever hold a complete, verified binary.
/// What: a digest mismatch returns `Checksum` (naming both digests) and
/// creates nothing. Then, under `.fetch.lock`, sweeps every leftover temp
/// file, writes the `uv` entry to a 0755 temp file (removed on failure),
/// renames it into place, and records its digest in `uv.sha256`.
/// Test: `digest_match_installs_an_executable_uv`,
/// `digest_mismatch_places_nothing_and_names_both_digests`,
/// `unsafe_archive_entries_are_refused`,
/// `concurrent_fetches_download_once_and_place_a_whole_file`,
/// `stale_temp_files_are_swept_before_a_fetch`.
pub fn install_verified(
    py_root: &Path,
    archive: &Path,
    expected: &Sha256Digest,
) -> Result<PathBuf, UvError> {
    let bytes = fs::read(archive).map_err(io_err("read uv archive", archive))?;
    if let Err(source) = Sha256Digest::of_bytes(&bytes).verify(expected) {
        return Err(UvError::Checksum {
            archive: archive.to_path_buf(),
            source,
        });
    }
    let uv = extract_uv(&bytes, archive)?;
    let dest = cache_path(py_root);
    let dir = dest.parent().unwrap_or(py_root);
    fs::create_dir_all(dir).map_err(io_err("create uv cache dir", dir))?;
    let lock_path = dir.join(".fetch.lock");
    let lock = fs::File::create(&lock_path).map_err(io_err("open uv lock", &lock_path))?;
    FileExt::lock_exclusive(&lock).map_err(io_err("flock uv lock", &lock_path))?;
    sweep_stale_temps(dir)?;
    // Unique under the flock, which excludes every other installer.
    let tmp = dir.join(format!("{TEMP_PREFIX}{}", std::process::id()));
    let placed = write_executable(&tmp, &uv)
        .and_then(|()| fs::rename(&tmp, &dest).map_err(io_err("rename uv to", &dest)));
    if placed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    placed?;
    // A torn digest file fails `cached_uv`'s parse: refused, never run.
    let digest = dest.with_file_name("uv.sha256");
    let line = format!("{}  uv\n", Sha256Digest::of_bytes(&uv).as_hex());
    fs::write(&digest, line).map_err(io_err("write uv digest", &digest))?;
    tracing::info!(uv = %dest.display(), "py-embedder: cached verified uv {UV_VERSION}");
    Ok(dest)
}

/// The bytes of the archive's `uv` entry; nothing is written to disk here.
///
/// Why: defence in depth behind the digest — an entry path that leaves the
/// archive root, or a `uv` that is a link, fails the whole archive.
/// What: any entry path with a `..`, root or prefix component is refused; the
/// first entry whose basename is `uv` must be a regular file.
/// Test: `unsafe_archive_entries_are_refused`.
fn extract_uv(tarball: &[u8], archive: &Path) -> Result<Vec<u8>, UvError> {
    let err = |reason: String| UvError::Archive {
        archive: archive.to_path_buf(),
        reason,
    };
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
    for entry in tar.entries().map_err(|e| err(format!("reading: {e}")))? {
        let mut entry = entry.map_err(|e| err(format!("reading entry: {e}")))?;
        let path = entry
            .path()
            .map_err(|e| err(format!("path: {e}")))?
            .into_owned();
        let shown = path.display();
        let safe = |c: Component| matches!(c, Component::Normal(_) | Component::CurDir);
        if !path.components().all(safe) {
            return Err(err(format!("entry {shown} escapes the archive root")));
        }
        if path.file_name().is_none_or(|name| name != "uv") {
            continue;
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() {
            let reason = format!("entry {shown} is a {kind:?}, not a regular file");
            return Err(err(reason));
        }
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|e| err(format!("reading uv: {e}")))?;
        return Ok(data);
    }
    Err(err("no `uv` file in the archive".to_owned()))
}

/// Remove every `.uv.tmp.*` entry in `dir`, whatever pid wrote it (#9468).
///
/// Why: an install killed mid-write never removes its temp file, which
/// would stay for good and, under a reused pid, fail `create_new`.
/// What: runs under the flock, so no live installer owns a match. A failed
/// list or removal is an `Io` error and the install stops before writing.
/// Test: `stale_temp_files_are_swept_before_a_fetch`,
/// `a_stale_temp_that_cannot_be_removed_stops_the_install`.
fn sweep_stale_temps(dir: &Path) -> Result<(), UvError> {
    let entries = fs::read_dir(dir).map_err(io_err("list uv cache dir", dir))?;
    for entry in entries {
        let entry = entry.map_err(io_err("list uv cache dir", dir))?;
        if entry.file_name().to_string_lossy().starts_with(TEMP_PREFIX) {
            let path = entry.path();
            fs::remove_file(&path).map_err(io_err("remove stale uv temp file", &path))?;
        }
    }
    Ok(())
}

/// Create `path` with mode 0755 (Unix) and sync it; [`sweep_stale_temps`]
/// has already removed any leftover at `path`.
fn write_executable(path: &Path, data: &[u8]) -> Result<(), UvError> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o755);
    let mut file = opts.open(path).map_err(io_err("create", path))?;
    let written = file.write_all(data).and_then(|()| file.sync_all());
    written.map_err(io_err("write", path))
}

fn io_err(action: &'static str, path: &Path) -> impl FnOnce(io::Error) -> UvError {
    let path = path.to_path_buf();
    move |source| UvError::Io {
        action,
        path,
        source,
    }
}

#[cfg(test)]
#[path = "uv_fetch_tests.rs"]
mod tests;
